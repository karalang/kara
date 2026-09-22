//! String, f-strings, chars, formatting, regex, JSON, display -- fixtures for `tests/memory_sanitizer.rs`.
//!
//! Split out of `tests/memory_sanitizer.rs` on 2026-09-21. The TEST TARGET is
//! unchanged: this file is a module of that target, so
//! `cargo test --features llvm --test memory_sanitizer` still runs everything
//! and CI needs no edit. Run this area alone with:
//!
//!     cargo test --features llvm --test memory_sanitizer strings::
//!
//! New fixtures about String, f-strings, chars, formatting, regex, JSON, display belong in this file.

use super::*;

/// B-2026-09-04-14 — the BALANCE half of
/// `e2e_retained_source_veto_does_not_escape_its_function`.
///
/// The fix CLEARS a veto set per function, and a veto exists precisely to
/// stop a disarm that would leak: a source a read-only arm left owning its
/// payload has no other owner to hand off to. So the failure mode this
/// change could introduce is the opposite of the crash it fixes — a
/// function's OWN arm's veto going missing, and the payload leaking rather
/// than being freed twice. A transcript cannot see that; the `dR` lines are
/// identical either way.
///
/// `retained` is the cell that carries the risk: its arm publishes the veto
/// and its own body must still honour it. `mover` and `moverstr` collide
/// with it on the name `o`, and `unshared` uses names nothing else does.
#[test]
fn asan_retained_source_veto_does_not_escape_its_function() {
    assert_clean_asan_run(
        r#"struct R { id: i64, s: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}:{self.s}") } }
fn mk(n: i64) -> R { return R { id: n, s: f"s{n}" }; }

fn retained() { let t = (mk(4), Option.Some(mk(104))); let (_, o) = t; match o { Option.Some(r) => { println(f"  g{r.id}") }, Option.None => { println("  n") } } }
fn mover()    { let o = Option.Some(mk(11)); let q = o; println(f"  m{q.is_some()}") }
fn moverstr() { let o = Option.Some(f"z12"); let q = o; println(f"  s{q.is_some()}") }
fn unshared() { let d = Option.Some(mk(13)); let e = d; println(f"  d{e.is_some()}") }

fn main() {
  println("mover");    mover()
  println("moverstr"); moverstr()
  println("unshared"); unshared()
  println("retained"); retained()
  println("done")
}
"#,
        &[
            "mover",
            "  mtrue",
            "dR11:s11",
            "moverstr",
            "  strue",
            "unshared",
            "  dtrue",
            "dR13:s13",
            "retained",
            "dR4:s4",
            "  g104",
            "dR104:s104",
            "done",
        ],
        "b12-retained-veto-scope",
    );
}

/// B-2026-08-30-3 — an f-string ARM TAIL is a minting tail, and every
/// consuming gate now frees it.
///
/// `expr_yields_fresh_owned_temp` recognizes `Call`/`MethodCall` only, so
/// `branch_tail_mints_fresh_owned_temp_inner` declined an
/// `InterpolatedStringLit` tail and the rendered buffer reached the merge
/// owned by nobody. The accumulator registers its own scope cleanup and
/// `suppress_block_tail_cleanup` zeroes that alloca's `cap` on the way out
/// of the arm — correct in itself, the value escapes — which is what leaves
/// the hole: a disarm with no counterpart.
///
/// THE ROW MEASURED TWO SPELLINGS; THERE WERE NINE. Every shape below
/// leaked 2 B per evaluation before the fix, unbounded in a loop
/// (`-O0`, valgrind, one fixture per shape):
///
/// | shape | pre `-O0` | pre `-O2` |
/// |---|---|---|
/// | `b` receiver `if` | 2 B | 2 B |
/// | `d` by-value call argument | 2 B | clean |
/// | `p1` bare block | 2 B | 2 B |
/// | `p2` nested block | 2 B | 2 B |
/// | `p3` `match` | 2 B | 2 B |
/// | `p4` `else if` chain | 2 B | 2 B |
/// | `p5` `len` fast path | 2 B | clean |
/// | `p6` f-string interpolation | 2 B | 2 B |
/// | `p7` concat operand | 2 B | 2 B |
/// | `v.push` (OWNING) | clean | clean |
/// | `p8` `let` (OWNING) | clean | clean |
///
/// Seven of the nine leak at the DEFAULT `-O2`, so this fixture gates on
/// the ordinary leg rather than needing `scripts/asan-o0-leg.sh` — worth
/// stating because the row's one opt-level note (`d` is `-O0`-only) reads
/// like the whole population is. `p5` is the second `-O0`-only spelling and
/// was not previously known.
///
/// THE TWO OWNING DESTINATIONS ARE THE POINT OF THE CONTROLS. `v.push` and
/// the `let` were ALREADY clean, on both backends and at both opt levels:
/// they take the value over rather than consuming it in place. They are
/// here to catch the failure this fix could have introduced — a second
/// owner, i.e. a double free — which ASAN reports as an error rather than a
/// leak. `p8` is read twice for the same reason.
///
/// The call-argument gate's extra `!rhs_stages_fstr_acc` term (`d`) is the
/// one thing the row flagged as unknown, and `d` being clean here is the
/// measurement that answers it: the predicate matches a DIRECT f-string,
/// never a branch wrapping one, so the double-materialization staging it
/// guards stays inert for this shape.
#[test]
fn asan_branch_tail_fstring_arm_frees_once() {
    assert_clean_asan_run_min_allocs(
        r#"
fn use_s(s: String) -> i64 { return s.len(); }

fn main() {
    let n: i64 = env.args().len();
    let c = n > 0;

    let b = if c { f"x{n}" } else { f"y{n}" }.contains("x");
    let d = use_s(if c { f"x{n}" } else { f"y{n}" });
    println(f"b={b} d={d}");

    let p1 = { f"p{n}" }.contains("p");
    let p2 = { { f"q{n}" } }.contains("q");
    println(f"p1={p1} p2={p2}");

    let p3 = match n { 0 => f"m{n}", _ => f"o{n}" }.contains("o");
    let p4 = if n < 0 { f"u{n}" } else if c { f"v{n}" } else { f"w{n}" }.contains("v");
    println(f"p3={p3} p4={p4}");

    let p5 = if c { f"r{n}" } else { f"s{n}" }.len();
    let p6 = f"[{if c { f"t{n}" } else { f"z{n}" }}]";
    let p7 = if c { f"c{n}" } else { f"e{n}" } + "-tail";
    println(f"p5={p5} p6={p6} p7={p7}");

    let mut v: Vec[String] = [];
    v.push(if c { f"k{n}" } else { f"l{n}" });
    println(f"v0={v[0]}");

    let p8 = if c { f"g{n}" } else { f"h{n}" };
    println(f"p8={p8} again={p8}");
}
"#,
        &[
            "b=true d=2",
            "p1=true p2=true",
            "p3=true p4=true",
            "p5=2 p6=[t1] p7=c1-tail",
            "v0=k1",
            "p8=g1 again=g1",
        ],
        "asan_branch_tail_fstring_arm_frees_once",
        // 35 measured under the audit (3 of those are the ASAN runtime's
        // own startup allocations, so 32 are the fixture's). The floor
        // guards the direction that makes a clean-run assertion vacuous:
        // if a future pipeline change folds these f-strings away, the
        // fixture would pass over memory it never touched.
        20,
    );
}

/// B-2026-09-01-9 — a branch MIXING an f-string arm with a string-LITERAL
/// arm frees the f-string, and an aliased-place arm still does not.
///
/// Its parent (B-2026-08-30-3, the fixture above) admitted an
/// `InterpolatedStringLit` tail, closing every all-f-string spelling. A
/// MIXED branch still declined, because the predicate was fail-closed on
/// EVERY tail and a `StringLit` tail was not admitted — so the f-string
/// arm's rendered buffer stranded once per evaluation. Measured at that
/// fix, `-O0`, valgrind: 2 B / 1 block for each of `a1` (mixed `if`),
/// `a2` (the mirror, literal `then`) and `a3` (`match`) below. The row
/// listed only the first; the other two were measured while closing it and
/// leak identically.
///
/// THE WIDENING IS NOT "MINTS OR IS HARMLESS", which is what the parent row
/// warned would put every future candidate up for re-argument at each of
/// the eight gates the predicate feeds. It is "every tail is ACCOUNTED FOR,
/// and at least one MINTS", over a CLOSED two-member set — `Mints`, or a
/// string literal, which is rodata (`cap == 0`, naming no place). See
/// `BranchTailClass` in `codegen/stmts.rs`.
///
/// `c1`/`c2` ARE THE CONTROLS THAT MATTER, and they are aliased-place arms:
/// a tail handing back a BINDING leaves it readable afterwards, so a
/// use-site free there would DANGLE rather than leak — strictly worse than
/// the leak being closed. Both read the binding AFTER the branch, once with
/// the place arm not taken and once with it taken, so a regression that
/// admitted a place tail shows up as an ASAN use-after-free rather than as
/// a quiet wrong answer. `c3` is the all-literal construct, which was
/// already clean and must stay declining: `InertLiteral` propagates upward
/// so it never qualifies on its own.
#[test]
fn asan_branch_tail_mixed_fstring_and_literal_frees_once() {
    assert_clean_asan_run_min_allocs(
        r#"
fn mk(n: i64) -> String { return f"v{n}"; }

fn main() {
    let n: i64 = env.args().len();
    let c = n > 0;

    let a1 = if c { f"i{n}" } else { "lit" }.contains("i");
    let a2 = if not c { "lit" } else { f"i{n}" }.contains("i");
    let a3 = match n { 0 => "lit", _ => f"m{n}" }.contains("m");
    println(f"a1={a1} a2={a2} a3={a3}");

    let a4 = if c { if c { f"p{n}" } else { "q" } } else { "lit" }.contains("p");
    println(f"a4={a4}");

    let loc1 = mk(3);
    let c1 = if c { loc1 } else { "lit" }.contains("v");
    println(f"c1={c1} loc1={loc1}");

    let loc2 = mk(4);
    let c2 = if not c { "lit" } else { loc2 }.contains("v");
    println(f"c2={c2} loc2={loc2}");

    let c3 = if c { "lit1" } else { "lit2" }.contains("lit");
    println(f"c3={c3}");
}
"#,
        &[
            "a1=true a2=true a3=true",
            "a4=true",
            "c1=true loc1=v3",
            "c2=true loc2=v4",
            "c3=true",
        ],
        "asan_branch_tail_mixed_fstring_and_literal_frees_once",
        // Same purpose as the parent's floor: guard the direction that
        // makes a clean-run assertion vacuous, so a future pipeline change
        // that folds these f-strings away cannot let the fixture pass over
        // memory it never touched.
        10,
    );
}

/// B-2026-09-03-5 — rendering a `ref` parameter must READ THROUGH the
/// reference and must not take ownership of what it reads.
///
/// Every Display arm that renders a named place used to hand the
/// synthesized Display fn `variables[name].ptr`. For a `ref`/`mut ref`
/// param that alloca holds the CALLER'S ADDRESS, so the renderer decoded
/// pointer bits as a control block: `ref Vec`/`ref Set` segfaulted,
/// `ref Map` printed `{}`, `ref Option` printed `None`, and `ref Result`
/// read out of bounds and printed adjacent process memory. The correctness
/// half is pinned by `e2e_ref_param_display_reaches_the_pointee_not_the_
/// pointer` in `tests/codegen.rs`; this is the MEMORY half, and it is a
/// distinct risk: now that the renderer reaches the caller's real buffers
/// rather than garbage, a renderer that registered them for cleanup would
/// free storage the CALLER still owns. Every value here is heap-bearing and
/// is read again after the call, in a loop so a per-iteration double-free
/// or leak accumulates rather than hiding in a single pass.
#[test]
fn asan_ref_param_display_does_not_take_the_callers_buffers() {
    assert_clean_asan_run_min_allocs(
        r#"
fn pv(x: ref Vec[String]) { println(x); println(f"{x}"); }
fn pa(a: ref Array[String, 2]) { println(f"{a}"); }
fn po(x: ref Option[String]) { println(x); println(f"{x}"); }
fn pr(x: ref Result[String, String]) { println(f"{x}"); }
fn pm(x: mut ref Map[String, String]) { println(x); }

fn main() {
    let mut i: i64 = 0;
    while i < 3 {
        let v: Vec[String] = [f"a{i}", f"b{i}"];
        pv(v);
        println(f"own-v={v[0]}");

        let arr: Array[String, 2] = [f"p{i}", f"q{i}"];
        pa(arr);
        println(f"own-a={arr[1]}");

        let o: Option[String] = Some(f"o{i}");
        po(o);
        println(f"own-o={o.is_some()}");

        let r: Result[String, String] = Ok(f"r{i}");
        pr(r);

        let mut m: Map[String, String] = Map.new();
        m.insert(f"k{i}", f"w{i}");
        pm(mut m);
        println(f"own-m={m.len()}");

        i = i + 1;
    }
}
"#,
        &[
            "[a0, b0]",
            "[a0, b0]",
            "own-v=a0",
            "[p0, q0]",
            "own-a=q0",
            "Some(o0)",
            "Some(o0)",
            "own-o=true",
            "Ok(r0)",
            "{k0: w0}",
            "own-m=1",
            "[a1, b1]",
            "[a1, b1]",
            "own-v=a1",
            "[p1, q1]",
            "own-a=q1",
            "Some(o1)",
            "Some(o1)",
            "own-o=true",
            "Ok(r1)",
            "{k1: w1}",
            "own-m=1",
            "[a2, b2]",
            "[a2, b2]",
            "own-v=a2",
            "[p2, q2]",
            "own-a=q2",
            "Some(o2)",
            "Some(o2)",
            "own-o=true",
            "Ok(r2)",
            "{k2: w2}",
            "own-m=1",
        ],
        "asan_ref_param_display_does_not_take_the_callers_buffers",
        // Floor guards the vacuous direction: if a future change constant-
        // folds these renders away the fixture would pass over memory it
        // never touched. Three iterations x five heap-bearing values is far
        // above this.
        30,
    );
}

/// B-2026-08-31-19 — the memory half of teaching codegen to render an
/// `Array[T, N]`.
///
/// An array owns no buffer of its own, but its ELEMENTS can: an
/// `Array[String, 2]` holds two `{ptr, len, cap}` triples inline. Display
/// appends a COPY of those bytes into a fresh accumulator and must not
/// touch the element buffers — so the two ways to get the new renderer
/// wrong are freeing what it rendered (a double free at the owner's scope
/// exit, or a use-after-free on the read below) and registering the
/// rendered temporary for a cleanup it does not own.
///
/// `a` is rendered TWICE and then READ again, so a renderer that freed it
/// surfaces here rather than staying latent. The loop mints a fresh array
/// per iteration through a CALL RESULT, the non-place spelling that spills
/// to a temporary slot, so a spurious registration leaks or double-frees 20
/// times over instead of once. `nested` puts the same elements one level
/// down, where a different renderer walks them.
#[test]
fn asan_array_display_does_not_take_its_elements() {
    let Some((out, status)) = run_under_asan(
        r#"fn mk(n: i64) -> Array[String, 2] {
    return [f"abcdefghijklmnopqrstuvwxyz{n}", f"0123456789012345678901234{n}"]
}

fn main() {
    let n: i64 = env.args().len();
    let mut i = 0;
    while i < 20 {
        println(f"call {mk(i + n)}");
        i = i + 1;
    }

    let a: Array[String, 2] = [f"first-element-aaaaaaaaaaaaaaa", f"second-element-bbbbbbbbbbbbbb"];
    println(f"var  {a}");
    println(f"var  {a}");
    println(f"len  {a[0].len()}");

    let mut v: Vec[Array[String, 2]] = Vec.new();
    v.push(a);
    println(f"nest {v}");
    println("end");
}
"#,
        "asan_array_display_does_not_take_its_elements",
    ) else {
        return;
    };
    assert!(status.success(), "ASAN/LSan reported a problem:\n{out}");
    assert_eq!(
        out.matches("call [abcdefghijklmnopqrstuvwxyz").count(),
        20,
        "every iteration must render its array:\n{out}"
    );
    for want in [
        "var  [first-element-aaaaaaaaaaaaaaa, second-element-bbbbbbbbbbbbbb]",
        "len  29",
        "nest [[first-element-aaaaaaaaaaaaaaa, second-element-bbbbbbbbbbbbbb]]",
        "end",
    ] {
        assert!(out.contains(want), "missing {want:?}:\n{out}");
    }
}

/// B-2026-08-31-10 — the memory half of widening the Option/Result Display
/// gate to multi-word payloads.
///
/// Every shape here used to be REFUSED by codegen, so none of them has ever
/// had its ownership exercised on the compiled side. The renderer appends a
/// COPY of the payload's bytes into a fresh accumulator and must not touch
/// the payload's own buffer — so the two ways to get this wrong are a leak
/// (the rendered value's `Vec`/`String`/`Map` never freed) and a
/// double-free/UAF (the renderer freeing a payload its owner still holds).
/// Neither shows up in an output comparison, which is why the E2E twin
/// cannot stand in for this.
///
/// `ov` is rendered TWICE and then READ again, so a renderer that freed what
/// it rendered surfaces as a use-after-free on the `len()` rather than
/// staying latent, and a renderer that freed it twice surfaces as a
/// double-free. `om` and `re` cover the other two payload families the
/// widening admits (a `Map` handle; a `Result`'s error arm), each carrying
/// `String`s so there is real heap behind the handle.
///
/// The loop is a `let`-bound Option per iteration. The interpolated
/// CALL-RESULT spelling (`f"{mk(i)}"`) used to leak its payload once per
/// evaluation — a pre-existing hole in the Option/Result f-string temp path,
/// not one this widening introduced — and now has its own fixture,
/// `asan_option_result_display_call_result_frees_once` (B-2026-08-31-17).
/// Both spellings are kept: this one covers the `let`-bound and repeated
/// variable renders, that one the non-place operands and the place
/// expressions that must NOT be freed here.
/// B-2026-08-31-17 — an `Option`/`Result` CALL RESULT interpolated in an
/// f-string frees its payload once per evaluation.
///
/// `try_compile_option_result_display` spills a non-place value into an
/// `optres.disp.tmp` entry alloca so the by-pointer Display fn has something
/// to load from, and nothing dropped it: `f"{mk(i)}"` in a 20-iteration loop
/// leaked 20 buffers, while the `let`-bound spelling of the same value was
/// clean because scope cleanup owned it. The bare-`String` version of the
/// same hole was B-2026-07-15-12.
///
/// Every shape here is a NON-PLACE operand, which is the whole class: a
/// direct call, a call nested among other interpolations, a constructor
/// applied inline, a `Result` rather than an `Option`, and a payload with
/// depth (`Vec[String]`, which leaked the element buffer AND the `String`s
/// inside it, so a fix that frees only the outer allocation still shows).
/// The `let`-bound and repeated-variable spellings ride along as controls:
/// they were already clean, and a fix that drops a PLACE expression would
/// double-free them rather than leak.
#[test]
fn asan_option_result_display_call_result_frees_once() {
    let Some((out, status)) = run_under_asan(
        r#"fn mks(n: i64) -> Option[String] {
    return Some(f"abcdefghijklmnopqrstuvwxyz{n}")
}

fn mkv(n: i64) -> Option[Vec[String]] {
    let mut v: Vec[String] = Vec.new();
    v.push(f"abcdefghijklmnopqrstuvwxyz{n}");
    return Some(v)
}

fn mkr(n: i64) -> Result[String, i64] {
    return Ok(f"rrrrrrrrrrrrrrrrrrrrrrrrrr{n}")
}

fn mke(n: i64) -> Result[i64, String] {
    return Err(f"eeeeeeeeeeeeeeeeeeeeeeeeee{n}")
}

struct H { o: Option[String], r: Result[String, i64] }

fn main() {
    let n: i64 = env.args().len();

    let mut i = 0;
    while i < 20 {
        println(f"call {mks(i + n)}");
        i = i + 1;
    }

    let mut j = 0;
    while j < 20 {
        println(f"deep {mkv(j + n)}");
        j = j + 1;
    }

    let mut k = 0;
    while k < 20 {
        println(f"ok   {mkr(k + n)} err {mke(k + n)}");
        k = k + 1;
    }

    let mut m = 0;
    while m < 20 {
        println(f"inln {Some(f"iiiiiiiiiiiiiiiiiiiiiiiiii{m + n}")}");
        m = m + 1;
    }

    let o = mks(n);
    println(f"var  {o}");
    println(f"var  {o}");

    let h = H { o: mks(n), r: mkr(n) };
    println(f"fld  {h.o} {h.r}");
    println(f"fld  {h.o} {h.r}");

    let mut vo: Vec[Option[String]] = Vec.new();
    vo.push(mks(n));
    println(f"elem {vo[0]}");
    println(f"elem {vo[0]}");

    let t: (Option[String], i64) = (mks(n), 1);
    println(f"tup  {t.0}");
    println(f"tup  {t.0}");
    println("end");
}
"#,
        "asan_option_result_display_call_result_frees_once",
    ) else {
        return;
    };
    assert!(status.success(), "ASAN/LSan reported a problem:\n{out}");
    for (want, n) in [
        ("call Some(abcdefghijklmnopqrstuvwxyz", 20),
        ("deep Some([abcdefghijklmnopqrstuvwxyz", 20),
        ("ok   Ok(rrrrrrrrrrrrrrrrrrrrrrrrrr", 20),
        ("inln Some(iiiiiiiiiiiiiiiiiiiiiiiiii", 20),
        ("var  Some(abcdefghijklmnopqrstuvwxyz", 2),
        // Place expressions: NOT identifiers, so they reach the same spill
        // arm, but each is owned by something else. Printing twice is the
        // point — a fix that registered them unconditionally would free the
        // owner's buffer on the first render and the second would read
        // freed memory, which is how the Vec sibling's B-2026-08-14-30
        // double free presented.
        ("fld  Some(abcdefghijklmnopqrstuvwxyz", 2),
        ("elem Some(abcdefghijklmnopqrstuvwxyz", 2),
        ("tup  Some(abcdefghijklmnopqrstuvwxyz", 2),
    ] {
        assert_eq!(
            out.matches(want).count(),
            n,
            "`{want}` should render {n} times:\n{out}"
        );
    }
    assert!(out.contains("end"), "program did not reach the end:\n{out}");
}

#[test]
fn asan_option_display_multiword_payload_frees_once() {
    let Some((out, status)) = run_under_asan(
        r#"fn mk(n: i64) -> Option[Vec[String]] {
    let mut v: Vec[String] = Vec.new();
    v.push(f"abcdefghijklmnopqrstuvwxyz{n}");
    return Some(v)
}

fn main() {
    let n: i64 = env.args().len();
    let mut i = 0;
    while i < 20 {
        let o = mk(i + n);
        println(f"call {o}");
        i = i + 1;
    }

    let mut w: Vec[String] = Vec.new();
    w.push(f"0123456789012345678901234567890123456789");
    let ov: Option[Vec[String]] = Some(w);
    println(f"var  {ov}");
    println(f"var  {ov}");
    match ov {
        Some(v) => { println(f"len  {v.len()}"); }
        None => { println("len  none"); }
    }

    let mut m: Map[String, String] = Map.new();
    m.insert(f"kkkkkkkkkkkkkkkkkkkkkkkk", f"vvvvvvvvvvvvvvvvvvvvvvvv");
    let om: Option[Map[String, String]] = Some(m);
    println(f"map  {om}");

    let mut e: Vec[String] = Vec.new();
    e.push(f"eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee");
    let re: Result[i64, Vec[String]] = Err(e);
    println(f"res  {re}");
    println("end");
}
"#,
        "asan_option_display_multiword_payload_frees_once",
    ) else {
        return;
    };
    assert!(status.success(), "ASAN/LSan reported a problem:\n{out}");
    assert_eq!(
        out.matches("call Some([abcdefghijklmnopqrstuvwxyz").count(),
        20,
        "the loop did not render every iteration:\n{out}"
    );
    for want in [
        "var  Some([0123456789012345678901234567890123456789])",
        "len  1",
        "map  Some({kkkkkkkkkkkkkkkkkkkkkkkk: vvvvvvvvvvvvvvvvvvvvvvvv})",
        "res  Err([eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee])",
        "end",
    ] {
        assert!(out.contains(want), "missing {want:?}:\n{out}");
    }
}

#[test]
fn asan_vec_filled_fstring_value_is_not_double_freed() {
    assert_clean_asan_run(
        r#"
fn main() {
    let v: Vec[String] = Vec.filled(3, f"n{1}");
    println(v[0]);
    println(v[2]);
}
"#,
        &["n1", "n1"],
        "asan_vec_filled_fstring_value_is_not_double_freed",
    );
}

/// The `Vec[v; n]` repeat-literal spelling of the same constructor — a
/// DIFFERENT codegen entry point into `build_vec_filled`, so a partial fix
/// that patched only `Vec.filled(...)` fails this test specifically
/// (B-2026-08-21-22).
#[test]
fn asan_vec_repeat_literal_fstring_value_is_not_double_freed() {
    assert_clean_asan_run(
        r#"
fn main() {
    let v: Vec[String] = Vec[f"r{2}"; 3];
    println(v[0]);
    println(v[2]);
}
"#,
        &["r2", "r2"],
        "asan_vec_repeat_literal_fstring_value_is_not_double_freed",
    );
}

/// B-2026-08-21-40 — `local = obj.field` over a live `String` local
/// orphaned the local's OLD buffer.
///
/// THE ROW'S DIAGNOSIS WAS WRONG, and the correction is the point of this
/// test. It was filed as a `mut ref` defect: `free_app(mut b.name)` with a
/// callee that reassigns the parameter, leaking the field's original
/// buffer. `mut ref` turned out to be incidental — it only served to put a
/// HEAP buffer into the field (the struct literal's `"n"` is a static with
/// `cap == 0`, so nothing was allocated until the callee appended). The
/// leak is entirely in the caller's next statement, `last = b.name`, and
/// reproduces with no `mut ref` anywhere.
///
/// The gate was `rhs_is_place_field_move = obj_name.is_none()` — deep
/// places only — justified by "the shallow forms are already covered,
/// their source struct is itself a local whose scope-exit drop reclaims
/// the displaced buffer". That conflates two buffers: the source's field
/// (which the suppression call immediately above has just handed to the
/// target anyway) and the one the TARGET already held, which the source
/// never had a claim on.
///
/// Boundary, measured — the leak needed all three of a one-hop field RHS,
/// a String payload, and a live target. An identifier RHS, a call RHS, a
/// NESTED field RHS, and a `Vec` payload were each already clean, which is
/// why the shape survived a corpus with ~1150 memory fixtures.
///
/// 50 iterations so a per-call imbalance accumulates into an unmissable
/// number rather than hiding in one allocation; `last` deliberately
/// escapes the final iteration's buffer, so a correct run leaks nothing.
#[test]
fn asan_string_local_reassign_from_a_struct_field_frees_the_old_buffer() {
    // (a) the shape as filed, with `mut ref` — 392 bytes (49 x 8) before.
    assert_clean_asan_run(
        r#"
struct Box { name: String }
fn free_app(s: mut ref String) { s = s + "!"; }
fn main() {
    let mut i = 0i64;
    let mut last: String = "";
    while i < 50i64 {
        let mut b = Box { name: "n" };
        free_app(mut b.name);
        last = b.name;
        i = i + 1;
    }
    println(last);
}
"#,
        &["n!"],
        "asan_string_local_reassign_from_a_struct_field_frees_the_old_buffer/mut_ref",
    );
    // (b) the SAME defect with no `mut ref` at all — 98 bytes (49 x 2)
    // before. This is the one that names the real bug, and it is the
    // reason (a) alone would have been a misleading regression test: a fix
    // aimed at the `mut ref` path would have made (a) pass and left this
    // leaking.
    assert_clean_asan_run(
        r#"
struct Box { name: String }
fn main() {
    let mut i = 0i64;
    let mut last: String = "";
    while i < 50i64 {
        let b = Box { name: "n" + "!" };
        last = b.name;
        i = i + 1;
    }
    println(last);
}
"#,
        &["n!"],
        "asan_string_local_reassign_from_a_struct_field_frees_the_old_buffer/plain",
    );
}

#[test]
fn asan_display_option_ref_string_from_get() {
    // B-2026-07-18-40 — displaying `Option[ref String]` (the borrow-typed
    // result of `Vec[String].get(i)` / `.first()` / `.last()`) routes through
    // the owned-`Option[String]` renderer (inline `{ptr,len,cap}` payload,
    // byte-identical). Display is read-only — it appends a COPY of the bytes
    // — so the borrowed Vec buffer must NOT be freed by the display (would
    // double-free with the Vec's own drop). The Vec stays usable afterward.
    assert_clean_asan_run(
        r#"
fn main() {
    let v: Vec[String] = ["alpha", "beta", "gamma"];
    println(v.get(0));
    println(v.first());
    let x = v.get(1);
    println(x);
    println(f"{x}");
    println(v.get(2));
    println(v.len());
}
"#,
        &[
            "Some(alpha)",
            "Some(alpha)",
            "Some(beta)",
            "Some(beta)",
            "Some(gamma)",
            "3",
        ],
        "asan_display_option_ref_string_from_get",
    );
}

#[test]
fn asan_vec_string_var_reassign_loop_no_leak() {
    // B-2026-07-18-52: a whole-`Vec[String]` VARIABLE reassignment
    // (`cur = nxt`, the BFS double-buffer / worklist idiom) freed only the
    // OLD Vec's OUTER buffer and stranded every element String — the
    // move-overwrite eager-free (`emit_owned_vec_element_release_on_overwrite`)
    // handled `Vec[shared]` elements (B-2026-07-12-30) but bailed for a
    // value `String`/`Vec` element, falling to the outer-buffer-only free.
    // 4 overwrites here → 4 stranded generations (each 2 heap Strings)
    // pre-fix; LSan-clean after. Surfaced by kata #126 Word Ladder II
    // (`cur = nxt` over the level frontier). Strings are heap-forced via a
    // char-push builder so the leak is real (a literal is cap-0 rodata).
    assert_clean_asan_run(
        r#"
fn mkstr(c: char, n: i64) -> String {
    let mut s = "";
    let mut i = 0i64;
    while i < n {
        s.push(c);
        i = i + 1;
    }
    s
}
fn main() {
    let mut cur: Vec[String] = Vec.new();
    cur.push(mkstr('a', 6i64));
    let mut r = 0i64;
    while r < 4i64 {
        let mut nxt: Vec[String] = Vec.new();
        nxt.push(mkstr('b', 6i64));
        nxt.push(mkstr('c', 6i64));
        cur = nxt;
        r = r + 1i64;
    }
    println(cur.len().to_string());
}
"#,
        &["2"],
        "asan_vec_string_var_reassign_loop_no_leak",
    );
}

#[test]
fn asan_arena_string_push_fresh_temp_no_leak() {
    // `push(p + "pha")` — the fresh concat temp's buffer is orphaned
    // after the runtime copies the bytes, so the push lowering must
    // materialize it for scope-exit free (the `Interner.intern`
    // posture). One leak per iteration without the materialization.
    assert_clean_asan_run(
        r#"
fn main() {
    let mut i: i64 = 0i64;
    let mut total: i64 = 0i64;
    while i < 40i64 {
        let a: Arena[String] = Arena.new();
        let p = "al";
        let r = a.push(p + "pha");
        total = total + a.get(r).len();
        i = i + 1;
    }
    println(total.to_string());
}
"#,
        // 40 * len("alpha") = 200
        &["200"],
        "arena_string_push_fresh_temp_no_leak",
    );
}

#[test]
fn asan_freshtemp_result_struct_field_println_no_double_free() {
    // B-2026-07-23-4: matching a FRESH-TEMP `Result[W, _]` (a direct `f()`
    // return, not a bound local) whose `Ok(w)` struct payload has a heap
    // field, and passing that field BY VALUE to a free fn (`println(w.s)`),
    // MOVES the field's buffer into the callee — which frees it. The
    // consuming-arm suppressor scored a free-fn arg as non-consuming, so the
    // source `FreeInlineResultPayload` stayed armed and freed the same buffer
    // a second time. Now the wrapper gate detects the heap-field-to-free-fn
    // move and suppresses. Single-shot: the tcache double-free detector
    // fires on the first `println(w.s)` (len("boom") == 4, well past the
    // len-2 reliability floor), and LSan flags any single unfreed block.
    assert_clean_asan_run(
        r#"
struct W { s: String }
fn f() -> Result[W, i64] { Ok(W { s: "boom".to_string() }) }
fn main() {
    match f() {
        Ok(w) => println(w.s),
        Err(e) => println(e.to_string()),
    }
}
"#,
        &["boom"],
        "freshtemp_result_struct_field_println_no_double_free",
    );
}

#[test]
fn asan_if_let_owned_var_enum_string_payload_no_double_free() {
    // B-2026-07-23-13: an OWNED-VARIABLE user-enum scrutinee with a heap
    // payload, destructured in an `if let` — `let e = E.B(s); if let B(t) =
    // e { … }`. The destructure MOVES the String into `t`, but the source
    // `e`'s `__karac_drop_<E>` (queued by `track_enum_var` at its let-site,
    // fires at the outer scope) still read the source's populated payload
    // words and re-freed the same buffer → double-free. The `match` on the
    // SAME enum was already fine — `compile_match` suppresses the source's
    // per-field cleanup for the consumed arm, but the `if let` leg only ran
    // that suppression for a FRESH-TEMP scrutinee, never a plain variable.
    // Fix: `compile_if_let` now mirrors the match path's variable-scrutinee
    // `suppress_destructured_enum_payload_cleanup(value, pattern)` in the
    // non-freshtemp / owned-binding branch. 200 iterations with ≥6 B
    // payloads so a re-free aborts (ASAN) and a missed suppression that
    // instead leaked would show under LSan.
    assert_clean_asan_run(
        r#"
enum E { A(i64), B(String), C }
fn main() {
    let mut i: i64 = 0i64;
    let mut total: i64 = 0i64;
    while i < 200i64 {
        let e: E = E.B(f"payload{i}");
        if let B(t) = e {
            total = total + t.len();
        } else {
            total = total + 1i64;
        }
        i = i + 1;
    }
    println(total.to_string());
}
"#,
        // "payload{i}": len 7 + digits — 10*8 + 90*9 + 100*10 = 1890.
        &["1890"],
        "if_let_owned_var_enum_string_payload_no_double_free",
    );
}

#[test]
fn asan_string_from_owned_source_copies_no_double_free() {
    // B-2026-07-13-8: `String.from(<String>)` returned the source aggregate
    // UNCHANGED (an alias of its `{ptr,len,cap}` buffer), so a fresh owned
    // source — an f-string temp `String.from(f"x")` or an owned String
    // binding `String.from(s)` — was freed BOTH by its own scope-exit
    // cleanup and by the result binding (`free(): double free detected in
    // tcache 2` under JIT/native; interpreter's value-copy was correct). The
    // fix builds a fresh owned copy (the `From` owning contract), so each
    // buffer frees exactly once. 300 iters over all three source shapes
    // (f-string temp, owned binding, string literal): a missed copy
    // double-frees (ASAN); a copy that failed to free the source leaks
    // (LSan).
    assert_clean_asan_run(
        r#"
fn main() {
    let mut i: i64 = 0;
    let mut total: i64 = 0;
    while i < 300 {
        let a: String = String.from(f"fstr{i}");
        total = total + a.len();
        let s: String = f"owned{i}";
        let b: String = String.from(s);
        total = total + b.len();
        let c: String = String.from("literal");
        total = total + c.len();
        i = i + 1;
    }
    println(total.to_string());
}
"#,
        &["6380"],
        "string_from_owned_source_copies_no_double_free",
    );
}

#[test]
fn asan_owned_string_param_if_branch_return_no_leak_or_double_free() {
    // B-2026-07-13-1: an owned String param returned from an `if` branch
    // tail deep-copies (the caller retains the arg buffer). 200 iterations:
    // each call frees two arg temps + one deep-copied result, so a missed
    // copy (double-free, ASAN) or a leaked copy (LSan on Linux CI) both
    // accumulate well past noise. Was: aborted with double-free on the
    // first call before the fix.
    assert_clean_asan_run(
        r#"
fn pick(a: String, b: String) -> String {
    if a > b { a } else { b }
}

fn main() {
    let mut i: i64 = 0i64;
    let mut total: i64 = 0i64;
    while i < 200i64 {
        let r = pick(f"apple{i}", f"banana{i}");
        total = total + r.len();
        i = i + 1;
    }
    println(total.to_string());
}
"#,
        // "banana{i}" is chosen for i 0..199; len is 7 for i<10 (banana0..9),
        // 8 for 10..99, 9 for 100..199. 10*7 + 90*8 + 100*9 = 70+720+900 = 1690.
        &["1690"],
        "owned_string_param_if_branch_return_no_leak_or_double_free",
    );
}

#[test]
fn asan_cstr_to_string_slice_view_not_freed_and_copy_clean() {
    // `CStr.to_string_slice()` returns a BORROWED `{ptr, len, cap=0}` view
    // over the literal's rodata bytes. The `cap == 0` drop-skip must keep
    // the view from being freed — freeing a rodata pointer is an
    // invalid-free (ASan) — while the `.to_string()` copy allocates an
    // owning String that must be freed exactly once (LSan catches a leak).
    // Loop so any invalid-free / leak accumulates and trips the sanitizer.
    assert_clean_asan_run(
        r#"
fn main() {
    let mut i: i64 = 0i64;
    while i < 3i64 {
        match c"hello-view".to_string_slice() {
            Ok(s) => {
                println(s);
                println(s.to_string());
            }
            Err(_) => println("ERR"),
        }
        i = i + 1;
    }
}
"#,
        &[
            "hello-view",
            "hello-view",
            "hello-view",
            "hello-view",
            "hello-view",
            "hello-view",
        ],
        "asan_cstr_to_string_slice_view_not_freed_and_copy_clean",
    );
}

#[test]
fn asan_string_to_cstring_owning_buffer_freed_once() {
    // `String.to_cstring()` returns an OWNING `CString` (`{ptr, len,
    // cap=len+1}`, heap buffer + trailing NUL). Unlike the borrowed
    // `to_string_slice` view, its `cap > 0` buffer MUST be freed exactly once
    // at the `Ok(cs)` binding's scope exit — LSan catches a leak, ASan a
    // double-free / use-after-free. Loop over a runtime-built (concatenated)
    // String so any per-iteration leak/double-free accumulates and trips the
    // sanitizer. The interior-NUL `Err` arm allocates nothing and must be
    // leak-clean too.
    assert_clean_asan_run(
        r#"
fn main() {
    let mut i: i64 = 0i64;
    while i < 3i64 {
        let s = "cs-" + "buf";
        match s.to_cstring() {
            Ok(cs) => {
                println(cs.len());
                let b = cs.as_bytes();
                println(b[0]);
            }
            Err(_) => println("ERR"),
        }
        let bad = "x\u{0}y";
        match bad.to_cstring() {
            Ok(_) => println("OK?"),
            Err(_) => println("interior-nul"),
        }
        i = i + 1;
    }
}
"#,
        &[
            "6",
            "99",
            "interior-nul",
            "6",
            "99",
            "interior-nul",
            "6",
            "99",
            "interior-nul",
        ],
        "asan_string_to_cstring_owning_buffer_freed_once",
    );
}

#[test]
fn asan_sorted_set_string_iter_min_max_no_leak() {
    // B-2026-07-09-16: `SortedSet[String]` ordered observation. The
    // `karac_map_sorted_keys` buffer holds ALIASES into the set's owned key
    // data; the for-loop binds them borrow-like (no free) and frees only the
    // header buffer, while `min`/`max` CLONE the picked key so the returned
    // `Option[String]` owns an independent buffer. LSan flags a leak if a
    // clone is dropped or the header buffer is not freed; ASan flags a
    // double-free if a for-loop binding or a min/max result aliases (and
    // frees) the set's key. Looped so any imbalance accumulates.
    assert_clean_asan_run(
        r#"
fn main() {
    let mut n: i64 = 0i64;
    while n < 3i64 {
        let mut s: SortedSet[String] = SortedSet.new();
        let _ = s.insert(f"banana-{n}-pad-pad");
        let _ = s.insert(f"apple-{n}-pad-pad");
        let _ = s.insert(f"cherry-{n}-pad-pad");
        let mut out: String = "";
        for w in s { out.push_str(w); out.push_str("|"); }
        println(out);
        match s.min() { Some(v) => println(v), None => println("none") }
        match s.max() { Some(v) => println(v), None => println("none") }
        n = n + 1i64;
    }
}
"#,
        &[
            "apple-0-pad-pad|banana-0-pad-pad|cherry-0-pad-pad|",
            "apple-0-pad-pad",
            "cherry-0-pad-pad",
            "apple-1-pad-pad|banana-1-pad-pad|cherry-1-pad-pad|",
            "apple-1-pad-pad",
            "cherry-1-pad-pad",
            "apple-2-pad-pad|banana-2-pad-pad|cherry-2-pad-pad|",
            "apple-2-pad-pad",
            "cherry-2-pad-pad",
        ],
        "asan_sorted_set_string_iter_min_max_no_leak",
    );
}

#[test]
fn asan_sorted_map_string_key_iter_no_leak() {
    // B-2026-07-09-17: `SortedMap[String, String]` ordered observation. The
    // `keys()`/`values()`/`entries()` producers DEEP-CLONE each half into
    // the owned result `Vec` (LSan flags a leak if a clone is dropped or the
    // sorted-key scratch buffer is not freed; ASan flags a double-free if a
    // clone aliases a map buffer), while the `for (k,v)` loop binds
    // borrow-like aliases and frees only the header buffer. Heap KEY *and*
    // heap VALUE, looped, so any imbalance accumulates.
    assert_clean_asan_run(
        r#"
fn main() {
    let mut n: i64 = 0i64;
    while n < 3i64 {
        let mut m: SortedMap[String, String] = SortedMap.new();
        let _ = m.insert(f"kb-{n}-pad-pad", f"vb-{n}-pad-pad");
        let _ = m.insert(f"ka-{n}-pad-pad", f"va-{n}-pad-pad");
        let _ = m.insert(f"kc-{n}-pad-pad", f"vc-{n}-pad-pad");
        let ks = m.keys();
        let mut ko: String = "";
        for k in ks { ko.push_str(k); ko.push_str("|"); }
        println(ko);
        let mut fo: String = "";
        for (k, v) in m { fo.push_str(k); fo.push_str("="); fo.push_str(v); fo.push_str(";"); }
        println(fo);
        n = n + 1i64;
    }
}
"#,
        &[
            "ka-0-pad-pad|kb-0-pad-pad|kc-0-pad-pad|",
            "ka-0-pad-pad=va-0-pad-pad;kb-0-pad-pad=vb-0-pad-pad;kc-0-pad-pad=vc-0-pad-pad;",
            "ka-1-pad-pad|kb-1-pad-pad|kc-1-pad-pad|",
            "ka-1-pad-pad=va-1-pad-pad;kb-1-pad-pad=vb-1-pad-pad;kc-1-pad-pad=vc-1-pad-pad;",
            "ka-2-pad-pad|kb-2-pad-pad|kc-2-pad-pad|",
            "ka-2-pad-pad=va-2-pad-pad;kb-2-pad-pad=vb-2-pad-pad;kc-2-pad-pad=vc-2-pad-pad;",
        ],
        "asan_sorted_map_string_key_iter_no_leak",
    );
}

#[test]
fn asan_string_strip_prefix_suffix_heap_no_leak() {
    // Phase 8 § String — `strip_{prefix,suffix}(p) -> Option[String]`
    // ALLOCATES the owned remainder copy for the matched case
    // (`karac_string_strip_*` → `alloc_string_result`). The memory contract:
    // the `Some(rest)` String drops exactly once, the receiver drops exactly
    // once, and a FRESH-OWNED f-string argument (`strip_prefix(f"exact-{i}")`)
    // is freed by `free_fresh_owned_str_arg` (else it leaks). Covers matched
    // (heap remainder), no-match (None, no alloc), and matched-empty
    // (`Some("")` = `{null,0,0}`, no alloc). Looped so any per-iteration
    // imbalance accumulates for LSan.
    assert_clean_asan_run(
        r#"
fn main() {
    let mut i: i64 = 0i64;
    while i < 2i64 {
        let s: String = f"prefix-{i}-tail-padding";
        match s.strip_prefix("prefix-") {
            Some(rest) => println(rest),
            None => println("none"),
        }
        let t: String = f"head-{i}-suffix-padding";
        match t.strip_suffix("-suffix-padding") {
            Some(head) => println(head),
            None => println("none"),
        }
        let u: String = f"zzz-{i}-padding";
        match u.strip_prefix("nope") {
            Some(r) => println(r),
            None => println("none"),
        }
        let v: String = f"exact-{i}-padding";
        match v.strip_prefix(f"exact-{i}-padding") {
            Some(r) => println(f"empty:{r}"),
            None => println("none"),
        }
        i = i + 1;
    }
}
"#,
        &[
            "0-tail-padding",
            "head-0",
            "none",
            "empty:",
            "1-tail-padding",
            "head-1",
            "none",
            "empty:",
        ],
        "asan_string_strip_prefix_suffix_heap_no_leak",
    );
}

#[test]
fn asan_string_replacen_heap_no_leak() {
    // Phase 8 § String — `replacen(from, to, n) -> String` ALLOCATES a
    // fresh owned result buffer (`karac_string_replacen` →
    // `alloc_string_result`). The memory contract mirrors `replace`: the
    // result String drops once, the receiver drops once, and FRESH-OWNED
    // f-string `from`/`to` arguments are freed by `free_fresh_owned_str_arg`
    // (else they leak once per call, unbounded in the loop). Looped so any
    // per-iteration imbalance accumulates for LSan.
    assert_clean_asan_run(
        r#"
fn main() {
    let mut i: i64 = 0i64;
    while i < 3i64 {
        let s: String = f"a-{i}-b-{i}-c-{i}-d";
        // Fresh-owned f-string args exercise the arg-free path.
        println(s.replacen(f"-{i}-", "|", 2));
        // Negative count clamps to 0 (replace nothing); receiver untouched.
        println(s.replacen(f"-{i}-", "|", -1i64));
        i = i + 1;
    }
}
"#,
        &[
            "a|b|c-0-d",
            "a-0-b-0-c-0-d",
            "a|b|c-1-d",
            "a-1-b-1-c-1-d",
            "a|b|c-2-d",
            "a-2-b-2-c-2-d",
        ],
        "asan_string_replacen_heap_no_leak",
    );
}

#[test]
fn asan_fs_read_lines_vec_string_elements_freed() {
    // B-2026-07-11-38: `fs.read_lines(path) -> Result[Vec[String], IoError]`
    // returns a `Vec[String]` whose per-element String buffers are heap
    // allocations the runtime hands over (`karac_runtime_fs_read_lines`).
    // The `?`-unwrapped binding must free each element String AND the Vec
    // buffer at scope exit — a missed element free leaks one buffer per
    // line per iteration (LSan on Linux CI catches it). The program is
    // self-contained: it writes the fixture with `fs.write`, then reads it
    // back 30× so any per-iteration leak accumulates well past noise.
    assert_clean_asan_run(
        r#"
fn count_bytes(path: String) -> Result[i64, IoError] with reads(FileSystem) {
    let lines = fs.read_lines(path)?;
    let mut total = 0;
    for line in lines {
        total = total + line.len();
    }
    Ok(total)
}
fn main() with reads(FileSystem) writes(FileSystem) {
    match fs.write("/tmp/karac_asan_b38_read_lines.txt", "alpha-line\nbeta-line\n\ndelta-line\n") {
        Ok(_) => {
            let mut i: i64 = 0i64;
            let mut last: i64 = 0i64;
            while i < 30i64 {
                match count_bytes("/tmp/karac_asan_b38_read_lines.txt") {
                    Ok(n) => { last = n; },
                    Err(_) => { last = -1i64; },
                }
                i = i + 1i64;
            }
            println(last.to_string());
        },
        Err(_) => println("write-err"),
    }
}
"#,
        // "alpha-line"(10)+"beta-line"(9)+""(0)+"delta-line"(10) = 29
        &["29"],
        "fs_read_lines_vec_string_elements_freed",
    );
}

#[test]
fn asan_single_element_fstring_vec_return_no_double_free() {
    // B-2026-07-04-1: a fn returning a SINGLE-element `Vec[String]` whose
    // element is an f-string literal (`return Vec[f"…"]`) double-freed the
    // element String under `karac build` (SIGTRAP / exit 133), while a
    // two-element f-string Vec or a `.to_string()` element was clean. The
    // f-string temp's owned-temp free must be suppressed when it is moved
    // into the returned Vec literal.
    assert_clean_asan_run(
        r#"
fn build(i: i64) -> Vec[String] {
    return Vec[f"result-payload-element-number-{i}-aaaaaaaaaaaaaaaaaaaa"];
}
fn main() {
    let mut i: i64 = 0i64;
    while i < 50i64 {
        let r: Vec[String] = build(i);
        println(r[0]);
        i = i + 1;
    }
}
"#,
        &[
            "result-payload-element-number-0-aaaaaaaaaaaaaaaaaaaa",
            "result-payload-element-number-1-aaaaaaaaaaaaaaaaaaaa",
            "result-payload-element-number-2-aaaaaaaaaaaaaaaaaaaa",
            "result-payload-element-number-3-aaaaaaaaaaaaaaaaaaaa",
            "result-payload-element-number-4-aaaaaaaaaaaaaaaaaaaa",
            "result-payload-element-number-5-aaaaaaaaaaaaaaaaaaaa",
            "result-payload-element-number-6-aaaaaaaaaaaaaaaaaaaa",
            "result-payload-element-number-7-aaaaaaaaaaaaaaaaaaaa",
            "result-payload-element-number-8-aaaaaaaaaaaaaaaaaaaa",
            "result-payload-element-number-9-aaaaaaaaaaaaaaaaaaaa",
            "result-payload-element-number-10-aaaaaaaaaaaaaaaaaaaa",
            "result-payload-element-number-11-aaaaaaaaaaaaaaaaaaaa",
            "result-payload-element-number-12-aaaaaaaaaaaaaaaaaaaa",
            "result-payload-element-number-13-aaaaaaaaaaaaaaaaaaaa",
            "result-payload-element-number-14-aaaaaaaaaaaaaaaaaaaa",
            "result-payload-element-number-15-aaaaaaaaaaaaaaaaaaaa",
            "result-payload-element-number-16-aaaaaaaaaaaaaaaaaaaa",
            "result-payload-element-number-17-aaaaaaaaaaaaaaaaaaaa",
            "result-payload-element-number-18-aaaaaaaaaaaaaaaaaaaa",
            "result-payload-element-number-19-aaaaaaaaaaaaaaaaaaaa",
            "result-payload-element-number-20-aaaaaaaaaaaaaaaaaaaa",
            "result-payload-element-number-21-aaaaaaaaaaaaaaaaaaaa",
            "result-payload-element-number-22-aaaaaaaaaaaaaaaaaaaa",
            "result-payload-element-number-23-aaaaaaaaaaaaaaaaaaaa",
            "result-payload-element-number-24-aaaaaaaaaaaaaaaaaaaa",
            "result-payload-element-number-25-aaaaaaaaaaaaaaaaaaaa",
            "result-payload-element-number-26-aaaaaaaaaaaaaaaaaaaa",
            "result-payload-element-number-27-aaaaaaaaaaaaaaaaaaaa",
            "result-payload-element-number-28-aaaaaaaaaaaaaaaaaaaa",
            "result-payload-element-number-29-aaaaaaaaaaaaaaaaaaaa",
            "result-payload-element-number-30-aaaaaaaaaaaaaaaaaaaa",
            "result-payload-element-number-31-aaaaaaaaaaaaaaaaaaaa",
            "result-payload-element-number-32-aaaaaaaaaaaaaaaaaaaa",
            "result-payload-element-number-33-aaaaaaaaaaaaaaaaaaaa",
            "result-payload-element-number-34-aaaaaaaaaaaaaaaaaaaa",
            "result-payload-element-number-35-aaaaaaaaaaaaaaaaaaaa",
            "result-payload-element-number-36-aaaaaaaaaaaaaaaaaaaa",
            "result-payload-element-number-37-aaaaaaaaaaaaaaaaaaaa",
            "result-payload-element-number-38-aaaaaaaaaaaaaaaaaaaa",
            "result-payload-element-number-39-aaaaaaaaaaaaaaaaaaaa",
            "result-payload-element-number-40-aaaaaaaaaaaaaaaaaaaa",
            "result-payload-element-number-41-aaaaaaaaaaaaaaaaaaaa",
            "result-payload-element-number-42-aaaaaaaaaaaaaaaaaaaa",
            "result-payload-element-number-43-aaaaaaaaaaaaaaaaaaaa",
            "result-payload-element-number-44-aaaaaaaaaaaaaaaaaaaa",
            "result-payload-element-number-45-aaaaaaaaaaaaaaaaaaaa",
            "result-payload-element-number-46-aaaaaaaaaaaaaaaaaaaa",
            "result-payload-element-number-47-aaaaaaaaaaaaaaaaaaaa",
            "result-payload-element-number-48-aaaaaaaaaaaaaaaaaaaa",
            "result-payload-element-number-49-aaaaaaaaaaaaaaaaaaaa",
        ],
        "asan_single_element_fstring_vec_return_no_double_free",
    );
}

#[test]
fn asan_b06_5_blanket_vec_string_impl_loop_no_leak() {
    // B-2026-07-06-5 (blanket `impl Trait for Vec[String]`): the impl body
    // iterates the borrowed receiver (`for s in self { out = out + s; }`).
    // `self` is a `ref Vec[String]` (`SelfValue`) — the loop must NOT free
    // the Vec buffer OR its per-element heap Strings (the caller still owns
    // them). Before the SelfValue for-loop arm, `self` fell to the
    // materialize-iterate-DROP value path and double-freed the borrowed
    // heap. Drives the impl both directly (`v.concat()`) and through a
    // bound-generic mono (`callit(v)`) over 40× ≥40-byte payloads for LSan
    // reachability; the source `v` is re-read each round to expose UAF.
    assert_clean_asan_run(
            r#"
trait Joiner {
    fn concat(ref self) -> String;
}
impl Joiner for Vec[String] {
    fn concat(ref self) -> String {
        let mut out = String.new();
        for s in self { out = out + s; }
        out
    }
}
fn callit[C: Joiner](c: ref C) -> String { c.concat() }
fn main() {
    let mut round: i64 = 0i64;
    while round < 40i64 {
        let mut v: Vec[String] = Vec[
            "blanket-vec-string-loop-alpha-aaaaaaaaaaaaaaaa".to_string(),
            "blanket-vec-string-loop-bravo-bbbbbbbbbbbbbbbb".to_string()
        ];
        let direct: String = v.concat();
        let mono: String = callit(v);
        println(f"{direct} {mono} {v.len()}");
        round = round + 1i64;
    }
}
"#,
            [
                "blanket-vec-string-loop-alpha-aaaaaaaaaaaaaaaablanket-vec-string-loop-bravo-bbbbbbbbbbbbbbbb blanket-vec-string-loop-alpha-aaaaaaaaaaaaaaaablanket-vec-string-loop-bravo-bbbbbbbbbbbbbbbb 2",
            ]
            .repeat(40)
            .as_slice(),
            "asan_b06_5_blanket_vec_string_impl_loop_no_leak",
        );
}

#[test]
fn asan_fstring_runtime_formatter_specs_no_leak() {
    // The runtime-formatter format-spec path (binary / center-align /
    // custom-fill, `karac_runtime_fmt_*`) renders into stack buffers whose
    // bytes are copied into the assembled f-string. Loop over a mix of
    // int/float/string holes — and a long-string no-pad branch — so LSan
    // catches any leak of the assembled String or an overrun of the
    // fixed-size render buffers. The final String is the only heap object
    // per iteration and must free exactly once.
    assert_clean_asan_run(
        r#"
fn main() {
    let mut i: i64 = 0i64;
    let mut total: i64 = 0i64;
    while i < 40i64 {
        let name = "kara";
        let s1 = f"[{i:08b}]";
        let s2 = f"[{name:*^12}]";
        let s3 = f"[{i:^10}]";
        let longer = "this-string-is-wider-than-the-width";
        let s4 = f"[{longer:^5}]";
        total = total + s1.len() + s2.len() + s3.len() + s4.len();
        i = i + 1;
    }
    println(total.to_string());
}
"#,
        // s1: "[" + 8 + "]" = 10; s2: "[" + 12 + "]" = 14; s3: "[" + 10 + "]" = 12;
        // s4: "[" + 35 (source wider than width, no pad) + "]" = 37. Sum = 73 per iter * 40 = 2920.
        &["2920"],
        "asan_fstring_runtime_formatter_specs_no_leak",
    );
}

#[test]
fn asan_b04_2_nonterminal_fstring_map_no_leak() {
    // B-2026-07-04-2 sub-part 3 (non-terminal f-string map): `v.iter()
    // .map(|x| f"..").filter(g).collect()` splits at the f-string map —
    // collect the prefix (terminal f-string map -> Vec[String]) into a temp,
    // then continue the filter over the temp. The f-string temp Vec and its
    // Strings are owned once by the temp then cloned once into the result;
    // no staged-accumulator double-free. 40x heap payloads; result elements
    // read inline.
    assert_clean_asan_run(
            r#"
fn main() {
    let mut round: i64 = 0i64;
    while round < 40i64 {
        let v: Vec[i64] = Vec[1i64, 2i64, 3i64, 4i64];
        let r: Vec[String] = v.iter().map(|x| f"payload-element-number-{x}-aaaaaaaaaaaaaaaaaaaa").filter(|s| s.len() > 0i64).collect();
        println(f"{r.len()} {r[0i64]} {r[3i64]}");
        round = round + 1i64;
    }
}
"#,
            [
                "4 payload-element-number-1-aaaaaaaaaaaaaaaaaaaa payload-element-number-4-aaaaaaaaaaaaaaaaaaaa",
            ]
            .repeat(40)
            .as_slice(),
            "asan_b04_2_nonterminal_fstring_map_no_leak",
        );
}

/// Regex codegen (B-2026-07-14-19) — `Regex.compile(pat).unwrap().is_match(s)`
/// looped over a HEAP-backed (cap>0, runtime-built) pattern, so the pattern
/// String is malloc'd every iteration. Exercises the whole ownership path:
/// the owned `pattern` arg into `Regex.compile`, the `Ok(Regex { pattern })`
/// payload, `.unwrap()`, `.is_match(...)`, and the per-iteration scope-exit
/// drop of the `Regex`. Asserts no leak (LSan) + no double-free / UAF
/// (ASAN): the pattern buffer must be owned by EXACTLY one of the arg-temp
/// and the `Regex` payload, freed once. The Err path uses a cap=0 static
/// message (never freed).
#[test]
fn asan_regex_compile_is_match_heap_pattern_no_leak() {
    assert_clean_asan_run(
        r#"
fn main() {
    let mut i = 0i64;
    let mut hits = 0i64;
    while i < 40i64 {
        let mut p = String.from("^a");
        p.push_str(".c$");
        let re = Regex.compile(p).unwrap();
        if re.is_match("abc") { hits = hits + 1i64; }
        if re.is_match("xyz") { hits = hits + 100i64; }
        i = i + 1;
    }
    println(f"{hits}");
}
"#,
        &["40"],
        "asan_regex_compile_is_match_heap_pattern_no_leak",
    );
}

/// Regex slice 2 (B-2026-07-14-19) — `re.find(s) -> Option[Match]` looped
/// over a heap pattern. The riskiest ownership path: the `Match` (a wide,
/// String-bearing struct) is heap-BOXED into the `Option` payload
/// (`coerce_to_payload_words`), and the `Some(m)` destructure unboxes it,
/// so the box, the extracted `Match`, and its owned `text` copy must each
/// be freed exactly once. No leak (LSan) + no double-free / UAF (ASAN)
/// across 40 iterations.
#[test]
fn asan_regex_find_option_match_heap_no_leak() {
    assert_clean_asan_run(
        r#"
fn main() {
    let mut i = 0i64;
    let mut total = 0i64;
    while i < 40i64 {
        let mut p = String.from("[0-9]");
        p.push_str("+");
        let re = Regex.compile(p).unwrap();
        match re.find("abc123def456") {
            Some(m) => { total = total + m.text.len(); }
            None => {}
        }
        i = i + 1;
    }
    println(f"{total}");
}
"#,
        // first match "123" (len 3) each iter: 40 * 3 = 120
        &["120"],
        "asan_regex_find_option_match_heap_no_leak",
    );
}

/// Regex slice 2 — `re.find_all(s) -> Vec[Match]` looped over a heap
/// pattern. Every `Match.text` is an owned substring copy stored in the
/// `Vec` buffer; the `Vec[Match]` drop must recursively free each element's
/// `text` (the B-2026-06-10-5 Vec-of-struct-with-String free machinery).
/// No per-iteration leak of the buffer or any element String.
#[test]
fn asan_regex_find_all_vec_match_heap_no_leak() {
    assert_clean_asan_run(
        r#"
fn main() {
    let mut i = 0i64;
    let mut total = 0i64;
    while i < 40i64 {
        let mut p = String.from("[0-9]");
        p.push_str("+");
        let re = Regex.compile(p).unwrap();
        let ms = re.find_all("a1b22c333");
        for m in ms {
            total = total + m.text.len();
        }
        i = i + 1;
    }
    println(f"{total}");
}
"#,
        // matches "1"+"22"+"333" = 1+2+3 = 6 per iter: 40 * 6 = 240
        &["240"],
        "asan_regex_find_all_vec_match_heap_no_leak",
    );
}

/// Regex slice 2 — `re.replace_all(s, repl) -> String` looped over a heap
/// pattern. Codegen adopts the runtime's fresh malloc'd result buffer as an
/// owned `String` (`cap = max(len, 1) > 0`), freed at scope exit; the extra
/// empty-result call exercises the `len == 0` (`cap = 1`) free path — the
/// runtime always allocates `max(len, 1)` so the drop's `free` still fires.
#[test]
fn asan_regex_replace_all_owned_string_heap_no_leak() {
    assert_clean_asan_run(
        r##"
fn main() {
    let mut i = 0i64;
    let mut total = 0i64;
    while i < 40i64 {
        let mut p = String.from("[0-9]");
        p.push_str("+");
        let re = Regex.compile(p).unwrap();
        let out = re.replace_all("a1b22c333", "#");
        total = total + out.len();
        let empty = re.replace_all("999", "");
        total = total + empty.len();
        i = i + 1;
    }
    println(f"{total}");
}
"##,
        // "a#b#c#" (6) + "" (0) per iter: 40 * 6 = 240
        &["240"],
        "asan_regex_replace_all_owned_string_heap_no_leak",
    );
}

/// String sibling of the Vec shadow above: a ≥36-byte heap `String`
/// (40 chars, past the SSO/short-string window LSan can't see) rebound to
/// the scalar `s.len()`. The old String's buffer must still drop after the
/// `string_vars` tag is purged. ASAN must report clean.
#[test]
fn asan_type_changing_shadow_string_to_scalar_frees_old_buffer() {
    assert_clean_asan_run(
        r#"
fn main() {
    let s = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    let s = s.len();
    println(s);
}
"#,
        &["40"],
        "type_changing_shadow_string_to_scalar",
    );
}

/// B-2026-09-13-22 — the OWNERSHIP half of the fused concat chain.
///
/// The nested form freed each level's fresh-owned operands, and the inner
/// concat's own buffer was itself a fresh temp the outer level freed.
/// Fusing deletes those intermediates, so the frees have to move to the
/// LEAVES — and the two ways to get that wrong are invisible in the printed
/// answer: free a leaf twice (a `.clone()` freed by the concat and again by
/// its own binding cleanup), or stop freeing one at all.
///
/// Cells cover the leaf kinds that differ in who owns them: fresh temps,
/// literals (never freed), a named binding (never freed), a borrowed `ref`
/// param, and a chain in a loop where a leak compounds.
#[test]
fn asan_fused_string_concat_chain_frees_each_leaf_once() {
    const H: &str = "fn seed() -> i64 { env.args().len() }\n\
             fn mk(n: i64) -> String { f\"leaf-{n}-{seed()}-aaaaaaaaaaaaaaaaaaaaaaaaaaaa\" }\n";
    for (label, body, want) in [
            (
                "all-fresh-temps",
                "fn main() { let s = mk(1) + mk(2) + mk(3); println(f\"n:{s.len() > 0}\"); println(\"end\") }\n",
                vec!["n:true", "end"],
            ),
            (
                "fresh-and-literal",
                "fn main() { let s = mk(1) + \"-\" + mk(2) + \"!\"; println(f\"n:{s.len() > 0}\"); println(\"end\") }\n",
                vec!["n:true", "end"],
            ),
            (
                "named-binding-leaf",
                "fn main() { let a = mk(1); let s = a + \"-\" + mk(2);\n\
                 \x20  println(f\"n:{s.len() > 0}\"); println(\"end\") }\n",
                vec!["n:true", "end"],
            ),
            (
                "borrowed-ref-leaf",
                "fn cat(p: ref String) -> String { return p + \"-\" + mk(9) + \"!\" }\n\
                 fn main() { let a = mk(1); let s = cat(ref a);\n\
                 \x20  println(f\"n:{s.len() > 0}/{a.len() > 0}\"); println(\"end\") }\n",
                vec!["end"],
            ),
            (
                "chain-in-a-loop",
                "fn main() { let mut i = 0; let mut n = 0;\n\
                 \x20  while i < 50 { let s = mk(i) + \":\" + mk(i + 1) + \";\"; n = n + s.len(); i = i + 1 }\n\
                 \x20  println(f\"n:{n > 0}\"); println(\"end\") }\n",
                vec!["n:true", "end"],
            ),
            (
                "two-leaf-unfused-control",
                "fn main() { let s = mk(1) + mk(2); println(f\"n:{s.len() > 0}\"); println(\"end\") }\n",
                vec!["n:true", "end"],
            ),
        ] {
            let src = format!("{H}{body}");
            assert_clean_asan_run(&src, &want, label);
        }
}

/// B-2026-09-16-1 — a heap slice is allocated by CODEGEN now, and still
/// has exactly one owner.
///
/// Slices longer than the 23-byte overlay used to be allocated inside
/// `karac_string_slice_into` and freed by the emitted scope cleanup. The
/// allocation moved into codegen; the free did not move. That is precisely
/// the split where an ownership mistake hides, and none of it is visible in
/// the printed bytes:
///
///  - the slice is allocated but never tracked, and every iteration leaks
///    (LSan on the Linux leg; invisible on macOS, which is why this is a
///    ratcheted `-O0` cell and not a local spot-check);
///  - it is tracked twice, or tracked and also freed by a `push_str`
///    reallocation, and the second free is a double free;
///  - the INLINE route's descriptor — which owns nothing — is queued for a
///    free, and libmalloc aborts on a stack address.
///
/// The cells walk the boundary in both directions (23 and 24 bytes), the
/// grow-after-slice case where the buffer is reallocated, a slice stored
/// into a container that outlives the expression, and a loop where a leak
/// compounds rather than showing up once.
/// COVERS THE DEFAULT (`KARAC_SSO=0`) ARM ONLY — `karac_string_slice`'s
/// allocation, not the codegen-owned one. This file compiles in-process and
/// `KARAC_SSO` is a per-process `OnceLock` defaulting to off, so no fixture
/// here can reach the inline-String surface at all. That is a real coverage
/// gap rather than a property of this fixture (B-2026-09-16-35): the ASAN
/// suite has never seen an inline descriptor. The `KARAC_SSO=1` ownership
/// check that exists today is `test_sso_string_slice_routes_match_the_interpreter`
/// in `tests/cli.rs`, which catches a mis-owned buffer by the abort rather
/// than by a sanitizer report.
#[test]
fn asan_heap_string_slice_has_one_owner() {
    const H: &str = "fn seed() -> i64 { env.args().len() }\n\
             fn mk() -> String { f\"slice-me-{seed()}-abcdefghijklmnopqrstuvwxyz-0123456789\" }\n";
    for (label, body, want) in [
            // Either side of the 23-byte overlay boundary, in one program: the
            // inline route owns nothing, the heap route owns its buffer, and
            // the two leave through the same scope cleanup.
            (
                "boundary-both-sides",
                "fn main() { let s = mk();\n\
                 \x20  let a = s[0..23]; let b = s[0..24];\n\
                 \x20  println(f\"n:{a.len()}/{b.len()}\"); println(\"end\") }\n",
                vec!["n:23/24", "end"],
            ),
            // Grown after slicing: `push_str` reallocates the codegen-allocated
            // buffer, so the free must land on the NEW pointer exactly once.
            (
                "grow-after-slice",
                "fn main() { let s = mk(); let mut a = s[0..30];\n\
                 \x20  a.push_str(\"-tail-tail-tail-tail\");\n\
                 \x20  println(f\"n:{a.len()}\"); println(\"end\") }\n",
                vec!["n:50", "end"],
            ),
            // Outliving the expression that made it: the Vec owns the slices
            // and frees them at scope exit, not the slice site.
            (
                "stored-in-a-vec",
                "fn main() { let s = mk(); let mut v: Vec[String] = Vec.new();\n\
                 \x20  let mut i = 0;\n\
                 \x20  while i < 12 { v.push(s[i..(i + 30)]); i = i + 1; }\n\
                 \x20  println(f\"n:{v.len()}/{v[11].len()}\"); println(\"end\") }\n",
                vec!["n:12/30", "end"],
            ),
            // A leak of one buffer per iteration is a rounding error once and
            // obvious at 200.
            (
                "slice-in-a-loop",
                "fn main() { let s = mk(); let mut i = 0; let mut n = 0;\n\
                 \x20  while i < 200 { let t = s[0..(24 + (i % 10))]; n = n + t.len(); i = i + 1; }\n\
                 \x20  println(f\"n:{n > 0}\"); println(\"end\") }\n",
                vec!["n:true", "end"],
            ),
            // The whole string, and the empty slice — the two ends that take
            // neither the inline nor the ordinary heap route.
            (
                "full-and-empty",
                "fn main() { let s = mk(); let f = s[0..s.len()]; let e = s[3..3];\n\
                 \x20  println(f\"n:{f.len() > 0}/{e.len()}\"); println(\"end\") }\n",
                vec!["n:true/0", "end"],
            ),
        ] {
            let src = format!("{H}{body}");
            assert_clean_asan_run(&src, &want, label);
        }
}

/// Borrow-elision negative: each `r` is moved into `keep`, so the gate must
/// KEEP the deep clone — `r` owns an independent buffer that outlives `out`.
/// ASAN confirms no use-after-free (a mis-borrowed `r` would dangle once
/// `out` drops) / double-free / leak. Inner vectors are 8 i64s (64 bytes) for
/// LSan reachability.
#[test]
fn asan_borrow_elision_escape_negative_clones_and_is_clean() {
    assert_clean_asan_run(
        r#"
fn main() {
    let mut out: Vec[Vec[i64]] = Vec.new();
    let mut k = 0i64;
    while k < 32i64 {
        let mut b: Vec[i64] = Vec.new();
        let mut p = 0i64;
        while p < 8i64 { b.push(7i64); p = p + 1i64; }
        out.push(b);
        k = k + 1i64;
    }
    let mut keep: Vec[Vec[i64]] = Vec.new();
    let mut j = 0i64;
    while j < out.len() {
        let r = out[j].clone();
        keep.push(r);
        j = j + 1i64;
    }
    let mut acc = 0i64;
    let mut i = 0i64;
    while i < keep.len() {
        let z = keep[i].clone();
        acc = acc + z[0i64];
        i = i + 1;
    }
    println(acc);
}
"#,
        &["224"],
        "borrow_elision_escape_negative",
    );
}

#[test]
fn asan_nested_receiver_push_string_field_no_leak() {
    // B-2026-07-11-11: `.push()` on a nested place-expression receiver
    // (`g.rows[i].cells.push(s)` — index-then-field) now resolves the
    // receiver pointer through the place chain instead of failing codegen.
    // The pushed String buffers are owned by the innermost Vec, itself
    // owned by the row, itself owned by the grid — dropped exactly once at
    // scope exit. ASAN/LSan guards no leak (every buffer reclaimed) and no
    // double-free (the aliasing field pointer does not mint a second owner).
    assert_clean_asan_run(
        r#"
struct Row { cells: Vec[String] }
struct Grid { rows: Vec[Row] }
fn main() {
    let mut g = Grid { rows: Vec.new() };
    let mut r = 0i64;
    while r < 8i64 {
        g.rows.push(Row { cells: Vec.new() });
        let mut c = 0i64;
        while c < 4i64 {
            g.rows[r].cells.push("cell".to_string());
            c = c + 1i64;
        }
        r = r + 1i64;
    }
    let mut total = 0i64;
    let mut i = 0i64;
    while i < 8i64 {
        total = total + g.rows[i].cells.len();
        i = i + 1;
    }
    println(total);
}
"#,
        &["32"],
        "nested_receiver_push_string_field",
    );
}

#[test]
fn asan_plain_enum_struct_variant_string_payload_no_double_free() {
    // The Weave `ParseError` class: a NON-shared enum struct-variant with a
    // heap (String) payload. A real (cap>0) String local moved into the
    // payload and then CONSUMED — matched both externally and through a
    // `Display(ref self)` impl — must free its buffer exactly once.
    // Pre-fix, three sites each double-freed: construction (no source
    // move-suppression), external match destructuring (struct-variant arm
    // skipped by the cap-suppression), and the `ref self` match (borrowed
    // scrutinee bindings tracked as owned). Looped so any per-iteration
    // double-free trips ASAN. The `cap==0` literal payload masked it before.
    assert_clean_asan_run(
        r#"
enum E { Empty, NoAt { value: String } }
impl Display for E {
    fn to_string(ref self) -> String {
        match self { Empty => "empty", NoAt { value } => f"no-at '{value}'" }
    }
}
fn make(raw: String) -> E {
    let v = raw.clone();
    if not v.contains("@") { return E.NoAt { value: v }; }
    E.Empty
}
fn main() {
    let mut i: i64 = 0;
    let mut count: i64 = 0;
    while i < 50 {
        let raw = f"bad-no-at-{i}";
        let e = make(raw);
        // Render through Display(ref self) — borrowed-scrutinee match ...
        let s = e.to_string();
        if s.len() > 0 { count = count + 1; }
        // ... then destructure externally — owned-scrutinee match.
        match e {
            NoAt { value } => { if value.len() > 0 { count = count + 1; } },
            Empty          => count = count + 0,
        }
        i = i + 1;
    }
    println(count);
}
"#,
        &["100"],
        "plain_enum_struct_variant_string_payload",
    );
}

#[test]
fn asan_string_slice_no_double_free() {
    // StringSlice v1: a borrowed view (`{ptr,len,cap=0}`) aliases the
    // source String's buffer. Its `cap == 0` must keep the scope-exit drop's
    // `cap > 0` guard a no-op, so the view never frees the source's buffer —
    // only the owned source frees it (once), and each `.to_string()` owned
    // copy frees its own. A view returned from `first_word` into the
    // caller's String is the escaping case. A spurious free of the cap=0
    // view (double-free vs the source) would trip ASAN here.
    assert_clean_asan_run(
        r#"
fn first_word(s: ref String) -> StringSlice {
    let sp = s.find(' ');
    let end = sp.unwrap_or(s.len());
    s.slice(0, end)
}
fn main() {
    let s = "hello world".to_string();
    let w = s.slice(0, 5);
    println(w.to_string());
    let fw = first_word(s);
    println(fw.to_string());
}
"#,
        &["hello", "hello"],
        "string_slice_no_double_free",
    );
}

/// B-2026-09-16-35 — the range-slice HEAP route, which is the only fixture
/// in this file that reaches it.
///
/// THIS FIXTURE EXISTS FOR `scripts/asan-sso-leg.sh`, and at the default
/// `KARAC_SSO=0` it asserts almost nothing new: every `String[a..b]` there
/// takes one allocating path regardless of length, already covered above.
/// At `KARAC_SSO=1` the same source splits in two — a slice of 23 bytes or
/// fewer becomes an INLINE descriptor that allocates nothing, and a longer
/// one takes the heap route `compile_string_slice` owns, where codegen
/// emits the allocation and the scope-exit drop frees it. That split is a
/// codegen-owned `malloc` paired with a runtime-owned `free`, which is
/// exactly the seam an ownership mistake hides in.
///
/// WHY THE LENGTHS ARE WHAT THEY ARE. `src[0..30]` is over the 23-byte
/// inline capacity (`Codegen::STRING_INLINE_CAPACITY`) and so is the only
/// thing here that allocates at SSO=1; `src[0..9]` and `src[0..5]` are
/// under it and must allocate NOTHING, so a spurious free of an inline
/// descriptor's interior shows up as an invalid free rather than as a leak.
/// `tail30` returns a 30-byte slice across a call boundary, so the heap
/// descriptor is moved out of the frame that built it. The loop runs the
/// pair three times, which is what turns a leak from 30 bytes into a
/// growing one LSan reports rather than rounds off.
///
/// MEASURED, and the measurement is the reason the fixture is here rather
/// than the lane alone. Reproducing the fault the filing row used — the
/// result aggregate made to point into the SOURCE buffer instead of its own
/// allocation — the whole `asan_string_*` set stayed GREEN at both
/// `KARAC_SSO=0` and `KARAC_SSO=1`, because no fixture sliced past 23 bytes
/// and the heap route was never entered. With this fixture the injected
/// fault aborts the run (`free(): double free detected in tcache 2`) on the
/// SSO lane and still passes at SSO=0, where the route is dead code. So the
/// lane and the fixture are each necessary and neither is sufficient.
#[test]
fn asan_sso_string_range_slice_heap_route_is_balanced() {
    assert_clean_asan_run(
        r#"
fn tail30(s: ref String) -> String {
    s[6..36]
}

fn main() {
    let src = "abcdefghijklmnopqrstuvwxyz0123456789".to_string();
    let mut i = 0;
    while i < 3 {
        let big = src[0..30];
        let small = src[0..9];
        println(big);
        println(small);
        i = i + 1;
    }
    println(src[0..5]);
    let esc = tail30(src);
    println(esc);
}
"#,
        &[
            "abcdefghijklmnopqrstuvwxyz0123",
            "abcdefghi",
            "abcdefghijklmnopqrstuvwxyz0123",
            "abcdefghi",
            "abcdefghijklmnopqrstuvwxyz0123",
            "abcdefghi",
            "abcde",
            "ghijklmnopqrstuvwxyz0123456789",
        ],
        "sso_string_range_slice_heap_route",
    );
}

// ── `String.split` — Vec[String] buffer + per-element String frees ──
//
// Each `split` returns a `Vec[String]` whose buffer and every element
// String are libc::malloc'd by `karac_runtime_string_split`; the binding's
// scope-exit drop must free each element's buffer AND the Vec buffer
// exactly once. Looped 1000× so the Linux-CI LSan gate catches a per-iter
// leak; local mac ASAN catches a double-free / UAF. GAP-W2.
#[test]
fn asan_string_split_no_leak_no_double_free() {
    assert_clean_asan_run(
        r#"
fn main() {
    let mut i: i64 = 0;
    let mut total: i64 = 0;
    while i < 1000 {
        let line = "a,bb,ccc,dddd";
        let fields = line.split(',');
        total = total + fields.len();
        for f in fields { total = total + f.len(); }
        i = i + 1;
    }
    println(f"{total}");
}
"#,
        &["14000"],
        "string_split_loop",
    );
}

// ── `String.lines()` → Vec[String] — per-line heap ownership ──
//
// `lines()` allocates a fresh `Vec[String]`, one malloc'd buffer per
// non-empty line (empty lines are non-owning `{null,0,0}`). Each buffer is
// owned by the result Vec and freed exactly once at scope exit (same
// ownership path as `split`); a missed element drop leaks (LSan), a stray
// alias double-frees (ASAN). Looped 1000× with ≥36-byte lines past any
// short-String fast path; the CRLF + empty-middle-line input exercises the
// `\r`-strip / empty-`{null,0,0}` branches, and the `for l in lines`
// consumes the materialized Vec.
#[test]
fn asan_string_lines_no_leak_no_double_free() {
    assert_clean_asan_run(
        r#"
fn main() {
    let mut i: i64 = 0;
    let mut total: i64 = 0;
    while i < 1000 {
        let text = "first-line-payload-aaaaaaaaaaaaaaaa\r\n\r\nthird-line-payload-bbbbbbbbbbbbbbbb\n";
        let ls = text.lines();
        total = total + ls.len();
        for l in ls { total = total + l.len(); }
        i = i + 1;
    }
    println(f"{total}");
}
"#,
        // lines: ["first…"(35), ""(0), "third…"(35)] → 3 lines/iter,
        // len sum 70; (3 + 70) × 1000 = 73000.
        &["73000"],
        "string_lines_loop",
    );
}

// `String.split_whitespace()` → Vec[String], same per-piece heap ownership
// as `lines` (every piece is a fresh malloc'd buffer — split_whitespace
// never yields an empty `{null,0,0}`). Each freed exactly once at scope
// exit; a missed drop leaks (LSan), a stray alias double-frees (ASAN).
// Looped 1000× with ≥36-byte tokens past any short-String fast path; the
// leading / trailing / repeated whitespace exercises the run-collapsing.
#[test]
fn asan_string_split_whitespace_no_leak_no_double_free() {
    assert_clean_asan_run(
        r#"
fn main() {
    let mut i: i64 = 0;
    let mut total: i64 = 0;
    while i < 1000 {
        let text = "   alpha-token-payload-aaaaaaaaaaaaaa   beta-token-payload-bbbbbbbbbbbbbb   ";
        let ws = text.split_whitespace();
        total = total + ws.len();
        for w in ws { total = total + w.len(); }
        i = i + 1;
    }
    println(f"{total}");
}
"#,
        // Tokens 34 + 33 bytes/iter → (2 + 67) × 1000 = 69000.
        &["69000"],
        "string_split_whitespace_loop",
    );
}

// ── `Vec[String].binary_search(fresh_needle)` — needle-temp ownership ──
//
// `binary_search` itself allocates nothing (the `Option[i64]` result is
// scalar) and only READS the receiver's String elements (no free). The one
// ownership obligation is a FRESH-owned String needle passed directly
// (`v.binary_search(key.to_string())`): the search must free that temp
// exactly once (`free_fresh_owned_str_arg`), and must NOT free the borrowed
// receiver elements. Looped 1000× — LSan catches a per-iter needle leak,
// local ASAN a double-free of the needle or a receiver element. ≥36-byte
// payloads keep every String heap-allocated.
#[test]
fn asan_vec_binary_search_string_needle_no_leak_no_double_free() {
    assert_clean_asan_run(
        r#"
fn main() {
    let mut i: i64 = 0;
    let mut total: i64 = 0;
    while i < 1000 {
        let v: Vec[String] = vec![
            "alpha_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "bravo_bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            "delta_dddddddddddddddddddddddddddddddd",
        ];
        let key: String = "bravo_bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        match v.binary_search(key.to_string()) {
            Some(idx) => total = total + idx,
            None => total = total + 100,
        }
        i = i + 1;
    }
    println(f"{total}");
}
"#,
        // "bravo…" is element 1 every iteration → total = 1000.
        &["1000"],
        "vec_binary_search_string_needle_loop",
    );
}

// ── allocating String→String transforms — fresh-buffer ownership ──
//
// trim / to_lowercase / to_uppercase / replace each return a FRESH heap
// buffer (`karac_string_{trim,to_lowercase,to_uppercase,replace}`), which the
// scope-cleanup machinery must free exactly once. The receiver is untouched
// (a literal's rodata buffer must never be freed; a derived String must not
// be aliased by its transform's result). Looped 1000× — LSan catches a
// per-iter leak (a result never freed), local ASAN catches a double-free (a
// result aliasing the receiver's buffer). The 36-byte trimmed payload stays
// heap-allocated past any short-buffer fast path (≥36 bytes — see the
// LSan-reachability note in lsan-reachability-short-string-leaks.md).
#[test]
fn asan_string_trim_replace_case_no_leak_no_double_free() {
    assert_clean_asan_run(
        r#"
fn main() {
    let mut i: i64 = 0;
    let mut total: i64 = 0;
    while i < 1000 {
        let s: String = "  abcdefghijklmnopqrstuvwxyz0123456789  ";
        let t = s.trim();
        let u = t.to_uppercase();
        let l = u.to_lowercase();
        let r = t.replace("abc", "XY");
        total = total + t.len() + u.len() + l.len() + r.len();
        i = i + 1;
    }
    println(f"{total}");
}
"#,
        &["143000"],
        "string_trim_replace_case_loop",
    );
}

// `String.trim_start()` / `.trim_end()` return FRESH heap buffers
// (`karac_string_trim_{start,end}`), the same allocate-and-hand-back shape
// as `trim` — scope cleanup must free each exactly once and never alias the
// receiver (the literal's rodata must not be freed). Looped 1000× so LSan
// catches a per-iter leak and local ASAN a double-free; the ≥36-byte inner
// payload keeps every result heap-allocated past any short-buffer fast path.
#[test]
fn asan_string_trim_start_end_no_leak_no_double_free() {
    assert_clean_asan_run(
        r#"
fn main() {
    let mut i: i64 = 0;
    let mut total: i64 = 0;
    while i < 1000 {
        let s: String = "   abcdefghijklmnopqrstuvwxyz0123456789   ";
        let a = s.trim_start();
        let b = s.trim_end();
        total = total + a.len() + b.len();
        i = i + 1;
    }
    println(f"{total}");
}
"#,
        // trim_start → 39 (drops 3 leading), trim_end → 39 (drops 3 trailing);
        // 78/iter × 1000 = 78000.
        &["78000"],
        "string_trim_start_end_loop",
    );
}

// `String.sorted()` returns a FRESH heap buffer (`karac_string_sorted`), the
// same allocate-and-hand-back shape as trim / to_uppercase — the scope-cleanup
// machinery must free it exactly once, and it must never alias the receiver's
// buffer (the literal's rodata must not be freed). Looped 1000× so LSan catches
// a per-iter leak (a result never freed) and local ASAN catches a double-free
// (a result aliasing the receiver). The 36-byte payload stays heap-allocated
// past any short-buffer fast path (lsan-reachability-short-string-leaks.md).
#[test]
fn asan_string_sorted_no_leak_no_double_free() {
    assert_clean_asan_run(
        r#"
fn main() {
    let mut i: i64 = 0;
    let mut total: i64 = 0;
    while i < 1000 {
        let s: String = "zyxwvutsrqponmlkjihgfedcba9876543210";
        let k = s.sorted();
        total = total + k.len();
        i = i + 1;
    }
    println(f"{total}");
}
"#,
        &["36000"],
        "string_sorted_loop",
    );
}

// ── `String.split` on a NON-identifier receiver — temp drop ownership ──
//
// `make_csv().split(',')` — a String method on a CALL-RESULT receiver. The
// `try_compile_nonident_collection_method` shim materializes the receiver
// into a synth local and routes through `compile_vec_method`. The receiver's
// heap buffer (`make_csv()`'s String) is freed by the statement-level
// owned-temp machinery; the shim must NOT separately drop-track its slot, or
// the buffer double-frees (a tracked variant SIGABRT'd at scope exit). Plus
// a string-LITERAL receiver (`"...".split`) whose rodata buffer must not be
// freed at all. Looped 1000× — LSan catches a per-iter leak of the receiver
// temp, local ASAN catches the double-free. (phase-7 non-identifier receiver)
#[test]
fn asan_string_method_nonident_receiver_no_leak_no_double_free() {
    assert_clean_asan_run(
        r#"
fn make_csv() -> String { return "a,bb,ccc,dddd"; }
fn main() {
    let mut i: i64 = 0;
    let mut total: i64 = 0;
    while i < 1000 {
        let call_fields = make_csv().split(',');
        total = total + call_fields.len();
        for f in call_fields { total = total + f.len(); }
        let lit_fields = "p,qq,rrr".split(',');
        total = total + lit_fields.len();
        i = i + 1;
    }
    println(f"{total}");
}
"#,
        &["17000"],
        "string_split_nonident_recv_loop",
    );
}

// ── push_str of a fresh-owned String temp (lexer token-text shape) ──
//
// `buffer.push_str(s.substring(a, b))` passes a freshly-malloc'd String
// to push_str, which copies its bytes then frees the temp immediately.
// Pre-fix the temp leaked ~48 bytes/call, unbounded (kata-katas #722
// bench: 93.6 MiB → 1.7 MiB at 2M iters). The new free() must not
// double-free the source buffer nor UAF a later read — looping the
// append a few times makes either trip ASAN.

#[test]
fn asan_push_str_substring_temp_no_double_free() {
    assert_clean_asan_run(
        r#"
fn main() {
    let s: String = "alpha beta gamma delta";
    let mut acc: String = "";
    let mut k = 0i64;
    while k < 4i64 {
        acc.push_str(s.substring(0i64, 5i64));
        acc.push_str("-");
        k = k + 1i64;
    }
    println(acc);
}
"#,
        &["alpha-alpha-alpha-alpha-"],
        "push_str_substring_temp",
    );
}

// ── contains / starts_with of a fresh-owned String temp ───────
//
// `keyword.contains(s.substring(a, b))` / `name.starts_with(tok)` — the
// lexer's keyword-membership and prefix-check surface — pass a freshly-
// malloc'd String the method reads then discards. Codegen frees the temp
// at the post-scan merge block via the shared `free_fresh_owned_str_arg`
// helper (same as push_str). Pre-fix each leaked unbounded (~32 MiB at 2M
// iters); this run guards the frees against double-free / UAF.

#[test]
fn asan_contains_starts_with_substring_temp_no_double_free() {
    assert_clean_asan_run(
        r#"
fn main() {
    let hay: String = "fn let mut while return match";
    let src: String = "returns_here_padded_xxxxxxxxxx";
    let mut hits = 0i64;
    let mut k = 0i64;
    while k < 4i64 {
        if hay.contains(src.substring(0i64, 6i64)) { hits = hits + 1i64; }
        if src.starts_with(src.substring(0i64, 3i64)) { hits = hits + 1i64; }
        k = k + 1i64;
    }
    println(f"{hits}");
}
"#,
        &["8"],
        "contains_starts_with_substring_temp",
    );
}

#[test]
fn asan_string_range_slice_reader_receiver_allocates_nothing() {
    // B-2026-08-18-22 — `s[a..b].len()` and its scalar-reader siblings
    // dispatch on a BORROWED `{ptr, len, cap = 0}` view of the source
    // rather than an allocated slice. `cap = 0` is the existing borrowed
    // marker every `cap > 0` free guard skips, so the correct result here
    // is zero allocations: a leak would mean the owning path was taken,
    // and a double free would mean the view was treated as owned.
    //
    // IN A LOOP, and reading a `ref String` PARAMETER, because both are
    // where the accounting would show. A `ref String` is recorded as
    // `Ref(Str)`, the shape that made the sibling borrowed-slice gate
    // decline in B-2026-08-18-21's investigation; iterating multiplies any
    // per-evaluation slip into something LSan cannot round away.
    assert_clean_asan_run(
        r#"
fn count(s: ref String) -> i64 {
    let mut n = 0;
    let mut i = 0;
    while i < 6 {
        n = n + s[0..5].len();
        if s[0..5].starts_with("he") { n = n + 1; }
        if s[6..11].contains("or") { n = n + 1; }
        i = i + 1;
    }
    return n;
}

fn main() {
    let src: String = "hello world";
    println(count(src));
    println(src.len());
}
"#,
        &["42", "11"],
        "string_range_slice_reader_receiver",
    );
}

#[test]
fn asan_push_str_range_slice_temp_no_double_free() {
    assert_clean_asan_run(
        r#"
fn make(s: ref String) -> String {
    let mut o: String = "";
    o.push_str(s[0..5]);
    o
}

fn main() {
    let src: String = "alpha beta gamma delta";
    let mut acc: String = "";
    let mut k = 0i64;
    while k < 4i64 {
        let d = make(src);
        acc.push_str(d[0..3]);
        k = k + 1i64;
    }
    println(acc);
}
"#,
        &["alpalpalpalp"],
        "push_str_range_slice_temp",
    );
}

#[test]
fn asan_println_method_result_temp_no_leak() {
    assert_clean_asan_run(
        r#"
fn main() {
    let mut i = 0i64;
    while i < 5i64 {
        println(i.to_string());
        i = i + 1;
    }
}
"#,
        &["0", "1", "2", "3", "4"],
        "println_method_result_temp",
    );
}

// ── Match arm whose VALUE is an f-string (phase-12 blocker #3) ──
//
// A direct-f-string match arm (`Some(name) => f"[{name}]"`) builds the
// f-string accumulator, which is `track_vec_var`-registered for the
// per-arm scope cleanup. Before the fix, the per-arm drain freed the
// acc's buffer between the value load and the merge phi, so the match
// result was an empty/dangling String. The fix zeroes the acc's `cap`
// when the arm tail is an f-string (ownership moves to the match result).
// This run exercises the consumed (let-bound / returned) AND discarded
// forms over non-foldable (concat-built) payloads — a stale free of the
// moved buffer trips ASAN's double-free, and the discarded form must
// still single-free via the expression-statement cleanup.

#[test]
fn asan_fstring_match_arm_value_no_double_free() {
    assert_clean_asan_run(
        r#"
enum E { A(String), B(i64) }
fn describe(e: E) -> String {
    match e {
        E.A(name) => f"A[{name}]",
        E.B(k) => f"B[{k}]",
    }
}
fn main() {
    let mut i: i64 = 0;
    while i < 3 {
        let e = E.A("dyn" + "amic");
        let s = describe(e);
        println(s);
        // discarded arm-f-string result — must single-free, no double-free
        let d = E.A("tmp" + "val");
        match d {
            E.A(n) => f"[{n}]",
            E.B(_) => "x",
        };
        i = i + 1;
    }
}
"#,
        &["A[dynamic]", "A[dynamic]", "A[dynamic]"],
        "fstring_match_arm_value",
    );
}

// ── `println(String)` — `%.*s` length-bounded format ──────────
//
// Pre-fix `compile_print`'s struct-value arm passed the String's
// data pointer to `printf("%s\n", str)` directly. LLVM rewrote
// the call to `puts(str)` as a libc-call optimization, and ASAN
// flagged the 1-byte overread when puts walked past the
// non-NUL-terminated heap buffer. String-literal cases worked
// by luck — clang's `c"...\0"` global form puts a NUL right
// after — but any heap-allocated String (concat result, function
// return) overran. The fix routes through `%.*s` with the
// explicit length, so printf reads exactly `len` bytes and the
// libc-call optimizer doesn't substitute puts. Covers four
// shapes that all hit the same struct-value arm: literal,
// heap concat, function-return-via-let-binding, ref-String
// parameter to a heap source.

#[test]
fn asan_println_string_literal_no_overread() {
    // Literal: `cap = 0`, buffer in .rodata. Pre-fix this case
    // worked by luck because the compiler's static-string emitter
    // (clang's `c"...\0"` form) writes a trailing NUL into
    // .rodata even though `cap` doesn't account for it. The fix
    // routes the same buffer through `%.*s` with the explicit
    // length, never depending on the trailing-byte coincidence.
    assert_clean_asan_run(
        r#"
fn main() {
    println("hello literal");
}
"#,
        &["hello literal"],
        "println_string_literal_no_overread",
    );
}

#[test]
fn asan_println_heap_string_concat() {
    // Heap-owning rvalue from concatenation. Pre-fix this case
    // failed with a heap-buffer-overflow at puts because the
    // concat helper allocates exactly `len` bytes (no trailing
    // NUL) and `printf("%s\n", str)` → `puts(str)` walked one
    // past the buffer.
    assert_clean_asan_run(
        r#"
fn main() {
    let a = "left ";
    let b = "right";
    println(a + b);
}
"#,
        &["left right"],
        "println_heap_string_concat",
    );
}

#[test]
fn asan_operand_temp_string_concat_freed() {
    // General owned-temp tracking, slice 3c: a fresh-temp String OPERAND of
    // a string binop. `make_s() + " [suffix]"` reads the fresh `make_s()`
    // buffer into a new concat result but never frees the operand, so it
    // leaks once per iteration (LeakSanitizer on Linux CI). The concat
    // RESULT is bound to `r` and freed by its binding (the operand is the
    // only new leak); a regression that frees the operand twice — or that
    // frees the still-read operand before the concat copies it — double-frees
    // / UAFs (macOS ASAN). ≥36-byte operand defeats LSan short-string
    // reachability; the loop forces per-iteration accumulation.
    assert_clean_asan_run(
        r#"
fn make_s() -> String {
    let s: String = "a freshly allocated heap operand string over thirty-six bytes";
    return s;
}

fn main() {
    let mut i = 0;
    while i < 3 {
        let r = make_s() + " [suffix]";
        println(r);
        i = i + 1;
    };
}
"#,
        &[
            "a freshly allocated heap operand string over thirty-six bytes [suffix]",
            "a freshly allocated heap operand string over thirty-six bytes [suffix]",
            "a freshly allocated heap operand string over thirty-six bytes [suffix]",
        ],
        "operand_temp_string_concat_freed",
    );
}

#[test]
fn asan_primitive_to_string_owning() {
    // `x.to_string()` mallocs an owning String (same shape as the f-string
    // builder). Exercise the let-bound, printed-temp, and concatenated
    // forms so ASAN catches any over-read / double-free / leak in the new
    // primitive `to_string` lowering.
    assert_clean_asan_run(
        r#"
fn main() {
    let n = -42i64;
    let s = n.to_string();
    println(s);
    println(n.to_string());
    println(n.to_string() + "!");
}
"#,
        &["-42", "-42", "-42!"],
        "primitive_to_string_owning",
    );
}

#[test]
fn asan_struct_display_to_string() {
    // User-struct Display renders via the synthetic-f-string path, which
    // mallocs an owning String (and, for nested structs, intermediate
    // Strings registered for scope cleanup). Exercise println + a bound
    // to_string of a nested struct so ASAN catches over-read / leak /
    // double-free in the struct Display lowering.
    assert_clean_asan_run(
        r#"
#[derive(Display)]
struct Point { x: i64, y: i64 }
#[derive(Display)]
struct Wrap { p: Point, name: String, ok: bool }
fn main() {
    let w = Wrap { p: Point { x: 1, y: 2 }, name: "hi", ok: true };
    println(w);
    let s = w.to_string();
    println(s);
}
"#,
        &[
            "Wrap { p: Point { x: 1, y: 2 }, name: hi, ok: true }",
            "Wrap { p: Point { x: 1, y: 2 }, name: hi, ok: true }",
        ],
        "struct_display_to_string",
    );
}

#[test]
fn asan_enum_display_to_string() {
    // All-unit enum `to_string()` mallocs an owning String of the variant
    // name; the enum-in-struct field path renders via the same lowering.
    // ASAN guards the variant-name copy + nested render.
    assert_clean_asan_run(
        r#"
#[derive(Display)]
enum Color { Red, Green, Blue }
#[derive(Display)]
struct Tagged { c: Color, n: i64 }
fn main() {
    let a = Color.Green;
    let s = a.to_string();
    println(s);
    let t = Tagged { c: Color.Blue, n: 9 };
    println(t.to_string());
}
"#,
        &["Green", "Tagged { c: Blue, n: 9 }"],
        "enum_display_to_string",
    );
}

#[test]
fn asan_collection_display_buffer() {
    // The unified buffer-render path mallocs/grows an accumulator for
    // collection println (freed inline), f-string interpolation (scope-
    // tracked), and `.to_string()` (binding-owned). Exercise all three so
    // ASAN catches over-read / leak / double-free across the paths.
    assert_clean_asan_run(
        r#"
fn main() {
    let mut v: Vec[i64] = Vec.new();
    v.push(1);
    v.push(2);
    println(v);
    println(f"v={v}");
    let s = v.to_string();
    println(s);
    let mut m: Map[String, i64] = Map.new();
    m.insert("k", 9);
    println(m.to_string());
}
"#,
        &["[1, 2]", "v=[1, 2]", "[1, 2]", "{k: 9}"],
        "collection_display_buffer",
    );
}

#[test]
fn asan_println_function_return_string_via_let_binding() {
    // Function returns owned heap String; bound to a local;
    // printed. This is the let-binding form the kata workaround
    // used pre-C — even with the materialization fix landed,
    // `println(m)` still hit the overread until this slice.
    assert_clean_asan_run(
        r#"
fn make() -> String {
    let a = "made ";
    let b = "string";
    return a + b;
}

fn main() {
    let m = make();
    println(m);
}
"#,
        &["made string"],
        "println_function_return_string_via_let_binding",
    );
}

#[test]
fn asan_fstring_into_returned_plain_struct_field_no_double_free() {
    // An f-string used DIRECTLY as a struct-literal field value
    // (`Resp { body: f"..." }`) moves the accumulator buffer into the
    // field; the struct is returned, so the caller owns the buffer.
    // Before the fix, `compile_struct_init` left `last_fstr_acc`
    // staged, so the accumulator's scope-exit `FreeVecBuffer` freed
    // the same buffer the returned struct carried — a double-free that
    // aborted under macOS malloc (exit 133). The fix takes + cap-zeros
    // the staged acc at the struct-field site, mirroring the Let /
    // Assign take points. Reading the field three times in the caller
    // surfaces a UAF under ASAN if the buffer were freed early. Covers
    // the non-shared (stack-aggregate) branch of `compile_struct_init`.
    assert_clean_asan_run(
        r#"
struct Resp { status: i64, body: String }
fn make(id: i64, name: String) -> Resp {
    Resp { status: 200, body: f"id={id} name={name}" }
}
fn main() {
    let r = make(7, "Alice");
    println(r.status);
    println(r.body);
    println(r.body);
    println(r.body);
}
"#,
        &[
            "200",
            "id=7 name=Alice",
            "id=7 name=Alice",
            "id=7 name=Alice",
        ],
        "fstring_into_returned_plain_struct_field_no_double_free",
    );
}

#[test]
fn asan_fstring_into_returned_shared_struct_field_no_double_free() {
    // Same double-free, the shared-struct branch of
    // `compile_struct_init` (Arc heap-RC layout — fields stored inline
    // after the refcount header). An f-string field value transfers the
    // buffer into the heap slot, so the staged acc must be suppressed
    // identically. Reading the field twice through the returned handle
    // surfaces a UAF under ASAN if the accumulator freed it early.
    assert_clean_asan_run(
        r#"
shared struct Holder { label: String }
fn make(n: i64) -> Holder {
    Holder { label: f"n={n}" }
}
fn main() {
    let h = make(42);
    println(h.label);
    println(h.label);
}
"#,
        &["n=42", "n=42"],
        "fstring_into_returned_shared_struct_field_no_double_free",
    );
}

#[test]
fn asan_fstring_explicit_return_no_double_free() {
    // Sibling double-free site: a DIRECT `return f"..."` mid-function
    // moves the accumulator buffer to the caller. The `Return` arm's
    // pre-compile suppression is Identifier-only; the accumulator is
    // staged only during `compile_expr`, so without post-compile
    // suppression the scope-cleanup walk freed the returned buffer — a
    // double-free aborting under macOS malloc. Exercises both the
    // early-return arm and the tail-expr arm of the same fn; the caller
    // reads each returned String, which surfaces a UAF under ASAN if the
    // buffer were freed early.
    assert_clean_asan_run(
        r#"
fn pick(id: i64) -> String {
    if id > 0 { return f"pos={id}"; }
    f"nonpos={id}"
}
fn main() {
    let a = pick(5);
    let b = pick(-3);
    println(a);
    println(b);
}
"#,
        &["pos=5", "nonpos=-3"],
        "fstring_explicit_return_no_double_free",
    );
}

#[test]
fn asan_generic_tail_fstring_no_double_free() {
    // A generic (mono) fn whose IMPLICIT TAIL is a bare `f"…"`. The mono
    // path lacked the InterpolatedStringLit-tail cap suppression the
    // non-generic `compile_function` has, so the accumulator buffer was
    // freed between the return-value load and `ret` and the caller's
    // binding then freed the dangling pointer again — a double-free (and a
    // use-after-free when the caller read it). Surfaced via `describe[T:
    // Display](x) { f"..{x}.." }`; the explicit-`return`/`let`-bound forms
    // already had suppression. Covers a `Display` struct arg, a primitive,
    // a String arg, and a no-interp tail — each returned String is read
    // (println) so a UAF trips under ASAN; looped to accumulate any leak.
    assert_clean_asan_run(
        r#"
struct P { x: i64, y: i64 }
impl Display for P { fn to_string(ref self) -> String { f"({self.x}, {self.y})" } }
fn describe[T: Display](item: T) -> String { f"item is {item} padded padded padded" }
fn tag[T](item: T) -> String { f"constant tail padded padded padded" }
fn main() {
    let mut i: i64 = 0i64;
    while i < 2i64 {
        println(describe(P { x: i, y: 2i64 }));
        println(describe(42i64));
        println(describe("hi".to_string()));
        println(tag(7i64));
        i = i + 1;
    }
}
"#,
        &[
            "item is (0, 2) padded padded padded",
            "item is 42 padded padded padded",
            "item is hi padded padded padded",
            "constant tail padded padded padded",
            "item is (1, 2) padded padded padded",
            "item is 42 padded padded padded",
            "item is hi padded padded padded",
            "constant tail padded padded padded",
        ],
        "generic_tail_fstring_no_double_free",
    );
}

#[test]
fn asan_println_ref_string_param_over_heap_source() {
    // `s: ref String` parameter, heap-source caller. The
    // identifier `s` inside `show` loads through the ref param
    // and arrives at compile_print as a struct value (per
    // `load_variable`'s ref-deref). Pre-fix the struct-value
    // arm overread; with `%.*s` it reads exactly `len` bytes.
    // This was the canonical failure shape during the
    // C-followup ASAN test development.
    assert_clean_asan_run(
        r#"
fn show(s: ref String) {
    println(s);
}

fn main() {
    let a = "left ";
    let b = "right";
    show(a + b);
}
"#,
        &["left right"],
        "println_ref_string_param_over_heap_source",
    );
}

#[test]
fn asan_freshtemp_vec_string_get_no_double_free() {
    // Slice 3b-heap: `make_strvec().get(i)` on a FRESH-TEMP `Vec[String]`
    // receiver in a loop. `get` returns `Option[ref String]` — the payload
    // borrows an element *inside* the temp's buffer, which the
    // `__vrecv_tmp` `FreeVecBuffer` frees at the enclosing frame's exit.
    // Two distinct hazards this gates:
    //   (1) DOUBLE-FREE — the `Some(s)` arm binds `s: ref String`; if it
    //       were dropped independently it would free the same buffer the
    //       per-element `FreeVecBuffer` recursion frees. The match path's
    //       `scrutinee_is_borrow_call` suppression must hold for a temp
    //       receiver (it keys off the method, not the object). Caught here
    //       on macOS.
    //   (2) LEAK — the receiver's three per-element String buffers must be
    //       freed by the vec-struct recursion before the outer buffer; a
    //       regression that frees only the outer buffer leaks all three.
    //       Caught by LeakSanitizer on Linux CI. The strings are ≥36 bytes
    //       so LSan cannot dismiss them as reachable short-strings, and the
    //       loop forces per-iteration accumulation.
    assert_clean_asan_run(
        r#"
fn names() -> Vec[String] {
    let mut v: Vec[String] = Vec.new();
    v.push("alpha element string padded well past thirty-six bytes");
    v.push("beta element string also padded past thirty-six bytes!!");
    v.push("gamma element string likewise padded beyond thirty-six b");
    return v;
}

fn main() {
    let mut i = 0;
    while i < 3 {
        match names().get(i) {
            Some(s) => println(s),
            None => println("none"),
        };
        i = i + 1;
    };
}
"#,
        &[
            "alpha element string padded well past thirty-six bytes",
            "beta element string also padded past thirty-six bytes!!",
            "gamma element string likewise padded beyond thirty-six b",
        ],
        "freshtemp_vec_string_get_no_double_free",
    );
}

#[test]
fn asan_freshtemp_vec_struct_string_field_read_no_double_free() {
    // Slice 3f companion: read the borrowed struct's STRING field through the
    // `Option[ref Rec]` borrow (`r.name`), not just the scalar. This exercises
    // the borrow aliasing the very String buffer the agg drop frees — a
    // tighter check that the borrow is read before the frame-exit free and
    // that the field String is freed exactly once.
    assert_clean_asan_run(
        r#"
struct Rec { name: String, n: i64 }

fn make_recs() -> Vec[Rec] {
    let mut v: Vec[Rec] = Vec.new();
    v.push(Rec { name: "alpha name field padded out beyond thirty-six bytes ok", n: 1_i64 });
    v.push(Rec { name: "beta name field padded out beyond thirty-six bytes okk", n: 2_i64 });
    return v;
}

fn main() {
    let mut i = 0;
    while i < 2 {
        match make_recs().get(i) {
            Some(r) => println(r.name),
            None => println("none"),
        };
        i = i + 1;
    };
}
"#,
        &[
            "alpha name field padded out beyond thirty-six bytes ok",
            "beta name field padded out beyond thirty-six bytes okk",
        ],
        "freshtemp_vec_struct_string_field_read_no_double_free",
    );
}

#[test]
fn asan_freshtemp_vec_iter_string_no_double_free() {
    // Slice 3h: `for s in make_v().iter()` on a fresh-temp `Vec[String]`,
    // looped. The for-loop peels `.iter()` and recurses on the temp receiver;
    // codegen materializes it into a `__for_vec_` synth local whose
    // `FreeVecBuffer` (per-element vec-struct recursion) frees each element
    // String + the outer buffer at scope exit. Pre-fix the body was silently
    // skipped (output 0). Hazards: (1) each iteration binds `s` borrowing an
    // element inside the temp's buffer that must NOT be independently dropped,
    // else it double-frees the element String the buffer drop also frees
    // (macOS ASAN); (2) every element String must be freed by the per-element
    // drop before the buffer, else they leak (Linux LSan). ≥36-byte strings
    // defeat LSan short-string reachability; outer loop re-materializes the
    // temp each pass to accumulate any per-pass leak.
    assert_clean_asan_run(
        r#"
fn make_v() -> Vec[String] {
    let mut v: Vec[String] = Vec.new();
    v.push("first iter string padded out beyond thirty-six bytes");
    v.push("second iter string padded out beyond thirty-six byte");
    return v;
}

fn main() {
    let mut pass = 0;
    while pass < 3 {
        let mut total = 0_i64;
        for s in make_v().iter() {
            total = total + s.len();
        };
        println(total);
        pass = pass + 1;
    };
}
"#,
        &["104", "104", "104"],
        "freshtemp_vec_iter_string_no_double_free",
    );
}

#[test]
fn asan_freshtemp_map_iter_string_key_no_double_free() {
    // Slice 3i: `for (k, v) in make_map().iter()` on a fresh-temp
    // `Map[String, i64]`, looped. The for-loop peels `.iter()` and recurses on
    // the temp receiver; codegen materializes the handle into a
    // `__for_mapset_` synth local whose `FreeMapHandle`
    // (`karac_map_free_with_drop_vec`) frees the handle + each stored String
    // key at scope exit. Pre-fix the body was silently skipped (output 0).
    // Hazards: (1) each iteration's `k` String struct points into the map's
    // storage (a borrow) and must NOT be independently dropped, else it
    // double-frees the key the handle drop also frees (macOS ASAN); (2) every
    // stored String key must be freed by the per-entry drop, else they leak
    // (Linux LSan). ≥36-byte keys defeat LSan short-string reachability; outer
    // loop re-materializes the temp each pass.
    assert_clean_asan_run(
        r#"
fn make_map() -> Map[String, i64] {
    let mut m: Map[String, i64] = Map.new();
    m.insert("first map key padded out beyond thirty-six bytes ok", 10_i64);
    m.insert("second map key padded out beyond thirty-six bytes o", 20_i64);
    return m;
}

fn main() {
    let mut pass = 0;
    while pass < 3 {
        let mut total = 0_i64;
        for (k, v) in make_map().iter() {
            total = total + v + k.len();
        };
        println(total);
        pass = pass + 1;
    };
}
"#,
        &["132", "132", "132"],
        "freshtemp_map_iter_string_key_no_double_free",
    );
}

#[test]
fn asan_freshtemp_set_bare_string_no_double_free() {
    // Slice 3i companion: `for x in make_set()` (bare, no `.iter()`) on a
    // fresh-temp `Set[String]`. The bare form reaches the materialize path via
    // `owned_temp_drops` (Set is droppable) rather than
    // `temp_recv_mapset_types`; same `__for_mapset_` synth local + handle drop.
    // Each iteration's `x` String borrows the set's storage (must not be
    // independently dropped → macOS ASAN), and every stored element must be
    // freed by the per-entry drop (→ Linux LSan). Loops to accumulate leaks.
    assert_clean_asan_run(
        r#"
fn make_set() -> Set[String] {
    let mut s: Set[String] = Set.new();
    s.insert("first set element padded out beyond thirty-six byte");
    s.insert("second set element padded out beyond thirty-six byt");
    return s;
}

fn main() {
    let mut pass = 0;
    while pass < 3 {
        let mut total = 0_i64;
        for x in make_set() {
            total = total + x.len();
        };
        println(total);
        pass = pass + 1;
    };
}
"#,
        &["102", "102", "102"],
        "freshtemp_set_bare_string_no_double_free",
    );
}

#[test]
fn asan_freshtemp_vec_string_first_last_no_double_free() {
    // Slice 3b-heap companion: `first`/`last` on a fresh-temp `Vec[String]`
    // — the other two borrow-returning (`Option[ref String]`) read methods
    // routed through `scrutinee_is_borrow_call`. Same single-free / borrow-
    // not-dropped obligation as `get`; verifies the method set, not just
    // `get`. (`contains` on a String temp is covered separately below;
    // `get_unchecked` — bare `ref String` via a builtin-method let-binding
    // suppression path that doesn't fire, plus an `unsafe` block — stays
    // scalar-only as a follow-on.)
    assert_clean_asan_run(
        r#"
fn names() -> Vec[String] {
    let mut v: Vec[String] = Vec.new();
    v.push("first padded element string beyond thirty-six bytes ok");
    v.push("middle padded element string beyond thirty-six bytes k");
    v.push("last padded element string beyond thirty-six bytes okk");
    return v;
}

fn main() {
    match names().first() {
        Some(s) => println(s),
        None => println("none"),
    };
    match names().last() {
        Some(s) => println(s),
        None => println("none"),
    };
}
"#,
        &[
            "first padded element string beyond thirty-six bytes ok",
            "last padded element string beyond thirty-six bytes okk",
        ],
        "freshtemp_vec_string_first_last_no_double_free",
    );
}

#[test]
fn asan_freshtemp_vec_string_contains_no_double_free() {
    // Slice 3b-heap follow-on: `contains` on a fresh-temp `Vec[String]` in
    // a loop. Unlike `get`/`first`/`last`, `contains` returns `bool` — no
    // borrow escapes, so there is no arm-binding to suppress; the only
    // obligation is that the receiver temp's per-element String buffers AND
    // outer buffer are freed once per iteration (the `FreeVecBuffer`
    // vec-struct recursion). A regression that frees only the outer buffer
    // leaks the three element Strings (LeakSanitizer on Linux CI); the
    // compared arg is a static literal (`cap = 0`), so it is not part of the
    // free accounting. ≥36-byte elements defeat LSan short-string
    // reachability; the loop forces per-iteration accumulation.
    assert_clean_asan_run(
        r#"
fn names() -> Vec[String] {
    let mut v: Vec[String] = Vec.new();
    v.push("alpha element string padded well past thirty-six bytes");
    v.push("beta element string also padded past thirty-six bytes!!");
    v.push("gamma element string likewise padded beyond thirty-six b");
    return v;
}

fn main() {
    let mut i = 0;
    while i < 3 {
        println(names().contains("beta element string also padded past thirty-six bytes!!"));
        i = i + 1;
    };
}
"#,
        &["true", "true", "true"],
        "freshtemp_vec_string_contains_no_double_free",
    );
}

#[test]
fn asan_surface_binary_string_concat_temp_no_leak_no_double_free() {
    // B-2026-07-21-12: a string concat that STAYS a surface `Binary`
    // (the `String.add` desugar skips it when an operand is a ref-typed
    // Vec-accessor payload — `"f:".to_string() + s` with `s` from
    // `v.first()`) bypassed the assoc-call route's operand frees AND the
    // consumer sites' fresh-temp gate (Call/MethodCall-only), leaking
    // both the `.to_string()` operand temp and the concat result once
    // per evaluation. Now the Binary compile frees fresh-owned operand
    // temps after the concat (mirroring the desugared route) and
    // `free_fresh_owned_str_arg` admits a vec-struct-shaped Binary-Add
    // arg. Double-free half: identifiers/places stay untouched (expr
    // gates + cap>0), and a returned concat is freed exactly once by its
    // consumer. Loop covers the print-arg form, the return form, and a
    // nested concat with a bound intermediate.
    assert_clean_asan_run(
        r#"
fn tag_first(items: ref Vec[String]) -> String {
    match items.first() {
        Some(s) => { return "f:".to_string() + s; }
        None => { return "empty".to_string(); }
    }
    return "?".to_string();
}
fn main() {
    let mut i: i64 = 0;
    let mut acc: i64 = 0;
    let v: Vec[String] = ["aa", "bb"];
    while i < 40 {
        acc = acc + tag_first(v).len();
        match v.first() {
            Some(s) => { acc = acc + ("p:".to_string() + s).len(); }
            None => { acc = acc - 1; }
        }
        match v.first() {
            Some(s) => {
                let t = "pre".to_string() + s;
                acc = acc + (t + "!".to_string()).len();
            }
            None => { acc = acc - 1; }
        }
        i = i + 1;
    }
    println(acc);
}
"#,
        &["560"],
        "surface_binary_string_concat_temp",
    );
}

/// B-2026-08-05-41 — the MEMORY leg of the shared-receiver `mut ref`
/// place-argument fix.
///
/// This is NOT the value witness — a String field already produced the
/// right value before the fix, reaching the caller's storage through
/// B-2026-08-05-39's aggregate-reassignment path rather than through the
/// argument pointer. The scalar arms in
/// `e2e_mut_ref_place_argument_shared_receiver_writes_back` are what
/// witness the miscompile.
///
/// What this test guards is the reclaim behaviour of the path the fix
/// newly takes. The callee now writes THROUGH the field's real address
/// inside the RC box, so the store must displace and free the old buffer
/// exactly ONCE — free it twice and the box's field is left dangling for
/// the reads below and for teardown; free it never and the buffer leaks
/// once per iteration. Neither failure is possible on the rvalue-copy path
/// the fix replaced, so the guarantee is new and needs its own evidence
/// rather than an assumption inherited from the value test.
///
/// The RC header is why a scalar arm cannot stand in for this: the field
/// lives inside the shared node's box and its buffer is reclaimed by the
/// node teardown at refcount zero, not by a scope-exit struct drop. ASAN
/// catches either failure — LSan the leak, the use-after-free check the
/// dangling read.
///
/// Written to B-2026-08-04-17's three rules — `env.args().len()` seed,
/// runtime-derived payload content, byte-level read — and floored, so a
/// regression to zero allocations fails loudly instead of passing.
#[test]
fn asan_shared_field_mut_ref_arg_string_no_leak() {
    assert_clean_asan_run_min_allocs(
        r#"
shared struct T { mut s: String }
fn app(x: mut ref String, tag: String) { x = x + tag; }
fn main() {
    let n = env.args().len() as i64;
    let mut acc: i64 = 0;
    let mut i: i64 = 0;
    while i < 200 {
        let mut seed: String = String.new();
        seed.push_str("payload-");
        seed.push_str((n + i).to_string());
        seed.push_str("-padding-to-force-heap");
        let t = T { s: seed };
        app(mut t.s, "-suffix-also-heap-sized".to_string());
        if t.s.contains("payload") { acc = acc + 1; }
        if t.s.ends_with("heap-sized") { acc = acc + 1; }
        i = i + 1;
    }
    println(acc);
}
"#,
        &["400"],
        "shared_field_mut_ref_arg_string",
        100,
    );
}

#[test]
fn asan_freshtemp_map_string_key_no_double_free() {
    // Slice 3d-heap: `make_map().get(k)` / `.contains_key(k)` on a fresh-temp
    // `Map[String, i64]` (heap KEY). The handle drop must per-entry free each
    // key String (`karac_map_free_with_drop_vec`) before the handle. The
    // value is scalar (`Option[ref i64]` — no value borrow concern); the
    // looked-up key arg is a static literal. Leak (entry keys) caught by
    // Linux LSan; a double-free of the handle/keys by macOS ASAN. ≥36-byte
    // keys defeat LSan short-string reachability; loop accumulates.
    assert_clean_asan_run(
        r#"
fn kmap() -> Map[String, i64] {
    let mut m: Map[String, i64] = Map.new();
    m.insert("alpha key padded out well beyond thirty-six bytes ok", 11_i64);
    m.insert("beta key padded out well beyond thirty-six bytes okk", 22_i64);
    return m;
}

fn main() {
    let mut i = 0;
    while i < 3 {
        match kmap().get("beta key padded out well beyond thirty-six bytes okk") {
            Some(v) => println(v),
            None => println(0_i64),
        };
        println(kmap().contains_key("alpha key padded out well beyond thirty-six bytes ok"));
        i = i + 1;
    };
}
"#,
        &["22", "true", "22", "true", "22", "true"],
        "freshtemp_map_string_key_no_double_free",
    );
}

#[test]
fn asan_freshtemp_map_string_value_no_double_free() {
    // Slice 3d-heap, the riskiest case: `make_map().get(k)` on a fresh-temp
    // `Map[i64, String]` (heap VALUE). `get` returns `Option[ref String]`
    // borrowing a value String *inside* the handle, which the
    // `karac_map_free_with_drop_vec` per-entry drop frees at frame exit. The
    // `Some(s)` arm binds a `ref String` that must NOT be dropped
    // independently (`scrutinee_is_borrow_call`) — otherwise it double-frees
    // the entry String the handle drop also frees (macOS ASAN). A handle drop
    // that skipped the per-entry free leaks every value String (Linux LSan).
    assert_clean_asan_run(
        r#"
fn vmap() -> Map[i64, String] {
    let mut m: Map[i64, String] = Map.new();
    m.insert(1_i64, "first value string padded out beyond thirty-six bytes");
    m.insert(2_i64, "second value string padded out beyond thirty-six byte");
    m.insert(3_i64, "third value string padded out beyond thirty-six bytess");
    return m;
}

fn main() {
    let mut i = 1;
    while i < 4 {
        match vmap().get(i) {
            Some(s) => println(s),
            None => println("none"),
        };
        i = i + 1;
    };
}
"#,
        &[
            "first value string padded out beyond thirty-six bytes",
            "second value string padded out beyond thirty-six byte",
            "third value string padded out beyond thirty-six bytess",
        ],
        "freshtemp_map_string_value_no_double_free",
    );
}

#[test]
fn asan_freshtemp_map_keys_string_key_no_double_free() {
    // Slice 3l-heap: `make_map().keys()` on a fresh-temp `Map[String, i64]`
    // (heap KEY), iterated, looped. Two independent heap owners: (1) the map
    // handle, whose per-entry drop (`karac_map_free_with_drop_vec`) frees each
    // stored key String; (2) the returned `Vec[String]`, into which `.keys()`
    // CLONED each key — freed by the for-loop's Vec drop. Both must free
    // exactly once: a double-free (aliased clone-vs-stored key) is caught by
    // macOS ASAN, a leak of either by Linux LSan. ≥36-byte keys defeat LSan
    // short-string reachability; the loop accumulates.
    assert_clean_asan_run(
        r#"
fn kmap() -> Map[String, i64] {
    let mut m: Map[String, i64] = Map.new();
    m.insert("alpha key padded out well beyond thirty-six bytes ok", 11_i64);
    m.insert("beta key padded out well beyond thirty-six bytes okk", 22_i64);
    return m;
}
fn main() {
    let mut i = 0;
    while i < 3 {
        let mut s = 0;
        for k in kmap().keys() { s = s + k.len(); }
        println(s);
        i = i + 1;
    };
}
"#,
        &["104", "104", "104"],
        "freshtemp_map_keys_string_key_no_double_free",
    );
}

#[test]
fn asan_freshtemp_map_values_string_value_no_double_free() {
    // Slice 3l-heap sibling: `make_map().values()` on a fresh-temp
    // `Map[i64, String]` (heap VALUE), looped. Same two-owner shape — the
    // handle's per-entry drop frees the stored value Strings, and the returned
    // `Vec[String]` (cloned values) frees its own. Guards the same
    // double-free / leak hazards for the value side.
    assert_clean_asan_run(
        r#"
fn vmap() -> Map[i64, String] {
    let mut m: Map[i64, String] = Map.new();
    m.insert(1_i64, "alpha value padded out beyond thirty-six bytes okay");
    m.insert(2_i64, "beta value padded out beyond thirty-six bytes okayy");
    return m;
}
fn main() {
    let mut i = 0;
    while i < 3 {
        let vs: Vec[String] = vmap().values();
        println(vs.len());
        i = i + 1;
    };
}
"#,
        &["2", "2", "2"],
        "freshtemp_map_values_string_value_no_double_free",
    );
}

#[test]
fn asan_freshtemp_map_entries_string_key_no_double_free() {
    // Slice 3m-heap: `make_map().entries()` on a fresh-temp `Map[String, i64]`
    // (heap KEY), iterated, looped. Two independent heap owners: (1) the map
    // handle, whose per-entry drop (`karac_map_free_with_drop_vec`) frees each
    // stored key String; (2) the returned `Vec[(String,i64)]`, into which
    // `.entries()` CLONED each pair — freed by the for-loop's tuple-Vec drop.
    // Both free exactly once: a double-free (aliased clone-vs-stored key) is
    // caught by macOS ASAN, a leak of either by Linux LSan. ≥36-byte keys
    // defeat LSan short-string reachability; the loop accumulates.
    assert_clean_asan_run(
        r#"
fn kmap() -> Map[String, i64] {
    let mut m: Map[String, i64] = Map.new();
    m.insert("alpha key padded out well beyond thirty-six bytes ok", 11_i64);
    m.insert("beta key padded out well beyond thirty-six bytes okk", 22_i64);
    return m;
}
fn main() {
    let mut i = 0;
    while i < 3 {
        let mut s = 0;
        for pair in kmap().entries() { s = s + pair.0.len() + pair.1; }
        println(s);
        i = i + 1;
    };
}
"#,
        &["137", "137", "137"],
        "freshtemp_map_entries_string_key_no_double_free",
    );
}

#[test]
fn asan_freshtemp_map_entries_string_value_no_double_free() {
    // Slice 3m-heap sibling: `make_map().entries()` on a fresh-temp
    // `Map[i64, String]` (heap VALUE), iterated, looped. Same two-owner shape
    // — the handle's per-entry drop frees the stored value Strings, and the
    // returned `Vec[(i64,String)]` (cloned pairs) frees its own tuple elements
    // via the SAME machinery the named-map entries path uses. Guards the same
    // double-free / leak hazards on the value side.
    assert_clean_asan_run(
        r#"
fn vmap() -> Map[i64, String] {
    let mut m: Map[i64, String] = Map.new();
    m.insert(1_i64, "alpha value padded out beyond thirty-six bytes okay");
    m.insert(2_i64, "beta value padded out beyond thirty-six bytes okayy");
    return m;
}
fn main() {
    let mut i = 0;
    while i < 3 {
        let mut s = 0;
        for pair in vmap().entries() { s = s + pair.0 + pair.1.len(); }
        println(s);
        i = i + 1;
    };
}
"#,
        &["105", "105", "105"],
        "freshtemp_map_entries_string_value_no_double_free",
    );
}

// ── B-2026-06-10-6: inline-heap `Option[T]` payload drop ──────
//
// An `Option[String]` / `Option[Vec[_]]` dropped WITHOUT being
// destructured leaks its inline heap payload — the type-erased `Option`
// layout's drop switch can't free a payload that's a buffer for
// `Option[String]` but a scalar for `Option[i64]`, so a concrete-typed
// `FreeInlineOptionPayload` is registered at the binding / discard site.
// On Linux these run under LeakSanitizer (the leak itself is caught); on
// macOS LSan is off, so these primarily guard the DOUBLE-FREE risk — a
// `match`/`if let` arm binds the payload (its own cleanup frees it) AND
// the source `Option`'s scope-exit free must be suppressed (source `cap`
// zeroed), else the buffer is freed twice. Runtime/non-foldable payloads
// (`f"..{n}.."`) so the heap allocation actually happens (a constant
// concat folds to a static string and hides the path).

#[test]
fn asan_option_string_let_unused_freed() {
    // `let x = mk(42)` never destructured → the scope-exit
    // FreeInlineOptionPayload must free the `Some` String. (Linux LSan
    // catches the leak; macOS confirms no spurious double-free.)
    assert_clean_asan_run(
        r#"
fn mk(n: i64) -> Option[String] { Some(f"value-{n}-runtime-heap") }
fn main() {
    let x = mk(42);
    println("done");
}
"#,
        &["done"],
        "option_string_let_unused_freed",
    );
}

#[test]
fn asan_option_string_let_then_match_no_double_free() {
    // `let x = mk(); match x { Some(s) => ... }`: the arm binding `s`
    // frees the payload; the source `Option`'s scope-exit free must be
    // suppressed (cap zeroed) or this double-frees the same buffer.
    assert_clean_asan_run(
        r#"
fn mk(n: i64) -> Option[String] { Some(f"value-{n}-runtime-heap") }
fn main() {
    let x = mk(42);
    match x {
        Some(s) => { println(s); }
        None => { println("none"); }
    };
}
"#,
        &["value-42-runtime-heap"],
        "option_string_let_then_match_no_double_free",
    );
}

#[test]
fn asan_option_string_let_then_if_let_no_double_free() {
    // `if let Some(s) = x` companion to the match double-free guard.
    assert_clean_asan_run(
        r#"
fn mk(n: i64) -> Option[String] { Some(f"v-{n}-runtime-heap-payload") }
fn main() {
    let x = mk(7);
    if let Some(s) = x {
        println(s);
    } else {
        println("none");
    }
}
"#,
        &["v-7-runtime-heap-payload"],
        "option_string_let_then_if_let_no_double_free",
    );
}

#[test]
fn asan_option_string_discarded_freed() {
    // Discarded `mk();` statement temp — no binding, unconditional free.
    assert_clean_asan_run(
        r#"
fn mk(n: i64) -> Option[String] { Some(f"discarded-{n}-runtime-heap") }
fn main() {
    mk(3);
    println("done");
}
"#,
        &["done"],
        "option_string_discarded_freed",
    );
}

#[test]
fn asan_option_string_some_wildcard_arm_freed() {
    // `Some(_)` binds nothing, so the source free must STILL fire (the
    // payload isn't moved out) — and exactly once (no double-free).
    assert_clean_asan_run(
        r#"
fn mk(n: i64) -> Option[String] { Some(f"wild-{n}-runtime-heap") }
fn main() {
    let x = mk(9);
    match x {
        Some(_) => { println("some"); }
        None => { println("none"); }
    };
}
"#,
        &["some"],
        "option_string_some_wildcard_arm_freed",
    );
}

// ── B-2026-06-10-6 follow-ons: Result / Option[Map] / non-Call RHS ──
// The Option-core fix's three open follow-ons (each a leak on Linux LSan,
// a no-double-free guard on macOS): `Result[T,E]` inline Ok/Err payloads,
// `Option[Map]`/`Option[Set]` inline handle payloads, and non-`Call`
// let-RHS (`if`/`match`/block yielding a fresh inline Option/Result).

#[test]
fn asan_result_ok_string_undestructured_freed() {
    // `Result[String, i64]` dropped without destructuring → the
    // scope-exit `FreeInlineResultPayload` frees the `Ok` String.
    assert_clean_asan_run(
        r#"
fn mk(n: i64) -> Result[String, i64] { Ok(f"ok-value-{n}-runtime-heap") }
fn main() {
    let x = mk(42);
    println("done");
}
"#,
        &["done"],
        "result_ok_string_undestructured_freed",
    );
}

#[test]
fn asan_result_err_string_undestructured_freed() {
    // `Result[i64, String]` — the heap is on the `Err` side; the cleanup
    // reads the tag and frees the `Err` overlay.
    assert_clean_asan_run(
        r#"
fn mk(bad: bool) -> Result[i64, String] {
    if bad { Err(f"err-value-runtime-heap") } else { Ok(7i64) }
}
fn main() {
    let x = mk(true);
    println("done");
}
"#,
        &["done"],
        "result_err_string_undestructured_freed",
    );
}

// ── extend_from_slice + from_slice: RC-bearing element types ─
// The bit-copy code path bit-copies String / Vec / shared-T
// aggregates between source and dest. Both observers then alias
// the same inner heap pointers, so the first scope-exit free
// wins and the second hits double-free / UAF. Fix routes through
// per-element synth_clone for non-trivially-copyable elements.
// These tests verify the fix; they fail under the bit-copy v1
// implementation.

#[test]
fn asan_vec_extend_from_slice_string_smallest_repro_with_cap() {
    // Smallest repro for debugging: 2 heap strings, no grow on
    // src (uses with_capacity(4)), no grow on dst (uses
    // with_capacity(4)). If this passes, the bug is in the
    // grow-path interaction. If it fails, the bug is in the
    // per-element String clone path itself.
    assert_clean_asan_run(
        r#"
fn main() {
    let mut a: String = String.new();
    a.push_str("hi");
    let mut b: String = String.new();
    b.push_str("ho");
    let mut src: Vec[String] = Vec.with_capacity(4);
    src.push(a);
    src.push(b);
    let mut dst: Vec[String] = Vec.with_capacity(4);
    dst.extend_from_slice(src);
    println(dst[0]);
}
"#,
        &["hi"],
        "vec_extend_from_slice_string_smallest_repro_with_cap",
    );
}

#[test]
fn asan_vec_extend_from_slice_string_elements_independent() {
    // Vec[String] source — each String must be deep-cloned into
    // dest, not bit-copied. Strings here are heap-allocated (via
    // push_str on a fresh String) so cap > 0 and the scope-exit
    // free does fire. Without the fix, dst[0]'s String
    // {ptr, len, cap} aliases src[0]'s; scope-exit frees both,
    // ASAN reports double-free of the char buffer.
    //
    // The string-literal version of this test (push("hello"))
    // doesn't catch the bug because literals are rodata-backed
    // with cap=0 and the free path skips them — that's the same
    // shape that hid the bug pre-fix.
    assert_clean_asan_run(
        r#"
fn main() {
    let mut a: String = String.new();
    a.push_str("hello");
    let mut b: String = String.new();
    b.push_str("world");
    let mut src: Vec[String] = Vec.new();
    src.push(a);
    src.push(b);
    let mut dst: Vec[String] = Vec.new();
    dst.extend_from_slice(src);
    println(dst[0]);
    println(dst[1]);
}
"#,
        &["hello", "world"],
        "vec_extend_from_slice_string_elements_independent",
    );
}

#[test]
fn asan_vec_try_extend_from_slice_string_elements_independent() {
    // `try_extend_from_slice` must take the same per-element clone path as
    // the panicking base for heap-bearing elements — bit-copying String
    // aggregates would alias src/dst inner buffers and double-free at
    // scope exit. Heap-allocated Strings (cap > 0) so the free fires.
    assert_clean_asan_run(
        r#"
fn main() {
    let mut a: String = String.new();
    a.push_str("hello");
    let mut b: String = String.new();
    b.push_str("world");
    let mut src: Vec[String] = Vec.new();
    src.push(a);
    src.push(b);
    let mut dst: Vec[String] = Vec.new();
    let _ = dst.try_extend_from_slice(src);
    println(dst[0]);
    println(dst[1]);
}
"#,
        &["hello", "world"],
        "vec_try_extend_from_slice_string_elements_independent",
    );
}

#[test]
fn asan_vec_from_slice_string_elements_independent() {
    // Same hazard for `Vec.from_slice` — pre-dates
    // `extend_from_slice` but inherits the same v1 limitation.
    // Heap-allocated Strings to ensure cap > 0 and the
    // scope-exit free actually fires.
    assert_clean_asan_run(
        r#"
fn main() {
    let mut a: String = String.new();
    a.push_str("alpha");
    let mut b: String = String.new();
    b.push_str("beta");
    let mut src: Vec[String] = Vec.new();
    src.push(a);
    src.push(b);
    let dst: Vec[String] = Vec.from_slice(src);
    println(dst[0]);
    println(dst[1]);
}
"#,
        &["alpha", "beta"],
        "vec_from_slice_string_elements_independent",
    );
}

#[test]
fn asan_vec_try_from_slice_string_elements_independent() {
    // Fallible sibling of the above (phase-8-stdlib-floor item 8).
    // `try_from_slice` wraps the new Vec in `Result.Ok(_)`; the
    // Vec[String]-in-Result payload must drop exactly once at scope
    // exit (no double-free against the per-element-cloned source).
    // Heap-allocated Strings (cap > 0) so the free fires.
    assert_clean_asan_run(
        r#"
fn main() {
    let mut a: String = String.new();
    a.push_str("alpha");
    let mut b: String = String.new();
    b.push_str("beta");
    let mut src: Vec[String] = Vec.new();
    src.push(a);
    src.push(b);
    match Vec.try_from_slice(src) {
        Ok(dst) => { println(dst[0]); println(dst[1]); }
        Err(_) => { println("err"); }
    }
}
"#,
        &["alpha", "beta"],
        "vec_try_from_slice_string_elements_independent",
    );
}

// ── String: push_str + scope-exit free ────────────────────────
// String shares the Vec-shaped layout; scope-exit cleanup should free
// the UTF-8 buffer. Static literals have cap=0 and must NOT be freed —
// catches bugs where the free path doesn't check the `cap > 0` guard.

#[test]
fn asan_string_new_push_str() {
    assert_clean_asan_run_min_allocs(
        r#"
fn digits(i: i64) -> String { let mut d: String = String.new(); d.push_str(f"{i}"); return d; }
fn main() {
    let base: i64 = env.args().len();
    let mut s = String.new();
    s.push_str("hello ");
    s.push_str(f"world-{base}");
    if s.contains(digits(base)) { println(f"{s.len()}"); } else { println("BAD"); }
}
"#,
        &["13"],
        "string_new_push_str",
        // B-2026-08-04-17: a literal seed let this fold away entirely at -O2
        // (measured 0 program allocations). Opaque seed + a live read of the
        // buffer, with a floor so it cannot drift back.
        7,
    );
}

// ── String literal: cap=0 must not be freed ───────────────────
// A `let s = "static"` binds to a string-literal global with cap=0.
// If scope-exit cleanup incorrectly frees it, ASAN catches the
// invalid-free on a non-heap pointer.

#[test]
fn asan_string_literal_no_free() {
    assert_clean_asan_run(
        r#"
fn main() {
    let s = "static string never freed";
    println(s.len());
}
"#,
        &["25"],
        "string_literal_no_free",
    );
}

#[test]
fn asan_set_string_scope_exit_free() {
    // Set[String] keeps the bucket array on the heap and references the
    // String literal's static buffer (cap = 0) by value-copy. The set
    // free should release the bucket array; static String buffers must
    // NOT be freed.
    assert_clean_asan_run(
        r#"
fn main() {
    let mut s: Set[String] = Set.new();
    s.insert("alice");
    s.insert("bob");
    s.insert("alice");
    println(s.len());
}
"#,
        &["2"],
        "set_string_scope_exit_free",
    );
}

#[test]
fn asan_set_union_string_independent_handles() {
    // Set[String].union — every surviving element is per-element-cloned
    // into a freshly-allocated bucket array. ASAN catches both the new
    // bucket-array leak (if `u` is not scope-tracked) and any UAF if
    // the per-element String clone aliases the source's heap buffer.
    assert_clean_asan_run(
        r#"
fn main() {
    let mut a: Set[String] = Set.new();
    a.insert("alpha");
    a.insert("beta");
    let mut b: Set[String] = Set.new();
    b.insert("beta");
    b.insert("gamma");
    let u: Set[String] = a.union(b);
    println(u.contains("alpha"));
    println(u.contains("beta"));
    println(u.contains("gamma"));
}
"#,
        &["true", "true", "true"],
        "set_union_string_independent_handles",
    );
}

// ── Compound-payload enum drop-path (Phase 7.2 Slice DP, 2026-05-09) ──
// Exercises `track_enum_var` + `emit_enum_drop_switch`: a value-type
// enum binding that goes out of scope without being moved into a
// downstream consumer must invoke its per-enum drop function, which
// walks the variant's heap-bearing payload fields and frees their
// data buffers. Without the slice's machinery these tests would leak
// (Linux ASAN/LSan) or, on hosts with DP move-suppression bugs,
// double-free at scope exit.

#[test]
fn asan_compound_enum_drop_invokes_string_destructor() {
    // Headline regression gate (DP5). A `String` payload's heap
    // buffer must be freed at scope exit — `__karac_drop_E` runs
    // the cap > 0 ? free(data) shape on the V variant's payload
    // words. Without DP4's drain hook the buffer leaks.
    assert_clean_asan_run(
        r#"
enum E { V(String) }
fn main() {
    let mut s: String = String.new();
    s.push_str("disk full");
    let _e = V(s);
    println(1);
}
"#,
        &["1"],
        "compound_enum_drop_invokes_string_destructor",
    );
}

// ── Compound-payload tuple-payload destructure ────────────────
// Theme 5 (2026-05-10) — heap-bearing element inside a tuple payload
// survives destructure with no double-free / use-after-free. The
// String element is constructed at the call site, moved into the
// variant payload, then re-bound on the destructure side; per-element
// word reconstruction must hand off ownership cleanly so the buffer
// is freed exactly once at scope exit.

#[test]
fn asan_compound_tuple_payload_string_int() {
    assert_clean_asan_run(
        r#"
enum E { V((String, i64)) }
fn main() {
    let mut s = String.new();
    s.push_str("payload");
    let e = V((s, 7));
    match e {
        V((t, n)) => {
            println(t.len());
            println(n);
        }
    }
}
"#,
        &["7", "7"],
        "compound_tuple_payload_string_int",
    );
}

#[test]
fn asan_map_remove_string_key_vec_value_both_heap_halves() {
    // Heap KEY (String) + heap VALUE (Vec): the `drop_key=1` stored-key
    // free (B-2026-06-20-10) and the moved-out value free must each
    // fire exactly once, with no cross-talk, under churn.
    assert_clean_asan_run(
        r#"
fn keyfor(n: i64) -> String {
    let mut s = "k".to_string();
    s.push_str(n.to_string());
    s
}
fn inner() -> i64 {
    let mut bucket: Map[String, Vec[i64]] = Map.new();
    let mut i = 0i64;
    while i < 120 {
        let mut j = 0i64;
        while j < 12 {
            bucket.entry(keyfor(i)).or_insert(Vec.new()).push(i + j);
            j = j + 1;
        }
        i = i + 1;
    }
    let mut acc = 0i64;
    let mut k = 0i64;
    while k < 120 {
        match bucket.remove(keyfor(k)) {
            Some(indices) => {
                acc = acc + indices.len();
            },
            None => {},
        }
        k = k + 1;
    }
    acc
}
fn main() {
    let mut s = 0i64;
    let mut iter = 0i64;
    while iter < 15 {
        s = s + inner();
        iter = iter + 1;
    }
    println(s);
}
"#,
        &["21600"],
        "map_remove_string_key_vec_value_both_heap_halves",
    );
}

// ── Map/Set heap-owning key + value drops (2026-05-14) ────────
// Slice α + β of the recursive-drop work: `karac_map_free_with_drop_vec
// (handle, drop_key, drop_val)` walks live buckets and frees per-entry
// Vec/String content on both sides per the flags. Closes leaks for
// `Set[String]` / `Set[Vec[T]]` (key only), `Map[String, V]` /
// `Map[Vec[T], V]` (key only), and `Map[String, Vec[U]]` / similar
// (both sides). Pre-fix these shapes leaked silently because the
// narrower val-only helper missed every key-side allocation and the
// primitive-only `karac_map_free` was used as a fallback.

#[test]
fn asan_set_string_keys_no_leak() {
    // `Set[String]` — the canonical pervasive shape. Each inserted
    // String is the bucket's KEY; on scope exit the runtime helper
    // must free each live key's data buffer. ASAN catches the leak
    // pre-fix (every inserted string's buffer leaked); post-fix the
    // set drops clean.
    assert_clean_asan_run(
        r#"
fn main() {
    let mut s: Set[String] = Set.new();
    let mut a = String.new();
    a.push_str("apple");
    s.insert(a);
    let mut b = String.new();
    b.push_str("banana");
    s.insert(b);
    let mut c = String.new();
    c.push_str("cherry");
    s.insert(c);
    println(s.len());
}
"#,
        &["3"],
        "set_string_keys_no_leak",
    );
}

// ── `for w in vec` heap element BORROW consumed by a retaining sink
//    (B-2026-06-20-13) ──
// `for` over a Vec is borrow-iteration: `w` aliases `data[i]` and the
// source Vec retains ownership (usable after the loop). A consume site that
// RETAINS `w` (entry/push/insert) must deep-copy it — else the sink's drop
// and the source Vec's drop free the same buffer (double-free; the
// interpreter clones, so this was an A/B mismatch). The fix marks heap
// for-loop element bindings (for_loop_borrow_vars) and routes them through
// the same defensive copy as owned params.

#[test]
fn asan_for_loop_string_elem_into_entry_counter_no_double_free() {
    // The flagship histogram: `for w in words { *m.entry(w).or_insert(0) += 1 }`.
    // Repeated keys exercise vacant (adopt the copy) + occupied (free the
    // copy) paths; `words` stays live and frees its own elements once.
    assert_clean_asan_run(
        r#"
fn main() {
    let mut words: Vec[String] = Vec.new();
    words.push("alpha-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string());
    words.push("beta-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".to_string());
    words.push("alpha-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string());
    let mut m: Map[String, i64] = Map.new();
    for w in words {
        *m.entry(w).or_insert(0_i64) += 1_i64;
    }
    println(m.len());
    println(words.len());
}
"#,
        &["2", "3"],
        "for_loop_string_elem_into_entry_counter_no_double_free",
    );
}

#[test]
fn asan_for_loop_string_elem_into_push_and_insert_no_double_free() {
    // Same borrow-element copy at `Vec.push` and `Map.insert` consume sites.
    assert_clean_asan_run(
        r#"
fn main() {
    let mut words: Vec[String] = Vec.new();
    words.push("one-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string());
    words.push("two-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".to_string());
    let mut out: Vec[String] = Vec.new();
    let mut m: Map[String, i64] = Map.new();
    for w in words {
        out.push(w);
    }
    for w in out {
        m.insert(w, 1_i64);
    }
    println(out.len());
    println(m.len());
    println(words.len());
}
"#,
        &["2", "2", "2"],
        "for_loop_string_elem_into_push_and_insert_no_double_free",
    );
}

#[test]
fn asan_module_scope_map_string_keys_no_double_free() {
    // Module-scope `Map.new()` (phase-8-stdlib-floor.md "Map.new() /
    // Set.new() as module-binding initialisers"). The handle lives in
    // a global filled by the `__karac_static_init` prologue and is
    // intentionally NEVER freed — a module binding lives for the whole
    // process, so there is no scope-exit `karac_map_free`. The handle
    // (and every heap key buffer it owns) stays reachable through the
    // global at exit, so LSan must NOT report it (Linux CI), and ASAN
    // must see no double-free / UAF here. Heap String keys exercise
    // the key_is_vec path under the static-init handle.
    assert_clean_asan_run(
        r#"
let mut REGISTRY: Map[String, i64] = Map.new();
fn put(k: String, v: i64) { REGISTRY.insert(k, v); }
fn main() {
    let mut k1 = String.new();
    k1.push_str("alpha-key-with-long-padding-to-exceed-sso-buffer");
    put(k1, 1i64);
    let mut k2 = String.new();
    k2.push_str("beta-key-with-long-padding-to-exceed-sso-buffer");
    put(k2, 2i64);
    println(REGISTRY.get("alpha-key-with-long-padding-to-exceed-sso-buffer").unwrap_or(0i64));
    println(REGISTRY.len());
}
"#,
        &["1", "2"],
        "module_scope_map_string_keys_no_double_free",
    );
}

#[test]
fn asan_map_string_keys_no_leak() {
    // `Map[String, i64]` — the canonical `key_is_vec, !val_is_vec`
    // shape. Pre-fix the key buffers leaked because the val-only
    // helper never touched them and primitive-only `karac_map_free`
    // was used by default.
    assert_clean_asan_run(
        r#"
fn main() {
    let mut m: Map[String, i64] = Map.new();
    let mut k1 = String.new();
    k1.push_str("alpha");
    m.insert(k1, 1i64);
    let mut k2 = String.new();
    k2.push_str("beta");
    m.insert(k2, 2i64);
    println(m.len());
}
"#,
        &["2"],
        "map_string_keys_no_leak",
    );
}

#[test]
fn asan_map_string_keys_vec_values_no_leak() {
    // `Map[String, Vec[i64]]` — both flags set. The runtime helper
    // must walk live buckets and free BOTH the key's String buffer
    // and the value's Vec buffer before deallocating bucket storage.
    // Catches the case where one side's drop fires correctly but
    // the other is silently skipped.
    assert_clean_asan_run(
        r#"
fn main() {
    let mut m: Map[String, Vec[i64]] = Map.new();
    let mut k = String.new();
    k.push_str("key");
    let mut v: Vec[i64] = Vec.new();
    v.push(7i64);
    v.push(8i64);
    m.insert(k, v);
    println(m.len());
}
"#,
        &["1"],
        "map_string_keys_vec_values_no_leak",
    );
}

#[test]
fn asan_map_string_keys_values_entries_deep_copy_no_double_free() {
    // `keys()` / `values()` / `entries()` over a `Map[String,String]`
    // return OWNED Vecs whose heap halves are DEEP-CLONED from the bucket
    // (B-2026-06-20-11). A shallow `{ptr,len,cap}` copy aliased the map's
    // stored buffer, so the result Vec's scope-exit drop and the map's drop
    // freed the same allocation — a double-free (it crashed `keys()` even
    // before any read). ≥36-byte payloads; the result Vecs and the map all
    // drop independently and cleanly.
    assert_clean_asan_run(
        r#"
fn main() {
    let mut m: Map[String, String] = Map.new();
    m.insert("key-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(),
             "val-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".to_string());
    m.insert("key-cccccccccccccccccccccccccccccccccccc".to_string(),
             "val-dddddddddddddddddddddddddddddddddddd".to_string());
    let ks: Vec[String] = m.keys();
    let vs: Vec[String] = m.values();
    let es: Vec[(String, String)] = m.entries();
    // Counts only — entries() order is non-deterministic; a clean exit proves
    // no double-free (it crashed before any read pre-fix). Content correctness
    // is covered by the codegen E2E `test_e2e_map_string_values_entries_owned`.
    println(ks.len());
    println(vs.len());
    println(es.len());
}
"#,
        &["2", "2", "2"],
        "map_string_keys_values_entries_deep_copy_no_double_free",
    );
}

// ── Owned String/Vec PARAM moved into Map/Set insert (Cluster 1) ──
// `m.insert(k, v)` / `set.insert(v)` where the key/value/element is an
// owned `String`/`Vec` PARAMETER of the current function. Under the
// by-value header ABI the CALLER retains the buffer's scope-exit free,
// so a bucket that bit-copies the param's `{ptr,len,cap}` aliases the
// caller's buffer — both the caller's `FreeVecBuffer` and the Map's
// `karac_map_free_with_drop_vec` then free the same allocation
// (double-free), and a post-call read of the bucket walks freed memory
// (UAF). The fix wires `maybe_defensive_copy_param_arg` into the insert
// arms so the collection owns a private copy. Same family as the
// already-covered `Vec.push` / enum-payload / struct-field consume
// sites (kata-22, 2026-06-06); these are the Map/Set siblings.

#[test]
fn asan_map_insert_owned_string_param_value() {
    // `Map[i64, String]`, VALUE side. The helper takes the String by
    // value (owned param) and inserts it; `m` lives in main and is
    // passed `mut ref`, so its bucket free and main's free of the
    // moved-in source double-hit the same buffer pre-fix. Allocation
    // churn between insert and read-back exposes a UAF if the bucket
    // aliased a freed buffer.
    assert_clean_asan_run(
        r#"
fn store(m: mut ref Map[i64, String], v: String) {
    m.insert(7i64, v);
}

fn main() {
    let mut m: Map[i64, String] = Map.new();
    let mut s = String.new();
    s.push_str("payload-string-value");
    store(mut m, s);
    let mut churn: Vec[String] = Vec.new();
    let mut i = 0i64;
    while i < 16i64 {
        let mut t = String.new();
        t.push_str("xxxxxxxxxxxxxxxxxxxx");
        churn.push(t);
        i = i + 1;
    }
    match m.get(7i64) { Some(g) => println(g), None => println("missing") }
}
"#,
        &["payload-string-value"],
        "map_insert_owned_string_param_value",
    );
}

#[test]
fn asan_map_insert_owned_string_param_key() {
    // `Map[String, i64]`, KEY side. The owned String param is the
    // bucket key; the recursive-drop helper frees each live key, so an
    // aliased key buffer double-frees against the caller's source.
    assert_clean_asan_run(
        r#"
fn store(m: mut ref Map[String, i64], k: String) {
    m.insert(k, 1i64);
}

fn main() {
    let mut m: Map[String, i64] = Map.new();
    let mut a = String.new();
    a.push_str("alpha-key-string");
    store(mut m, a);
    let mut b = String.new();
    b.push_str("beta-key-string");
    store(mut m, b);
    println(m.len());
}
"#,
        &["2"],
        "map_insert_owned_string_param_key",
    );
}

#[test]
fn asan_set_insert_owned_string_param() {
    // `Set[String]` (lowers to `Map[T, ()]`), element side. Owned
    // String param moved into a `mut ref` Set living in main.
    assert_clean_asan_run(
        r#"
fn add(set: mut ref Set[String], v: String) {
    set.insert(v);
}

fn main() {
    let mut s: Set[String] = Set.new();
    let mut a = String.new();
    a.push_str("apple-element");
    add(mut s, a);
    let mut b = String.new();
    b.push_str("banana-element");
    add(mut s, b);
    println(s.len());
}
"#,
        &["2"],
        "set_insert_owned_string_param",
    );
}

#[test]
fn asan_map_insert_fstring_value() {
    // F-string sibling of the owned-param path: `m.insert(k, f"…")`.
    // The f-string's staged accumulator must be disarmed at the insert
    // (the bucket takes the buffer) — otherwise the acc's scope-exit
    // free and the Map's bucket free double-hit it. Covers the
    // `suppress_fstr_acc_if_moved_out` half of the fix.
    assert_clean_asan_run(
        r#"
fn store(m: mut ref Map[i64, String], n: i64) {
    m.insert(n, f"value-{n}-suffix");
}

fn main() {
    let mut m: Map[i64, String] = Map.new();
    store(mut m, 1i64);
    store(mut m, 2i64);
    println(m.len());
    match m.get(1i64) { Some(g) => println(g), None => println("missing") }
}
"#,
        &["2", "value-1-suffix"],
        "map_insert_fstring_value",
    );
}

#[test]
fn asan_vec_map_string_value_param_deep_copy_no_double_free() {
    // `Vec[Map[i64, String]]` — the cloned maps must additionally
    // deep-copy their String VALUES (recursion lands in
    // `emit_map_clone_fn`'s val-clone). Catches an aliased inner String
    // buffer surviving the handle clone.
    assert_clean_asan_run(
        r#"
fn id(v: Vec[Map[i64, String]]) -> Vec[Map[i64, String]] {
    v
}

fn main() {
    let mut v: Vec[Map[i64, String]] = Vec.new();
    let mut m: Map[i64, String] = Map.new();
    let mut s = String.new();
    s.push_str("payload-in-nested-map");
    m.insert(7i64, s);
    v.push(m);
    let r = id(v);
    println(r.len());
    match r[0].get(7i64) { Some(g) => println(g), None => println("missing") }
}
"#,
        &["1", "payload-in-nested-map"],
        "vec_map_string_value_param_deep_copy_no_double_free",
    );
}

#[test]
fn asan_map_get_or_string_value_owned_copy_no_double_free() {
    // `get_or` returns an OWNED `V`, so a heap V (String, ≥36 bytes) must be
    // deep-cloned from the bucket on a hit — else the caller's scope-exit
    // drop and the map's drop free the same buffer (double-free). The miss
    // path returns the freshly-built default. Both bindings drop cleanly.
    assert_clean_asan_run(
        r#"
fn main() {
    let mut m: Map[String, String] = Map.new();
    let k = "key-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string();
    let v = "val-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".to_string();
    m.insert(k.clone(), v);
    let hit = m.get_or(k.clone(), "dflt-cccccccccccccccccccccccccccccccc".to_string());
    let miss = m.get_or("absent-dddddddddddddddddddddddddddddd".to_string(),
                        "dflt-eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee".to_string());
    println(hit);
    println(miss);
}
"#,
        &[
            "val-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            "dflt-eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee",
        ],
        "map_get_or_string_value_owned_copy_no_double_free",
    );
}

#[test]
fn asan_struct_param_string_field_move_no_double_free() {
    // String field (layout-equivalent to `Vec[u8]`) moved out of a by-value
    // struct param — same deep-copy path, exercises the `String`/i8-elem arm.
    assert_clean_asan_run(
        r#"
struct Named { name: String }
fn build() -> Named { let mut s = String.new(); s.push_str("hi"); let n: Named = Named { name: s }; n }
fn firstn(n: Named) -> i64 { let inner = n.name; inner.len() }
fn main() { let n = build(); println(firstn(n)); }
"#,
        &["2"],
        "struct_param_string_field_move",
    );
}

#[test]
fn asan_struct_with_string_field_freed_on_scope_exit() {
    // String is layout-equivalent to Vec[u8] (`{ptr, len, cap}`)
    // and is treated identically by the struct-drop synthesis.
    assert_clean_asan_run_min_allocs(
        r#"
fn digits(i: i64) -> String { let mut d: String = String.new(); d.push_str(f"{i}"); return d; }
struct Named { name: String }
fn build(k: i64) -> i64 {
    let mut s = String.new();
    s.push_str("hello-");
    s.push_str(f"{k}");
    let n: Named = Named { name: s };
    if n.name.contains(digits(k)) { return n.name.len(); }
    return 0i64;
}
fn main() {
    let base: i64 = env.args().len();
    let mut sum = 0i64;
    let mut i = base;
    while i < base + 5i64 {
        sum = sum + build(i);
        i = i + 1;
    }
    println(f"{sum}");
}
"#,
        &["35"],
        "struct_with_string_field_freed_on_scope_exit",
        // B-2026-08-04-17: a literal seed let this fold away entirely at -O2
        // (measured 0 program allocations). Opaque seed + a live read of the
        // buffer, with a floor so it cannot drift back.
        10,
    );
}

#[test]
fn asan_shared_user_drop_nll_alias_and_escape_no_uaf() {
    // B-2026-08-09-3 — the safety half of moving a `shared` binding's
    // refcount dec from scope exit to the binding's last use. The
    // retiming is only sound because a dec is not a free: it reaches
    // zero (and frees) exactly when the LAST handle goes.
    //
    // This program puts a live read after every retimed dec, so a
    // dec that freed instead of decrementing is a use-after-free ASAN
    // catches rather than an output reordering a reader has to notice:
    //
    //   • `let q = p;` then `p.name.len()` — `p`'s dec now fires at
    //     THAT statement, while `q` still holds the object. Under the
    //     `Res` body's own `self.tag` read too.
    //   • `keep.push(q)` then `keep[0].name.len()` — `q`'s name dies
    //     at the push, but the Vec owns the object and reads it after.
    //
    // The String field is deliberately >36 bytes: LSan treats a short
    // string's inline buffer as reachable, which would mask a leak of
    // the payload if the dec were retimed without its field walk.
    //
    // Output is measured on both backends, not derived. `--interp`
    // prints `53 53 0` — it has no drop hook for a container-held
    // shared element (the same pre-existing gap the recursive-chain
    // fixture above documents), so codegen's trailing `1` is the more
    // complete answer, not a divergence this bug introduced.
    assert_clean_asan_run(
        r#"
shared struct Res { tag: i64, name: String }
impl Drop for Res {
    fn drop(mut ref self) { println(self.tag); }
}
fn main() {
    let p = Res { tag: 1, name: "a heap string payload well over thirty-six bytes long".to_string() };
    let q = p;
    println(p.name.len());
    let mut keep: Vec[Res] = Vec.new();
    keep.push(q);
    println(keep[0].name.len());
    println(0);
}
"#,
        &["53", "53", "0", "1"],
        "shared_user_drop_nll_alias_and_escape_no_uaf",
    );
}

// ── `ref T` arg from a non-place rvalue ──────────────────────
//
// C-followup (534c5b6 landed the materialization itself): when the
// rvalue at a `ref T` arg position carries heap ownership — a
// function returning an owned String/Vec, a `String + String`
// concatenation, etc. — the materialized temp inside `compile_call`
// owns that heap buffer. Without a cleanup registration, the
// buffer is unreachable after the call returns (LeakSanitizer
// would catch it on Linux; macOS ASAN can't surface leaks but can
// still catch the inverse — a double-free if the registration
// overshoots). Fix: temps whose value-type matches the
// `{ptr,len,cap}` Vec/String layout are routed through
// `track_vec_var`, picking up the same `FreeVecBuffer` cleanup
// that `let`-bindings use. The walker's `cap > 0` guard makes
// the registration safe for non-owning rvalues (string literals
// are stored with `cap = 0` and short-circuit to no-op).

// Observation: `println(s)` where `s: ref String` over a heap-
// backed buffer trips an unrelated pre-existing
// heap-buffer-overflow on macOS ASAN (puts reads 1 byte past
// the buffer expecting a NUL; karac's heap-allocated Strings
// don't NUL-terminate). The bug is shared by the let-binding
// workaround too, so it's not part of this slice. The tests
// below intentionally avoid `println(ref String)` of a heap
// String — they observe via `.len()` instead so the ASAN check
// focuses on whether the temp's FreeVecBuffer registration
// handles the call-site materialization cleanly.

#[test]
fn asan_ref_arg_string_literal_no_double_free() {
    // Literal rvalue: cap=0, the FreeVecBuffer walker's `cap > 0`
    // guard must skip the free. A miss here would surface as a
    // double-free against the static buffer at scope exit.
    // `println(s.len())` avoids the println(ref String)
    // heap-buffer-overflow noted above.
    assert_clean_asan_run(
        r#"
fn show_len(s: ref String) {
    println(s.len());
}

fn main() {
    show_len("from literal rvalue");
}
"#,
        &["19"],
        "ref_arg_string_literal_no_double_free",
    );
}

#[test]
fn asan_ref_arg_heap_string_concat_freed() {
    // Heap-owning rvalue from concatenation. The materialized
    // temp owns the joined buffer; cleanup must free it once at
    // scope exit (LeakSanitizer arm on Linux; UAF / double-free
    // on macOS). A double-free would fire if both the concat
    // helper and the temp's FreeVecBuffer registration ran.
    assert_clean_asan_run(
        r#"
fn show_len(s: ref String) {
    println(s.len());
}

fn main() {
    let a = "left ";
    let b = "right";
    show_len(a + b);
}
"#,
        &["10"],
        "ref_arg_heap_string_concat_freed",
    );
}

#[test]
fn asan_ref_arg_function_return_string_freed() {
    // Function-return rvalue. Without this slice's track_vec_var
    // call on the materialized temp, the heap allocated inside
    // `make` would have no owner after the call returns. The
    // canonical case the commit message calls out.
    assert_clean_asan_run(
        r#"
fn make() -> String {
    let a = "made ";
    let b = "string";
    return a + b;
}

fn show_len(s: ref String) {
    println(s.len());
}

fn main() {
    show_len(make());
}
"#,
        &["11"],
        "ref_arg_function_return_string_freed",
    );
}

// A discarded fresh String from a MethodCall (`s.to_upper();` shape —
// here `concat()`-style via `+` wrapped in a returning fn, called and
// discarded). Confirms the chokepoint covers String (same `{ptr,len,cap}`
// layout as Vec) and that draining the one-shot statement frame does not
// double-free the *bound* `keep` String living in the same function.
#[test]
fn asan_discarded_string_temp_coexists_with_bound_string() {
    assert_clean_asan_run(
        r#"
fn make_str() -> String {
    let a = "discarded ";
    let b = "temp";
    return a + b;
}

fn main() {
    let keep = "kept value";
    make_str();
    println(keep);
}
"#,
        &["kept value"],
        "discarded_string_temp_coexists_with_bound_string",
    );
}

#[test]
fn asan_discarded_nested_vec_string_temp_freed() {
    // A discarded `Vec[String]`: slice 1 freed the outer buffer but
    // leaked the inner String element buffers (elem_ty was `None`). The
    // hint table supplies the element type, so the recursive `FreeVecBuffer`
    // walk frees each element. The bound `keep` String in the same frame
    // pins that draining the one-shot discard frame doesn't double-free a
    // live binding. Leak oracle (Linux) is the real gate for the element
    // closure; macOS catches any double-free.
    assert_clean_asan_run(
        r#"
fn make_vv() -> Vec[String] {
    let mut v: Vec[String] = Vec.new();
    v.push("alpha");
    v.push("beta");
    return v;
}

fn main() {
    let keep = "kept";
    let mut i = 0;
    while i < 8 {
        make_vv();
        i = i + 1;
    }
    println(keep);
}
"#,
        &["kept"],
        "discarded_nested_vec_string_temp_freed",
    );
}

#[test]
fn asan_vec_of_vec_of_string_scope_exit_drop_no_leak() {
    // Two-level nested heap: a `Vec[Vec[String]]` dropped at scope exit. The
    // inline `FreeVecBuffer` cleanup's vec-struct fast path is ONE level deep —
    // it frees each inner `Vec[String]`'s data buffer but treats that buffer's
    // elements as opaque, so the innermost String char-buffers leak (documented
    // one-level limit; the recursive `emit_vec_drop_fn` family existed but was
    // unwired). This routes the `Vec[heap-inner]` element through the recursive
    // per-element drop (`karac_drop_Vec_String`), which drops every level. The
    // binding is only `.len()`-read (never consumed), so it drops whole at
    // scope exit. ≥36-byte innermost strings defeat LSan short-string
    // reachability; the loop re-materializes each pass. Leak (innermost
    // Strings) is the Linux-LSan gate; a double-free would show on macOS ASAN.
    assert_clean_asan_run(
        r#"
fn build(n: i64) -> Vec[Vec[String]] {
    let mut outer: Vec[Vec[String]] = Vec.new();
    let mut a: Vec[String] = Vec.new();
    a.push(f"alpha string padded out well beyond thirty-six bytes {n}");
    a.push(f"beta string padded out well beyond thirty-six bytes {n}");
    outer.push(a);
    let mut b: Vec[String] = Vec.new();
    b.push(f"gamma string padded out well beyond thirty-six byte {n}");
    outer.push(b);
    return outer;
}
fn main() {
    let mut i = 0;
    while i < 4 {
        let vv = build(i);
        println(vv.len());
        i = i + 1;
    };
}
"#,
        &["2", "2", "2", "2"],
        "vec_of_vec_of_string_scope_exit_drop_no_leak",
    );
}

#[test]
fn asan_vec_of_option_string_scope_exit_drop_no_leak() {
    // Slice 3p: a `Vec[Option[String]]` dropped at scope exit, looped. An
    // `Option[String]` ELEMENT is the type-erased `{tag, w0, w1, w2}` layout
    // whose `Some` payload {ptr,len,cap} overlays w0..w2 — not a vec-struct,
    // so the one-level fast path skipped it, and `vec_elem_agg_drop_for_-
    // type_expr` early-returned None for Option (the type-erased enum drop
    // switch can't know the payload type — B-2026-06-10-6's concrete-typed
    // binding cleanup covers only BINDINGS, not Vec elements). The `Some`
    // payload Strings leaked. Fixed by the payload-type-aware
    // `karac_drop_Option_String` threaded through the agg-drop loop:
    // tag-guarded (None elements skipped), payload dropped via the recursive
    // family. Leak is the Linux-LSan gate; a double-free (payload freed by
    // both the element drop and a binding) shows on macOS ASAN. Payloads are
    // runtime f-strings so the heap allocation actually happens — a constant
    // literal folds to a static cap=0 string and hides the path
    // (B-2026-06-10-6's discipline).
    assert_clean_asan_run(
        r#"
fn build(n: i64) -> Vec[Option[String]] {
    let mut v: Vec[Option[String]] = Vec.new();
    v.push(Some(f"alpha string padded out beyond thirty-six bytes {n}"));
    v.push(None);
    v.push(Some(f"beta string padded out beyond thirty-six bytes {n}"));
    return v;
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
        &["3", "3", "3"],
        "vec_of_option_string_scope_exit_drop_no_leak",
    );
}

#[test]
fn asan_vec_of_vec_of_option_string_scope_exit_drop_no_leak() {
    // Slice 3p two-level sibling: `Vec[Vec[Option[String]]]`. The recursive
    // `karac_drop_Vec_Option_String` (3n's family) calls the tag-guarded
    // Option drop per innermost element. Guards that the Option arm composes
    // with the nested-Vec recursion. Runtime f-string payloads (real heap).
    assert_clean_asan_run(
        r#"
fn build(n: i64) -> Vec[Vec[Option[String]]] {
    let mut outer: Vec[Vec[Option[String]]] = Vec.new();
    let mut a: Vec[Option[String]] = Vec.new();
    a.push(Some(f"alpha string padded out beyond thirty-six bytes {n}"));
    a.push(None);
    outer.push(a);
    return outer;
}
fn main() {
    let mut i = 0;
    while i < 3 {
        let vv = build(i);
        println(vv.len());
        i = i + 1;
    };
}
"#,
        &["1", "1", "1"],
        "vec_of_vec_of_option_string_scope_exit_drop_no_leak",
    );
}

#[test]
fn asan_vec_of_result_string_scope_exit_drop_no_leak() {
    // Slice 3q: a `Vec[Result[String, String]]` dropped at scope exit,
    // looped. The tag-dispatching `karac_drop_Result_<ok>_<err>` frees the
    // live side's inline payload overlay per element (Ok and Err overlay the
    // same w0..w2). LSan-RED pre-fix (every payload leaked, 6 allocs).
    // Runtime f-string payloads per the 3p spelling-trap discipline.
    assert_clean_asan_run(
        r#"
fn build(n: i64) -> Vec[Result[String, String]] {
    let mut v: Vec[Result[String, String]] = Vec.new();
    v.push(Ok(f"alpha ok payload padded out beyond thirty-six bytes {n}"));
    v.push(Err(f"beta err payload padded out beyond thirty-six bytes {n}"));
    return v;
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
        "vec_of_result_string_scope_exit_drop_no_leak",
    );
}

// ── SoA heap-field (String / Vec) element drops ───────────────
// String / Vec[POD] element fields are now allowed in SoA layouts.
// Their per-element heap buffers are freed by the synthesized
// `__karac_soa_drop_<layout>` at scope exit, on overwrite (index /
// field store drop-old), and on the carried-grid reassignment. Each
// String payload is ≥36 bytes so a missed free is past LSan's
// short-allocation reachability blind spot. All paths run ×20 frames
// to amplify a per-frame leak; the Linux-CI LSan job is the gate.

#[test]
fn asan_soa_string_field_scope_cleanup_no_leak() {
    // The CORE leak fix: a SoA Vec whose element has a heap String
    // field, built fresh each frame and dropped at scope exit. Pre-fix
    // the FreeSoaGroups cleanup freed only the group buffers (POD
    // assumption), leaking every element's String payload — 8 × 20 =
    // 160 heap strings. Reads only the primitive `id` (the heap field
    // read-back is a separate read-path concern). Sum of ids =
    // 28/frame × 20 = 560.
    assert_clean_asan_run(
        r#"
struct Cell { id: i64, name: String }
layout cells: Vec[Cell] { group ids { id } group names { name } }
fn main() with panics {
    let mut total = 0;
    let mut k = 0;
    while k < 20 {
        let mut cells: Vec[Cell] = Vec.new();
        let mut i = 0;
        while i < 8 {
            cells.push(Cell { id: i, name: f"soa-heap-owning-string-payload-element-{i}" });
            i = i + 1;
        }
        let mut j = 0;
        while j < cells.len() { total = total + cells[j].id; j = j + 1; }
        k = k + 1;
    }
    println(total);
}
"#,
        &["560"],
        "soa_string_field_scope_cleanup",
    );
}

#[test]
fn asan_soa_string_field_index_store_overwrite_no_leak() {
    // Whole-element overwrite `cells[i] = Cell { … }` over a String
    // field: `compile_soa_index_store` drops the OLD element's String
    // buffer before scattering the new one. Pre-fix the old "initial-…"
    // strings leaked (8 × 20 = 160). After overwrite ids = i+10, so the
    // sum = (28 + 80)/frame × 20 = 2160.
    assert_clean_asan_run(
        r#"
struct Cell { id: i64, name: String }
layout cells: Vec[Cell] { group ids { id } group names { name } }
fn main() with panics {
    let mut total = 0;
    let mut k = 0;
    while k < 20 {
        let mut cells: Vec[Cell] = Vec.new();
        let mut i = 0;
        while i < 8 {
            cells.push(Cell { id: i, name: f"initial-soa-heap-string-payload-element-{i}" });
            i = i + 1;
        }
        let mut j = 0;
        while j < cells.len() {
            cells[j] = Cell { id: j + 10, name: f"rewritten-soa-heap-string-payload-element-{j}" };
            j = j + 1;
        }
        let mut r = 0;
        while r < cells.len() { total = total + cells[r].id; r = r + 1; }
        k = k + 1;
    }
    println(total);
}
"#,
        &["2160"],
        "soa_string_field_index_store_overwrite",
    );
}

#[test]
fn asan_soa_string_field_field_store_overwrite_no_leak() {
    // Field-level overwrite `cells[i].name = f"…"` over a heap String:
    // `compile_soa_field_store` frees the displaced buffer before the
    // store, and the f-string accumulator's own cleanup is suppressed
    // (else the acc + the SoA drop double-free — the SIGTRAP guard).
    // Pre-fix the old "initial-…" strings leaked. ids unchanged
    // (i = 0..8), sum = 28/frame × 20 = 560.
    assert_clean_asan_run(
        r#"
struct Cell { id: i64, name: String }
layout cells: Vec[Cell] { group ids { id } group names { name } }
fn main() with panics {
    let mut total = 0;
    let mut k = 0;
    while k < 20 {
        let mut cells: Vec[Cell] = Vec.new();
        let mut i = 0;
        while i < 8 {
            cells.push(Cell { id: i, name: f"initial-soa-heap-string-payload-element-{i}" });
            i = i + 1;
        }
        let mut j = 0;
        while j < cells.len() {
            cells[j].name = f"replaced-soa-heap-string-payload-element-{j}";
            j = j + 1;
        }
        let mut r = 0;
        while r < cells.len() { total = total + cells[r].id; r = r + 1; }
        k = k + 1;
    }
    println(total);
}
"#,
        &["560"],
        "soa_string_field_field_store_overwrite",
    );
}

#[test]
fn asan_soa_string_field_reassign_carried_no_leak() {
    // Carried-grid double-buffer (`cells = rebuild(cells)`) where the
    // element has a heap String field: the reassignment's inline
    // group-buffer free must FIRST drop the old generation's String
    // payloads (via the synthesized drop fn), else every rebuilt frame
    // leaks the prior generation's strings. The by-value param is
    // caller-retains, so the old buffers are owned at the assignment
    // and the displaced strings are this site's to free. Rebuilt 5×
    // per frame × 20. `id` is bumped +1 each rebuild: final id = i + 5,
    // sum = (28 + 40)/frame × 20 = 1360.
    assert_clean_asan_run(
        r#"
struct Cell { id: i64, name: String }
layout cells: Vec[Cell] { group ids { id } group names { name } }
fn rebuild(src: Vec[Cell]) -> Vec[Cell] {
    let mut dst: Vec[Cell] = Vec.new();
    let mut i = 0;
    while i < src.len() {
        dst.push(Cell { id: src[i].id + 1, name: f"rebuilt-soa-heap-string-payload-element-{i}" });
        i = i + 1;
    }
    dst
}
fn init() -> Vec[Cell] {
    let mut cells: Vec[Cell] = Vec.new();
    let mut i = 0;
    while i < 8 {
        cells.push(Cell { id: i, name: f"initial-soa-heap-string-payload-element-{i}" });
        i = i + 1;
    }
    cells
}
fn main() with panics {
    let mut total = 0;
    let mut k = 0;
    while k < 20 {
        let mut cells: Vec[Cell] = init();
        let mut f = 0;
        while f < 5 { cells = rebuild(cells); f = f + 1; }
        let mut j = 0;
        while j < cells.len() { total = total + cells[j].id; j = j + 1; }
        k = k + 1;
    }
    println(total);
}
"#,
        &["1360"],
        "soa_string_field_reassign_carried",
    );
}

#[test]
fn asan_soa_string_push_named_binding_no_double_free() {
    // Move-in of a NAMED owned struct binding into push
    // (`let c = Cell{…}; cells.push(c)`): push bit-copies `c`'s String
    // header into the group buffer, so the SoA Vec owns it. Without the
    // move-in cap-zero, `c`'s own StructDrop AND the SoA cleanup free
    // the same buffer — a double-free ASAN catches on every host (not a
    // leak, so even the macOS run flags it). Sum of ids = 560.
    assert_clean_asan_run(
        r#"
struct Cell { id: i64, name: String }
layout cells: Vec[Cell] { group ids { id } group names { name } }
fn main() with panics {
    let mut total = 0;
    let mut k = 0;
    while k < 20 {
        let mut cells: Vec[Cell] = Vec.new();
        let mut i = 0;
        while i < 8 {
            let c: Cell = Cell { id: i, name: f"named-binding-soa-heap-string-payload-{i}" };
            cells.push(c);
            i = i + 1;
        }
        let mut j = 0;
        while j < cells.len() { total = total + cells[j].id; j = j + 1; }
        k = k + 1;
    }
    println(total);
}
"#,
        &["560"],
        "soa_string_push_named_binding",
    );
}

/// B-2026-07-28-12: printing a Vec that has no variable name to key on
/// materializes the value at the print site, so the print site is also its
/// only owner — the buffer must be freed there, and only after the Display
/// fn has read it. Both sinks are covered (`println` and f-string
/// interpolation) across element types, since a `Vec[String]` additionally
/// carries a per-cell heap that the outer-buffer free does NOT reclaim.
///
/// The loop at the end is the case that decides WHERE cleanup is timed:
/// the temporary lives in one entry alloca reused every iteration, so
/// registering it for cleanup only at function-scope exit would free the
/// last iteration's buffer and leak the other 63.
#[test]
fn asan_unbound_vec_display_no_leak() {
    let label = "unbound_vec_display";
    if !asan_available() {
        eprintln!("[{label}] ASAN unavailable on this host — skipping");
        return;
    }
    let src = r#"
fn mk() -> Vec[i64] { vec![2i64, 3i64] }
// HEAP-owning elements: a literal-only `Vec[String]` is all rodata views
// (cap == 0) and would not exercise the per-cell heap at all.
fn names() -> Vec[String] { vec!["ada".to_uppercase(), "bob" + "!"] }
fn empty() -> Vec[i64] { Vec.new() }
fn main() {
    println(vec![9i64, 8i64]);
    println(mk());
    println(names());
    println(empty());
    println(f"<{mk()}>");
    println(f"<{names()}>");
    let t = Tensor.from([[1, 2, 3], [4, 5, 6]]);
    println(t.shape());
    println(f"<{t.shape()}>");
    // The bound form still frees exactly once through scope cleanup.
    let b: Vec[String] = names();
    println(b);
    println(f"<{b}>");
    // IN A LOOP: the temporary's slot is a single entry alloca reused every
    // iteration, so cleanup timed at scope exit would free only the last
    // iteration's buffer and leak the rest. 64 iterations makes any per-
    // iteration leak unmistakable to LSan rather than borderline.
    let mut n: i64 = 0;
    while n < 64 {
        println(names());
        println(f"<{mk()}>");
        n = n + 1;
    }
    println("done");
}
"#;
    let Some((stdout, status)) = run_under_asan(src, label) else {
        eprintln!("[{label}] setup failed — skipping");
        return;
    };
    assert!(
        status.success(),
        "[{label}] ASAN reported a memory error (exit code {:?}) — check the \
             temporary-Vec free at the print site (after the render, outer buffer \
             only for POD elements) and that the BOUND form is not double-freed",
        status.code()
    );
    assert!(
        stdout.contains("[9, 8]") && stdout.contains("[ADA, bob!]") && stdout.ends_with("done\n"),
        "[{label}] ASAN passed but output mismatched: {stdout:?}"
    );
}

#[test]
fn asan_match_bound_string_slice_is_not_freed() {
    // A `StringSlice` is a BORROW (`cap == 0`) — it must never be freed.
    // Registering a match-bound view for method dispatch meant touching
    // `vec_elem_types`, the same table that feeds the end-of-arm
    // `track_vec_var` buffer free, so this pins the half that was
    // deliberately left out: dispatch yes, free no. Re-conflating the two
    // would free the SOURCE string's buffer once per token here.
    //
    // 20k iterations over a view that keeps re-slicing itself, so a
    // double-free or use-after-free is immediate rather than probabilistic.
    let label = "match_bound_string_slice";
    if !asan_available() {
        eprintln!("[{label}] ASAN unavailable on this host — skipping");
        return;
    }
    let src = r#"
struct SplitIter { rest: StringSlice, done: bool }
impl SplitIter {
    fn next(mut ref self) -> Option[StringSlice] {
        if self.done { return None; }
        match self.rest.find(",") {
            Some(k) => {
                let head = self.rest.slice(0, k);
                self.rest = self.rest.slice(k + 1, self.rest.len());
                return Some(head);
            }
            None => { self.done = true; return Some(self.rest); }
        }
    }
}
fn main() {
    let mut s = "".to_string();
    let mut i = 0;
    while i < 2000 { s.push_str("abcdefg,"); i = i + 1; }
    let mut total = 0;
    let mut r = 0;
    while r < 10 {
        let mut it = SplitIter { rest: s.slice(0, s.len()), done: false };
        while true {
            match it.next() { None => break, Some(f) => { total = total + f.len(); } }
        }
        r = r + 1;
    }
    println(total);
}
"#;
    let Some((stdout, status)) = run_under_asan(src, label) else {
        eprintln!("[{label}] setup failed — skipping");
        return;
    };
    assert!(
        status.success(),
        "[{label}] ASAN run failed (status {status:?}); stdout:\n{stdout}"
    );
    // 2000 tokens of 7 bytes + a trailing empty field, 10 rounds.
    assert_eq!(stdout.trim(), "140000");
}

#[test]
fn asan_user_impl_display_in_container_no_leak() {
    // B-2026-08-26-29. Rendering a container element through the element's
    // user `impl Display` calls that method once per element, and each call
    // hands back an OWNING String the synthesized wrapper has to free.
    //
    // Both payload shapes are exercised on purpose, because they need
    // OPPOSITE handling and one guard covers both: `A` returns an f-string
    // (heap, `cap > 0`) which MUST be freed or every element leaks, while
    // `B` returns a string LITERAL (a read-only global, `cap == 0`) which
    // must NOT be freed or the program aborts. 60 elements so a per-element
    // leak is far above noise.
    let label = "user_impl_display_in_container";
    if !asan_available() {
        eprintln!("[{label}] ASAN unavailable on this host — skipping");
        return;
    }
    let src = r#"
enum Ue { A { n: i64 }, B }
impl Display for Ue {
    fn to_string(ref self) -> String {
        match self { A { n } => f"aye-{n}-padding-padding-padding", B => "bee" }
    }
}
fn main() {
    let mut v: Vec[Ue] = Vec.new();
    let mut i = 0;
    while i < 60 {
        if i % 2 == 0 { v.push(Ue.A { n: i }); } else { v.push(Ue.B); }
        i = i + 1;
    }
    let s = f"{v}";
    println(s.len() > 0);
    println(v.len());
}
"#;
    let Some((stdout, status)) = run_under_asan(src, label) else {
        eprintln!("[{label}] setup failed — skipping");
        return;
    };
    assert!(
        status.success(),
        "[{label}] ASAN run failed (status {status:?}); stdout:\n{stdout}"
    );
    assert_eq!(stdout.trim(), "true\n60");
}

#[test]
fn asan_priority_queue_string_drain_no_leak() {
    let label = "priority_queue_string_drain";
    if !asan_available() {
        eprintln!("[{label}] ASAN unavailable on this host — skipping");
        return;
    }
    let src = r#"
fn main() {
    let mut q: PriorityQueue[String] = PriorityQueue.new();
    q.push("pear"); q.push("apple"); q.push("fig"); q.push("banana");
    let sorted = q.into_sorted_vec();
    let mut i = 0;
    while i < sorted.len() { println(sorted[i]); i = i + 1; }
    // Elements dropped in place rather than transferred out.
    let mut r: PriorityQueue[String] = PriorityQueue.max_first();
    r.push("x"); r.push("yy");
    r.clear();
    println(r.len());
    // O(n) heapify then a partial drain: some elements handed out, the rest
    // dropped with the queue.
    let mut p = PriorityQueue.from(["delta", "alpha", "charlie", "bravo"]);
    match p.pop() { Some(v) => { println(v); } None => {} }
    println(p.len());
}
"#;
    let Some((stdout, status)) = run_under_asan(src, label) else {
        eprintln!("[{label}] setup failed — skipping");
        return;
    };
    assert!(
        status.success(),
        "[{label}] ASAN/LSan reported a memory error (exit code {:?}) — \
             check pop's transfer out of the backing Vec and swap's element moves",
        status.code()
    );
    assert_eq!(
        stdout.trim(),
        "apple\nbanana\nfig\npear\n0\nalpha\n3",
        "[{label}] unexpected stdout (ASAN passed, output mismatched)"
    );
}

#[test]
fn asan_secret_string_zeroize_no_leak() {
    let label = "secret_string_zeroize";
    if !asan_available() {
        eprintln!("[{label}] ASAN unavailable on this host — skipping");
        return;
    }
    let src = r#"
import std.secret.{Secret};
fn check(tok: ref Secret[String]) -> bool {
    let ref_val: Secret[String] = Secret.new("hunter2-token-01");
    return tok.ct_eq(ref_val)
}
fn main() {
    let a: Secret[String] = Secret.new("hunter2-token-01");
    let b: Secret[String] = Secret.new("different-secret1");
    println(check(a));
    println(check(b));
}
"#;
    let Some((stdout, status)) = run_under_asan(src, label) else {
        eprintln!("[{label}] setup failed — skipping");
        return;
    };
    assert!(
        status.success(),
        "[{label}] ASAN reported a memory error (exit code {:?}) — \
             check the Secret[String] zeroize memset vs the buffer free",
        status.code()
    );
    assert_eq!(
        stdout.trim(),
        "true\nfalse",
        "[{label}] unexpected stdout (ASAN passed, output mismatched)"
    );
}

#[test]
fn asan_owned_string_param_let_move_grow() {
    // String sibling with a realloc after the move — without the
    // deep copy the caller frees a stale (realloc-moved) pointer.
    assert_clean_asan_run(
        r#"
fn bang(s: String) -> String {
    let mut t = s;
    t.push_str("!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!");
    t
}

fn main() {
    let a = f"abc{1}";
    let b = bang(a);
    println(b.len());
}
"#,
        &["36"],
        "owned_string_param_let_move_grow",
    );
}

// ── `ref name @ PATTERN` borrow bindings (phase-8 @ slice 4) ──
//
// `ref x @ Foo { a }` under an owned scrutinee: the subtree
// borrows — pattern bindings must NOT register heap cleanup
// (`pattern_binding_is_borrow` suppression in the by_ref
// AtBinding bind path) while the source keeps its own drop
// (`pattern_consumes_field` → false for by_ref). If either half
// regresses, the String buffer is freed twice (binding cleanup +
// source drop) and ASAN flags it here.

#[test]
fn asan_ref_at_binding_struct_string_field_single_free() {
    assert_clean_asan_run(
        r#"
struct Foo { a: String, n: i64 }
fn main() {
    let foo = Foo { a: "heap-owned string content", n: 7 };
    match foo {
        ref x @ Foo { a, n } => {
            println(a);
            println(n);
            println(x.n);
        }
    }
    println(foo.a);
}
"#,
        &[
            "heap-owned string content",
            "7",
            "7",
            "heap-owned string content",
        ],
        "ref_at_binding_struct_string_field_single_free",
    );
}

#[test]
fn asan_ref_at_binding_option_string_payload_single_free() {
    assert_clean_asan_run(
        r#"
fn main() {
    let opt = Some("payload string on the heap");
    match opt {
        ref x @ Some(y) => { println(y); }
        None => { println("none"); }
    }
    match opt {
        Some(z) => { println(z); }
        None => { }
    }
}
"#,
        &["payload string on the heap", "payload string on the heap"],
        "ref_at_binding_option_string_payload_single_free",
    );
}

// ── Borrowed String-slice map keys (allocation-free lookups) ──────
//
// `m.get(s[a..b])` / `m.insert(s[a..b], v)` pass a borrowed
// `{ptr, len, cap=0}` view into `s` instead of a freshly-allocated
// owned `String`. Lookups never retain the key; `insert` deep-copies it
// only on a *fresh* insertion (`karac_map_insert_borrowed_str_old`). This
// test proves the deep-copy happens: the source heap string is freed
// (reassigned) and the allocator churned *before* the map is read, so a
// borrowed key that was wrongly stored verbatim would be a use-after-free
// ASAN catches. Also exercises the empty-slice key (`s[0..0]` → null ptr,
// len 0) and scope-exit free of the deep-copied keys.
#[test]
fn asan_borrowed_string_slice_map_keys_deep_copy() {
    assert_clean_asan_run(
        r#"
fn main() {
    let mut m: Map[String, i64] = Map.new();
    let mut s = String.new();
    s.push_str("foo");
    s.push_str("bar");        // heap buffer "foobar"
    m.insert(s[0..3], 1);     // borrowed slice -> deep-copied into the map
    m.insert(s[3..6], 2);
    s = String.new();         // frees the old "foobar" buffer
    let mut junk = String.new();
    let mut i = 0i64;
    while i < 2000 { junk.push_str("zzzz"); i = i + 1; }
    match m.get("foo") { Some(v) => println(v), None => println(-1) }
    match m.get("bar") { Some(v) => println(v), None => println(-1) }
    m.insert(s[0..0], 9);     // empty borrowed key (null ptr, len 0)
    println(m.len())
}
"#,
        &["1", "2", "3"],
        "borrowed_string_slice_map_keys_deep_copy",
    );
}

// ── `Map[String, _].clear()` frees heap key buffers ───────────────
//
// Plain `karac_map_clear` only zeroed the bucket status bytes, leaking
// every live String key's heap buffer (the map-free frees only occupied
// slots, and a clear leaves none). With many insert→clear rounds this
// leaks unboundedly. The fix routes heap-keyed/valued maps through
// `karac_map_clear_with_drop_vec`. LeakSanitizer fails this test pre-fix.
#[test]
fn asan_map_string_key_clear_frees_heap_keys() {
    assert_clean_asan_run(
        r#"
fn main() {
    let base = "abcdefghij";
    let mut m: Map[String, i64] = Map.new();
    let mut round = 0i64;
    while round < 50 {
        let mut v = 0i64;
        while v < 5 { m.insert(base[v*2 .. v*2+2], v); v = v + 1; }
        m.clear();
        round = round + 1;
    }
    println(m.len());
}
"#,
        &["0"],
        "map_string_key_clear_frees_heap_keys",
    );
}

#[test]
fn asan_push_str_borrowed_slice_no_uaf() {
    // `push_str(src[a..b])` borrows a zero-copy view into `src` instead of
    // allocating a temp String (the 30× #405 fix). The view points into
    // `src`'s buffer; `out` grows repeatedly (the destination buffer is
    // freed/reallocated each grow). ASAN confirms the borrowed source —
    // which is `hexd`/`words[k]`, NOT `out` — stays valid across `out`'s
    // grows (no use-after-free), and that nothing leaks (the cap-0 view is
    // never freed; no temp is allocated to leak). Literal- and
    // heap-element-sourced slices both exercise the path.
    assert_clean_asan_run(
        r#"
fn main() {
    let hexd: String = "0123456789abcdef";
    let mut out: String = "";
    let mut k = 0i64;
    while k < 2000 {
        let d = k & 0xfi64;
        out.push_str(hexd[d..d + 1i64]);   // literal-sourced borrow, out grows
        k = k + 1i64;
    }
    println(out.bytes().len());

    let mut words: Vec[String] = Vec.new();
    let mut i = 0i64;
    while i < 8 { words.push(f"token-{i}-payload"); i = i + 1i64; }
    let mut joined: String = "";
    let mut j = 0i64;
    while j < 500 {
        joined.push_str(words[j & 7i64][0..5i64]);   // heap-element-sourced borrow + grow
        j = j + 1i64;
    }
    println(joined.bytes().len());
}
"#,
        &["2000", "2500"],
        "push_str_borrowed_slice_no_uaf",
    );
}

#[test]
fn asan_try_clone_vec_string_deep_independent_free() {
    // phase-8-stdlib-floor item 8: `Vec[String].try_clone()` deep-clones
    // every element into a fresh buffer. Source and clone own independent
    // String buffers, so both must free exactly once with no double-free /
    // leak. Both go out of scope here (source + the `Ok`-bound clone).
    assert_clean_asan_run(
        r#"
fn main() {
    let mut src: Vec[String] = Vec.new();
    src.push(f"alpha{1}");
    src.push(f"beta{2}");
    match src.try_clone() {
        Ok(c) => {
            println(c.len());
            println(c[0]);
            println(c[1]);
        }
        Err(_) => println("err"),
    }
    src.push(f"gamma{3}");
    println(src.len());
}
"#,
        &["2", "alpha1", "beta2", "3"],
        "try_clone_vec_string_deep_independent_free",
    );
}

#[test]
fn asan_vec_tuple_heap_element_string_var_push() {
    // Sibling of the push-read case where the String field arrives as an
    // IDENTIFIER (an owned binding) rather than an inline f-string. The
    // tuple-construction move-suppression (identifier source) + the Vec
    // scope-exit recursive drop must keep this single-free clean.
    assert_clean_asan_run(
        r#"
fn main() {
    let mut src: Vec[(i64, String)] = Vec.new();
    let s = f"p{1}";
    src.push((1i64, s));
    println(src[0].1);
    println(src.len());
}
"#,
        &["p1", "1"],
        "vec_tuple_heap_element_string_var_push",
    );
}

#[test]
fn asan_match_variant_name_shared_across_enums_string_payload_no_leak() {
    // #39 (phase-12 self-hosting, parser stage): a bare variant name shared
    // by a value enum (`Tok.Str`) and a shared enum (`Expr.Str`) used to
    // bind a shared-enum String payload off the WRONG enum's word offsets —
    // reading a single word for a multi-word `SLit` and reconstructing a
    // garbage buffer pointer. Now that resolution pins to the match
    // scrutinee's own enum, the bound `n.value` owns a real buffer; the
    // arm consumes it (returns it out) so the LSan gate proves the
    // String is freed exactly once per iteration — no leak, no double-free.
    // Looped so a per-iteration leak accumulates visibly. This is the
    // parser's `Token.Str`/`Expr.Str` payload-read shape.
    assert_clean_asan_run(
        r#"
struct Sp { line: i64, column: i64, offset: i64, length: i64 }
struct SLit { value: String, span: Sp }
enum Tok { Str(String, Sp), Int(i64) }
shared enum Expr { Int(i64), Boolish(i64), Str(SLit) }
fn text_of(e: Expr) -> String {
    match e {
        Int(v) => v.to_string(),
        Boolish(v) => v.to_string(),
        Str(n) => n.value,
    }
}
fn tok_kind(t: Tok) -> i64 {
    match t { Str(s, sp) => 1, Int(v) => 2 }
}
fn main() {
    let mut total: i64 = 0;
    let mut i: i64 = 0;
    while i < 50 {
        let t = Tok.Str("tok".to_string(), Sp { line: 1, column: 1, offset: 0, length: 3 });
        total = total + tok_kind(t);
        let n = SLit { value: "hello world".to_string(), span: Sp { line: 1, column: 1, offset: 7, length: 11 } };
        let e = Expr.Str(n);
        let s = text_of(e);
        total = total + s.len();
        i = i + 1;
    }
    println(total);
}
"#,
        &["600"],
        "match_variant_name_shared_across_enums_string_payload",
    );
}

#[test]
fn asan_shared_enum_recursive_struct_payload_string_freed_no_leak() {
    // A `shared enum E` with a recursive Binary variant (`Add(Bin)`,
    // `Bin { left: E, right: E }`) AND a leaf variant whose plain-struct
    // payload owns a String (`Ident(Id)`, `Id { name: String }`) — the
    // self-hosted parser's `Expr.Binary(BinaryExpr)` / `Expr.Ident(IdentExpr
    // { name })` shape. When a leaf is a CHILD of a Binary box, the box's
    // recursive rc-drop frees the child box but used to SKIP its inline
    // struct payload's String (`field_is_walkable` only flagged structs
    // owning a SHARED field, not a String). The single-level case was freed
    // via the top-level match path, masking it. Fix: `field_is_walkable` /
    // the rc-drop struct branch now also walk a struct payload that owns a
    // String/Vec/heap field (`type_expr_has_drop_heap`), and
    // `emit_nested_struct_shared_rc_decs` gained a direct-String arm. Long
    // identifiers (≥36 B) so the leak is unambiguous under LSan (short ones
    // evade it via freed-but-reachable pointers).
    assert_clean_asan_run(
        r#"
struct Id { name: String, off: i64 }
struct Bin { left: E, right: E, off: i64 }
shared enum E { Ident(Id), Add(Bin) }
fn render(e: E) -> String {
    let mut out = "".to_string();
    match e {
        Ident(n) => { out.push_str(n.name); }
        Add(b) => {
            out.push_str(render(b.left));
            out.push_str(render(b.right));
        }
    }
    out
}
fn ident(s: String) -> E {
    E.Ident(Id { name: s, off: 0 })
}
fn make() -> E {
    E.Add(Bin {
        left: ident("left_identifier_long_enough_to_force_heap".to_string()),
        right: ident("right_identifier_long_enough_to_force_heap".to_string()),
        off: 0,
    })
}
fn main() {
    let mut total: i64 = 0;
    let mut i: i64 = 0;
    while i < 20 {
        total = total + render(make()).len();
        i = i + 1;
    }
    println(total);
}
"#,
        &["1660"],
        "shared_enum_recursive_struct_payload_string_freed",
    );
}

// ── `String ==` / `!=` must clamp its memcmp span to min(len) ──
//
// The equality path used to `memcmp(l_ptr, r_ptr, l_len)` unconditionally
// (the `Lt/Gt` path already clamped to `min_len`). When `l_len > r_len`
// that reads past the end of the SHORTER right buffer — a heap-buffer-
// overflow (ASAN-caught here, a latent OOB read in release). The
// `len_eq && data_eq` AND already makes unequal-length operands compare
// `false`, so clamping the compare span to `min(l_len, r_len)` is both
// memory-safe and semantics-preserving.
//
// This surfaced during phase-12 self-hosting slice 3a: the parser's
// `current_ident_matches` borrow-matches the `String` payload out of an
// indexed place (`self.tokens[self.pos].token`) and compares it against a
// short keyword (`dup_str(n) == "Fn"`). A type name longer than the keyword
// (`l_len > r_len`) overran the keyword literal's buffer. It was originally
// mis-attributed to a double-free of the borrowed payload; the borrow-match
// itself is sound (the indexed enum scrutinee is deep-cloned, so the bound
// payload owns an independent buffer freed exactly once). The faithful
// failing shape is reproduced below: a multi-variant enum whose `String`
// payload is borrow-matched out of a `Vec` element a struct owns, then
// compared against shorter literals. The payloads share a prefix with the
// shorter literal (`"OnceFnHandler"` vs `"OnceFn"`) so the read runs past
// the short buffer's end rather than stopping at a leading mismatch.
#[test]
fn asan_string_eq_mismatched_len_no_overread_in_indexed_payload_match() {
    assert_clean_asan_run(
        r#"
enum Tok {
    KwFn,
    KwOnceFn,
    Ident(String),
    Punct(String),
    Eof,
}
struct Span { line: i64, col: i64 }
struct SpannedTok { tok: Tok, span: Span }
struct Lexer { toks: Vec[SpannedTok], pos: i64 }

fn dup_str(s: ref String) -> String {
    let mut out = "".to_string();
    out.push_str(s);
    out
}

impl Lexer {
    // Borrow-match the payload out of an indexed place, then compare it (by an
    // owned dup) against a SHORTER literal. The dup's length exceeds the
    // literal's, so the pre-fix `memcmp(.., l_len)` overran the literal buffer.
    fn ident_matches(ref self, target: String) -> bool {
        match self.toks[self.pos].tok {
            Ident(n) => {
                let got = dup_str(n);
                got == target
            }
            _ => false,
        }
    }
}

fn mk(name: String) -> SpannedTok {
    SpannedTok { tok: Tok.Ident(name), span: Span { line: 1, col: 1 } }
}

fn main() {
    let mut toks = Vec.new();
    toks.push(mk("OnceFnHandler".to_string()));
    toks.push(mk("FnPtrFactory".to_string()));
    let lx = Lexer { toks: toks, pos: 0 };
    // Long payload (shares a prefix with the short literal) vs short keyword:
    // the comparison span must clamp to the keyword length, not the payload's.
    let m_fn = lx.ident_matches("Fn".to_string());
    let m_once = lx.ident_matches("OnceFn".to_string());
    let m_exact = lx.ident_matches("OnceFnHandler".to_string());
    println(m_fn);
    println(m_once);
    println(m_exact);
}
"#,
        &["false", "false", "true"],
        "asan_string_eq_mismatched_len_no_overread_in_indexed_payload_match",
    );
}

#[test]
fn asan_shared_enum_string_payload_moveout_no_double_free() {
    // B-2026-06-20: moving a `String`/`Vec` payload OUT of a SHARED-enum RC
    // box (`match e { S(s) => s }`, returned) only neutralized the LOCAL
    // binding's cap — the box's payload words still pointed at the moved-out
    // buffer, so the box's `__karac_rc_drop_<E>` (Vec/String arm frees
    // `cap > 0`) re-freed it after the caller already had: a double-free.
    // The minimal isolated shape (the recursive self-host render leak hit it
    // nested, where the boxed-struct-drop gap masked it as a leak instead).
    // Fixed by `suppress_shared_enum_payload_move_out`: zero the field's words
    // in the BOX so its rc-drop skips the buffer the binding now owns. The
    // ≥36-byte payload makes the (pre-fix) freed-then-freed buffer a real heap
    // block ASAN flags; Linux LSan covers the symmetric no-leak arm.
    assert_clean_asan_run(
        r#"
shared enum E { S(String), Other }
fn get(e: E) -> String {
    match e {
        S(s) => s,
        Other => "other".to_string(),
    }
}
fn main() {
    let e = E.S("shared-enum-moveout-payload-long-enough-string".to_string());
    println(get(e));
}
"#,
        &["shared-enum-moveout-payload-long-enough-string"],
        "shared_enum_string_payload_moveout_no_double_free",
    );
}

#[test]
fn asan_mapval_string_key_struct_value_no_leak() {
    // Slice 3r: `Map[String, Holder]` — the key half keeps the
    // `drop_key` flag contract while the value half rides the drop fn;
    // `karac_map_free_with_val_drop_fn` handles both simultaneously.
    assert_clean_asan_run(
        r#"
struct Holder { name: String, id: i64 }
fn main() {
    let mut m: Map[String, Holder] = Map.new();
    let h = Holder { name: f"holder payload padded out beyond thirty-six bytes {7}", id: 7 };
    m.insert(f"key padded out beyond thirty-six bytes for lsan {7}", h);
    println(m.len());
}
"#,
        &["1"],
        "mapval_string_key_struct_value_no_leak",
    );
}

#[test]
fn asan_getmove_match_string_payload_moveout_no_double_free() {
    // Slice 3s (B-2026-07-01-12): `let s = match m.get(k) { Some(x) => x,
    // … }` — Map.get is VALUE-typed (`Option[V]`), so the typechecker
    // blesses the move-out, but codegen bound `x` as an ALIAS of the
    // bucket's value (borrow-mode bind) and the escaping arm-tail handed
    // that alias to `s`, which frees it — double-free against the map's
    // `drop_val` walk (exit 133 pre-fix). The arm now deep-clones the
    // payload when the arm body moves the binding.
    assert_clean_asan_run(
        r#"
fn main() {
    let mut i = 0;
    while i < 3 {
        let mut m: Map[i64, String] = Map.new();
        m.insert(i, f"map string payload padded beyond thirty-six bytes {i}");
        let s = match m.get(i) {
            Some(x) => x,
            None => f"none-{i}",
        };
        println(s.len());
        i = i + 1;
    };
}
"#,
        &["51", "51", "51"],
        "getmove_match_string_payload_moveout_no_double_free",
    );
}

#[test]
fn asan_getmove_iflet_string_payload_moveout_no_double_free() {
    // Slice 3s: the if-let form of the arm-tail move-out.
    assert_clean_asan_run(
        r#"
fn main() {
    let mut i = 0;
    while i < 3 {
        let mut m: Map[i64, String] = Map.new();
        m.insert(i, f"map string payload padded beyond thirty-six bytes {i}");
        let s = if let Some(x) = m.get(i) { x } else { f"none-{i}" };
        println(s.len());
        i = i + 1;
    };
}
"#,
        &["51", "51", "51"],
        "getmove_iflet_string_payload_moveout_no_double_free",
    );
}

#[test]
fn asan_structpat_mapget_field_escape_no_double_free() {
    // Slice 3t: an ESCAPING destructured field over a `Map.get`
    // scrutinee (`Some(Holder { name, .. }) => name`) — the 3s clone
    // fixup extended to FIELD granularity (read-only fields stay
    // zero-cost aliases; the escapee owns an independent copy).
    assert_clean_asan_run(
        r#"
struct Holder { name: String, id: i64 }
fn main() {
    let mut i = 0;
    while i < 3 {
        let mut m: Map[i64, Holder] = Map.new();
        m.insert(i, Holder { name: f"holder payload padded beyond thirty-six bytes {i}", id: i });
        let s = match m.get(i) {
            Some(Holder { name, .. }) => name,
            None => f"none-{i}",
        };
        println(s.len());
        i = i + 1;
    };
}
"#,
        &["47", "47", "47"],
        "structpat_mapget_field_escape_no_double_free",
    );
}

#[test]
fn asan_boxelem_tuple_payload_escape_no_double_free() {
    // Slice 3u leg A: `Some((a, b)) => a` over `m.get(k)` — the tuple
    // flavor of the 3s escaping-borrow clone (exit 133 pre-fix). The
    // escaping tuple ELEMENT is cloned; read-only elements stay
    // aliases.
    assert_clean_asan_run(
        r#"
fn main() {
    let mut i = 0;
    while i < 3 {
        let mut m: Map[i64, (String, i64)] = Map.new();
        m.insert(i, (f"tuple payload padded beyond thirty-six bytes {i}", i));
        let s = match m.get(i) {
            Some((a, b)) => a,
            None => f"none-{i}",
        };
        println(s.len());
        i = i + 1;
    };
}
"#,
        &["46", "46", "46"],
        "boxelem_tuple_payload_escape_no_double_free",
    );
}

#[test]
fn asan_string_eq_temp_vs_ref_param_no_leak() {
    // B-2026-08-11-24 — a String equality between an UNBOUND TEMPORARY and
    // a `ref String` PARAMETER leaked the temporary on every evaluation.
    // Three conditions are jointly necessary: (1) one operand is an
    // unbound temp (a call result, not a binding); (2) the other is a
    // `ref`-mode parameter (a local or an owned param is fine); (3) the
    // operation is String equality. Side does not matter and `!=` behaves
    // like `==`.
    //
    // The cause was that the equality lowering suppressed the temp
    // operand's drop whenever EITHER operand was borrowed, instead of
    // suppressing it only for the borrowed one. The `ref` param is
    // correctly not dropped — the caller owns it — and the fresh temp on
    // the other side was then also not dropped, with nothing else ever
    // freeing it.
    //
    // These four are the regression pins; the over-fire controls (shapes
    // that were already clean and must stay clean) are in
    // `asan_string_eq_borrowed_operand_controls_stay_clean`.
    for (shape, src, expect) in [
        (
            "temp_eq_ref_param",
            r#"
fn f(hay: ref String, needle: ref String) -> bool { hay.substring(0, 4) == needle }
fn main() { let h = "abcdefgh"; let n = "abcd"; println(f"{f(h, n)}"); }
"#,
            "true",
        ),
        (
            // Operands swapped — the borrowed side is now on the left.
            "ref_param_eq_temp",
            r#"
fn f(hay: ref String, needle: ref String) -> bool { needle == hay.substring(0, 4) }
fn main() { let h = "abcdefgh"; let n = "abcd"; println(f"{f(h, n)}"); }
"#,
            "true",
        ),
        (
            "temp_ne_ref_param",
            r#"
fn f(hay: ref String, needle: ref String) -> bool { hay.substring(0, 4) != needle }
fn main() { let h = "abcdefgh"; let n = "zzzz"; println(f"{f(h, n)}"); }
"#,
            "true",
        ),
        (
            // The temp's producer is irrelevant — a user fn returning
            // String leaks exactly like the builtin `substring`.
            "user_fn_temp_eq_ref_param",
            r#"
fn mk(s: ref String) -> String { s.substring(0, 4) }
fn f(hay: ref String, needle: ref String) -> bool { mk(hay) == needle }
fn main() { let h = "abcdefgh"; let n = "abcd"; println(f"{f(h, n)}"); }
"#,
            "true",
        ),
    ] {
        assert_clean_asan_run(src, &[expect], &format!("string_eq_{shape}"));
    }
}

#[test]
fn asan_string_eq_scan_loop_is_not_unbounded() {
    // B-2026-08-11-24, the magnitude case and the reason it was filed
    // `high` rather than as a 4-byte curiosity. The triggering shape is
    // the ordinary substring-scan inner loop, so the leak was
    // proportional to INPUT SIZE with no bound — the dogfood program this
    // came from, a `{{key}}` renderer over one ~120-character template,
    // leaked 290 bytes in 145 allocations, one per scan step.
    //
    // The early `return` is deliberately absent here: an accumulator
    // variant leaks once per iteration too, so the loop (not the exit)
    // is the multiplier. A per-iteration leak is what LSan reports as
    // many small allocations rather than one, which is also why this
    // never looked like a "big" leak in any single fixture.
    assert_clean_asan_run(
        r#"
fn count_from(hay: ref String, needle: ref String) -> i64 {
    let h = hay.len();
    let n = needle.len();
    let mut i = 0;
    let mut hits = 0;
    while i + n <= h {
        if hay.substring(i, i + n) == needle { hits = hits + 1; }
        i = i + 1;
    }
    hits
}
fn main() {
    let hay = "abcabcabcabcabc";
    let needle = "abc";
    println(f"{count_from(hay, needle)}");
}
"#,
        &["5"],
        "string_eq_scan_loop",
    );
}

#[test]
fn asan_string_eq_borrowed_operand_controls_stay_clean() {
    // B-2026-08-11-24 over-fire controls. Every shape here was ALREADY
    // clean before the fix, and a naive fix — always drop the temp
    // operand — would break the ones whose operand is genuinely borrowed
    // or already owned elsewhere. They are the half of the matrix that
    // says the fix suppressed the drop for the RIGHT operand rather than
    // simply stopping suppressing.
    for (shape, src, expect) in [
        // A LOCAL on the other side: no borrowed operand, so no
        // suppression was ever triggered.
        (
            "temp_eq_local",
            r#"
fn f(hay: ref String) -> bool { let n = "abcd"; hay.substring(0, 4) == n }
fn main() { let h = "abcdefgh"; println(f"{f(h)}"); }
"#,
            "true",
        ),
        // No parameters at all.
        (
            "temp_eq_local_in_main",
            r#"
fn main() {
    let h = "abcdefgh";
    let n = "abcd";
    println(f"{h.substring(0, 4) == n}");
}
"#,
            "true",
        ),
        // `let`-binding the temp gives it ordinary scope cleanup instead
        // of the comparison's temp path.
        (
            "bound_temp_eq_ref_param",
            r#"
fn f(hay: ref String, needle: ref String) -> bool { let s = hay.substring(0, 4); s == needle }
fn main() { let h = "abcdefgh"; let n = "abcd"; println(f"{f(h, n)}"); }
"#,
            "true",
        ),
        // An OWNED-mode param is not borrowed, so nothing is suppressed.
        (
            "temp_eq_owned_param",
            r#"
fn f(hay: ref String, needle: String) -> bool { hay.substring(0, 4) == needle }
fn main() { let h = "abcdefgh"; let n = "abcd"; println(f"{f(h, n)}"); }
"#,
            "true",
        ),
        // Same two values, no comparison — isolates the comparison as the
        // faulty step rather than the temp's construction.
        (
            "bound_temp_no_comparison",
            r#"
fn f(hay: ref String, needle: ref String) -> i64 { let s = hay.substring(0, 4); s.len() + needle.len() }
fn main() { let h = "abcdefgh"; let n = "abcd"; println(f"{f(h, n)}"); }
"#,
            "8",
        ),
        // CONCAT with a `ref` param — the same operand pairing under a
        // different operation, which never leaked. Confirms the defect is
        // local to the equality lowering.
        (
            "temp_concat_ref_param",
            r#"
fn f(hay: ref String, suffix: ref String) -> i64 { let s = hay.substring(0, 4) + suffix; s.len() }
fn main() { let h = "abcdefgh"; let x = "!!"; println(f"{f(h, x)}"); }
"#,
            "6",
        ),
        // The temp as a method ARGUMENT rather than an equality operand.
        (
            "temp_as_method_arg",
            r#"
fn mk(s: ref String) -> String { s.substring(0, 4) }
fn f(hay: ref String, other: ref String) -> bool { other.contains(mk(hay)) }
fn main() { let h = "abcdefgh"; let o = "xxabcdyy"; println(f"{f(h, o)}"); }
"#,
            "true",
        ),
        // Derived struct equality carrying a String field, against a `ref`
        // param — a different equality lowering, which stayed clean.
        // Derived struct equality against a `ref` param with a LITERAL
        // String field — nothing heap-allocated, so nothing to leak. Its
        // heap-field sibling DOES leak and is deferred as
        // B-2026-08-11-33; this row stays because it records that the
        // struct path only misbehaves when there is something to free,
        // which is exactly why the filing row's control read as clean.
        (
            "derive_eq_struct_literal_field",
            r#"
#[derive(Eq)]
struct P { name: String }
fn mk() -> P { P { name: "abcd" } }
fn f(other: ref P) -> bool { mk() == other }
fn main() {
    let p = P { name: "abcd" };
    println(f"{f(p)}");
}
"#,
            "true",
        ),
    ] {
        assert_clean_asan_run(src, &[expect], &format!("string_eq_ctl_{shape}"));
    }
}

#[test]
fn asan_gsort_vec_of_vec_string_sorts_and_frees() {
    // B-2026-06-30-15: `Vec[Vec[String]].sort()` — codegen previously
    // errored ("supports integer and String element types") and the
    // INTERPRETER silently no-op'd (value_compare had no Array arm, so
    // nested Vecs compared Equal and stable sort preserved insertion
    // order). Both fixed: the recursive `karac_cmp_Vec_String`
    // lexicographic comparator + the interp Array/Slice compare arms.
    // Assertions drain via pop() (owned move-out) — index reads of
    // heap elements have a PRE-EXISTING leak class unrelated to sort.
    assert_clean_asan_run(
        r#"
fn main() {
    let mut outer: Vec[Vec[String]] = Vec.new();
    let mut a: Vec[String] = Vec.new();
    a.push(f"banana payload padded beyond thirty-six bytes {1}");
    a.push(f"apple payload padded beyond thirty-six bytes {1}");
    let mut b: Vec[String] = Vec.new();
    b.push(f"apple payload padded beyond thirty-six bytes {1}");
    let mut c: Vec[String] = Vec.new();
    outer.push(a);
    outer.push(b);
    outer.push(c);
    outer.sort();
    while let Some(row) = outer.pop() {
        println(row.len());
    }
    println(outer.len());
}
"#,
        &["2", "1", "0", "0"],
        "gsort_vec_of_vec_string_sorts_and_frees",
    );
}

#[test]
fn asan_generic_bound_default_method_string_no_leak() {
    // B-2026-07-03-11: a trait DEFAULT method (`greeting`) dispatched
    // through a generic BOUND (`describe[G: Greeter]`) on a String-carrying
    // implementor. `greeting()` concatenates a fresh heap String
    // (`"hi " + self.name()`) which flows out through the mono return. Loop
    // it with a >=36-byte payload so any leak of the returned String buffer
    // (or the intermediate `name()` result) trips Linux LSan / macOS ASan.
    assert_clean_asan_run(
        r#"
trait Greeter {
    fn name(self) -> String;
    fn greeting(self) -> String { "hi " + self.name() }
}
struct Person { id: i64 }
impl Greeter for Person {
    fn name(self) -> String { "a sufficiently long greeter name payload" }
}
fn describe[G: Greeter](g: G) -> String { g.greeting() }
fn main() {
    let mut i = 0i64;
    let mut acc = 0i64;
    while i < 50i64 {
        let g = describe(Person { id: i });
        acc = acc + g.len();
        i = i + 1;
    }
    println(f"{acc}");
}
"#,
        &["2150"],
        "generic_bound_default_method_string_no_leak",
    );
}

#[test]
#[ignore = "B-2026-08-05-35 sweep: does not compile — no codegen handler for `len` on a generic slice element binding. Silently SKIPPED until the harness learned to fail on a codegen error."]
fn asan_generic_slice_elem_string_return_no_leak() {
    // B-2026-07-03-22: a generic `-> T` whose `T` binds from a `Slice[T]`
    // param element (`gsum[T](s: Slice[T]) -> T { s[0] }`) called with a
    // `Vec[String]` now resolves `T = String`, so `s[0]` returns a genuine
    // String struct rather than reading the element's 8-byte heap pointer
    // as an `i64`. `s[0]` must CLONE the element out (the Vec still owns its
    // copy); loop it with a >=36-byte payload so any missing clone (alias →
    // double-free with the Vec's drop) or leaked clone trips macOS ASan /
    // Linux LSan.
    assert_clean_asan_run(
        r#"
fn gsum[T](s: Slice[T]) -> T { s[0] }
fn main() {
    let mut i = 0i64;
    let mut acc = 0i64;
    while i < 50i64 {
        let vs: Vec[String] = ["a sufficiently long slice element payload", "second sufficiently long element payload"];
        let e = gsum(vs);
        acc = acc + e.len();
        i = i + 1;
    }
    println(f"{acc}");
}
"#,
        &["2050"],
        "generic_slice_elem_string_return_no_leak",
    );
}

/// B-2026-07-03-30 (Vec-element drain) — a struct field `Vec[String]` (whose
/// elements own heap the outer buffer-free misses) is DRAINED per element by
/// the synthesized struct drop when the owning struct is PLAIN-dropped (a
/// `Vec[A]` element). Before the fix, `emit_struct_drop_synthesis`'s
/// VecOrString arm freed only the `{ptr,len,cap}` buffer, leaking every
/// element's char buffer (`vec_elem_agg_drop_for_type_expr` returned `None`
/// for a direct `String` element). Payloads >=40 bytes for LSan visibility.
#[test]
fn asan_struct_vec_string_field_plain_drop_drains_elements() {
    assert_clean_asan_run(
        r#"
struct A { path: Vec[String] }
fn main() {
    let mut v: Vec[A] = Vec.new();
    let mut i = 0;
    while i < 6 {
        let mut p: Vec[String] = Vec.new();
        p.push("struct_vec_string_plaindrop_element_payload".to_string());
        p.push("struct_vec_string_plaindrop_element_second_".to_string());
        v.push(A { path: p });
        i = i + 1;
    }
    println(v.len());
}
"#,
        &["6"],
        "struct_vec_string_field_plain_drop_drains_elements",
    );
}

/// B-2026-07-03-30 (Vec-element drain) — destructure-consume peer of the plain-drop
/// test: `let A { path } = a` (with `a` a callee-owned by-value param that is
/// deep-copied at entry) then `for s in path` consumes the elements. The
/// entry-copy is element-DEEP for the drained `Vec[String]` field
/// (`param_own.rs`, restoring the copy-depth == drop-depth invariant), so the
/// callee's copy owns independent char buffers — no double-free against the
/// caller's retained original, no leak.
#[test]
fn asan_struct_vec_string_field_destructure_consume_clean() {
    assert_clean_asan_run(
        r#"
struct A { path: Vec[String] }
fn f(a: A) -> i64 {
    let A { path } = a;
    let mut t = 0;
    for s in path { if s.len() >= 0 { t = t + 1; } }
    t
}
fn build() -> Vec[A] {
    let mut v: Vec[A] = Vec.new();
    let mut i = 0;
    while i < 6 {
        let mut p: Vec[String] = Vec.new();
        p.push("struct_vec_string_destructure_element_payld".to_string());
        v.push(A { path: p });
        i = i + 1;
    }
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
        "struct_vec_string_field_destructure_consume_clean",
    );
}

#[test]
fn asan_forloop_element_destructure_option_string_field_match_consume_no_double_free() {
    // B-2026-07-10-4 (the residual attr-item double-free, minimal E1 form):
    // DESTRUCTURING an `Option[String]` field OUT of a for-loop element and
    // then match-consuming it. Bare `for` BORROWS the collection (design.md
    // §2601/§2751), so `a` is a bit-copy VIEW of `items`'s element slot; the
    // extracted `string_value` aliases the element's `Option[String]` buffer,
    // which `items`'s scope-exit per-element drain frees. Without the
    // clone-on-extract routing (`for_loop_owned_agg_vars` → `view_src` +
    // `clone_on_extract_view_field`'s `Option[inline-heap]` leg) the match
    // frees the aliased buffer AND the drain frees it again — a double-free
    // (`corrupted size vs. prev_size in fastbins`). This is the exact shape of
    // the 12 residual attribute-item crashers (`AttrNode.string_value`).
    assert_clean_asan_run(
        r#"
struct AttrNode { string_value: Option[String] }
fn parse_attrs() -> Vec[AttrNode] {
    let mut a: Vec[AttrNode] = Vec.new();
    let mut i = 0;
    while i < 6 {
        a.push(AttrNode { string_value: Some("forloop_destructure_option_string_payload_xx".to_string()) });
        i = i + 1;
    }
    a
}
fn main() {
    let items = parse_attrs();
    let mut n: i64 = 0;
    for a in items {
        let AttrNode { string_value } = a;
        match string_value {
            Some(s) => { n = n + s.len(); }
            None => {}
        }
    }
    println(n);
}
"#,
        &["264"], // 6 * len("forloop_destructure_option_string_payload_xx") = 6 * 44
        "forloop_element_destructure_option_string_field_match_consume_no_double_free",
    );
}

#[test]
fn asan_forloop_element_destructure_string_field_move_into_collector_no_double_free() {
    // B-2026-07-10-4 sibling (E4 form): DESTRUCTURE a `String` field out of a
    // for-loop element and MOVE it into a collector Vec. The borrowed-element
    // leaf aliases `items`'s per-element buffer; moving it into `collected`
    // gives `collected` an aliasing owner, so `collected`'s drain AND `items`'s
    // per-element drain free the same buffer. Clone-on-extract's String leg
    // deep-copies the leaf so `collected` owns an independent buffer. (The
    // borrow-only sibling — `let A { s } = a; use s` without a move — is
    // covered by `..._clean_shapes_stay_clean`, which the clone must keep leak-
    // free: the extra copy is freed at scope exit.)
    assert_clean_asan_run(
        r#"
struct A { s: String }
fn build() -> Vec[A] {
    let mut v: Vec[A] = Vec.new();
    let mut i = 0;
    while i < 5 { v.push(A { s: "forloop_destructure_string_move_into_collector_x".to_string() }); i = i + 1; }
    v
}
fn main() {
    let items = build();
    let mut collected: Vec[String] = Vec.new();
    for a in items {
        let A { s } = a;
        collected.push(s);
    }
    let mut n: i64 = 0;
    for c in collected {
        n = n + c.len();
    }
    println(n);
}
"#,
        &["240"], // 5 * len("forloop_destructure_string_move_into_collector_x") = 5 * 48
        "forloop_element_destructure_string_field_move_into_collector_no_double_free",
    );
}

#[test]
fn asan_option_string_field_survives_caller_retains_vec_copy() {
    // B-2026-07-10-4 final residual (the last 2 attr-item crashers):
    // a `Vec[<struct{Option[String]}>]` local moved into a by-value
    // callee that WRAPS it into a returned struct. By-value Vec params
    // are caller-retains, so the consume site deep-copies — but (1)
    // `emit_vecstr_defensive_copy`'s aggregate-element leg was gated on
    // `type_expr_has_drop_heap`, which hardcodes Option => false, so an
    // Option-only-heap element skipped the per-element deep clone
    // entirely; and (2) even when the leg fired (element also owns a
    // `Vec[String]` field, the real `AttrNode.path`),
    // `karac_clone_struct_<S>`'s `Option[String]` field child fell
    // through to the SHALLOW primitive clone (the type-erased `Option`
    // layout records no heap kinds). Either way both copies' drops freed
    // the same `Some` payload. Fixed by `emit_option_value_clone_fn`
    // (tag-guarded deep clone) + the `te_owns_option_heap_payload`
    // copy-side gate. Covers both shapes: Option-only element (gate) and
    // Vec[String]+Option element (clone-fn child), plus a None element
    // (tag guard no-op) and a method-call chain (the self-host parser's
    // `parse_item` → `parse_trait_def(attrs)` shape).
    assert_clean_asan_run(
        r#"
struct AttrNode { path: Vec[String], string_value: Option[String] }
struct Bare { string_value: Option[String] }
struct Node { attributes: Vec[AttrNode] }
struct BareNode { attributes: Vec[Bare] }
struct P { pos: i64 }
impl P {
    fn wrap(mut ref self, attrs: Vec[AttrNode]) -> Node {
        self.pos = self.pos + 1;
        Node { attributes: attrs }
    }
}
fn wrap_bare(attrs: Vec[Bare]) -> BareNode {
    BareNode { attributes: attrs }
}
fn build() -> Vec[AttrNode] {
    let mut v: Vec[AttrNode] = Vec.new();
    let mut p: Vec[String] = Vec.new();
    p.push("path_segment_payload_alpha_x".to_string());
    v.push(AttrNode { path: p, string_value: Some("option_string_payload_beta_yy".to_string()) });
    v.push(AttrNode { path: Vec.new(), string_value: None });
    v
}
fn build_bare() -> Vec[Bare] {
    let mut v: Vec[Bare] = Vec.new();
    v.push(Bare { string_value: Some("bare_option_payload_gamma_zzz".to_string()) });
    v
}
fn main() {
    let mut n: i64 = 0;
    let mut i = 0;
    while i < 6 {
        let mut prs = P { pos: 0 };
        let attrs = build();
        let node = prs.wrap(attrs);
        let Node { attributes } = node;
        for a in attributes {
            let AttrNode { path, string_value } = a;
            for seg in path { n = n + seg.len(); }
            match string_value { Some(s) => { n = n + s.len(); } None => {} }
        }
        let battrs = build_bare();
        let bnode = wrap_bare(battrs);
        let BareNode { attributes } = bnode;
        for b in attributes {
            let Bare { string_value } = b;
            match string_value { Some(s) => { n = n + s.len(); } None => {} }
        }
        i = i + 1;
    }
    println(n);
}
"#,
        &["516"], // 6 * (28 + 29 + 29)
        "option_string_field_survives_caller_retains_vec_copy",
    );
}

#[test]
fn asan_forloop_string_element_whole_move_let_no_double_free() {
    // B-2026-07-05-2 sibling (Vec/String leg): `for s in words { let x = s }`
    // — the Vec/String element type the struct fix did not touch. Only the
    // push/insert/entry consume sites were covered; the plain whole-move
    // let-bind aliased the container element and double-freed.
    assert_clean_asan_run(
        r#"
fn build() -> Vec[String] {
    let mut v: Vec[String] = Vec.new();
    let mut i = 0;
    while i < 5 {
        let mut s = String.new();
        s.push_str("string_element_whole_move_heap_payload_pi_");
        s.push_str(i.to_string());
        v.push(s);
        i = i + 1;
    }
    v
}
fn main() {
    let words = build();
    let mut n: i64 = 0;
    for s in words {
        let x = s;
        n = n + x.len();
    }
    println(n);
}
"#,
        &["215"], // 5 * 43 (each payload is 42 + 1 digit)
        "forloop_string_element_whole_move_let_no_double_free",
    );
}

// ── B-2026-07-11-32: non-Copy element index-swap / projection-assign ──
// An index-read of a NON-COPY Vec element in ASSIGNMENT-RHS position
// (`s = v[i]`, `v[i] = v[j]`) aliased the source slot's buffer — the assign
// path only loaded the `{ptr,len,cap}` header, unlike the Let arm which
// deep-clones — so the destination and the source element co-owned the
// buffer and double-freed at scope exit. The natural in-place swap idiom
// `let t = v[i]; v[i] = v[j]; v[j] = t;` over any non-Copy element (String,
// Vec, struct) was therefore a silent double-free (correct output, then
// `free(): double free detected` on a hardened allocator / ASAN). Separately,
// an f-string TEMPORARY stored into a projection place (`v[i] = f"…"`,
// `p.field = f"…"`) never had its accumulator cap zeroed for the index /
// AoS-field targets, double-freeing the acc buffer. Fixed in
// src/codegen/stmts.rs (clone the index-read assign-RHS; generalise the
// acc-zero to the index/field stores).

#[test]
fn asan_index_swap_string_no_double_free() {
    // The flagship: the classic swap idiom over `Vec[String]`. Before the
    // fix `v[0] = v[1]` aliased slot 1's buffer, double-freeing at scope
    // exit. Value semantics (interpreter oracle): the sequence swaps slots
    // 0 and 1.
    assert_clean_asan_run(
        r#"
fn main() {
    let mut v: Vec[String] = [f"alpha-padding", f"bravo-padding", f"charlie-pad"];
    let t = v[0].clone();
    v[0] = v[1].clone();
    v[1] = t;
    println(v[0]);
    println(v[1]);
    println(v[2]);
}
"#,
        &["bravo-padding", "alpha-padding", "charlie-pad"],
        "index_swap_string_no_double_free",
    );
}

#[test]
fn asan_fstring_into_vec_element_no_double_free() {
    // `v[i] = f"…"` — an f-string TEMPORARY stored into a Vec element slot.
    // The store moves the acc buffer into the slot; the acc's own scope-exit
    // free double-freed it until the index-store arm learned to zero the acc
    // cap (mirroring the Identifier arm). The old element must also be freed
    // (no leak).
    assert_clean_asan_run(
        r#"
fn main() {
    let mut v: Vec[String] = [f"aa-padding", f"bb-padding"];
    v[0] = f"zz-padding";
    println(v[0]);
    println(v[1]);
}
"#,
        &["zz-padding", "bb-padding"],
        "fstring_into_vec_element_no_double_free",
    );
}

#[test]
fn asan_fstring_into_struct_field_no_double_free() {
    // `p.name = f"…"` — an f-string temporary stored into an AoS struct
    // field. Same acc double-free as the Vec-element case; the field-store
    // arm previously zeroed the acc only for SoA element fields.
    assert_clean_asan_run(
        r#"
struct P { name: String }
fn main() {
    let mut p = P { name: f"aa-padding" };
    p.name = f"zz-padding";
    println(p.name);
}
"#,
        &["zz-padding"],
        "fstring_into_struct_field_no_double_free",
    );
}

#[test]
fn asan_char_to_string_no_leak() {
    // `From[char] for String`: `String.from(c)` / `c.into()` allocate a
    // fresh heap String per call. Loops both surfaces so any per-iteration
    // leak or bad free accumulates for ASAN + LSan. The bound String is
    // consumed (`.len()`) and dropped each iteration; a multibyte char
    // exercises a >1-byte allocation.
    assert_clean_asan_run(
        r#"
fn main() {
    let mut i: i64 = 0;
    let mut total: i64 = 0;
    while i < 200 {
        let a: String = String.from('A');
        total = total + a.len();
        let c: char = '😀';
        let b: String = c.into();
        total = total + b.len();
        i = i + 1;
    }
    println(f"{total}");
}
"#,
        &["1000"],
        "char_to_string_no_leak",
    );
}

#[test]
fn asan_enum_self_to_string_no_leak() {
    // `self.to_string()` inside an impl method renders a payload
    // `#[derive(Display)]` enum into a fresh heap String each call
    // (B-2026-07-12-15). Loops both variants so any per-iteration leak or
    // bad free accumulates for ASAN + LSan; the rendered String is consumed
    // (`.len()`) directly — not through a generic f-string, whose
    // interpolation leak (B-2026-07-12-18) is a separate pre-existing path.
    assert_clean_asan_run(
        r#"
#[derive(Display)]
enum IoErr { NotFound, Other(String) }
trait Error { fn message(ref self) -> String; }
impl Error for IoErr { fn message(ref self) -> String { self.to_string() } }
fn main() {
    let mut i: i64 = 0;
    let mut total: i64 = 0;
    while i < 200 {
        let a: IoErr = IoErr.NotFound;
        let b: IoErr = IoErr.Other(String.from("disk full"));
        total = total + a.message().len();
        total = total + b.message().len();
        i = i + 1;
    }
    println(f"{total}");
}
"#,
        &["4800"],
        "enum_self_to_string_no_leak",
    );
}

#[test]
fn asan_struct_to_string_returned_from_fn_no_double_free() {
    // B-2026-07-12-17: a struct `.to_string()` returned directly from a
    // function double-freed the rendered buffer (the return-position
    // fstr-acc ownership transfer missed the `.to_string()` shape). Loops a
    // `ref`-param return and a `self.to_string()` return so any double-free
    // / leak accumulates for ASAN + LSan; the rendered String is consumed
    // (`.len()`) each iteration.
    assert_clean_asan_run(
        r#"
#[derive(Display)]
struct Point { x: i64, y: i64 }
impl Point { fn describe(ref self) -> String { self.to_string() } }
fn render(p: ref Point) -> String { p.to_string() }
fn main() {
    let mut i: i64 = 0;
    let mut total: i64 = 0;
    while i < 200 {
        let p: Point = Point { x: 3, y: 4 };
        total = total + render(p).len();
        total = total + p.describe().len();
        i = i + 1;
    }
    println(f"{total}");
}
"#,
        &["8000"],
        "struct_to_string_returned_from_fn_no_double_free",
    );
}

#[test]
fn asan_generic_ref_enum_display_no_leak() {
    // B-2026-07-12-18: rendering a payload `#[derive(Display)]` enum through
    // a generic `ref E` param (`f"{e}"`) leaked the render buffer under
    // codegen (a symptom of reading the value from the wrong address; fixed
    // via `get_data_ptr`). Loops the generic-ref f-string so any per-call
    // leak accumulates for ASAN + LSan; the rendered String is consumed
    // (`.len()`).
    assert_clean_asan_run(
        r#"
#[derive(Display)]
enum IoErr { NotFound, Other(String) }
fn wrap[E: Display](e: ref E) -> String { f"error: {e}" }
fn main() {
    let mut i: i64 = 0;
    let mut total: i64 = 0;
    while i < 200 {
        let b: IoErr = IoErr.Other(String.from("boom"));
        total = total + wrap(b).len();
        i = i + 1;
    }
    println(f"{total}");
}
"#,
        &["3600"],
        "generic_ref_enum_display_no_leak",
    );
}

#[test]
fn asan_generic_fn_string_temp_arg_no_leak() {
    // B-2026-07-14-12: a fresh-heap `String` TEMP arg (a fn-return, not a
    // named binding) passed to a GENERIC fn leaked the temp's buffer — the
    // mono body clones the `String` param into its owned copy, orphaning the
    // caller's temp, which the generic-call path (unlike the non-generic one)
    // never materialized a drop for. Exercises the multi-use (`dup`) shape
    // and the passthrough (`passthru`) shape, both with a temp arg. Must be
    // leak-clean.
    assert_clean_asan_run(
        r#"
fn mk() -> String { let mut s = String.from(""); s.push_str("abcdefghijklmno"); s }
fn wrap[T](x: T) -> Vec[T] { let mut v: Vec[T] = Vec.new(); v.push(x); v }
fn passthru[T](x: T) -> T { x }
fn main() {
    let a = wrap(mk());
    let b = passthru(mk());
    println(a.len());
    println(b.len());
}
"#,
        &["1", "15"],
        "generic_fn_string_temp_arg_no_leak",
    );
}

#[test]
fn asan_fold_string_accumulator_no_double_free() {
    // B-2026-07-13-18: `iter().fold(String.from(""), |acc,x| f"{acc}-{x}")`
    // — a heap-accumulator string-join fold. Codegen desugars it AFTER
    // typecheck, so without the accumulator's recorded type the synthetic
    // `let mut acc` never registered as a tracked String and the Assign
    // move-machinery was skipped: the accumulator buffer double-freed
    // (`free(): double free`), and an intermediate restructure leaked every
    // middle buffer. The fix stamps the typechecker-recorded accumulator
    // type on the synthetic `let` and lowers to the self-referential
    // `acc = f"{acc}-{x}"` shape a hand-written loop uses. Must be
    // double-free-AND-leak-clean.
    assert_clean_asan_run(
        r#"
fn main() {
    let v: Vec[i64] = [1, 2, 3];
    let j = v.iter().fold(String.from(""), |acc, x| f"{acc}-{x}");
    println(j);
}
"#,
        &["-1-2-3"],
        "fold_string_accumulator_no_double_free",
    );
}

#[test]
fn asan_question_nested_option_string_payload_no_leak() {
    // B-2026-07-13-19: `?` on `Result[Option[String], E]` rebuilds the
    // extracted `Option[String]` from the Result's payload words. A wrong
    // reconstruction (dropped `cap` word, or truncation to `w0`) would leave
    // the String with a garbage cap → invalid free / leak. Must round-trip
    // the heap String through `?`, the `match`, and the returned `Ok(s)`
    // leak-clean.
    assert_clean_asan_run(
        r#"
enum E { X }
fn inner(n: i64) -> Result[Option[String], E] {
    if n < 0 { return Err(E.X); }
    Ok(Some(f"got-{n}"))
}
fn outer(n: i64) -> Result[String, E] {
    let opt = inner(n)?;
    match opt { Some(s) => Ok(s), None => Ok(f"empty") }
}
fn main() {
    match outer(7) { Ok(v) => println(v), Err(_) => println("e") }
}
"#,
        &["got-7"],
        "question_nested_option_string_payload_no_leak",
    );
}

#[test]
fn asan_fold_string_accumulator_over_map_no_leak() {
    // Sibling of the above with a fused `map` adaptor ahead of the fold, so
    // the loop var is the adaptor's param (`y`) and the accumulator is the
    // fold's own param (`acc`). Exercises the collision-free direct-acc path
    // with a non-trivial fused chain. Leak/double-free-clean.
    assert_clean_asan_run(
        r#"
fn main() {
    let v: Vec[i64] = [1, 2, 3];
    let j = v.iter().map(|y| y * 2).fold(String.from("<"), |acc, x| f"{acc}{x},");
    println(j);
}
"#,
        &["<2,4,6,"],
        "fold_string_accumulator_over_map_no_leak",
    );
}

/// B-2026-08-05-7 (boxed-envelope leg 2) — the owned boxed-payload enum
/// param drop must fire ONLY for a param consumed in place.
///
/// That registration originally ran for every owned param, which freed a
/// box the callee had already handed on. Both escaping shapes are here and
/// both were hard failures, not leaks: `ident` returns its param (freeing
/// the box it just returned — `free(): double free detected in tcache 2`,
/// and at the DEFAULT -O2 as well as -O0), and `fwd` forwards its param to
/// a second by-value param (freed by both callees, then SIGSEGV at -O0).
/// The fix reuses the type-agnostic escape walk the `Result[shared]` param
/// arm already runs, so an escaping param stays unregistered and the
/// terminal consumer's free remains the only one.
///
/// `get` is the consume-in-place shape the leak fix targets and must stay
/// registered — a green run here means neither over- nor under-freeing.
///
/// Floored per B-2026-08-04-17, same three rules as its sibling above.
#[test]
fn asan_boxed_enum_param_escape_no_double_free() {
    assert_clean_asan_run_min_allocs(
        r#"
enum Opt[T] { Yes(T), No }
fn mk(n: i64, i: i64) -> String {
    let mut s: String = String.new();
    s.push_str("payload-");
    s.push_str((n + i).to_string());
    s.push_str("-padding-to-force-heap");
    s
}
fn get(o: Opt[String], d: i64) -> i64 {
    match o {
        Opt.Yes(s) => if s.contains("payload") { 1 } else { 0 },
        Opt.No => d,
    }
}
fn fwd(o: Opt[String]) -> i64 { get(o, -1) }
fn ident(o: Opt[String]) -> Opt[String] { o }
fn main() {
    let n = env.args().len() as i64;
    let mut acc: i64 = 0;
    let mut i: i64 = 0;
    while i < 60 {
        acc = acc + get(Opt.Yes(mk(n, i)), -1);
        acc = acc + fwd(Opt.Yes(mk(n, i)));
        let back: Opt[String] = ident(Opt.Yes(mk(n, i)));
        match back {
            Opt.Yes(s) => { if s.ends_with("heap") { acc = acc + 1; } }
            Opt.No => { acc = acc - 1; }
        }
        acc = acc + get(Opt.No, 1);
        i = i + 1;
    }
    println(acc);
}
"#,
        &["240"],
        "boxed_enum_param_escape",
        200,
    );
}

#[test]
fn asan_fstring_fresh_string_temp_interp_no_leak() {
    // B-2026-07-15-12: a String-returning call/method/slice embedded
    // DIRECTLY in an f-string interpolation (`f"{obj.describe()}"`,
    // `f"{greet(x)}"`, `f"{s[a..b]}"`) leaked its buffer — the f-string
    // append COPIES the bytes into the accumulator, leaving the temp
    // unreferenced, and fstr_render_part's plain-String path (unlike the
    // user-Display / enum / collection arms) never scope-tracked it. Scales
    // with the interpolation count and is unbounded in a loop. Fixed by
    // tracking the fresh-owned-temp String buffer for scope-exit free.
    // This test ALSO pins the double-free boundary: an identifier String
    // (owned elsewhere) and the f-string result moved into a Vec must NOT
    // be freed twice.
    assert_clean_asan_run(
        r#"
struct Dog { name: String }
impl Dog {
    fn describe(ref self) -> String { f"dog {self.name}" }
}
fn greet(n: String) -> String { f"hi {n}" }
fn make(n: i64) -> String { f"item-{n}" }
fn main() {
    let d: Dog = Dog { name: "Rex" };
    // method-call String temp, twice in one f-string
    println(f"{d.describe()} and {d.describe()}");
    // free-fn String temp
    println(f"[{greet("bob")}]");
    // identifier String (owned) must not double-free
    let name: String = f"world";
    println(f"hi {name}");
    println(name);
    // String slice temp
    let s: String = "hello world";
    println(f"[{s[0..5]}]");
    // fresh-temp interp in a loop, result moved into a Vec (no double-free)
    let mut v: Vec[String] = Vec.new();
    let mut i: i64 = 0;
    while i < 5 {
        v.push(f"row {make(i)}");
        i = i + 1;
    }
    println(f"{v.len()}");
}
"#,
        &[
            "dog Rex and dog Rex",
            "[hi bob]",
            "hi world",
            "world",
            "[hello]",
            "5",
        ],
        "fstring_fresh_string_temp_interp_no_leak",
    );
}

#[test]
fn asan_option_ok_or_string_err_no_leak() {
    // B-2026-07-15-15: `Option.ok_or(<String>)` on a `None` packs the String
    // Err payload into the Result's Err slots. Verify the whole
    // build → match → consume round-trip of the heap Err payload is
    // leak-clean (and the fix that builds the Result layout doesn't strand
    // the packed String buffer).
    assert_clean_asan_run(
        r#"
fn main() {
    let mut i: i64 = 0;
    let mut errs: i64 = 0;
    while i < 6 {
        let o: Option[i64] = if i % 2 == 0 { Some(i * 10) } else { None };
        let r: Result[i64, String] = o.ok_or(f"absent-{i}-padded-well-past-inline-width");
        match r {
            Ok(v) => println(v),
            Err(e) => { errs = errs + e.len(); },
        }
        i = i + 1;
    }
    println(errs);
}
"#,
        &["0", "20", "40", "114"],
        "option_ok_or_string_err_no_leak",
    );
}

#[test]
fn asan_partition_heap_string_no_leak() {
    // B-2026-07-19-15 follow-on — `partition` over HEAP elements. Each pushed
    // element is CLONED into its target Vec, so the two result Vecs own
    // independent buffers and the BORROWED source keeps its own. The memory
    // contract: every cloned String frees exactly once (via the for-loop
    // consumers), the source Vec frees its own buffers exactly once at scope
    // exit — a bug is a per-iteration leak (clone not freed) or a double-free
    // (target Vec aliasing the source element). Looped for LSan.
    assert_clean_asan_run(
        r#"
fn main() {
    let mut i: i64 = 0;
    let mut acc: i64 = 0;
    while i < 40 {
        let words: Vec[String] = ["apple".to_string(), "banana".to_string(), "avocado".to_string(), "cherry".to_string()];
        let (a, other): (Vec[String], Vec[String]) = words.iter().partition(|w| w.starts_with("a"));
        for w in a { acc = acc + w.len(); }
        for w in other { acc = acc + w.len(); }
        // Source survives and is used, so its buffers must still be live here.
        acc = acc + words.len();
        i = i + 1;
    }
    println(acc);
}
"#,
        // per iter: a=[apple(5),avocado(7)]=12, other=[banana(6),cherry(6)]=12,
        // + words.len()=4 → 28; ×40 = 1120
        &["1120"],
        "partition_heap_string_no_leak",
    );
}

#[test]
fn asan_chained_scalar_to_string_no_leak_no_double_free() {
    // B-2026-08-11-22 — the fix reroutes which calls reach the String-copy
    // path, and that path both mallocs a fresh buffer AND frees the
    // intermediate receiver when it is a fresh owned String. Getting the
    // routing right but the ownership wrong would be a leak (per iteration,
    // so unbounded) or a double free, neither of which the value pins can
    // see.
    //
    // Loops so a per-iteration imbalance accumulates rather than hiding in
    // the noise, and covers both sides of the veto: a SCALAR receiver
    // (which must now take the scalar path and allocate its own String) and
    // a STRING receiver chained through a builtin (which must still take
    // the copy path and free the intermediate).
    assert_clean_asan_run(
        r#"
fn main() {
    let n: i64 = 12345;
    let mut i: i64 = 0;
    let mut total: i64 = 0;
    while i < 40 {
        total = total + n.to_string().len();
        total = total + n.to_string().to_string().len();
        i = i + 1;
    }
    let s: String = "  hello  ".to_string();
    let mut j: i64 = 0;
    while j < 40 {
        total = total + s.trim().to_string().len();
        total = total + s.clone().to_string().len();
        j = j + 1;
    }
    println(f"{total}");
}
"#,
        // 40*(5+5) + 40*(5+9) = 400 + 560 = 960
        &["960"],
        "chained_scalar_to_string_no_leak_no_double_free",
    );
}

#[test]
fn asan_chained_string_method_intermediate_temp_no_leak() {
    // B-2026-07-16-21: a heap-String-returning method used as the RECEIVER
    // of another method (`s.to_uppercase().to_lowercase()`,
    // `c.trim().to_string().to_uppercase()`, `e.to_uppercase().split(",")`)
    // produces a fresh owned intermediate String whose buffer nothing else
    // owned — the statement-level owned-temp machinery tracks only the
    // OUTERMOST temp, so the intermediate leaked once per call (unbounded in
    // a loop). Fixed by freeing the materialized receiver temp in the
    // expression-receiver string-method path (and the `to_string` copy
    // path), gated on `expr_yields_fresh_owned_temp` so a place-expr /
    // borrowed receiver (`p.name.to_uppercase()`, `v[0]...`) is never
    // freed, and on the method's result being receiver-INDEPENDENT (the
    // freshly-allocating xform family + the copy-into-owned split family) so
    // a borrowing result can't dangle. Loops 200× so any per-iteration
    // strand shows as a large LSan leak; the field-receiver leg also guards
    // against wrongly freeing a struct field's buffer (a double-free / UAF
    // that ASAN would trip even without LSan).
    assert_clean_asan_run(
        r#"
struct Rec { name: String }
fn main() {
    let mut total: i64 = 0;
    let mut i: i64 = 0;
    while i < 200 {
        let a = "Hello-World".to_string();
        let b = a.to_uppercase().to_lowercase();
        total = total + (b.len() as i64);
        let c = "  Trim Me  ".to_string();
        let d = c.trim().to_string().to_uppercase();
        total = total + (d.len() as i64);
        let e = "x,y,z".to_string();
        let parts = e.to_uppercase().split(",");
        for p in parts { total = total + (p.len() as i64); }
        let r = Rec { name: "field-string-value".to_string() };
        let u = r.name.to_uppercase();
        total = total + (u.len() as i64) + (r.name.len() as i64);
        i = i + 1;
    }
    println(total);
}
"#,
        &["11400"],
        "chained_string_method_intermediate_temp_no_leak",
    );
}

#[test]
fn asan_unwrap_or_fresh_string_default_no_leak() {
    // B-2026-07-16-22: `Option[String].unwrap_or(default)` /
    // `Result[String, E].unwrap_or(default)` evaluate the default EAGERLY
    // (before the tag branch); the present (Some/Ok) path discarded a fresh
    // heap-String default without freeing it — a leak once per call,
    // unbounded in a loop. Only surfaces on a data-dependent receiver (a
    // constant Some elides the default). Fixed by freeing the discarded
    // default in the present block, gated on a fresh-owned Call/MethodCall /
    // String-slice default so a borrowed / moved-binding default is never
    // touched. Loops 200× over both Option and Result receivers, each with a
    // fresh `"…".to_string()` default, mixing present/absent per iteration
    // so any per-call strand accumulates into a large LSan leak.
    assert_clean_asan_run(
        r#"
fn opt(i: i64) -> Option[String] {
    if i % 2 == 0 { Some("even-value".to_string()) } else { None }
}
fn res(i: i64) -> Result[String, String] {
    if i % 3 == 0 { Ok("ok-value".to_string()) } else { Err("e".to_string()) }
}
fn main() {
    let mut total: i64 = 0;
    let mut i: i64 = 0;
    while i < 200 {
        let a = opt(i).unwrap_or("odd-default".to_string());
        total = total + (a.len() as i64);
        let b = res(i).unwrap_or("res-default".to_string());
        total = total + (b.len() as i64);
        i = i + 1;
    }
    println(total);
}
"#,
        &["4099"],
        "unwrap_or_fresh_string_default_no_leak",
    );
}

#[test]
fn asan_string_replace_fresh_temp_args_no_leak() {
    // B-2026-07-16-24: `String.replace(from, to)` never freed its fresh-owned
    // String ARGUMENTS — the runtime helper copies the matched/replacement
    // bytes into its own result buffer, so a fresh-temp arg
    // (`s.replace("-".to_string(), "_".to_string())`) had no other owner and
    // leaked once per arg per call (unbounded in a loop). The sibling
    // arg-consuming methods (`contains`/`starts_with`/`split`/`find`) already
    // called `free_fresh_owned_str_arg`; `replace` was the sole omission.
    // Fixed by freeing both args after the runtime call. `free_fresh_owned_str_arg`
    // self-gates on a fresh-temp expr + the cap>0 marker, so a borrowed
    // identifier / literal arg is never freed (a moved owned-binding arg is
    // reclaimed by the move machinery instead). 200× loop over three arg
    // shapes — two fresh temps, a chained-temp arg, and a moved binding —
    // so any per-call strand accumulates into a large LSan leak.
    assert_clean_asan_run(
        r#"
fn main() {
    let s = "a-b-c-d".to_string();
    let mut total: i64 = 0;
    let mut i: i64 = 0;
    while i < 200 {
        let r1 = s.replace("-".to_string(), "_".to_string());
        total = total + (r1.len() as i64);
        let r2 = s.replace("-".to_string().to_uppercase().to_lowercase(), "SEP".to_string());
        total = total + (r2.len() as i64);
        let from = "-".to_string();
        let r3 = s.replace(from, "+".to_string());
        total = total + (r3.len() as i64);
        i = i + 1;
    }
    println(total);
}
"#,
        &["5400"],
        "string_replace_fresh_temp_args_no_leak",
    );
}

#[test]
fn asan_unwrap_or_literal_and_fstring_default_clean() {
    // B-2026-07-16-23 legs 2 + 3: `Option[T].unwrap_or(<default>)` where the
    // default is a DIRECT literal, not a binding.
    //   • leg 2 — a collection literal (`["a","b","c"]` / `Vec[9,9,9]`)
    //     LEAKED on the present (Some) path: the materialized Vec buffer was
    //     discarded without a free (24B × present-iters lost).
    //   • leg 3 — an f-string (`f"def-{i}"`) DOUBLE-FREED on the absent
    //     (None) path: the f-string's `acc` alloca was `track_vec_var`'d for
    //     scope-exit free AND the same buffer became the unwrap_or result
    //     (freed again by the result binding) — `free(): double free`.
    // Fix: free the literal default on the present path (it materializes to a
    // Vec struct and registers no scope cleanup, so a present-path free is
    // sufficient and never double-frees); for the f-string, additionally
    // suppress the acc's scope cleanup before the branch so the buffer is
    // freed exactly once per path. Loops 200× mixing present/absent over a
    // heap-element array literal, a `Vec[..]` prefix literal, and an f-string
    // default, so a double-free aborts immediately and a per-iteration leak
    // accumulates for LSan.
    assert_clean_asan_run(
        r#"
fn opt_v(i: i64) -> Option[Vec[String]] {
    if i % 2 == 0 { Some(["x", "y"]) } else { None }
}
fn opt_i(i: i64) -> Option[Vec[i64]] {
    if i % 2 == 0 { Some([i, i]) } else { None }
}
fn opt_s(i: i64) -> Option[String] {
    if i % 2 == 0 { Some("hi") } else { None }
}
fn main() {
    let mut total: i64 = 0;
    let mut i: i64 = 0;
    while i < 200 {
        let v: Vec[String] = opt_v(i).unwrap_or(["a", "b", "c"]);
        total = total + (v.len() as i64);
        let w: Vec[i64] = opt_i(i).unwrap_or(Vec[9, 9, 9]);
        total = total + (w.len() as i64);
        let s: String = opt_s(i).unwrap_or(f"def-{i}");
        total = total + (s.len() as i64);
        i = i + 1;
    }
    println(total);
}
"#,
        &["1845"],
        "unwrap_or_literal_and_fstring_default_clean",
    );
}

#[test]
fn asan_mut_ref_struct_string_field_reassign_across_calls_no_leak() {
    // B-2026-07-22-8: reassigning a heap-owning `String` STRUCT FIELD through a
    // `mut ref S` param leaked the OLD buffer when the field's current value was
    // set by a PRIOR function call. `compile_field_store`'s mut-ref-param path
    // stored over the field WITHOUT dropping the displaced value; the old-value
    // free was gated on an intra-function "field is currently heap-owned" fact
    // that resets at function entry, so a reassignment whose first touch of the
    // field is the store (the value having been set in an earlier call) leaked.
    // Fixed by always loading + cap-guard-freeing the displaced field value before
    // the store in the mut-ref-param path, mirroring the owned-struct path.
    // Minimal shape is the Read4-II state machine (leetcode #158): a Reader struct
    // with a `buf: String` chunk refilled via `s.buf = read4(s)` when it drains,
    // where across calls the field already holds the previous chunk. Looped so a
    // per-call strand shows as a large LSan leak.
    assert_clean_asan_run(
        r#"
struct S { src: String, mut pos: i64, mut buf: String, mut bp: i64 }
fn read4(s: mut ref S) -> String {
    let n = s.src.len();
    let mut chunk: String = "";
    let mut c = 0i64;
    while c < 4i64 and s.pos < n {
        chunk.push_str(s.src[s.pos..s.pos + 1i64]);
        s.pos = s.pos + 1i64;
        c = c + 1i64;
    }
    return chunk;
}
fn consume(s: mut ref S, n: i64) -> String {
    let mut r: String = "";
    let mut t = 0i64;
    while t < n {
        if s.bp >= s.buf.len() {
            s.buf = read4(s);
            s.bp = 0i64;
            if s.buf.len() == 0i64 { return r; }
        }
        r.push_str(s.buf[s.bp..s.bp + 1i64]);
        s.bp = s.bp + 1i64;
        t = t + 1i64;
    }
    return r;
}
fn main() {
    let mut total: i64 = 0;
    let mut iter = 0i64;
    while iter < 100 {
        let mut s = S { src: "HelloWorldFromKara", pos: 0i64, buf: "", bp: 0i64 };
        let a = consume(mut s, 4i64);
        let b = consume(mut s, 4i64);
        let c = consume(mut s, 4i64);
        total = total + a.len() + b.len() + c.len();
        iter = iter + 1i64;
    }
    println(total);
}
"#,
        // 100 iterations x (4 + 4 + 4) consumed chars = 1200.
        &["1200"],
        "mut_ref_struct_string_field_reassign_across_calls_no_leak",
    );
}

#[test]
fn asan_map_string_value_overwrite_discard_no_leak() {
    // B-2026-07-22-12: `m.insert(k, v)` over an EXISTING key on a
    // `Map[K, String]` / `Map[K, Vec[…]]` leaked the displaced old value's
    // heap buffer when the `Option[V]` result was discarded with
    // `let _ = …`. The insert arm loaded the old value into a `Some(old)`
    // payload nobody holds, and only the shared/RC-value path dec'd it —
    // owned `String`/`Vec` values (which need a buffer FREE, not a dec) fell
    // through, so each overwrite leaked one buffer. Fixed by freeing the
    // displaced buffer in the discard path for owned-heap V (wildcard-let
    // form only; a bare `m.insert(…);` already drops its Option temp, so
    // freeing there too would double-free). Exercises: a looped
    // `Map[i64,String]` overwrite (heap `push_str` values so the buffer is
    // real, not rodata), a `Map[i64,Vec[i64]]` overwrite, and — as a
    // no-double-free guard — a BOUND result (`match m.insert(...)`) whose
    // old value the caller consumes. Looped so a per-overwrite strand shows
    // as a large LSan leak.
    assert_clean_asan_run(
        r#"
fn heapstr(seed: String) -> String {
    let mut s: String = "";
    s.push_str(seed);
    s.push_str("-heaptail");
    return s;
}
fn mk(a: i64) -> Vec[i64] {
    let mut v: Vec[i64] = Vec.new();
    v.push(a);
    v.push(a + 1);
    return v;
}
fn mks(a: String) -> Vec[String] {
    let mut v: Vec[String] = Vec.new();
    v.push(heapstr(a));
    v.push(heapstr("second-inner"));
    return v;
}
fn main() {
    let mut total: i64 = 0;
    let mut i: i64 = 0;
    while i < 200 {
        // Map[i64, String] overwrite (discarded result) — old buffer must free.
        let mut m: Map[i64, String] = Map.new();
        let _ = m.insert(1, heapstr("first"));
        let _ = m.insert(1, heapstr("second"));
        let _ = m.insert(1, heapstr("third"));
        total = total + m.len();

        // Map[i64, Vec[i64]] overwrite (discarded result).
        let mut g: Map[i64, Vec[i64]] = Map.new();
        let _ = g.insert(7, mk(10));
        let _ = g.insert(7, mk(20));
        total = total + g.len();

        // Map[i64, Vec[String]] overwrite — deep drop: the displaced Vec's
        // INNER Strings must free too, not just the outer buffer.
        let mut gs: Map[i64, Vec[String]] = Map.new();
        let _ = gs.insert(5, mks("aa"));
        let _ = gs.insert(5, mks("bb"));
        total = total + gs.len();

        // Bound result: the caller consumes the displaced old value — must NOT
        // also be freed by the fix (no double-free).
        let mut b: Map[i64, String] = Map.new();
        let _ = b.insert(3, heapstr("old"));
        match b.insert(3, heapstr("new")) {
            Some(prev) => { total = total + prev.len(); }
            None => {}
        }

        // remove sibling: `let _ = m.remove(k)` moves the heap value out into a
        // discarded Some(old) — its buffer must free too (same B-2026-07-22-12).
        let mut r: Map[i64, String] = Map.new();
        let _ = r.insert(9, heapstr("removeme"));
        let _ = r.remove(9);
        total = total + r.len();
        i = i + 1;
    }
    println(total);
}
"#,
        // Per iter: m.len()=1, g.len()=1, gs.len()=1,
        // prev.len()=len("old-heaptail")=12, r.len()=0 → 15; x200 = 3000.
        &["3000"],
        "map_string_value_overwrite_discard_no_leak",
    );
}

/// B-2026-07-25-1 — an owned `String` PARAM consumed TWICE in one function
/// (`cursor.insert(airport, …)` then, after a recursive descent,
/// `route.push(airport)`) must not have its `cap` zeroed by the first
/// consume. Owned Vec/String params are caller-retains: the callee never
/// registers a `FreeVecBuffer` for them and every retaining consume
/// deep-copies, so there is no cleanup to suppress — but the move-out
/// suppression zeroed the header's `cap` anyway, and the SECOND consume then
/// read `cap == 0`, took itself for a borrowed view, skipped its defensive
/// copy and stored a raw alias into the caller's map-derived `Vec[String]`
/// element. That element is freed the moment the recursive call returns, so
/// `route` finished holding dangling pointers (ASan: `main` reads a block
/// allocated by `karac_string_clone` and freed in `visit`). The tree-walk
/// interpreter was correct throughout, making it a run-vs-build divergence
/// too. Kata source: LeetCode #332 Reconstruct Itinerary; standalone repro at
/// kara-katas/oracle/recursive-owned-string-param-uaf/repro.kara.
#[test]
fn asan_owned_string_param_consumed_twice_no_uaf() {
    let label = "owned_string_param_consumed_twice";
    if !asan_available() {
        eprintln!("[{label}] ASAN unavailable on this host — skipping");
        return;
    }
    let Some((stdout, status)) = run_under_asan_with_full_pipeline(
        r#"
fn visit(
    adj: mut ref Map[String, Vec[String]],
    cursor: mut ref Map[String, i64],
    airport: String,
    route: mut ref Vec[String],
) {
    while true {
        let dests: Vec[String] = match adj.get(airport) { Some(d) => d, None => Vec.new() };
        let used = match cursor.get(airport) { Some(u) => u, None => 0i64 };
        if used >= dests.len() { break; }
        let _ = cursor.insert(airport, used + 1i64);
        visit(adj, cursor, dests[used].clone(), route);
    }
    route.push(airport);
}

fn main() {
    let mut adj: Map[String, Vec[String]] = Map.new();
    let mut a: Vec[String] = Vec.new();
    a.push("ATL"); a.push("SFO");
    let _ = adj.insert("JFK", a);
    let mut b: Vec[String] = Vec.new();
    b.push("JFK"); b.push("SFO");
    let _ = adj.insert("ATL", b);
    let mut c: Vec[String] = Vec.new();
    c.push("ATL");
    let _ = adj.insert("SFO", c);

    let mut cursor: Map[String, i64] = Map.new();
    let mut route: Vec[String] = Vec.new();
    visit(mut adj, mut cursor, "JFK", mut route);
    let mut i = 0i64;
    while i < route.len() { println(route[i]); i = i + 1i64; }
}
"#,
        label,
    ) else {
        eprintln!("[{label}] setup failed — skipping");
        return;
    };
    assert!(
        status.success(),
        "[{label}] ASAN reported a memory error (exit code {:?}) — look for \
             heap-use-after-free on a String buffer freed while an owned param \
             still aliases it",
        status.code()
    );
    assert_eq!(
        stdout.trim().lines().collect::<Vec<_>>(),
        vec!["SFO", "ATL", "SFO", "JFK", "ATL", "JFK"],
        "[{label}] unexpected stdout (ASAN passed, output mismatched)"
    );
}

// B-2026-07-31-12 — `Json.parse` of a string/array/object leaked the
// lifted Kāra-side tree: the match-bound `Ok(j)` payload had no drop
// registration (the consuming arm zeroes the SOURCE, so `j` was sole
// owner and nobody freed it), and `__karac_drop_Json` freed only the
// OUTER buffer of an Array/Object anyway. Fixed by (a) the inline-optres
// ENUM payload registration in `bind_pattern_values` (the enum sibling of
// the B-2026-07-10-3 struct arm) and (b) a recursive `__karac_drop_Json`
// walker (json.rs). Unlike the bodies-only Drop tests above, LSan
// witnesses this leak DIRECTLY — real malloc'd buffers — so this test is
// the permanent gate on both halves, and it equally guards the unsafe
// direction: an over-eager registration (double-free of a moved-out
// payload) or a walker recursing into a cap-zeroed node trips ASAN, not
// just LSan.
#[test]
fn asan_json_parse_tree_freed_once() {
    assert_clean_asan_run(
        r#"
fn main() {
    let mut n = 0i64;
    let mut i = 0i64;
    while i < 200i64 {
        // Deep-nested object: pair keys, element strings, child array,
        // child object — everything below the first level.
        match Json.parse("{\"list\":[\"alpha-string-long-enough\",{\"k\":\"nested-value-string\"},[1,2,3]],\"name\":\"outer-name-string-payload\"}") {
            Result.Ok(j) => {
                let s = j.stringify();
                n = n + s.len();
            }
            Result.Err(e) => { n = n + 100000; }
        }
        // Destructure MOVE-OUT: the arm takes the Vec; the source's walker
        // must skip the wiped node (no double-free), and the bound Vec's own
        // cleanup frees the buffer once.
        match Json.parse("[10, 20]") {
            Result.Ok(j) => {
                match j {
                    Json.Array(xs) => { n = n + xs.len(); }
                    Json.Null => { n = n + 7; }
                    Json.Bool(b) => { n = n + 7; }
                    Json.Number(f) => { n = n + 7; }
                    Json.Int(v) => { n = n + 7; }
                    Json.String(s2) => { n = n + s2.len(); }
                    Json.Object(kv) => { n = n + kv.len(); }
                }
            }
            Result.Err(e) => { n = n + 100000; }
        }
        // Unused payload — the registration alone must free it.
        match Json.parse("\"disposable-string-payload\"") {
            Result.Ok(j) => { n = n + 1; }
            Result.Err(e) => { n = n + 100000; }
        }
        i = i + 1;
    }
    println(n);
}
"#,
        // 108 stringify bytes + 2 + 1 per iteration x 200 = 22200.
        &["22200"],
        "json_parse_tree_freed_once",
    );
}

// B-2026-08-12-16 — the ERROR half of the same API. `json.rs` copies the
// runtime's diagnostic into a Kāra String but pinned that String's `cap`
// to 0, so the scope-exit free was a permanent no-op: 39 bytes leaked per
// failed parse, unbounded in a loop over attacker-supplied JSON. Fixed by
// packing the REAL cap (the malloc'd size, which is exactly `len`) into
// w4 of the widened `Result[Json, JsonError]`.
//
// The pin was safe-by-construction before B-2026-08-12-14: with no
// `struct_types` registration for `JsonError`, no drop was synthesized,
// and a nonzero cap would have been read out of an UNDEF stack word.
// That row seeded the layout, so a real `cap > 0`-guarded drop now
// exists to fire — which is what makes this fixable and, equally, what
// makes a real cap DANGEROUS if any position produces a second owner.
//
// So this is a matrix over the four positions that could, not a
// single-shape smoke test. Each is a place a real cap turns one owner
// into two if the move is not tracked, and ASAN catches that direction
// (double-free / use-after-free) as loudly as LSan catches the leak:
//
//   1. bare scope exit          — the leak itself; nothing else touches it
//   2. pass BY VALUE to a fn    — callee frame and caller frame both see it
//   3. field-read then by-value — the read must not be mistaken for a move
//   4. `?`-propagation          — the payload crosses a frame boundary
//
// Looped so a per-iteration leak accumulates well past LSan's noise
// floor rather than sitting at one 39-byte allocation.
#[test]
fn asan_json_parse_error_message_freed_once() {
    assert_clean_asan_run(
        r#"
fn describe(e: JsonError) -> i64 { return e.message.len() + (e.line as i64); }

fn propagate(text: String) -> Result[i64, JsonError] {
    let j = Json.parse(text)?;
    return Result.Ok(1i64);
}

fn main() {
    let mut n = 0i64;
    let mut i = 0i64;
    while i < 100i64 {
        // 1. Bare scope exit — the message is read and then dropped.
        match Json.parse("{bad") {
            Result.Ok(j) => { n = n + 100000; }
            Result.Err(e) => { n = n + (e.line as i64); }
        }
        // 2. Passed BY VALUE into a callee that reads the String field.
        match Json.parse("[1,") {
            Result.Ok(j) => { n = n + 100000; }
            Result.Err(e) => { n = n + describe(e); }
        }
        // 3. Field read in the caller AND then handed over by value.
        match Json.parse("nope") {
            Result.Ok(j) => { n = n + 100000; }
            Result.Err(e) => {
                let pre = e.message.len();
                n = n + pre + describe(e);
            }
        }
        // 4. `?`-propagated across a frame boundary, then destructured.
        match propagate("{\"a\":") {
            Result.Ok(v) => { n = n + 100000; }
            Result.Err(e) => { n = n + (e.column as i64); }
        }
        i = i + 1;
    }
    println(n > 0i64);
}
"#,
        &["true"],
        "json_parse_error_message_freed_once",
    );
}

/// B-2026-08-08-25 — matching a payload out of a LIVE `Option[String]`
/// binding, where the arms only READ it, must not free the buffer at arm
/// exit and leave the still-in-scope source pointing at it.
///
/// Pre-fix this was a genuine use-after-free, not just wrong output —
/// valgrind: `Invalid read of size 2 … 0 bytes inside a block of size 2
/// free'd`. The source's `cap` word was zeroed so its own scope-exit free
/// skipped, and the arm binding's `track_vec_var` freed the buffer while the
/// source stayed readable.
///
/// Loop-stressed so a mis-fire in the other direction shows up too: if the
/// source were disarmed AND nothing freed, LSan would report the per-
/// iteration leak. f-string payloads keep the heap non-foldable.
#[test]
fn asan_match_out_of_live_option_string_no_use_after_free() {
    assert_clean_asan_run(
        r#"
fn main() {
    let mut i: i64 = 0;
    while i < 8 {
        let o: Option[String] = Some(f"hi{i}");
        match o { Some(v) => { println(v); } None => { println("-"); } }
        // The read that was a use-after-free: `o` is still in scope and owns
        // the buffer the arm above only borrowed.
        match o { Some(v) => { println(v); } None => { println("-"); } }
        i = i + 1;
    }
    println("end");
}
"#,
        &[
            "hi0", "hi0", "hi1", "hi1", "hi2", "hi2", "hi3", "hi3", "hi4", "hi4", "hi5", "hi5",
            "hi6", "hi6", "hi7", "hi7", "end",
        ],
        "match_out_of_live_option_string",
    );
}

/// B-2026-08-09-5 — mapping a borrowed String payload with `|s| s` now
/// returns the whole `{ptr,len,cap}` aggregate instead of one word, which
/// means the mapper hands back an OWNED deep copy while the surface type of
/// the result is still `Option[ref String]`. That is exactly the shape where
/// an ownership mistake hides: nothing frees the copy (a leak), or both the
/// copy and the source Vec element free the same buffer (a double free).
///
/// Loop-stressed with f-string payloads so LSan sees a per-iteration leak if
/// the copy goes unowned, and the source Vec is read again after the match
/// so a double free or a freed-buffer read surfaces as well.
///
/// Also clean at `KARAC_OPT_LEVEL=0`, which is worth stating because the
/// neighbouring `asan_option_map_heap_payload_no_leak` is NOT (B-2026-08-09-4,
/// 200 bytes in 40 allocations, open). That leak sits on the same map-via-
/// match synthesis and is unchanged by this fix — measured identical before
/// and after — so the two are independent despite the shared lowering.
#[test]
fn asan_map_over_borrowed_string_payload_owns_its_copy_exactly_once() {
    assert_clean_asan_run(
        r#"
fn main() {
    let mut i: i64 = 0;
    while i < 8 {
        let v: Vec[String] = vec![f"hi{i}", f"yo{i}"];
        match v.first().map(|s| s) { Some(w) => { println(w); } None => { println("-"); } }
        // The source element must still own its own buffer afterwards.
        println(v[0]);
        i = i + 1;
    }
    println("end");
}
"#,
        &[
            "hi0", "hi0", "hi1", "hi1", "hi2", "hi2", "hi3", "hi3", "hi4", "hi4", "hi5", "hi5",
            "hi6", "hi6", "hi7", "hi7", "end",
        ],
        "map_over_borrowed_string_payload",
    );
}

/// B-2026-08-08-25 leg 1 — `.map` over a live `Option[String]` now leaves
/// the source owning its buffer, so the arm must NOT free it. This is the
/// leg that turns the borrow classification on for a whole family of
/// matches, and both failure directions are memory bugs rather than wrong
/// output: if the arm still frees, the source's own scope-exit free is a
/// double free and its later read is a use-after-free (valgrind called the
/// pre-fix program `Invalid read of size 2`); if the classification instead
/// disarms BOTH, nothing frees and LSan sees a per-iteration leak.
///
/// Loop-stressed with f-string payloads so a per-iteration leak is
/// unmistakable, and the source is re-read after every `.map` so a stale
/// pointer surfaces rather than passing silently. The hand-written `None =>
/// None` arm is in the loop too — it reaches the same classifier through the
/// unit-variant path, with no combinator involved.
#[test]
fn asan_map_over_live_option_string_leaves_source_owning_its_buffer() {
    assert_clean_asan_run(
        r#"
fn main() {
    let mut i: i64 = 0;
    while i < 8 {
        let o: Option[String] = Some(f"hi{i}");
        match o.map(|x| x.to_uppercase()) { Some(v) => { println(v); } None => { println("-"); } }
        // The source must still own its buffer after the mapper ran.
        match o { Some(v) => { println(v); } None => { println("-"); } }
        // Same classifier, reached without a combinator: the `None` arm body
        // mentions `None`, which used to read as an escaping binding.
        let n: Option[i64] = match o { Some(v) => { Some(v.len()) } None => { None } };
        println(n.unwrap_or(0i64));
        i = i + 1;
    }
    println("end");
}
"#,
        &[
            "HI0", "hi0", "3", "HI1", "hi1", "3", "HI2", "hi2", "3", "HI3", "hi3", "3", "HI4",
            "hi4", "3", "HI5", "hi5", "3", "HI6", "hi6", "3", "HI7", "hi7", "3", "end",
        ],
        "map_over_live_option_string",
    );
}

/// B-2026-08-14-20 — `Slice[T].to_vec()` over HEAP elements, and the
/// fresh-owned-temporary argument of `String.from_utf8`.
///
/// This is the gate the output comparison cannot be: every line prints the
/// same numbers whether or not the memory is right. Three distinct
/// failures live here. A `to_vec` that memcpy'd a `String` or `Vec[i64]`
/// element instead of deep-cloning it would give the copy and the source
/// the SAME inner buffers, and both containers free them at frame exit —
/// a double free. A `to_vec` that deep-cloned but whose result was not
/// drop-tracked leaks the whole copy. And `String.from_utf8(<fresh owned
/// Vec temp>)` — which this row's widening makes reachable as
/// `s.bytes().to_vec()` — has no binding to carry a scope-exit drop, so
/// nothing freed the argument's buffer (a pre-existing leak on the
/// `mk_bytes()` spelling, measured identically on the parent commit).
///
/// The `s.bytes()` and `.as_slice()` arguments are the other side of that
/// fix: they are BORROWED views, so freeing them would take the source
/// String's / Vec's storage with it.
#[test]
fn asan_slice_to_vec_and_from_utf8_temporaries() {
    assert_clean_asan_run(
        r#"
fn mk_bytes() -> Vec[u8] {
    let mut v: Vec[u8] = Vec.new();
    v.push(104u8); v.push(105u8); v.push(106u8);
    v
}

fn main() {
    let s = "a-fairly-long-subject-string";
    match String.from_utf8(s.bytes()) { Ok(t) => println(f"{t.len()}"), Err(_) => println("bad") }
    match String.from_utf8(s.bytes().to_vec()) { Ok(t) => println(f"{t.len()}"), Err(_) => println("bad") }
    match String.from_utf8(mk_bytes()) { Ok(t) => println(t), Err(_) => println("bad") }
    let mut named: Vec[u8] = Vec.new();
    named.push(120u8); named.push(121u8);
    match String.from_utf8(named.as_slice()) { Ok(t) => println(t), Err(_) => println("bad") }
    println(f"{named.len()}");

    let words: Vec[String] = ["alphabetical", "betamax", "gamma-ray-burst"];
    let mut wc = words.as_slice().to_vec();
    wc[0i64] = "zulu-time-signal";
    println(f"{wc[0i64]} {words[0i64]}");
    let again = wc.as_slice().to_vec();
    println(f"{again[2i64]}");
    let win = words[1..3].to_vec();
    println(f"{win.len()} {win[0i64]}");

    let rows: Vec[Vec[i64]] = [[1i64, 2i64, 3i64], [4i64, 5i64, 6i64]];
    let mut rc = rows.as_slice().to_vec();
    rc[0i64][0i64] = 77i64;
    println(f"{rc[0i64][0i64]} {rows[0i64][0i64]}");

    let empty: Vec[String] = Vec.new();
    println(f"{empty.as_slice().to_vec().len()}");
    let es = empty.as_slice();
    println(f"{es.chunks(2i64).len()} {es.windows(2i64).len()}");
    println("end");
}
"#,
        &[
            "28",
            "28",
            "hij",
            "xy",
            "2",
            "zulu-time-signal alphabetical",
            "gamma-ray-burst",
            "2 betamax",
            "77 1",
            "0",
            "0 0",
            "end",
        ],
        "slice_to_vec_and_from_utf8_temporaries",
    );
}

/// B-2026-08-15-2 — `s.push_str(s)` on a heap string that must grow.
///
/// This is the only test that can see the bug. The E2E twin
/// (`test_e2e_string_push_str_self_append`) printed every value correctly
/// both before and after the fix: `emit_string_buffer_grow` reallocs the
/// destination, and the copy then read through the source pointer captured
/// BEFORE that realloc — a heap-use-after-free of the whole string, whose
/// freed bytes are usually still mapped, so the answer came out right and
/// nothing surfaced. Under a different allocator or with concurrent
/// allocation it is silent corruption instead of silent success.
///
/// THE SIZE IS LOAD-BEARING, measured rather than assumed: a 4-byte or
/// 16-byte self-append does NOT trip the sanitizer on the parent commit,
/// because realloc extends those in place and frees nothing. Only a buffer
/// large enough to force a real MOVE — 40 KB here — makes the dangling
/// read observable, which is why the row's repro needed its 5,000-iteration
/// preamble and why shrinking this fixture would quietly make it vacuous.
///
/// The second half pins the deleted guard. `push_str`'s grow path used to
/// PANIC on a borrowed source that overlapped the destination
/// ("source slice aliases destination buffer"), emitted only under
/// `src_borrowed`; the rebase makes that shape correct rather than
/// detected, so the panic is gone and a borrowed slice of a DIFFERENT
/// string — the hot path the borrowed-source optimisation exists for —
/// must still run clean across a grow.
#[test]
fn asan_string_push_str_self_append_no_use_after_free() {
    assert_clean_asan_run(
        r#"
fn main() {
    let mut d = String.new();
    let mut i = 0i64;
    while i < 5000i64 { d.push_str("abcdefgh"); i = i + 1i64; }
    d.push_str(d);
    println(f"{d.len()}");
    println(d[39992..40000]);
    println(d[40000..40008]);

    let mut c = String.new();
    c.push_str("pq");
    c.push_str(c);
    c.push_str(c);
    println(f"{c} {c.len()}");

    let src = "abcdefghijklmnopqrstuvwxyz";
    let mut out = String.new();
    let mut j = 0i64;
    while j < 2000i64 { out.push_str(src[3..9]); j = j + 1i64; }
    println(f"{out.len()}");
    println("end");
}
"#,
        &[
            "80000",
            "abcdefgh",
            "abcdefgh",
            "pqpqpqpq 8",
            "12000",
            "end",
        ],
        "string_push_str_self_append",
    );
}

#[test]
fn asan_shared_struct_string_field_reassign_no_leak() {
    // Originally B-2026-08-14-18's over-reach guard, resting on "a String
    // field of a shared struct was ALREADY released on reassignment —
    // measured". That measurement was an -O2 artifact: this fixture's
    // concat is loop-invariant, LICM hoists it to a single allocation that
    // stays reachable through the field at exit, and LSan sees nothing. At
    // -O0 the loop allocates per iteration and the truth surfaced — no
    // release existed anywhere on the path, 627 B leaked over 19
    // allocations (the same signature earlier misread as a "separate,
    // layout-dependent leak" of the four-field fixture; it was this bug,
    // visible whenever the allocations survive to run time). Since
    // B-2026-08-15-5's fix the displaced String rides the same
    // release_old_shared_container_field arm as the container family.
    //
    // B-2026-08-14-25 — the same defect from the other end, and the reason
    // this program is no longer the one described above. As written it was
    // vacuous at the DEFAULT opt level even after the fix landed: reverting
    // the release arm and re-running left it GREEN, so only the -O0 leg
    // gated the bug it was written for. Two independent optimizer effects
    // were hiding it, and defeating LICM alone is not enough — the concat is
    // now interpolated (per-iteration, so it cannot be hoisted) AND the
    // final read is the field's CONTENT rather than `.len()`, which reads
    // the `{ptr, len, cap}` header and never touches the buffer, letting the
    // allocation be elided as dead. With both, the pre-fix compiler strands
    // 693 bytes over 19 allocations at the default opt level.
    //
    // The general lesson is the one B-2026-08-14-25's row learned the hard
    // way: it read this same vacuity as a LAYOUT boundary and filed twelve
    // probes tabulating which field mixes leaked. Layout was never the axis.
    // A leak fixture must OBSERVE the value under test, or it measures the
    // optimizer.
    assert_clean_asan_run(
        r#"
shared struct P { mut label: String }
fn main() {
    let p = P { label: "seedseedseedseedseed" };
    let mut k = 0;
    while k < 20 {
        let fresh: String = f"epsilonepsilonepsilon-{k}-zetazetazeta";
        p.label = fresh;
        k = k + 1;
    }
    println(p.label);
}
"#,
        &["epsilonepsilonepsilon-19-zetazetazeta"],
        "asan_shared_struct_string_field_reassign_no_leak",
    );
}

/// Aliasing companion to the fixture above, mirroring the container
/// family's aliasing fixture: the displaced-String release is
/// UNCONDITIONAL, so every shape where the RHS reads the node's own buffer
/// must stay both correct and ASAN-clean. Safe for the same reason the
/// container release is — a String field read off a shared node yields an
/// independent copy, so by the time the release fires the RHS holds its
/// own buffer, never the one being freed. Covers self-assign
/// (`p.label = p.label`), cross-alias through a second handle to the same
/// node (`p.label = q.label`), and a compound append through the field
/// (`p.label = p.label + "x"`), which reads the old buffer during RHS
/// evaluation — before the release — and displaces it after.
///
/// B-2026-08-14-25 adds the CHAINED store (`o.inner.label = …`, a shared
/// field of a shared parent). It reaches the same release through a
/// different call site in `compile_field_store` — the one B-2026-08-14-26
/// taught to land at all — and was covered by nothing.
#[test]
fn asan_shared_struct_string_field_reassign_aliasing() {
    assert_clean_asan_run(
        r#"
shared struct Inner { mut label: String }
shared struct Outer { mut inner: Inner }
shared struct P { mut label: String }
fn main() {
    let o = Outer { inner: Inner { label: "start" } };
    let mut n = 0i64;
    while n < 20i64 {
        let deep: String = f"kappakappakappakappa-{n}-lambdalambda";
        o.inner.label = deep;
        println(o.inner.label);
        n = n + 1i64;
    }
    let p = P { label: "seedseedseedseedseed" + "tail" };
    p.label = p.label;
    println(f"{p.label.len()}");
    let q = p;
    p.label = q.label;
    println(f"{p.label.len()}");
    let mut k = 0;
    while k < 5 {
        p.label = p.label + "x";
        k = k + 1;
    }
    println(p.label);
}
"#,
        (0..20)
            .map(|n| format!("kappakappakappakappa-{n}-lambdalambda"))
            .collect::<Vec<_>>()
            .iter()
            .map(String::as_str)
            .chain(["24", "24", "seedseedseedseedseedtailxxxxx"])
            .collect::<Vec<_>>()
            .as_slice(),
        "asan_shared_struct_string_field_reassign_aliasing",
    );
}

#[test]
fn asan_sorted_map_and_set_display_frees_its_key_buffer() {
    // B-2026-08-14-35 — the sorted Display fns do not walk the runtime
    // iterator the unsorted ones use. They materialize an ASCENDING KEY
    // BUFFER with `karac_map_sorted_keys` (the same call `keys()` and
    // `for (k, v) in m` make), walk it by index, and free it. That is a
    // fresh malloc per render, on a path that runs once per `println`, so
    // it has both failure modes the unsorted path does not:
    //
    //   - forget the trailing `free` and every print leaks a buffer;
    //   - free the ELEMENTS as well as the buffer and a `String` key is
    //     released while the map still owns it — the buffer holds aliasing
    //     `{ptr, len, cap}` headers, not copies.
    //
    // String keys (heap halves, so a wrong drop is visible) rendered 20
    // times from both the bound spelling and a struct field, plus the
    // integer-key set for the scalar shape. LSan on the Linux CI leg is
    // what makes the leak half of this assert; the ASAN redzones catch the
    // double-free half on every host.
    assert_clean_asan_run(
        r#"
struct H { m: SortedMap[String, i64], s: SortedSet[String] }
fn main() {
    let mut bm: SortedMap[String, i64] = SortedMap.new();
    let _ = bm.insert("zebrazebrazebrazebra", 1);
    let _ = bm.insert("applesapplesapples", 2);
    let mut hm: SortedMap[String, i64] = SortedMap.new();
    let _ = hm.insert("mangomangomangomango", 3);
    let mut hs: SortedSet[String] = SortedSet.new();
    hs.insert("elemelemelemelemelem");
    let h = H { m: hm, s: hs };
    let mut k = 0;
    while k < 20 {
        println(f"{bm}");
        println(f"{h.m}");
        println(h.s);
        k = k + 1;
    }
    println("done");
}
"#,
        &[
            "SortedMap{applesapplesapples: 2, zebrazebrazebrazebra: 1}",
            "SortedMap{mangomangomangomango: 3}",
            "SortedSet{elemelemelemelemelem}",
        ]
        .iter()
        .cycle()
        .take(60)
        .copied()
        .chain(std::iter::once("done"))
        .collect::<Vec<_>>(),
        "asan_sorted_map_and_set_display_frees_its_key_buffer",
    );
}

/// B-2026-08-14-22 — `s += x` on a LOCAL `String` must release the buffer
/// its store displaces.
///
/// The operator built a fresh concatenation and stored it over the binding
/// without freeing what the binding held, so every intermediate was
/// stranded: a 20,000-append loop peaked at 1.565 GB against a live string
/// of 160 KB, the exact sum of the series. The bare local was the only
/// target that leaked — a field, an index and a `mut ref` parameter each
/// reach a store path that already reclaims — so this fixture appends
/// through a plain local on purpose.
///
/// The three spellings are pinned together: `push_str` and `s = s + x`
/// produce the same string and were already balanced, so a fix that traded
/// this leak for a double free in either of them fails here rather than
/// somewhere else.
#[test]
fn asan_compound_append_on_a_local_string_is_balanced() {
    assert_clean_asan_run(
        r#"
fn main() {
    let k = env.args().len() as i64;
    let mut piece = String.new(); piece.push_str("abcdefg"); piece.push_str(k.to_string());
    let mut a = String.new(); a.push_str("seed");
    let mut b = String.new(); b.push_str("seed");
    let mut c = String.new(); c.push_str("seed");
    let mut i = 0i64;
    while i < 200i64 {
        a += piece;
        b.push_str(piece);
        c = c + piece;
        i = i + 1;
    }
    println(a.len());
    println(b.len());
    println(c.len());
}
"#,
        // 4 + 200 * 8 for each. `k` is a stable 1 (the binary runs with no
        // args), appended so `piece` is a heap buffer rather than a static
        // literal — a `cap == 0` source would make the reclaim a no-op and
        // hide the thing being asserted.
        &["1604", "1604", "1604"],
        "compound_append_on_a_local_string_is_balanced",
    );
}

/// B-2026-08-14-36 — a `Map` / `Set` TEMPORARY that is printed and never
/// bound leaks its WHOLE handle: control block, bucket storage, and every
/// stored key. Pre-fix this fixture stranded 94400 bytes over 500
/// allocations; the loss scales with the collection's CONTENTS, not a fixed
/// header, so a program that prints a freshly-built map in a loop bleeds
/// proportionally to its data.
///
/// B-2026-08-14-31 gave this arm its render and deliberately left the
/// ownership alone — the value was equally unfreed when it printed as an
/// address, so that row neither caused nor cured this. What it needed first
/// was the per-half drop classification: `FreeMapHandle` decides between
/// three different runtime frees plus two shared-half rc_dec walks, and
/// getting that wrong in the permissive direction is B-2026-08-14-30, a hard
/// double free. The fix reaches the binding path's own derivation through
/// `map_cleanup_parts_from_halves` rather than restating it.
///
/// Every producer shape the arm serves is here, chosen to exercise a
/// different arm of that classification: `String` keys (the key-side
/// `{ptr,len,cap}` walk), a `#[derive(Display)]` STRUCT key with a heap
/// field (`key_drop_fn` — the per-key release, which the flag walk cannot
/// express), `i64` / `Vec[String]` / `Set[String]` values (no value walk,
/// the value-side walk, and a nested handle), and both sorted siblings,
/// which share the storage and so the same free.
///
/// The two arms NOT reachable here are `val_drop_fn` and
/// `val_shared_heap_type`: the typechecker refuses `Display` for a
/// `Map[K, StructV]` or `Map[K, sharedV]`, so a value owning heap beyond
/// the one-level overlay can never reach a display site. They stay covered
/// by the binding path's own fixtures, which is the point of sharing one
/// derivation rather than writing a second.
#[test]
fn asan_printed_map_or_set_temporary_frees_its_handle() {
    assert_clean_asan_run(
        r#"
#[derive(Hash, Eq, Display)]
struct P { name: String }
fn mk() -> Map[String, i64] {
    let mut m: Map[String, i64] = Map.new();
    let _ = m.insert("kkkkkkkkkkkkkkkk", 1);
    return m;
}
fn mkset() -> Set[String] {
    let mut s: Set[String] = Set.new();
    s.insert("eeeeeeeeeeeeeeeeee");
    return s;
}
fn mksorted() -> SortedMap[String, i64] {
    let mut m: SortedMap[String, i64] = SortedMap.new();
    let _ = m.insert("zzzzzzzzzzzzzzzz", 1);
    let _ = m.insert("aaaaaaaaaaaaaaaa", 2);
    return m;
}
fn mksortedset() -> SortedSet[String] {
    let mut s: SortedSet[String] = SortedSet.new();
    s.insert("wwwwwwwwwwwwwwwww");
    return s;
}
fn mkvecval() -> Map[String, Vec[String]] {
    let mut m: Map[String, Vec[String]] = Map.new();
    let inner: Vec[String] = ["one_one_one_one", "two_two_two_two"];
    let _ = m.insert("vvvvvvvvvvvvvvvv", inner);
    return m;
}
fn mknested() -> Map[String, Set[String]] {
    let mut inner: Set[String] = Set.new();
    inner.insert("nnnnnnnnnnnnnnnnnn");
    let mut m: Map[String, Set[String]] = Map.new();
    let _ = m.insert("qqqqqqqqqqqqqqqq", inner);
    return m;
}
fn mkstructkey() -> Map[P, i64] {
    let mut m: Map[P, i64] = Map.new();
    let _ = m.insert(P { name: "pppppppppppppppp" }, 4);
    return m;
}
fn main() {
    let mut k = 0;
    while k < 20 {
        println(f"{mk()}");
        println(f"{mkset()}");
        println(f"{mksorted()}");
        println(f"{mksortedset()}");
        println(f"{mkvecval()}");
        println(f"{mknested()}");
        println(f"{mkstructkey()}");
        k = k + 1;
    }
    println("done");
}
"#,
        &[
            "{kkkkkkkkkkkkkkkk: 1}",
            "Set{eeeeeeeeeeeeeeeeee}",
            "SortedMap{aaaaaaaaaaaaaaaa: 2, zzzzzzzzzzzzzzzz: 1}",
            "SortedSet{wwwwwwwwwwwwwwwww}",
            "{vvvvvvvvvvvvvvvv: [one_one_one_one, two_two_two_two]}",
            "{qqqqqqqqqqqqqqqq: Set{nnnnnnnnnnnnnnnnnn}}",
            "{P { name: pppppppppppppppp }: 4}",
        ]
        .iter()
        .cycle()
        .take(140)
        .copied()
        .chain(std::iter::once("done"))
        .collect::<Vec<_>>(),
        "asan_printed_map_or_set_temporary_frees_its_handle",
    );
}

/// The two shapes where the fix above could have been a DOUBLE FREE instead
/// of a leak fix, and the reason they are safe.
///
/// `print_vec_operand_is_owned_temp` admits any non-borrow-returning call,
/// so both of these reach the new drop-tracking: a free function returning
/// its by-value parameter's map field, and a `ref self` method returning
/// `self.m`. Read as source, each looks like it hands back storage its
/// container still owns — which is exactly the aliasing B-2026-08-14-30 was.
/// They are not: Kāra's caller-retains convention deep-copies an owned
/// argument at callee entry, so the returned handle is genuinely fresh.
///
/// Measured, not assumed: this LEAKED 24340 bytes over 140 allocations
/// pre-fix and is clean after, which is only possible if the returned
/// handle had no other owner. Had either shape been an alias, the fixture
/// would report a double free rather than passing — which is why it is kept
/// apart from the producer fixture above rather than folded into it.
///
/// THE LOOP IS LOAD-BEARING, not decoration. A straight-line version of
/// this program — three prints, no `while` — passed against the LEAKING
/// compiler: each print site gets its own entry alloca, so at exit every
/// stranded handle was still reachable from the stack and LeakSanitizer
/// reported nothing. Iterating overwrites those slots and makes the loss
/// visible. Do not "simplify" this fixture by unrolling it.
#[test]
fn asan_printed_map_temporary_from_a_field_returning_callee_is_not_aliased() {
    assert_clean_asan_run(
        r#"
struct B { m: Map[String, i64] }
impl B {
    pub fn get_m(ref self) -> Map[String, i64] { return self.m; }
}
fn take_field(b: B) -> Map[String, i64] { return b.m; }
fn mk() -> Map[String, i64] {
    let mut m: Map[String, i64] = Map.new();
    let _ = m.insert("kkkkkkkkkkkkkkkk", 1);
    return m;
}
fn main() {
    let mut k = 0;
    while k < 20 {
        let b1 = B { m: mk() };
        println(f"{take_field(b1)}");
        let b2 = B { m: mk() };
        println(f"{b2.get_m()}");
        println(f"{b2.m}");
        k = k + 1;
    }
    println("done");
}
"#,
        &["{kkkkkkkkkkkkkkkk: 1}"; 60]
            .iter()
            .copied()
            .chain(std::iter::once("done"))
            .collect::<Vec<_>>(),
        "asan_printed_map_temporary_from_a_field_returning_callee_is_not_aliased",
    );
}

/// B-2026-08-14-25, the layout that tells the truth unaided: a `shared
/// struct` that is the TARGET of a `weak` reference leaks its displaced
/// String even with no read of the field at all, at the default opt level,
/// 693 bytes over 19 allocations.
///
/// Weak-targeting is not a second defect and changes nothing about the
/// release — the field GEP goes through `shared_gep_layout` either way, and
/// the two-word header it implies is exactly what that funnel exists to
/// handle. What it changes is the optimizer's freedom: the allocations
/// survive to run time here, so the leak is visible without the
/// interpolated string and content read the sibling fixtures need. That is
/// worth its own fixture precisely because it does not depend on defeating
/// an optimization to fail — a future opt-level or pipeline change cannot
/// quietly turn this one vacuous the way it did the other two.
#[test]
fn asan_weak_targeted_shared_struct_string_field_reassign_releases_its_buffer() {
    assert_clean_asan_run(
        r#"
shared struct P { mut label: String, mut back: Option[weak P] }
fn main() {
    let p = P { label: "start", back: None };
    let mut i = 0i64;
    while i < 20i64 {
        let f: String = f"epsilonepsilonepsilon-{i}-zetazetazeta";
        p.label = f;
        i = i + 1;
    }
    println("end");
}
"#,
        &["end"],
        "asan_weak_targeted_shared_struct_string_field_reassign_releases_its_buffer",
    );
}

// B-2026-08-17-12 — READ-ONLY MULTI-BRANCH PAR SHARING of a plain
// container. Plain-struct par captures lower as a bitwise header copy
// through the branch env with NO branch-side cleanup (ParCaptureMode::Copy
// — see its doc), so admitting two read-only branches is sound iff no
// branch writes through, drops, or escapes its copy. These fixtures are
// the permanent ASAN gate for that admission: same buffer read from two
// concurrent branches via `ref` params, parent frees after join. A
// double-free (a branch adopting ownership of its header copy) or a leak
// (the parent's cleanup suppressed by the multi-branch capture) fails
// here.

// B-2026-08-17-18 — DEFERRED INITIALIZATION of heap-class locals
// (`let s: String;` / `let v: Vec[T];` + branch assignment). The lowering
// zero-inits the `{ptr, len, cap}` header at the declaration and relies
// on every displaced-value path being cap-guarded, so the pre-assignment
// state must be a structural no-op: no free of the zero header, no leak
// of the first assigned value on REASSIGNMENT, and a clean scope-exit
// drain of the final value (per-element for Vec[String]). These fixtures
// are the measurement the row required before trusting that argument.

#[test]
fn asan_deferred_init_string_branch_reassign_append() {
    assert_clean_asan_run(
        r#"
fn main() {
    let c = true;
    let mut s: String;
    if c { s = f"hi{1}"; } else { s = f"no{2}"; }
    s = f"replaced{3}";
    s = s + "!";
    println(s);
    println(s.len());
}
"#,
        &["replaced3!", "10"],
        "asan_deferred_init_string_branch_reassign_append",
    );
}

#[test]
fn asan_deferred_init_vec_string_elements_drain() {
    assert_clean_asan_run(
        r#"
fn main() {
    let c = true;
    let mut v: Vec[String];
    if c {
        v = Vec.new();
        v.push(f"aaaaaaaaaaaaaaaa{1}");
        v.push(f"bbbbbbbbbbbbbbbb{2}");
        v.push(f"cccccccccccccccc{3}");
    } else {
        v = Vec.new();
    }
    println(v.len());
    println(v[2].len());
}
"#,
        &["3", "17"],
        "asan_deferred_init_vec_string_elements_drain",
    );
}

/// B-2026-08-20-41 — `String.normalize(form)` returns a FRESH runtime
/// allocation (`karac_unicode_normalize` -> `alloc_string_result`), so the
/// caller owns it and must free it exactly once. Same ownership contract as
/// the `karac_string_to_lowercase` family this arm was modelled on.
///
/// Three shapes in one loop, because they take different cleanup paths: a
/// bare temporary whose result is consumed and dropped at the statement, a
/// binding that lives to the end of the iteration, and a CHAIN whose
/// intermediate normalize result must be freed while the outer
/// `to_uppercase` result is handed on. The loop makes a per-call imbalance
/// accumulate rather than hide in one iteration; the growing NFD case makes
/// the allocation a real one rather than something an optimizer can fold.
#[test]
fn asan_normalize_result_has_exactly_one_owner() {
    assert_clean_asan_run(
        r#"fn main() {
    let mut i = 0i64;
    let mut total = 0i64;
    let mut last: String = "";
    while i < 50i64 {
        let src = "e\u{0301}fg";
        total = total + src.normalize(Nfc).len();
        let held = src.normalize(Nfd);
        total = total + held.len();
        last = src.normalize(Nfc).to_uppercase();
        i = i + 1;
    }
    println(last);
    println(total);
}
"#,
        &["\u{00C9}FG", "450"],
        "asan_normalize_result_has_exactly_one_owner",
    );
}

// ── B-2026-08-24-13 — a break value that owns heap leaves the loop with
//    EXACTLY ONE owner.
//
// `compile_break` stores the value into the loop's result slot and then
// drains the frames inside the loop. Before the fix that drain freed the
// very buffer the slot points at, so the receiver got a dangling pointer
// and freed it again: "free(): double free detected in tcache 2" on all
// three carriers below. The fix suppresses the source's scope-exit free
// between the store and the drain — the break-site twin of
// `suppress_cleanup_for_tail_return`.
//
// These are ASAN tests rather than output comparisons on purpose: the
// failure mode is a FREE, not a wrong value, and both directions have to
// be caught. A too-eager suppression trades the double free for a leak,
// which only LeakSanitizer sees — and only on Linux (CLAUDE.md: a green
// macOS asan run is silent about leaks). The `min_allocs` floors keep a
// fixture from silently optimising its allocation away and asserting
// nothing.
//
// THE FLOORS DID NOT DO THAT, for two and a half years' worth of these
// rows, and B-2026-09-07-26 is where it surfaced. `min_allocs` compared
// against ASAN's RAW process-wide count, which includes a per-host
// start-up floor larger than any threshold here — 10 on arm64 Linux, 199
// on macOS. Every fixture in this family cleared its floor on the ASAN
// runtime's own allocations, and MEASURED once the count was made
// floor-relative, nine of them performed ZERO OR ONE allocation of their
// own: `let mut i = 0` with a constant trip count folds the whole loop
// away at -O2, payloads included, so a suite built to catch a double free
// was running over a program that never allocated. Identical counts on
// both hosts, so this was never a platform difference.
//
// Hence `env.args().len() - 1` — the file's established opaque-seed idiom
// (B-2026-08-04-17), worth exactly 0 under this harness and a bare
// invocation alike, so every expected transcript is unchanged while the
// loop can no longer be folded. `loop-break-map-handle` and
// `loop-break-map-rvalue` keep their literal seeds: a Map handle survives
// -O2 regardless, and both were already above their floors.
//
// These do NOT guard the COMPILE path: `assert_clean_asan_run` skips when
// setup fails, so if aggregates ever regressed to being refused outright,
// every fixture here would print "setup failed — skipping" and pass
// vacuously. That half is guarded in tests/codegen.rs, whose `run_program`
// PANICS on a codegen failure —
// `test_e2e_loop_break_string_value_round_trips` and its Identifier
// sibling are the loud pair. Keep both: one asserts the value arrives, the
// other that it arrives owned exactly once.

/// The f-string carrier: the buffer belongs to the interpolation
/// accumulator, not to any binding, so it needs the `rhs_stages_fstr_acc`
/// suppressor rather than the Identifier one.
#[test]
fn asan_loop_break_fstring_value_single_owner() {
    assert_clean_asan_run_min_allocs(
        "fn pick() -> String {\n\
             \x20   let mut i: i64 = env.args().len() - 1;\n\
             \x20   loop { i = i + 1; if i == 2 { break f\"got{i}\" } }\n\
             }\n\
             fn main() { println(pick()); }\n",
        &["got2"],
        "loop-break-fstring",
        4,
    );
}

/// The other half of B-2026-08-27-29's measurement, pinned so the fix stays
/// scoped: a `String` / `Vec` clone in the same position was ALREADY clean
/// and must stay so. Those pass their `{ptr,len,cap}` header by value and
/// the callee frees it directly — there is no entry copy to orphan an
/// original, which is why the leak was struct-specific despite the shape
/// looking identical in source.
#[test]
fn asan_string_and_vec_clone_call_arguments_stay_clean() {
    assert_clean_asan_run(
        r#"
fn take_s(s: String) -> i64 { return s.len(); }
fn take_v(v: Vec[i64]) -> i64 { return v.len(); }
fn main() {
    let a: String = f"payload_aaaaaaaaaaaaaaaaaaaaaaaa{1}";
    let mut b: Vec[i64] = Vec.new();
    b.push(1); b.push(2); b.push(3);
    let mut t = 0i64;
    let mut i = 0i64;
    while i < 20i64 { t = t + take_s(a.clone()) + take_v(b.clone()); i = i + 1i64; }
    println(f"{t}");
}
"#,
        &["720"],
        "asan-string-vec-clone-call-arg",
    );
}

/// B-2026-08-27-21 leg 2 — a `ref` binding to a STRING element
/// (`let s = ref names[i]`) had no `string_vars` registration at all, so
/// every String method on it hit the loud "no handler for method" arm under
/// codegen while `--interp` ran all of them.
///
/// The registration added for it is dispatch metadata only, and this is the
/// fixture that proves it: `s` borrows a buffer the Vec owns, so a
/// scope-exit free queued on `s` would double-free it. `.clone()` is
/// included because it is the one method here that legitimately allocates —
/// its result is owned and must be freed exactly once, while the borrowed
/// receiver must not be freed at all.
#[test]
fn asan_string_methods_through_ref_binding_are_clean() {
    assert_clean_asan_run(
        r#"
fn main() {
    let mut d: Vec[String] = Vec.new();
    d.push(f"payload_hhhhhhhhhhhhhhhhhhhh{1}");
    d.push(f"payload_wwwwwwwwwwwwwwwwwwww{2}");
    let s = ref d[0];
    let mut t = 0i64;
    let mut i = 0i64;
    while i < 20i64 {
        let c: String = s.clone();
        t = t + c.len() + s.len() + s.substring(0, 3).len();
        i = i + 1;
    }
    println(f"{t}");
    println(f"{d[1]}");
}
"#,
        &["1220", "payload_wwwwwwwwwwwwwwwwwwww2"],
        "string-methods-through-ref-binding",
    );
}

/// CONTROL that isolates the row to the STRUCT key shape: the same 40
/// insert/remove pairs over a BARE `String` key, which the `drop_key` flag
/// already covers. It was clean when the row was filed and must stay clean
/// — a fix that started freeing through both the flag and a new key path
/// would double-free exactly here.
#[test]
fn asan_map_remove_with_a_bare_string_key_stays_clean() {
    assert_clean_asan_run(
        r#"
fn main() {
    let mut m: Map[String, i64] = Map.new();
    let mut i = 0i64;
    while i < 40i64 {
        m.insert(f"key-{i}-padding-padding-padding", i);
        m.remove(f"key-{i}-padding-padding-padding");
        i = i + 1;
    }
    println(f"{m.len()}");
}
"#,
        &["0"],
        "map-remove-bare-string-key-control",
    );
}

/// PROBE (B-2026-08-27-3 blast radius): `Set.clear`. Unlike `Map.clear`,
/// this arm calls plain `karac_map_clear` — no flags, no walks — so it is
/// worth asking whether even the BARE heap element it already claims to
/// cover survives.
#[test]
fn asan_set_clear_releases_bare_string_elements() {
    assert_clean_asan_run(
        r#"
fn main() {
    let mut s: Set[String] = Set.new();
    let mut i = 0i64;
    while i < 40i64 {
        s.insert(f"key-{i}-padding-padding-padding");
        i = i + 1;
    }
    s.clear();
    println(f"{s.len()}");
}
"#,
        &["0"],
        "set-clear-bare-string-elements",
    );
}

/// PROBE (B-2026-08-27-3 blast radius): a `Vec[String]` KEY — the shape
/// where the `drop_key` flag and a per-key drop fn BOTH apply. The flag
/// sees a `{ptr,len,cap}` and frees the outer buffer; the elements below it
/// need the walk. This is the one shape where a careless fix double-frees,
/// so it is worth having on record in both directions.
#[test]
fn asan_map_remove_releases_a_vec_string_keys_elements() {
    assert_clean_asan_run(
        r#"
fn main() {
    let mut m: Map[Vec[String], i64] = Map.new();
    let mut i = 0i64;
    while i < 20i64 {
        let mut k: Vec[String] = Vec.new();
        k.push(f"key-{i}-padding-padding-padding");
        let mut probe: Vec[String] = Vec.new();
        probe.push(f"key-{i}-padding-padding-padding");
        m.insert(k, i);
        m.remove(probe);
        i = i + 1;
    }
    println(f"{m.len()}");
}
"#,
        &["0"],
        "map-remove-vec-string-key-elements",
    );
}

#[test]
fn asan_optres_temp_arg_ownership_follows_callee_escape() {
    // Escape routes — each of these aborted or leaked before.
    assert_clean_asan_run(
        r#"
struct Bag { xs: Vec[Option[String]] }
struct Mk { n: i64 }
impl Bag {
    fn wrap(x: Option[String]) -> Bag { let mut v: Vec[Option[String]] = Vec.new(); v.push(x); return Bag { xs: v }; }
}
impl Mk {
    fn wrap(ref self, x: Option[String]) -> Bag { let mut v: Vec[Option[String]] = Vec.new(); v.push(x); return Bag { xs: v }; }
}
fn wrapf(x: Option[String]) -> Bag { let mut v: Vec[Option[String]] = Vec.new(); v.push(x); return Bag { xs: v }; }
fn pass(x: Option[Vec[String]]) -> Option[Vec[String]] { return x; }
fn mks(n: i64) -> String { return f"payloadpayload{n}"; }
fn main() {
    let m = Mk { n: 1 };
    let mut i = 0;
    while i < 3 {
        println(f"{Bag.wrap(Some(mks(i))).xs.len()}");
        println(f"{m.wrap(Some(mks(i))).xs.len()}");
        println(f"{wrapf(Some(mks(i))).xs.len()}");
        let mut vs: Vec[String] = Vec.new();
        vs.push(f"payloadpayload{i}");
        println(f"{pass(Some(vs))}");
        i = i + 1;
    }
    println("done");
}
"#,
        &[
            "1",
            "1",
            "1",
            "Some([payloadpayload0])",
            "1",
            "1",
            "1",
            "Some([payloadpayload1])",
            "1",
            "1",
            "1",
            "Some([payloadpayload2])",
            "done",
        ],
        "b35-optres-temp-escape-routes",
    );

    // Non-escaping callees — the leak half, plus both controls.
    assert_clean_asan_run(
        r#"
struct H { n: i64 }
struct W { f: Vec[String] }
impl H { fn show(ref self, x: Option[String]) { println(f"{self.n} {x}"); } }
fn mkv(n: i64) -> Vec[String] { let mut v: Vec[String] = Vec.new(); v.push(f"payloadpayload{n}"); return v; }
fn show(x: Option[Vec[String]]) { println(f"{x}"); }
fn showr(x: Result[i64, Vec[String]]) { println(f"{x}"); }
fn main() {
    let h = H { n: 7 };
    let mut i = 0;
    while i < 3 {
        let mut a: Vec[String] = Vec.new();
        a.push(f"payloadpayload{i}");
        show(Some(a));
        let mut b: Vec[String] = Vec.new();
        b.push(f"payloadpayload{i}");
        show(Some(b.clone()));
        println(f"{b.len()}");
        show(Some(mkv(i)));
        let mut c: Vec[String] = Vec.new();
        c.push(f"payloadpayload{i}");
        let w = W { f: c };
        show(Some(w.f));
        let mut d: Vec[String] = Vec.new();
        d.push(f"payloadpayload{i}");
        showr(Err(d));
        let s: String = f"payloadpayload{i}";
        h.show(Some(s));
        let mut e: Vec[String] = Vec.new();
        e.push(f"payloadpayload{i}");
        let named: Option[Vec[String]] = Some(e);
        show(named);
        i = i + 1;
    }
    println("done");
}
"#,
        &[
            "Some([payloadpayload0])",
            "Some([payloadpayload0])",
            "1",
            "Some([payloadpayload0])",
            "Some([payloadpayload0])",
            "Err([payloadpayload0])",
            "7 Some(payloadpayload0)",
            "Some([payloadpayload0])",
            "Some([payloadpayload1])",
            "Some([payloadpayload1])",
            "1",
            "Some([payloadpayload1])",
            "Some([payloadpayload1])",
            "Err([payloadpayload1])",
            "7 Some(payloadpayload1)",
            "Some([payloadpayload1])",
            "Some([payloadpayload2])",
            "Some([payloadpayload2])",
            "1",
            "Some([payloadpayload2])",
            "Some([payloadpayload2])",
            "Err([payloadpayload2])",
            "7 Some(payloadpayload2)",
            "Some([payloadpayload2])",
            "done",
        ],
        "b35-optres-temp-nonescaping",
    );
}

/// B-2026-09-04-33 — AN `Option` TUPLE ELEMENT WHOSE PAYLOAD IS AN F-STRING
/// IS UNTYPED, so the tuple falls to the enum-blind LLVM-type walker and the
/// payload is never freed. `let t = (mkD(9), Option.Some(f"p{9}"))` strands
/// its `String` on every compiled surface while the `Drop` body fires
/// exactly once — nothing in the output says anything is wrong.
///
/// Split out of B-2026-09-03-28, which filed it as a second victim of the
/// memo-key collision. It is not: the layout-keyed walker cleared that row's
/// 90 bytes and left these exactly as they were. The chain is one namer
/// short — `infer_arg_elem_te` resolves an f-string through
/// `enum_name_of_expr` / `type_name_of` / `scalar_type_name_of_expr`, every
/// one of which declines a literal, so the payload derives as the EMPTY path,
/// `tuple_elem_optres_drop_ok` refuses `Option[<empty>]`, and the `let`
/// takes `aggregate_has_heap_field`'s walker, which steps over the `Option`'s
/// payload words.
///
/// THREE SHAPES, ALL MEASURED, pre-fix 8 B in 3 allocations at `-O2`:
///
///   * `ctlOptStr` — the row's `let … ; let t2 = t;` rebind, 2 B.
///   * `ctlNoReb`  — the same `let` with NO rebind, 3 B. The rebind in the
///     row's title is incidental; a plain tuple `let` leaks on its own.
///   * `argOptStr` — the tuple ARGUMENT path, 3 B. `tuple_arg_elem_type_exprs`
///     maps each element straight through the same namer, so one fix covers
///     the `let` and the call alike.
///
/// The parent row's "only when other tuple shapes are DEFINED" framing was
/// OPTIMIZER DCE, not a shape dependence: `ctlOptStr` as a one-function
/// program leaks the same 2 B at `KARAC_OPT_LEVEL=0` and is clean at `-O2`
/// only because the lone `malloc` is provably dead. This fixture keeps the
/// population of shapes so `-O2` cannot hide the allocation.
///
/// CONTROLS that pin the axis to "literal vs call", all clean at `-O0`:
/// `Option.Some(mkS(9))` with `fn mkS -> String` (the fn-return arm names
/// it, and the SAME downstream walk frees it — proof the admit gate already
/// takes `Option[String]`); a plain `Option.Some("p9")` (rodata, `cap = 0`,
/// so the walker's `cap > 0` guard makes the drop a no-op rather than a
/// wild free — and why a static literal was never a leak); an annotated
/// `let t: (D, Option[String])` (the declared type takes the deep walk); and
/// a bare non-tuple `let o = Option.Some(f"p{9}")` (a different path
/// entirely). `ctlScalar`'s `(0, Option.Some(mkD(8)))` is clean since
/// B-2026-09-03-28 and is kept here as the reason the INTEGER half of the
/// parent's "in the same breath" suggestion is deliberately not taken: it
/// buys nothing measurable and risks a width the typechecker inferred.
///
/// The fix asks `span_tables.string_typed_exprs` — the typechecker's own
/// `Type::Str` spans, the same source `concrete_type_expr_of_expr` reads —
/// LAST in the namer chain, so it changes only elements that mangled empty
/// before. The output assertion is the other half: every body exactly once,
/// in order, which is what fails if naming the payload ever double-frees it.
#[test]
fn asan_untyped_option_tuple_elem_frees_its_fstring_payload() {
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
             fn takes(t: (D, Option[String])) { println(\"  tk\") }\n\
             fn optHeap()   { let t = (mkD(1), Option.Some(mkD(2))); let t2 = t; println(\"  oh\") }\n\
             fn noDrop()    { let t = (mkQ(3), Option.Some(mkQ(4))); let t2 = t; println(\"  nd\") }\n\
             fn strElem0()  { let t = (f\"s{5}\", Option.Some(mkD(5))); let t2 = t; println(\"  se\") }\n\
             fn ctlPlain()  { let t = (mkD(6), mkD(7)); let t2 = t; println(\"  cp\") }\n\
             fn ctlScalar() { let t = (0, Option.Some(mkD(8))); let t2 = t; println(\"  cs\") }\n\
             fn ctlOptStr() { let t = (mkD(9), Option.Some(f\"p{9}\")); let t2 = t; println(\"  co\") }\n\
             fn ctlNoReb()  { let t = (mkD(10), Option.Some(f\"p{10}\")); println(\"  cn\") }\n\
             fn argOptStr() { takes((mkD(11), Option.Some(f\"p{11}\"))); println(\"  ao\") }\n\
             fn ctlNone()   { let t: (D, Option[D]) = (mkD(12), Option.None); let t2 = t; println(\"  cz\") }\n\
             fn main() {\n\
             \x20   optHeap(); noDrop(); strElem0(); ctlPlain();\n\
             \x20   ctlScalar(); ctlOptStr(); ctlNoReb(); argOptStr(); ctlNone();\n\
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
                "  dD8:1:n8",
                "  cs",
                "  dD9:1:n9",
                "  co",
                "  dD10:1:n10",
                "  cn",
                "  tk",
                "  dD11:1:n11",
                "  ao",
                "  dD12:1:n12",
                "  cz",
                "end",
            ],
            "b33-untyped-option-tuple-elem-payload",
        );
}

/// B-2026-09-11-3 — the `String` / `Vec` half of the row above, found by
/// the drop fuzzer's corpus rather than by hand.
///
/// `enum Slot[T] { Filled(T), Blank }` at `T = String` leaked the payload's
/// character buffer every time the value reached scope exit instead of
/// being matched out. A generic enum's erased payload area is ONE word (the
/// classifier reads the DECLARATION, where the payload is the bare
/// parameter `T`) and a `String` is three, so `coerce_to_payload_words`
/// heap-boxes it; the box drop then reclaimed the envelope and nothing
/// owned what was inside it. `enum_boxed_payload_interior_drop` resolved an
/// interior only as a user struct or enum — exactly the remainder its own
/// doc comment recorded as unmeasured.
///
/// THE CELLS THAT MATTER ARE `c2` AND `c3`, and they are here because the
/// row was FILED WRONG. It claimed the leak needed a `ref` callee that
/// MATCHES the enum, listing "a `ref` callee that does not match" and
/// "never read at all" as clean controls. Both were measured at the default
/// `-O2`, where LLVM deletes an allocation nothing observes — so they
/// reported the optimizer, not the drop path. At `-O0` all three leak
/// identically and the match is not the axis at all. The same correction
/// retired the row's "context sensitivity" section: the two neighbouring
/// locals it named as required were only making the allocation observable.
///
/// `c4`, `c5` and `c8` are DOUBLE-FREE cells, not leak cells. An arm that
/// binds the payload out takes the interior, and `clear_boxed_enum_inner_drop`
/// retracts the box's interior walk when it does; an interior drop installed
/// without that coordination frees the same buffer twice. They were clean
/// before this fix and must stay clean — the half a leak count alone does
/// not check.
///
/// `c6` is the monomorphic twin, clean throughout: a concretely-declared
/// `String` payload classifies `VecOrString` and never boxes.
///
/// NOT VACUOUS (B-2026-08-04-17): every payload is seeded from the opaque
/// `env.args().len()` and read through `contains` — its BYTES, not its
/// length. That is load-bearing here rather than routine: the literal-seeded,
/// `len()`-only first draft of this fixture folded to nothing at `-O2` and
/// passed against the very compiler it was written to fail, which is the
/// same non-measurement that put the two wrong controls in the row. Pre-fix
/// this program loses 480 B in 24 blocks at the default `-O2` and 864 B in
/// 32 blocks under `KARAC_OPT_LEVEL=0`.
#[test]
fn asan_generic_enum_boxed_string_payload_frees_its_interior() {
    let mut expected: Vec<&str> = Vec::new();
    for _ in 0..8 {
        expected.extend_from_slice(&[
            "c1:true", "c2:true", "c3", "c4:true", "c5:true", "c6:true", "c7:2", "c8:2",
        ]);
    }
    expected.push("end");
    assert_clean_asan_run_min_allocs(
        r#"
enum Slot[T] { Filled(T), Blank }
enum StrSlot { FilledS(String), BlankS }

fn peek(s: ref Slot[String]) -> bool { match s { Filled(x) => x.contains("row"), Blank => false, } }
fn blind(s: ref Slot[String]) -> bool { return true; }
fn take(s: Slot[String]) -> bool { match s { Filled(x) => x.contains("row"), Blank => false, } }
fn mono(s: ref StrSlot) -> bool { match s { FilledS(x) => x.contains("row"), BlankS => false, } }
fn vpeek(s: ref Slot[Vec[String]]) -> i64 {
    match s {
        Filled(x) => {
            let mut c: i64 = 0i64;
            for e in x { if e.contains("row") { c = c + 1i64; } }
            return c;
        },
        Blank => 0i64,
    }
}
fn vtake(s: Slot[Vec[String]]) -> i64 {
    match s {
        Filled(x) => {
            let mut c: i64 = 0i64;
            for e in x { if e.contains("row") { c = c + 1i64; } }
            return c;
        },
        Blank => 0i64,
    }
}

fn main() {
    let n = env.args().len() as i64;
    let mut i: i64 = 0i64;
    while i < 8i64 {
        let c1: Slot[String] = Filled(f"row-aaaaaaaaaaaa-{i}-{n}");
        println(f"c1:{peek(c1)}");
        let c2: Slot[String] = Filled(f"row-bbbbbbbbbbbb-{i}-{n}");
        println(f"c2:{blind(c2)}");
        let c3: Slot[String] = Filled(f"row-cccccccccccc-{i}-{n}");
        println("c3");
        let c4: Slot[String] = Filled(f"row-dddddddddddd-{i}-{n}");
        println(f"c4:{take(c4)}");
        let c5: Slot[String] = Filled(f"row-eeeeeeeeeeee-{i}-{n}");
        println(f"c5:{match c5 { Filled(x) => x.contains("row"), Blank => false, }}");
        let c6: StrSlot = FilledS(f"row-ffffffffffff-{i}-{n}");
        println(f"c6:{mono(c6)}");
        let c7: Slot[Vec[String]] = Filled(Vec[f"row-gggggggggggg-{i}-{n}", f"row-hhhhhhhhhhhh-{i}-{n}"]);
        println(f"c7:{vpeek(c7)}");
        let c8: Slot[Vec[String]] = Filled(Vec[f"row-iiiiiiiiiiii-{i}-{n}", f"row-jjjjjjjjjjjj-{i}-{n}"]);
        println(f"c8:{vtake(c8)}");
        i = i + 1i64;
    }
    println("end");
}
"#,
        &expected,
        "asan_generic_enum_boxed_string_payload_frees_its_interior",
        // 133 measured (143 raw minus a 10 host floor). A version the
        // optimizer folded away reaches ~10, so this floor separates them
        // with room for host drift in either direction.
        80,
    );
}

/// B-2026-09-16-15 — a boxed generic-enum `String` payload whose match arm
/// BINDS the payload and never USES it.
///
/// Every pre-existing fixture in this family reads its binding —
/// `asan_generic_enum_boxed_string_payload_frees_its_interior`'s `take`
/// does `x.contains("row")` — and that is exactly why this survived. The
/// binding's metadata is registered from its SURFACE TYPE NAME, and the
/// name is what was missing, so a cell that goes on to dispatch a method
/// through that same table cannot be the one that notices.
///
/// CAUSE, and it is not the one the row was filed under. The typechecker
/// lowers `Type::Str` to the head `"str"`
/// (`typechecker/patterns.rs`, `Type::Str => path("str", vec![])`) while
/// the pattern tables spell it `"String"`; `seed_synthetic_pattern_binding_type`
/// (`codegen/calls.rs`) normalizes between the two and says so in its doc.
/// `mono_payload_binding_surface` — the FALLBACK those tables use for a
/// bare-`T` payload the checker records no surface type for — did not.
/// So `bind_pattern_values` saw `"str"`, matched neither its
/// `"Vec" | "VecDeque"` arm nor its `"String" | "CString"` one, left
/// `bound_vec_elem` at `None`, and registered no end-of-arm buffer free —
/// while `clear_boxed_enum_inner_drop` retracted the box's own interior
/// drop for the same arm, because `boxed_payload_interior_taken_by_arm`
/// answers TRUE for a String payload. Envelope freed, buffer owned by
/// nobody.
///
/// The row attributed it to that predicate's `_ => generic_args.is_none()`
/// tail answering TRUE for a bare `T`. That cannot be right, and `c3` is
/// the control that shows it: a CONCRETE `fn c(g: G1[String])` reaches the
/// same predicate with the same answer and is clean, because the checker
/// records its surface type directly and the fallback never runs.
///
/// MEASURED at `KARAC_OPT_LEVEL=0`, distinct string LENGTHS so a leaked
/// byte count names its own cell: 11 B in 1 block per call before, 0 after,
/// with `c2`/`c3`/`c4`/`c5` byte-identical across the change.
///
/// NOT FIXED HERE, and deliberately not folded in: an `Array[String, N]`
/// payload still strands its ELEMENT buffers, with or without a match
/// (measured both ways in one binary — the match is irrelevant to it, which
/// is why the row's "two shapes" are really one mechanism plus this one).
/// That path wants an element-draining registration and sits next to the
/// `array_interior_ok` gate B-2026-09-12-18 added to stop a measured double
/// free, so it is its own change with its own reduction.
#[test]
fn asan_boxed_generic_enum_string_payload_unused_arm_binding_frees_its_buffer() {
    let mut expected: Vec<&str> = Vec::new();
    for _ in 0..8 {
        expected.extend_from_slice(&["c1:1", "c2:2", "c3:3", "c4", "c5:5"]);
    }
    expected.push("end");
    assert_clean_asan_run_min_allocs(
        r#"
enum G1[T] { Y(T), N }

// c1 — THE CELL. Generic callee, arm binds `va` and never reads it.
fn g_unused[T](g: G1[T]) -> i64 { match g { Y(va) => { return 1i64; } N => { return 0i64; } } }
// c2 — same callee, WILDCARD arm: binds nothing, so no retraction at all.
fn g_wild[T](g: G1[T]) -> i64 { match g { Y(_) => { return 2i64; } N => { return 0i64; } } }
// c3 — CONTROL that refutes the filed attribution. Concrete callee, same
// boxed layout, same unused binding, clean before this fix and after it.
fn c_unused(g: G1[String]) -> i64 { match g { Y(vc) => { return 3i64; } N => { return 0i64; } } }
// c4 — generic callee whose arm CONSUMES its binding. Must stay a single
// free: the arm owns the interior and the box's drop is retracted for it.
fn g_take[T](g: G1[T]) -> G1[T] { match g { Y(vb) => { return Y(vb); } N => { return N; } } }
// c5 — generic callee that READS its binding, the shape existing fixtures
// already cover, included so a regression here cannot hide behind c1.
fn g_read(g: G1[String]) -> i64 { match g { Y(vd) => { if vd.contains("row") { return 5i64; } return 9i64; } N => { return 0i64; } } }

fn main() {
    let n = env.args().len() as i64;
    let mut i: i64 = 0i64;
    while i < 8i64 {
        let c1: G1[String] = Y(f"row-aaaaaaaaaaaa-{i}-{n}");
        println(f"c1:{g_unused(c1)}");
        let c2: G1[String] = Y(f"row-bbbbbbbbbbbb-{i}-{n}");
        println(f"c2:{g_wild(c2)}");
        let c3: G1[String] = Y(f"row-cccccccccccc-{i}-{n}");
        println(f"c3:{c_unused(c3)}");
        let c4: G1[String] = Y(f"row-dddddddddddd-{i}-{n}");
        let r4 = g_take(c4);
        println("c4");
        let c5: G1[String] = Y(f"row-eeeeeeeeeeee-{i}-{n}");
        println(f"c5:{g_read(c5)}");
        i = i + 1i64;
    }
    println("end");
}
"#,
        &expected,
        "asan_boxed_generic_enum_string_payload_unused_arm_binding_frees_its_buffer",
        // Five heap strings per round over eight rounds, minus a host
        // floor — the same shape of floor the sibling above uses, chosen
        // so an optimizer-folded version (~10) cannot pass.
        30,
    );
}

/// B-2026-09-17-29 — a boxed `Option`/`Result` payload handed to an owning
/// callee from INSIDE an f-string interpolation hole.
///
/// `consume_class` decides whether a match arm merely reads its payload
/// binding or hands it to a new owner, and the boxed-`Array` interior walk
/// (B-2026-09-14-13) is registered only for a reading arm — a consuming one
/// already has an owner in the callee's `make_array_param_callee_owned`
/// copy. Neither of the two walks that answer that question had an arm for
/// `ExprKind::InterpolatedStringLit`, so a call sitting in a hole fell
/// through their catch-alls and the arm scored borrow-only. The walk then
/// became a SECOND owner of every element buffer.
///
/// The cells, and what each one is for:
///
///   interp    the row's own spelling, `println(f"e:{eat(a)}")` over a
///             `Map[i64, Array[String, 2]]`. Aborted `free(): double free
///             detected in tcache 2`, 39 allocs against 46 frees.
///   nomap     the same arm with NO container — a plain
///             `fn mk() -> Option[Array[String, 2]]`. This is what settles
///             that the container is not the axis: it aborted identically,
///             24 allocs against 32 frees, so the bug is the hand-back.
///   letbound  `let n = eat(a); println(f"e:{n}")`. The SAME transfer with
///             the call lifted out of the hole, which was already clean
///             (24/24) and is what isolated the interpolation as the cause.
///             It is here so a future narrowing cannot quietly re-blind the
///             hole while this file still reports green.
///   readonly  an arm that reads the array and hands it to nobody. The
///             control for the opposite error: the walk is the SOLE owner
///             here, so a fix that retracted it outright would leak instead.
///
/// Each asserts the interpreter's own output, which was correct throughout.
#[test]
fn asan_boxed_array_payload_consumed_in_fstring_hole_no_double_free() {
    const EAT: &str = "fn eat(a: Array[String, 2]) -> i64 { return a[0].len() as i64; }\n";

    // The row's headline cell: the consuming call lives in a hole.
    assert_clean_asan_run(
            &format!(
                "{EAT}\
                 fn main() {{\n\
                 \x20   let mut v: Map[i64, Array[String, 2]] = Map.new();\n\
                 \x20   let mut i: i64 = 0i64;\n\
                 \x20   while i < 4i64 {{ v.insert(i, [f\"row-aaaaaaaaaaaaaaaa-{{i}}\", f\"col-bbbbbbbbbbbbbbbb-{{i}}\"]); i = i + 1i64; }}\n\
                 \x20   let mut j: i64 = 0i64;\n\
                 \x20   while j < 2i64 {{\n\
                 \x20       match v.remove(j) {{ Option.Some(a) => {{ println(f\"e:{{eat(a)}}\") }} Option.None => {{ println(\"n\") }} }}\n\
                 \x20       j = j + 1i64;\n\
                 \x20   }}\n\
                 \x20   println(\"end\");\n\
                 }}\n"
            ),
            &["e:22", "e:22", "end"],
            "b1729-fstring-hole-consumes-array-payload-interp",
        );

    // No container anywhere — the hand-back is the axis, not the `Map`.
    assert_clean_asan_run(
            &format!(
                "{EAT}\
                 fn mk(i: i64) -> Option[Array[String, 2]] {{\n\
                 \x20   return Option.Some([f\"row-aaaaaaaaaaaaaaaa-{{i}}\", f\"col-bbbbbbbbbbbbbbbb-{{i}}\"]);\n\
                 }}\n\
                 fn main() {{\n\
                 \x20   let mut j: i64 = 0i64;\n\
                 \x20   while j < 2i64 {{\n\
                 \x20       match mk(j) {{ Option.Some(a) => {{ println(f\"e:{{eat(a)}}\") }} Option.None => {{ println(\"n\") }} }}\n\
                 \x20       j = j + 1i64;\n\
                 \x20   }}\n\
                 \x20   println(\"end\");\n\
                 }}\n"
            ),
            &["e:22", "e:22", "end"],
            "b1729-fstring-hole-consumes-array-payload-nomap",
        );

    // The same transfer with the call lifted OUT of the hole. Clean before
    // the fix; here so a narrowing cannot re-blind the hole unnoticed.
    assert_clean_asan_run(
            &format!(
                "{EAT}\
                 fn mk(i: i64) -> Option[Array[String, 2]] {{\n\
                 \x20   return Option.Some([f\"row-aaaaaaaaaaaaaaaa-{{i}}\", f\"col-bbbbbbbbbbbbbbbb-{{i}}\"]);\n\
                 }}\n\
                 fn main() {{\n\
                 \x20   let mut j: i64 = 0i64;\n\
                 \x20   while j < 2i64 {{\n\
                 \x20       match mk(j) {{ Option.Some(a) => {{ let n = eat(a); println(f\"e:{{n}}\") }} Option.None => {{ println(\"n\") }} }}\n\
                 \x20       j = j + 1i64;\n\
                 \x20   }}\n\
                 \x20   println(\"end\");\n\
                 }}\n"
            ),
            &["e:22", "e:22", "end"],
            "b1729-fstring-hole-consumes-array-payload-letbound",
        );

    // The opposite-error control: nobody else takes the array, so the
    // interior walk must STAY. A fix that retracted it outright leaks here.
    assert_clean_asan_run(
            "fn mk(i: i64) -> Option[Array[String, 2]] {\n\
             \x20   return Option.Some([f\"row-aaaaaaaaaaaaaaaa-{i}\", f\"col-bbbbbbbbbbbbbbbb-{i}\"]);\n\
             }\n\
             fn main() {\n\
             \x20   let mut j: i64 = 0i64;\n\
             \x20   while j < 2i64 {\n\
             \x20       match mk(j) { Option.Some(a) => { println(f\"e:{a[0].len()}\") } Option.None => { println(\"n\") } }\n\
             \x20       j = j + 1i64;\n\
             \x20   }\n\
             \x20   println(\"end\");\n\
             }\n",
            &["e:22", "e:22", "end"],
            "b1729-fstring-hole-readonly-control",
        );
}
