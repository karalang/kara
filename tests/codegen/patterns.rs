//! match, arms, if/while let, destructuring -- fixtures for `tests/codegen.rs`.
//!
//! Split out of `tests/codegen.rs` on 2026-09-21. The TEST TARGET is
//! unchanged: this file is a module of that target, so
//! `cargo test --features llvm --test codegen` still runs everything
//! and CI needs no edit. Run this area alone with:
//!
//!     cargo test --features llvm --test codegen patterns::
//!
//! New fixtures about match, arms, if/while let, destructuring belong in this file.

use super::*;

/// B-2026-09-02-24 — a `match` arm over an OWNED by-value param that hands
/// its bound-out element/payload straight back out of the function runs that
/// value's `Drop` body exactly ONCE.
///
/// The tuple spelling `fn p4(t: (R, i64)) -> R { match t { (r, k) => r } }`
/// and the enum spelling `fn e4(t: E) -> R { match t { E.A(r) => r … } }`
/// both printed `dR4 got4 dR4` — TWO bodies where one is due — against the
/// interpreter's `got4 dR4`. The first body fired caller-side: the fresh-temp
/// argument's own bodies walk ran the element/payload's `Drop` on a value the
/// callee had already returned, and the result's binding ran it again. The
/// caller could not skip it because the escape analysis
/// (`fn_returns_param_part_paths`) never recorded a MATCH ARM's leaf bindings
/// as denoting the scrutinee's parts — only a `let` destructure did — so the
/// tuple element looked non-escaping; the enum payload rode a separate skip
/// (`callee_returns_enum_arg_payload`) that the fresh-temp enum arg registrar
/// never consulted.
///
/// `pf` is the non-escaping control (a field READ, `r.id`), whose single body
/// must survive the fix — masking it too would trade a double for a lost
/// one — and `pr` pins the explicit-`return` spelling beside the tail one.
/// A no-heap `R` on purpose: the row is a body-COUNT defect (valgrind-clean,
/// the entry copy gives each fire its own buffer); the heap-payload spelling
/// is a distinct MEMORY double-free tracked separately.
#[test]
fn test_e2e_match_arm_returns_owned_param_element_runs_one_body() {
    let out = run_program(
        r#"
struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
enum E { A(R), B }
fn p4(t: (R, i64)) -> R { match t { (r, k) => { r } } }
fn e4(t: E) -> R { match t { E.A(r) => { r } E.B => { R { id: 99 } } } }
fn pf(t: (R, i64)) -> i64 { match t { (r, k) => { r.id } } }
fn pr(t: (R, i64)) -> R { match t { (r, k) => { return r; } } }
fn main() {
  let a = p4((R { id: 4 }, 0)); println(f"got{a.id}");
  let b = e4(E.A(R { id: 7 })); println(f"got{b.id}");
  let c = pf((R { id: 1 }, 0)); println(f"r{c}");
  let d = pr((R { id: 5 }, 0)); println(f"got{d.id}");
  println("end");
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(
            out, "got4\ndR4\ngot7\ndR7\ndR1\nr1\ngot5\ndR5\nend\n",
            "an escaping match-arm element/payload dies once (at the result's \
                 owner), the non-escaping field-read control keeps its single \
                 body inside the call; got {out:?}"
        );
    }
}

#[test]
fn e2e_tuple_with_shared_struct_element_destructured_from_option() {
    // B-2026-07-08-16: destructuring a tuple whose first element is a shared
    // struct (pointer-repr) out of an `Option` — `Some((current, d))` from
    // `Option[(Node, i64)]`, the shape `stack.pop()` produces in an iterative
    // tree/graph walk — reconstructed the pointer element as the raw i64
    // payload word and emitted `insertvalue i64 into ptr` (the tuple slot is
    // `ptr`), aborting at module verification. Non-shared tuples
    // (`Option[(i64, i64)]`) were unaffected. Fix: inttoptr each single-word
    // shared/pointer tuple element to its slot type. Unblocked
    // examples/leetcode/max_depth_binary_tree.kara (interp==AOT==JIT).
    if let Some(out) = run_program(
        "shared struct Node { val: i64 }\n\
             fn main() {\n\
                 let pair: Option[(Node, i64)] = Some((Node { val: 3 }, 7));\n\
                 match pair {\n\
                     None => { println(0); }\n\
                     Some((current, d)) => { println(current.val + d); }\n\
                 }\n\
                 let none: Option[(Node, i64)] = None;\n\
                 match none {\n\
                     None => { println(-1); }\n\
                     Some((c, d)) => { println(c.val + d); }\n\
                 }\n\
             }",
    ) {
        assert_eq!(out, "10\n-1\n");
    }
}

// ── Match ────────────────────────────────────────────────────

#[test]
fn test_ir_match_integer_literals() {
    let ir = ir_for(
        r#"
fn day_name(d: i64) -> i64 {
    match d {
        1 => 100,
        2 => 200,
        _ => 0,
    }
}
"#,
    );
    assert!(ir.contains("icmp eq"), "literal match uses integer compare");
    assert!(ir.contains("match.merge") || ir.contains("matchval"));
}

#[test]
fn test_ir_match_bool() {
    let ir = ir_for(
        r#"
fn negate(b: bool) -> bool {
    match b {
        true => false,
        false => true,
    }
}
"#,
    );
    assert!(ir.contains("icmp eq"));
}

/// B-2026-08-02-25 (match-arm leg) — a consuming `Some(x)` / `Ok(x)` arm on
/// a NAMED `Option`/`Result` binding runs the payload's user `impl Drop`
/// body when that payload is heap-BOXED.
///
/// The two registrations that could own the body both declined: the inline
/// arm on word count, and the boxed arm on its fresh-owning-temp test. So
/// the source's `__karac_dropelems_opt_*` walk — retracted by the arm, since
/// a binding sub-pattern consumes — was the body's only fire path, and with
/// it gone the destructor ran NOWHERE while `karac run` ran it.
///
/// The discriminator is WIDTH, not `Option`-vs-`Result`: the payload area is
/// 3 words for `Option` and 5 for `Result`, so `Wide` (4 words) is boxed in
/// an `Option` and inline in a `Result`, and `Wider` (7) is boxed in both.
/// An earlier note on the ledger row read the Result spelling of case 1 as
/// "correct" and the Option spelling as "broken"; both were really reports
/// about which side of the boxing threshold the payload landed on. Case 4
/// therefore uses `Wider`, or it would exercise the inline arm again.
///
/// No sanitizer sees this class — the payload's memory is freed correctly by
/// the untouched `BoxedEnumDrop` either way, so only the destructor's
/// side effect goes missing. Cases 5 and 6 are the over-fire guards in the
/// other direction: a non-consuming arm (whose source keeps its walk) and
/// an inline payload (the pre-existing arm) must each still print ONE body.
#[test]
fn e2e_named_optres_boxed_payload_arm_runs_user_drop_body() {
    const PRE: &str = "struct Wide { tag: i64, name: String }\n\
             impl Drop for Wide { fn drop(mut ref self) { println(f\"D{self.tag}\"); } }\n\
             struct Wider { tag: i64, a: String, b: String }\n\
             impl Drop for Wider { fn drop(mut ref self) { println(f\"W{self.tag}\"); } }\n\
             struct Narrow { name: String }\n\
             impl Drop for Narrow { fn drop(mut ref self) { println(f\"N{self.name}\"); } }\n\
             fn mkw(t: i64) -> Wide { return Wide { tag: t, name: \"payload-string-data\" }; }\n\
             fn mkr(t: i64) -> Wider {\n\
             \x20   return Wider { tag: t, a: \"payload-string-data\", b: \"second-payload-str\" };\n\
             }\n";
    let Some(out) = run_program(&format!(
        "{PRE}fn main() {{\n\
             \x20   {{\n\
             \x20       let o: Option[Wide] = Some(mkw(1i64));\n\
             \x20       match o {{\n\
             \x20           Some(r) => {{ println(f\"a{{r.tag}}\"); }}\n\
             \x20           None => {{ println(\"a-none\"); }}\n\
             \x20       }}\n\
             \x20       println(\"a-end\");\n\
             \x20   }}\n\
             \x20   {{\n\
             \x20       let o: Option[Wide] = Some(mkw(2i64));\n\
             \x20       if let Some(r) = o {{ println(f\"b{{r.tag}}\"); }}\n\
             \x20       println(\"b-end\");\n\
             \x20   }}\n\
             \x20   {{\n\
             \x20       let o: Option[Wide] = Some(mkw(3i64));\n\
             \x20       let Some(r) = o else {{ return; }}\n\
             \x20       println(f\"c{{r.tag}}\");\n\
             \x20       println(\"c-end\");\n\
             \x20   }}\n\
             \x20   {{\n\
             \x20       let o: Result[Wider, i64] = Ok(mkr(4i64));\n\
             \x20       match o {{\n\
             \x20           Ok(r) => {{ println(f\"d{{r.tag}}\"); }}\n\
             \x20           Err(e) => {{ println(f\"d-err{{e}}\"); }}\n\
             \x20       }}\n\
             \x20       println(\"d-end\");\n\
             \x20   }}\n\
             \x20   {{\n\
             \x20       let o: Option[Wide] = Some(mkw(5i64));\n\
             \x20       match o {{\n\
             \x20           Some(_) => {{ println(\"e\"); }}\n\
             \x20           None => {{ println(\"e-none\"); }}\n\
             \x20       }}\n\
             \x20       println(\"e-end\");\n\
             \x20   }}\n\
             \x20   {{\n\
             \x20       let o: Option[Narrow] = Some(Narrow {{ name: \"n6\" }});\n\
             \x20       match o {{\n\
             \x20           Some(r) => {{ println(f\"f{{r.name}}\"); }}\n\
             \x20           None => {{ println(\"f-none\"); }}\n\
             \x20       }}\n\
             \x20       println(\"f-end\");\n\
             \x20   }}\n\
             \x20   println(\"end\");\n\
             }}\n"
    )) else {
        return;
    };
    // Every body lands at the BINDING's death, before the block's trailing
    // print — including case 3, whose `let … else` binding lives in the
    // enclosing frame rather than an arm's. That case is why the NLL gate
    // in `fire_due_user_drops` had to learn the `__karac_dropbodies_only_`
    // prefix: registering the walk alone made it print after `c-end`.
    assert_eq!(
        out,
        "a1\nD1\na-end\n\
             b2\nD2\nb-end\n\
             c3\nD3\nc-end\n\
             d4\nW4\nd-end\n\
             e\nD5\ne-end\n\
             fn6\nNn6\nf-end\n\
             end\n"
    );
}

/// B-2026-08-04-1 — the FRESH-TEMP twin: a boxed payload bound out of
/// `match mk() { Some(r) => … }` runs its Drop body against the BOX.
///
/// The named-source leg (B-2026-08-02-25) re-homes the source's own
/// `__karac_dropelems_opt_*` action onto the arm binding; a fresh temp has
/// no named source, so it kept registering `__karac_dropbodies_only_<T>`
/// over the binding's reconstructed COPY of `{ptr,len,cap}`. The box still
/// owns the payload's memory, so a Drop body that MUTATES a heap field
/// freed the buffer and zeroed the copy's cap while the box kept the stale
/// pointer — and the scope-exit box drop freed it again. The fix threads
/// the staged `__freshtemp_boxed_scrut` slot to the bind site and registers
/// the same tag-guarded walk over THAT.
///
/// The bodies here mutate (`self.buf.clear()`) on purpose: with a read-only
/// body this whole family is green either way, which is exactly why the
/// defect outlived the leg that shares its root cause. Reading the buffer in
/// the body and in the arm also pins that the clear happens after the read,
/// so a resurrected copy-subject bug shows up as wrong output and not only
/// as an abort.
///
/// The body reads through a `for` loop rather than `buf[0]` because
/// B-2026-08-30-35 made an f-string hole contribute its effects: an index is
/// `panics`, and design.md requires a `Drop` body to be `panics`-free, so
/// `println(f"D{self.buf[0]}")` is now correctly rejected at the definition
/// site. It only compiled before because the hole hid the index from
/// inference. Each `buf` holds exactly one element, so the loop prints the
/// same single line and every assertion below is unchanged.
#[test]
fn e2e_freshtemp_boxed_optres_payload_arm_body_runs_against_the_box() {
    let Some(out) = run_program(
            "struct Full { name: String, buf: Vec[i64] }\n\
             impl Drop for Full {\n\
             \x20   fn drop(mut ref self) { for x in self.buf { println(f\"D{x}\"); } self.buf.clear(); }\n\
             }\n\
             struct Wide { tag: i64, a: String, buf: Vec[i64] }\n\
             impl Drop for Wide {\n\
             \x20   fn drop(mut ref self) { for x in self.buf { println(f\"W{x}\"); } self.buf.clear(); }\n\
             }\n\
             fn mk(i: i64) -> Full {\n\
             \x20   let mut b: Vec[i64] = Vec.new();\n\
             \x20   b.push(i);\n\
             \x20   return Full { name: \"payload-string-data\", buf: b };\n\
             }\n\
             fn mkw(i: i64) -> Wide {\n\
             \x20   let mut b: Vec[i64] = Vec.new();\n\
             \x20   b.push(i);\n\
             \x20   return Wide { tag: i, a: \"payload-string-data\", buf: b };\n\
             }\n\
             fn opt(i: i64) -> Option[Full] { return Option.Some(mk(i)); }\n\
             fn res(i: i64) -> Result[Wide, i64] { return Result.Ok(mkw(i)); }\n\
             fn main() {\n\
             \x20   match opt(1i64) {\n\
             \x20       Option.Some(r) => { println(f\"a{r.buf[0]}\"); }\n\
             \x20       Option.None => { println(\"a-none\"); }\n\
             \x20   }\n\
             \x20   println(\"a-end\");\n\
             \x20   if let Option.Some(r) = opt(2i64) { println(f\"b{r.buf[0]}\"); }\n\
             \x20   println(\"b-end\");\n\
             \x20   match res(4i64) {\n\
             \x20       Result.Ok(r) => { println(f\"d{r.buf[0]}\"); }\n\
             \x20       Result.Err(e) => { println(f\"d-err{e}\"); }\n\
             \x20   }\n\
             \x20   println(\"d-end\");\n\
             \x20   let mut v: Vec[Full] = Vec.new();\n\
             \x20   v.push(mk(5i64));\n\
             \x20   while let Option.Some(r) = v.pop() {\n\
             \x20       println(f\"e{r.buf[0]}\");\n\
             \x20   }\n\
             \x20   println(\"e-end\");\n\
             \x20   println(\"end\");\n\
             }\n",
        ) else {
            return;
        };
    // `Full` is 6 words and `Wide` 7, so both are boxed — `Wide` also past
    // Result's wider 5-word area, which is the point of including it.
    assert_eq!(
        out,
        "a1\nD1\na-end\n\
             b2\nD2\nb-end\n\
             d4\nW4\nd-end\n\
             e5\nD5\ne-end\n\
             end\n"
    );
}

/// B-2026-08-04-11 leg (b) — a fresh-temp `Result` arm that BINDS a struct
/// payload without consuming it must not leave the source's payload drop
/// armed: the binding already owns the buffer.
///
/// `match g(i) { Err(e) => println("bound") }` ABORTED with `free(): double
/// free detected`. The arm's wrapper skip in `compile_match` dates from
/// B-2026-07-12-2 gap 2 and rests on "a struct-wrapper binding registers no
/// cleanup of its own", so a borrow-only read needs the source to stay
/// armed or the payload leaks. B-2026-07-10-3 then falsified that premise
/// by tracking an inline `Option`/`Result` struct payload so its inner
/// `String`/`Vec` fields DO get freed — leaving two owners of one buffer.
///
/// Three things about this fixture are load-bearing against the OPTIMIZER
/// rather than against the compiler, and dropping any of them turns the
/// whole test vacuous at the default `-O2` — which is precisely how this
/// defect stayed hidden. An f-string-built payload field of a call whose
/// argument is a constant folds away, so nothing is ever allocated and the
/// second free has nothing to hit; leg (a)'s `d:unused` arm is exactly that
/// shape and passed throughout. And a payload read only through `.len()`
/// is a provably DEAD allocation, so LLVM deletes the malloc outright: the
/// first draft of this test, `.len()`-only with a `String.new()`+`push_str`
/// payload, still passed against the unfixed compiler.
///
/// So: the payload is built with `String.new()` + `push_str`, the seed is
/// `env.args().len()` rather than a literal (the harness execs with no
/// extra argv, so it is a stable 1 while staying opaque to the optimizer),
/// and every arm reads its buffer's BYTES via `contains` against a
/// runtime-derived needle before reporting the length. Each length is
/// distinct from every other arm's, and a corrupted buffer prints `BAD`,
/// so an emptied or cross-wired payload fails the assert rather than
/// reading as a pass. `run_program` builds at `-O2`, and the fixture aborts
/// against the unfixed compiler there.
///
/// Arms (a)-(c) are the defect: bound-and-unused, bound-and-read, and the
/// same through a two-field payload. Arms (d)-(f) are the immunities that
/// localize it and must stay clean — a wildcard binds nothing, a named
/// scrutinee takes the ordinary binding path, and a consuming arm moves the
/// field out. (f) is also the guard on the fix's blast radius: it is the
/// gap-2 recover-CONSUME shape, and the source must still suppress there.
#[test]
fn e2e_freshtemp_result_arm_binding_a_struct_payload_owns_it_once() {
    let Some(out) = run_program(
            "struct One { msg: String }\n\
             struct Two { code: i64, msg: String }\n\
             fn s_of(tag: String, i: i64) -> String {\n\
             \x20   let mut s: String = String.new();\n\
             \x20   s.push_str(tag);\n\
             \x20   s.push_str(f\"-payload-{i}\");\n\
             \x20   return s;\n\
             }\n\
             fn digits(i: i64) -> String {\n\
             \x20   let mut d: String = String.new();\n\
             \x20   d.push_str(f\"{i}\");\n\
             \x20   return d;\n\
             }\n\
             fn g1(i: i64) -> Result[i64, One] { return Result.Err(One { msg: s_of(\"one\", i) }); }\n\
             fn g2(i: i64) -> Result[i64, Two] { return Result.Err(Two { code: i, msg: s_of(\"two\", i) }); }\n\
             fn main() {\n\
             \x20   let n: i64 = env.args().len();\n\
             \x20   match g1(n) {\n\
             \x20       Result.Ok(v) => { println(f\"ok{v}\"); }\n\
             \x20       Result.Err(e) => { println(\"a:unused\"); }\n\
             \x20   }\n\
             \x20   match g1(n + 10i64) {\n\
             \x20       Result.Ok(v) => { println(f\"ok{v}\"); }\n\
             \x20       Result.Err(e) => {\n\
             \x20           if e.msg.contains(digits(n + 10i64)) { println(f\"b:{e.msg.len()}\"); }\n\
             \x20           else { println(\"b:BAD\"); }\n\
             \x20       }\n\
             \x20   }\n\
             \x20   match g2(n + 100i64) {\n\
             \x20       Result.Ok(v) => { println(f\"ok{v}\"); }\n\
             \x20       Result.Err(e) => {\n\
             \x20           if e.msg.contains(digits(n + 100i64)) { println(f\"c:{e.code}:{e.msg.len()}\"); }\n\
             \x20           else { println(\"c:BAD\"); }\n\
             \x20       }\n\
             \x20   }\n\
             \x20   match g1(n + 1000i64) {\n\
             \x20       Result.Ok(v) => { println(f\"ok{v}\"); }\n\
             \x20       Result.Err(_) => { println(\"d:wild\"); }\n\
             \x20   }\n\
             \x20   let r: Result[i64, One] = g1(n + 10000i64);\n\
             \x20   match r {\n\
             \x20       Result.Ok(v) => { println(f\"ok{v}\"); }\n\
             \x20       Result.Err(e) => {\n\
             \x20           if e.msg.contains(digits(n + 10000i64)) { println(f\"e:{e.msg.len()}\"); }\n\
             \x20           else { println(\"e:BAD\"); }\n\
             \x20       }\n\
             \x20   }\n\
             \x20   match g1(n + 100000i64) {\n\
             \x20       Result.Ok(v) => { println(f\"ok{v}\"); }\n\
             \x20       Result.Err(e) => {\n\
             \x20           let m: String = e.msg;\n\
             \x20           if m.contains(digits(n + 100000i64)) { println(f\"f:{m.len()}\"); }\n\
             \x20           else { println(\"f:BAD\"); }\n\
             \x20       }\n\
             \x20   }\n\
             \x20   println(\"end\");\n\
             }\n",
        ) else {
            return;
        };
    assert_eq!(out, "a:unused\nb:14\nc:101:15\nd:wild\ne:17\nf:18\nend\n");
}

/// B-2026-08-04-5 — destructuring a heap-BOXED `Option`/`Result` payload
/// with a STRUCT sub-pattern must debox first.
///
/// `struct Full { name: String, buf: Vec[i64] }` is 6 words, so it is
/// heap-boxed at `Option`'s 3-word payload area. `Some(Full { name, buf })`
/// used to CRASH the compiler with `ExtractOutOfRange`: the resolvers that
/// size the payload search variant names across every known enum, and the
/// prelude's `enum ChannelError { Full, .. }` claimed the bare path. Both
/// sizing arms then missed `enum_layouts["ChannelError"]`-shaped answers
/// and fell to their 1-word / i64 defaults, so the debox predicate
/// (`want > field_words.len()`, 1 > 3) was false and the struct bind arm
/// extracted field 1 out of a `{ i64 }` aggregate.
///
/// The name is `Full` on purpose — renaming the struct is the one-line
/// bisect, so a rename here would silently retire the test.
#[test]
fn e2e_boxed_optres_payload_struct_destructure_deboxes() {
    // (a) named scrutinee, (b) fresh temp, (c) `Result`'s Err half,
    // (d) PARTIAL destructure, (e) if-let, (f) while-let over `Vec.pop`,
    // (g) the INLINE control — a 3-word payload that is never boxed, and
    // whose path already worked, so the fix must leave it byte-identical.
    let Some(out) = run_program(
            "struct Full { name: String, buf: Vec[i64] }\n\
             struct Narrow { name: String }\n\
             fn mk(i: i64) -> Full {\n\
             \x20   let mut v: Vec[i64] = Vec.new();\n\
             \x20   v.push(i);\n\
             \x20   return Full { name: f\"pay-{i}\", buf: v };\n\
             }\n\
             fn opt(i: i64) -> Option[Full] { return Option.Some(mk(i)); }\n\
             fn res(i: i64) -> Result[i64, Full] { return Result.Err(mk(i)); }\n\
             fn main() {\n\
             \x20   let o: Option[Full] = Option.Some(mk(1i64));\n\
             \x20   match o {\n\
             \x20       Option.Some(Full { name, buf }) => { println(f\"a:{name}:{buf.len()}\"); }\n\
             \x20       Option.None => { println(\"a:none\"); }\n\
             \x20   }\n\
             \x20   match opt(2i64) {\n\
             \x20       Option.Some(Full { name, buf }) => { println(f\"b:{name}:{buf.len()}\"); }\n\
             \x20       Option.None => { println(\"b:none\"); }\n\
             \x20   }\n\
             \x20   match res(3i64) {\n\
             \x20       Result.Ok(v) => { println(f\"c:ok{v}\"); }\n\
             \x20       Result.Err(Full { name, buf }) => { println(f\"c:{name}:{buf.len()}\"); }\n\
             \x20   }\n\
             \x20   let o4: Option[Full] = Option.Some(mk(4i64));\n\
             \x20   match o4 {\n\
             \x20       Option.Some(Full { name, buf: _ }) => { println(f\"d:{name}\"); }\n\
             \x20       Option.None => { println(\"d:none\"); }\n\
             \x20   }\n\
             \x20   let o5: Option[Full] = Option.Some(mk(5i64));\n\
             \x20   if let Option.Some(Full { name, buf }) = o5 {\n\
             \x20       println(f\"e:{name}:{buf.len()}\");\n\
             \x20   }\n\
             \x20   let mut v6: Vec[Full] = Vec.new();\n\
             \x20   v6.push(mk(6i64));\n\
             \x20   while let Option.Some(Full { name, buf }) = v6.pop() {\n\
             \x20       println(f\"f:{name}:{buf.len()}\");\n\
             \x20   }\n\
             \x20   let o7: Option[Narrow] = Option.Some(Narrow { name: f\"nar-{7i64}\" });\n\
             \x20   match o7 {\n\
             \x20       Option.Some(Narrow { name }) => { println(f\"g:{name}\"); }\n\
             \x20       Option.None => { println(\"g:none\"); }\n\
             \x20   }\n\
             \x20   println(\"end\");\n\
             }\n",
        ) else {
            return;
        };
    assert_eq!(
        out,
        "a:pay-1:1\nb:pay-2:1\nc:pay-3:1\nd:pay-4\n\
             e:pay-5:1\nf:pay-6:1\ng:nar-7\nend\n"
    );
}

/// B-2026-08-04-5, the general hazard behind the ICE — a user struct whose
/// name is also some enum's variant name must still destructure as itself,
/// and the enum's variant must still win when the match really is over the
/// enum.
///
/// The collision resolver has three tiers and all three are exercised:
/// `s` is a plain `Full` (the struct must win over `Holder.Full`, which is
/// what the fix redirects); `h` is a `Holder` matched by the BARE variant
/// name (the scrutinee hint must still win, or the fix would over-reach and
/// turn every shadowed variant pattern into a struct destructure); `h2`
/// uses the QUALIFIED spelling, which was never ambiguous.
///
/// Before the fix `s`'s arm tag-compared the struct's first field against
/// `Holder.Full`'s tag, fell through every arm, and `main` returned junk.
#[test]
fn e2e_struct_pattern_wins_over_a_same_named_enum_variant() {
    let Some(out) = run_program(
        "struct Full { a: i64 }\n\
             enum Holder { Full { a: i64 }, Nothing }\n\
             fn main() {\n\
             \x20   let s: Full = Full { a: 11i64 };\n\
             \x20   match s {\n\
             \x20       Full { a } => { println(f\"s:{a}\"); }\n\
             \x20   }\n\
             \x20   let h: Holder = Holder.Full { a: 22i64 };\n\
             \x20   match h {\n\
             \x20       Full { a } => { println(f\"v:{a}\"); }\n\
             \x20       Nothing => { println(\"v:none\"); }\n\
             \x20   }\n\
             \x20   let h2: Holder = Holder.Nothing;\n\
             \x20   match h2 {\n\
             \x20       Holder.Full { a } => { println(f\"q:{a}\"); }\n\
             \x20       Holder.Nothing => { println(\"q:none\"); }\n\
             \x20   }\n\
             \x20   println(\"end\");\n\
             }\n",
    ) else {
        return;
    };
    assert_eq!(out, "s:11\nv:22\nq:none\nend\n");
}

/// B-2026-07-30-11 (match-arm leg) — a match arm binding that receives a
/// MOVED enum payload runs the payload's user `impl Drop` body at the
/// binding's NLL end, on every backend and for both block and
/// bare-expression arm bodies. Before this leg the consuming arm
/// retracted the scrutinee's payload walk (correct) but the arm binding
/// registered nothing, so the moved payload's body ran NOWHERE — the
/// exact hole the B-2026-07-30-11 ledger entry tracked as "the match arm
/// binding that receives a moved payload".
///
/// Twin of `tests/interpreter.rs`'s
/// `test_match_arm_moved_payload_runs_drop_body`.
#[test]
fn e2e_match_arm_moved_payload_runs_drop_body() {
    let Some(out) = run_program(
        "struct Res { id: i64 }\n\
             impl Drop for Res {\n\
             \x20   fn drop(mut ref self) {\n\
             \x20       println(f\"drop {self.id}\")\n\
             \x20   }\n\
             }\n\
             enum Box2 { Full(Res), Empty }\n\
             fn main() {\n\
             \x20   let b = Box2.Full(Res { id: 4 });\n\
             \x20   match b {\n\
             \x20       Box2.Full(r) => { println(f\"arm sees {r.id}\"); }\n\
             \x20       Box2.Empty => {}\n\
             \x20   }\n\
             \x20   println(\"between\");\n\
             \x20   let c = Box2.Full(Res { id: 9 });\n\
             \x20   match c {\n\
             \x20       Box2.Full(r) => println(r.id),\n\
             \x20       Box2.Empty => {}\n\
             \x20   }\n\
             \x20   println(\"end\");\n\
             }\n",
    ) else {
        return;
    };
    assert_eq!(out, "arm sees 4\ndrop 4\nbetween\n9\ndrop 9\nend\n");
}

/// B-2026-07-30-11 (discarded-temp leg) — `let _ = <owned temp>;` runs
/// the discarded value's user Drop work at the `;`, for every fresh
/// shape: struct literal, user-fn call, tuple temp, Option ctor, and
/// Option-returning call. Bare-call discard (s3) was the only shape
/// that fired before this leg. Twin of `tests/interpreter.rs`'s
/// `test_wildcard_let_discard_runs_drop_bodies` on identical source.
#[test]
fn e2e_wildcard_let_discard_runs_drop_bodies() {
    let Some(out) = run_program(
        "struct Res { id: i64 }\n\
             impl Drop for Res {\n\
             \x20   fn drop(mut ref self) {\n\
             \x20       println(f\"drop {self.id}\")\n\
             \x20   }\n\
             }\n\
             fn mk(n: i64) -> Res {\n\
             \x20   Res { id: n }\n\
             }\n\
             fn mkopt(n: i64) -> Option[Res] {\n\
             \x20   Option.Some(Res { id: n })\n\
             }\n\
             fn main() {\n\
             \x20   println(\"s1\");\n\
             \x20   let _ = Res { id: 1 };\n\
             \x20   println(\"s2\");\n\
             \x20   let _ = mk(2);\n\
             \x20   println(\"s3\");\n\
             \x20   mk(3);\n\
             \x20   println(\"s4\");\n\
             \x20   let _ = (Res { id: 4 }, 40);\n\
             \x20   println(\"s5\");\n\
             \x20   let _ = Option.Some(Res { id: 5 });\n\
             \x20   println(\"s6\");\n\
             \x20   let _ = mkopt(6);\n\
             \x20   println(\"end\");\n\
             }\n",
    ) else {
        return;
    };
    assert_eq!(
        out,
        "s1\ndrop 1\ns2\ndrop 2\ns3\ndrop 3\ns4\ndrop 4\ns5\ndrop 5\ns6\ndrop 6\nend\n"
    );
}

/// B-2026-07-30-11 (discarded-temp leg, place-shape pins) — discard
/// shapes that move an EXISTING binding must fire its body exactly
/// once: a moved binding in a tuple (`let _ = (r, 1)` — the tuple gate
/// declines, the source's own slot fires), a binding moved into an
/// Option ctor (`Option.Some(s)` — the ctor move retracts the source,
/// the discard walk is the single fire), an identifier-field literal
/// (`W { r: r0 }`), a bare moved identifier (`let _ = t`), and a
/// scalar-place field (`Res { id: k }` — `k` is a copy, so the temp
/// registers and fires). Twin of `tests/interpreter.rs`'s
/// `test_wildcard_let_discard_place_shapes_single_fire`.
#[test]
fn e2e_wildcard_let_discard_place_shapes_single_fire() {
    let Some(out) = run_program(
        "struct Res { id: i64 }\n\
             struct W { r: Res }\n\
             impl Drop for Res {\n\
             \x20   fn drop(mut ref self) {\n\
             \x20       println(f\"drop {self.id}\")\n\
             \x20   }\n\
             }\n\
             fn main() {\n\
             \x20   let r = Res { id: 31 };\n\
             \x20   println(\"a\");\n\
             \x20   let _ = (r, 1);\n\
             \x20   println(\"b\");\n\
             \x20   let s = Res { id: 32 };\n\
             \x20   let _ = Option.Some(s);\n\
             \x20   println(\"c\");\n\
             \x20   let r0 = Res { id: 33 };\n\
             \x20   let _ = W { r: r0 };\n\
             \x20   println(\"d\");\n\
             \x20   let t = Res { id: 34 };\n\
             \x20   let _ = t;\n\
             \x20   println(\"e\");\n\
             \x20   let k = 5;\n\
             \x20   let _ = Res { id: k };\n\
             \x20   println(\"end\");\n\
             }\n",
    ) else {
        return;
    };
    assert_eq!(
        out,
        "a\ndrop 31\nb\ndrop 32\nc\ndrop 33\nd\ndrop 34\ne\ndrop 5\nend\n"
    );
}

/// B-2026-07-30-11 (if-let leg) — an `if let` arm binding that receives
/// a MOVED Drop-bearing enum payload runs the payload's Drop body at
/// arm end, exactly like the equivalent `match` arm: owned-binding and
/// fresh-temp scrutinees, a miss edge that fires nothing, and a body
/// that moves the binding out still fires exactly once. Twin of
/// `tests/interpreter.rs`'s `test_if_let_moved_payload_runs_drop_body`.
#[test]
fn e2e_if_let_moved_payload_runs_drop_body() {
    let Some(out) = run_program(
        "struct Res { id: i64 }\n\
             impl Drop for Res {\n\
             \x20   fn drop(mut ref self) {\n\
             \x20       println(f\"drop {self.id}\")\n\
             \x20   }\n\
             }\n\
             enum Box2 { Full(Res), Empty }\n\
             fn mkbox(n: i64) -> Box2 {\n\
             \x20   Box2.Full(Res { id: n })\n\
             }\n\
             fn take_res(r: Res) {\n\
             \x20   println(f\"consumed {r.id}\")\n\
             }\n\
             fn main() {\n\
             \x20   let b = Box2.Full(Res { id: 33 });\n\
             \x20   println(\"a\");\n\
             \x20   if let Box2.Full(r) = b {\n\
             \x20       println(f\"arm sees {r.id}\");\n\
             \x20   }\n\
             \x20   println(\"b\");\n\
             \x20   if let Box2.Full(r) = mkbox(34) {\n\
             \x20       println(f\"arm sees {r.id}\");\n\
             \x20   }\n\
             \x20   println(\"c\");\n\
             \x20   let e = Box2.Empty;\n\
             \x20   if let Box2.Full(r) = e {\n\
             \x20       println(f\"arm sees {r.id}\");\n\
             \x20   } else {\n\
             \x20       println(\"empty\");\n\
             \x20   }\n\
             \x20   println(\"d\");\n\
             \x20   let c = Box2.Full(Res { id: 35 });\n\
             \x20   if let Box2.Full(r) = c {\n\
             \x20       take_res(r);\n\
             \x20       println(\"after move\");\n\
             \x20   }\n\
             \x20   println(\"end\");\n\
             }\n",
    ) else {
        return;
    };
    assert_eq!(
        out,
        "a\narm sees 33\ndrop 33\nb\narm sees 34\ndrop 34\nc\nempty\nd\n\
             consumed 35\nafter move\ndrop 35\nend\n"
    );
}

/// B-2026-09-02-40 — a `let`-destructure whose source is a FIELD CHAIN rooted at
/// an owned param (`let (r, k) = h.pe;`) binds views, exactly as the bare-param
/// spelling does since B-2026-09-02-25.
///
/// `plain` ran `b12 dR12 dR12` where one body is due, AGREED on all four
/// surfaces — and the agreement is what made it worth its own row rather than a
/// one-sided patch. Codegen's `owner_runs_bodies` already answered yes for
/// `h.pe` (`place_root_ident` walks to the param `h`); its marking site was
/// narrowed to `ExprKind::Identifier` for the sole purpose of matching the
/// interpreter's gate, which bailed outright on a non-identifier RHS. So the fix
/// is one rule taught to both sides: the source may be a bare parameter name OR
/// a pure `FieldAccess` chain rooted at one.
///
/// LIFTED BOTH SIDES TOGETHER, because either alone is a divergence — the row's
/// own opener measured that dropping codegen's gate by itself takes `plain` to
/// one body compiled and leaves the interpreter at two.
///
/// `ownstr` IS THE CELL THAT REFUTED AN ASSUMPTION, and it is here for that
/// reason. `finish_place_source_tuple_destructure` opens with an
/// `owned_struct_params` bail, so a param whose struct carries a direct
/// `Vec`/`VecDeque`/`String` field looked like it could never reach codegen's
/// marking site — which would have made marking it interpreter-side a
/// run-vs-build split. An exclusion mirroring that bail was written and
/// measured: the COMPILED backends went to one body while the excluded
/// interpreter stayed at two, i.e. the bail does not fire for this shape and the
/// guard caused the exact divergence it was added to prevent. It was removed.
/// This cell keeps that refutation standing.
///
/// `rebind` — `let h2 = h; let (r, k) = h2.pe;` — WAS the pinned-at-two cell
/// and is FIXED SINCE, by B-2026-09-02-44. It rooted at `h2`, which is not a
/// parameter, so codegen's `owner_runs_bodies` said no and its leaf took the
/// body. The interpreter had already retracted its own slots for an inherited
/// root, so the two were not merely agreed-wrong here: the same shape without
/// the trailing `let m = r;` was a live run-vs-build split (one body
/// interpreted, two on all three compiled surfaces) inside a row filed as
/// agreed. Teaching `owner_runs_bodies` to accept a `param_view_locals` root
/// closed the split and this cell together, in one commit with the
/// interpreter's matching widening.
///
/// `norebind` and `refparam` are the over-reach controls, and they fail in
/// opposite directions: withholding a body too eagerly shows up as `norebind`
/// running NONE, and `refparam` (a borrowed receiver the caller still owns) must
/// never gain one.
///
/// B-2026-09-03-15 — an `Option[T]` ELEMENT of a destructured tuple owns its
/// payload's `Drop` body.
///
/// `place_source_tuple_leaf_cleanups` classified leaves as ENUM or nested
/// STRUCT, and its `is_enum` test excluded the names `Option` and `Result`
/// outright while `is_struct` never matched them — so the leaf fell through
/// the `continue` under that gate: no cleanup registered, no cap-zero, and
/// the payload's body owned by NOBODY. The undestructured control (`ctl`) is
/// what proves the body was owed: the same tuple, not taken apart, runs it.
///
/// The cells span both slot positions and both sources, because the defect
/// did too. `loc1` / `loc0` are a bare LOCAL tuple, where nothing else could
/// run the body and it simply vanished on all four surfaces. `proj` is the
/// projection source, where the compiled backends printed the body anyway —
/// incidentally, out of the source struct's own walk, which is why it lands
/// BEFORE the live read — while `--interp` lost it: an agreed defect and a
/// run-vs-build split from one cause. `used` is the cell that shows this is
/// not only about unused leaves: even a leaf the program `match`es and binds
/// lost the body on the compiled side, because an unregistered leaf has
/// nothing for the arm to consume.
///
/// `annres` NO LONGER PINS A GAP — B-2026-09-03-22 closed it, and this line
/// records the deliberate change the old pin demanded. The cell used to
/// assert that a `Result` leaf ran NO payload body on any surface, because
/// the body half generalized while the memory half did not: taking the leaf
/// gave it the body and leaked 272 bytes in 9 allocations.
/// `track_inline_result_agg_payload_var` is the missing owner, and with it
/// the leaf takes the body on every surface — `dR77/t77` is the line that
/// changed here. The consuming-arm and whole-move interactions it needs are
/// pinned in `e2e_result_agg_destructure_leaf_owns_its_payload_body` and its
/// ASAN sibling rather than in this fixture.
///
/// Twin of `tests/interpreter.rs`'s
/// `test_tuple_destructure_optres_leaf_owns_its_payload_body`, pinned to the
/// same string.
#[test]
fn e2e_tuple_destructure_optres_leaf_owns_its_payload_body() {
    let Some(out) = run_program(
        r#"struct R { id: i64, tag: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}/{self.tag}") } }
struct Ho { pe: (R, Option[R]) }

fn mk(id: i64) -> R { return R { id: id, tag: f"t{id}" } }

fn loc1() { let t = (mk(1), Option.Some(mk(11))); let (r, o) = t; println(f"  rd{r.id}") }
fn loc0() { let t = (Option.Some(mk(20)), 0);     let (o, k) = t; println("  rd0") }
fn ctl()  { let t = (mk(3), Option.Some(mk(33))); println(f"  rd{t.0.id}") }
fn proj() { let h = Ho { pe: (mk(4), Option.Some(mk(44))) }; let (r, o) = h.pe; println(f"  rd{r.id}") }
fn used() { let t = (mk(5), Option.Some(mk(55)));
            let (r, o) = t;
            match o { Option.Some(x) => println(f"  got{x.id}"), Option.None => println("  none") }
            println(f"  rd{r.id}") }
fn none() { let n: Option[R] = Option.None; let t = (mk(6), n); let (r, o) = t; println(f"  rd{r.id}") }
fn annres() { let t: (R, Result[R, String]) = (mk(7), Result[R, String].Ok(mk(77)));
              let (r, o) = t; println(f"  rd{r.id}") }

fn main() {
    println("loc1");   loc1()
    println("loc0");   loc0()
    println("ctl");    ctl()
    println("proj");   proj()
    println("used");   used()
    println("none");   none()
    println("annres"); annres()
    println("done")
}
"#,
    ) else {
        return;
    };
    assert_eq!(
        out,
        r#"loc1
dR11/t11
  rd1
dR1/t1
loc0
dR20/t20
  rd0
ctl
  rd3
dR3/t3
dR33/t33
proj
dR44/t44
  rd4
dR4/t4
used
  got55
dR55/t55
  rd5
dR5/t5
none
  rd6
dR6/t6
annres
dR77/t77
  rd7
dR7/t7
done
"#
    );
}

/// B-2026-09-03-24 — a destructured STRUCT's `Option` FIELD leaf owns its
/// payload's `Drop` body. The struct-field spelling of what B-2026-09-03-15
/// fixed at the tuple leaf, and it needed its own pair of halves.
///
///     struct Ho2 { a: R, b: Option[R] }
///     let h = Ho2 { a: mk(1), b: Option.Some(mk(101)) };
///     let Ho2 { a, b } = h;        -> dR1 only; dR101 NOWHERE
///
/// The mechanism is the pair of steps that ALREADY worked, with nothing
/// joining them: `option_field_agg_drop_ok`'s arm hands the leaf the
/// payload's MEMORY and `zero_struct_field_move_cap` zeroes the SOURCE's
/// tag, so the source's field-bodies walk reaches a `None` and prints
/// nothing — while the leaf was never given a bodies walker. That is why
/// the body vanished rather than doubling, and why `ctl` (the same struct
/// left undestructured, which runs both bodies) is what proves it was owed.
///
/// THE CELLS SPAN EVERY SOURCE, because the defect did. `loc` is a place
/// source, `lit` a fresh struct literal and `call` a fresh call — all three
/// lost the body on ALL FOUR surfaces, and the filing row recorded only the
/// first. `ren` is the renaming spelling (`Ho2 { a: p, b: q }`), which the
/// interpreter half has to reach through a different pattern node than the
/// shorthand. `used` is the cell that shows this is not only about unused
/// leaves: a leaf the program `match`es and BINDS lost the body on the
/// compiled side alone — an unregistered leaf leaves the arm nothing to
/// consume — so that cell was a run-vs-build split where the others were
/// agreed defects.
///
/// `param` IS THE CONTROL THAT CONSTRAINS THE FIX, not decoration. A
/// by-value param already ran both bodies correctly on all four surfaces
/// before this change — measured with NO destructure in the function at
/// all, so it is the entry copy's own walk producing them, not the
/// destructure — and registering the leaf there as well took the cell from
/// one body to two. `owner_runs_bodies` is what keeps it at one.
///
/// `none` and `ostr` pin the two self-declining shapes: a `None` payload has
/// no body to run, and an `Option[String]` payload has no user `Drop` at
/// all, so the walker emitter declines it and the cell keeps its previous
/// behaviour.
///
/// NO `Result` CELL, deliberately, and the reason is a measurement rather
/// than the tuple row's. B-2026-09-03-15 left `Result` alone because its
/// memory half has no peer, recording that the spelling stayed "agreed on
/// all four surfaces". That is true of the TUPLE leaf and FALSE of this
/// one: `struct HoRes { a: R, b: Result[R, String] }` destructured runs a
/// HUSK body — `dR0/`, a `Drop` body reading a zeroed object — on jit / aot
/// / auto-par-off and nothing on `--interp`, with the real `dR109` running
/// nowhere on either side. That reproduces in a module containing ONLY that
/// shape, and reproduces identically on the pre-fix compiler, so it is
/// neither this fix's doing nor curable by it. It is also why the `R` here
/// is TWO fields: the husk fires for `R { id: i64, tag: String }` and NOT
/// for `R { id: i64, tag: String, xs: Vec[i64] }`, so a probe of the
/// `Result` gap can come back clean purely by picking the wider payload.
/// Putting the cell here would
/// have forced two different expected strings for one program and pinned a
/// husk as if it were intended; it is filed as its own row instead.
///
/// Twin of `tests/interpreter.rs`'s
/// `test_struct_field_destructure_option_leaf_owns_its_payload_body`,
/// pinned to the same string.
#[test]
fn e2e_struct_field_destructure_option_leaf_owns_its_payload_body() {
    let Some(out) = run_program(
        r#"struct R { id: i64, tag: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}/{self.tag}") } }
struct Ho2 { a: R, b: Option[R] }
struct HoS { a: R, b: Option[String] }

fn mk(id: i64) -> R { return R { id: id, tag: f"t{id}" } }
fn mkho(n: i64) -> Ho2 { return Ho2 { a: mk(n), b: Option.Some(mk(n + 100)) } }

fn loc()  { let h = Ho2 { a: mk(1), b: Option.Some(mk(101)) }; let Ho2 { a, b } = h; println(f"  rd{a.id}") }
fn lit()  { let Ho2 { a, b } = Ho2 { a: mk(2), b: Option.Some(mk(102)) }; println(f"  rd{a.id}") }
fn call() { let Ho2 { a, b } = mkho(3); println(f"  rd{a.id}") }
fn ctl()  { let h = Ho2 { a: mk(4), b: Option.Some(mk(104)) }; println(f"  rd{h.a.id}") }
fn param(h: Ho2) { let Ho2 { a, b } = h; println(f"  rd{a.id}") }
fn used() { let h = Ho2 { a: mk(6), b: Option.Some(mk(106)) }; let Ho2 { a, b } = h;
            match b { Option.Some(x) => println(f"  got{x.id}"), Option.None => println("  none") }
            println(f"  rd{a.id}") }
fn none() { let h = Ho2 { a: mk(7), b: Option.None }; let Ho2 { a, b } = h; println(f"  rd{a.id}") }
fn ostr() { let h = HoS { a: mk(8), b: Option.Some(f"s8") }; let HoS { a, b } = h; println(f"  rd{a.id}/{b.unwrap_or(f"-")}") }
fn ren()  { let h = Ho2 { a: mk(10), b: Option.Some(mk(110)) }; let Ho2 { a: p, b: q } = h; println(f"  rd{p.id}") }

fn main() {
    println("loc");   loc()
    println("lit");   lit()
    println("call");  call()
    println("ctl");   ctl()
    println("param"); param(mkho(5))
    println("used");  used()
    println("none");  none()
    println("ostr");  ostr()
    println("ren");   ren()
    println("done")
}
"#,
    ) else {
        return;
    };
    assert_eq!(
        out,
        r#"loc
dR101/t101
  rd1
dR1/t1
lit
dR102/t102
  rd2
dR2/t2
call
dR103/t103
  rd3
dR3/t3
ctl
  rd4
dR104/t104
dR4/t4
param
  rd5
dR105/t105
dR5/t5
used
  got106
dR106/t106
  rd6
dR6/t6
none
  rd7
dR7/t7
ostr
  rd8/s8
dR8/t8
ren
dR110/t110
  rd10
dR10/t10
done
"#
    );
}

/// B-2026-09-04-1's probe (B-2026-09-03-22 follow-up) — a `Result` tuple
/// destructure leaf whose consuming arm MOVES the binding out.
///
/// The B-2026-09-03-22 suppressor zeroed one word of the leaf's payload — the
/// box pointer, for the seven-word payload its own fixture used — and `R { id,
/// tag: String }` is four words, laid inline, so `id` was cleared and the
/// String's `{ptr,len,cap}` stayed live: `call`, `inner` and `esc` all aborted
/// with glibc's double-free on an ordinary build; `read` was clean only because
/// the borrow classifier skips the suppressor for a read-only arm. `errc` is
/// the `Err` side; the `w*` cells are the boxed twin, which was always correct
/// and pins that the wider zero does not disturb it.
#[test]
fn e2e_result_agg_leaf_moving_arm_zeroes_the_whole_payload() {
    let src = r#"struct R { id: i64, tag: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}/{self.tag}") } }
fn mk(n: i64) -> R { return R { id: n, tag: f"t{n}" }; }
struct W { id: i64, x: String, y: String }
impl Drop for W { fn drop(mut ref self) { println(f"dW{self.id}/{self.x}{self.y}") } }
fn mkw(n: i64) -> W { return W { id: n, x: f"x{n}", y: f"y{n}" }; }
fn eat(x: R) { println(f"  eat{x.id}") }
fn eatw(w: W) { println(f"  eatw{w.id}") }

fn read()   { let t: (R, Result[R, String]) = (mk(1), Result.Ok(mk(101))); let (a, b) = t;
              match b { Result.Ok(r) => println(f"  ok{r.id}"), Result.Err(e) => println(f"  er{e}") } }
fn call()   { let t: (R, Result[R, String]) = (mk(2), Result.Ok(mk(102))); let (a, b) = t;
              match b { Result.Ok(r) => eat(r), Result.Err(e) => println(f"  er{e}") } }
fn inner()  { let t: (R, Result[R, String]) = (mk(3), Result.Ok(mk(103))); let (a, b) = t;
              match b { Result.Ok(r) => { let g = r; println(f"  got{g.id}") }, Result.Err(e) => println(f"  er{e}") } }
fn esc()    { let t: (R, Result[R, String]) = (mk(4), Result.Ok(mk(104))); let (a, b) = t;
              let g = match b { Result.Ok(r) => r, Result.Err(_) => mk(0) }; println(f"  got{g.id}") }
fn errc()   { let t: (R, Result[String, R]) = (mk(5), Result.Err(mk(105))); let (a, b) = t;
              match b { Result.Ok(s) => println(f"  ok{s}"), Result.Err(r) => eat(r) } }
fn wread()  { let t: (R, Result[W, String]) = (mk(6), Result.Ok(mkw(106))); let (a, b) = t;
              match b { Result.Ok(w) => println(f"  ok{w.id}"), Result.Err(e) => println(f"  er{e}") } }
fn winner() { let t: (R, Result[W, String]) = (mk(7), Result.Ok(mkw(107))); let (a, b) = t;
              match b { Result.Ok(w) => { let g = w; println(f"  got{g.id}") }, Result.Err(e) => println(f"  er{e}") } }
fn wesc()   { let t: (R, Result[W, String]) = (mk(8), Result.Ok(mkw(108))); let (a, b) = t;
              let g = match b { Result.Ok(w) => w, Result.Err(_) => mkw(0) }; println(f"  got{g.id}") }

fn main() {
  println("read");   read()
  println("call");   call()
  println("inner");  inner()
  println("esc");    esc()
  println("errc");   errc()
  println("wread");  wread()
  println("winner"); winner()
  println("wesc");   wesc()
  println("done")
}
"#;
    assert_eq!(
        run_program(src).as_deref(),
        Some(
            r#"read
dR1/t1
  ok101
dR101/t101
call
dR2/t2
  eat102
dR102/t102
inner
dR3/t3
  got103
dR103/t103
esc
dR4/t4
  got104
dR104/t104
errc
dR5/t5
  eat105
dR105/t105
wread
dR6/t6
  ok106
dW106/x106y106
winner
dR7/t7
  got107
dW107/x107y107
wesc
dR8/t8
  got108
dW108/x108y108
done
"#
        )
    );
}

/// B-2026-09-03-22 — A `Result[O, E]` DESTRUCTURE LEAF OWNS ITS PAYLOAD'S
/// `Drop` BODY.
///
/// The row split out of B-2026-09-03-15 when its `Option` half landed: the
/// BODY half already generalized
/// (`emit_optres_payload_user_drop_bodies_fn` has a `Result` arm) and the
/// cap-zero half already did (B-2026-08-03-3), but the MEMORY half had no
/// peer — `track_inline_option_agg_payload_var` is `Option`-only and
/// `track_inline_result_payload_var` self-skips a heap-boxed payload — so
/// taking the leaf ran the due body and leaked 272 bytes in 9 allocations.
/// `track_inline_result_agg_payload_var` is that peer.
///
/// IT WAS AGREED-WRONG, NOT SPLIT, on four of these cells, which is why no
/// A/B gate saw it and why `annres` was pinned at the wrong output in
/// `e2e_tuple_destructure_optres_leaf_owns_its_payload_body` for a fix to
/// change deliberately. That line is changed there; this fixture is the
/// positive pin.
///
/// TEN LINES WERE LOST ON THE COMPILED BACKENDS and eight on the
/// interpreter, against the one cell the row recorded. `annres` is the
/// row's own; `unann` drops the binding annotation, `fresh` destructures
/// the literal in place, `errside` puts the payload on `Err`, `loopr` runs
/// it twice and `nested` reaches the leaf through a nested pattern — all
/// eight shared by both backends. `consok` and `conserr` are the extra two
/// the compiled side lost: an arm that BINDS the payload out, where the
/// interpreter was already right.
///
/// THE CONSUMING AND MOVING CELLS ARE THE POINT OF THE MEMORY HALF, not
/// decoration. `Result` has no empty tag to store — `Ok` and `Err` both
/// name a live variant — so its consume suppressor zeroes the PAYLOAD area
/// and leans on the emitted drop's null-box guard, where the `Option` twin
/// stores `None`. `moved`, `ret` and `field` exercise the transfer disarm
/// on the same set, and `arg` is the counter-case that must NOT be
/// disarmed. Balance for all of them is asserted in
/// `asan_result_agg_destructure_leaf_is_balanced`; the transcript here
/// cannot tell a handed-off box from a leaked one.
///
/// `wildres` (the wildcard leaf B-2026-09-03-39 fixed) and `strres`
/// (`Result[String, String]`, no user `Drop`, nothing owed) are the
/// controls for a body starting where none is due.
///
/// Twin of `tests/interpreter.rs`'s
/// `test_result_agg_destructure_leaf_owns_its_payload_body`, pinned to the
/// same string.
#[test]
fn e2e_result_agg_destructure_leaf_owns_its_payload_body() {
    let src = r#"struct R { id: i64, s: String, xs: Vec[i64] }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}:{self.s}:{self.xs.len()}") } }
struct W { p: Result[R, String] }
fn mk(n: i64) -> R { return R { id: n, s: f"s{n}", xs: [n, n] }; }
fn eat(xa: Result[R, String]) -> i64 { match xa { Result.Ok(ra) => { ra.id }, Result.Err(sa) => { 0 } } }
fn give() -> Result[R, String] { let (_, ob) = (mk(20), Result[R, String].Ok(mk(120))); return ob }

fn annres()  { let tc: (R, Result[R, String]) = (mk(7), Result[R, String].Ok(mk(77)));
               let (rc, oc) = tc; println(f"  a{rc.id}") }
fn unann()   { let td = (mk(1), Result[R, String].Ok(mk(11))); let (rd, od) = td; println(f"  u{rd.id}") }
fn fresh()   { let (re, oe) = (mk(3), Result[R, String].Ok(mk(33))); println(f"  h{re.id}") }
fn errside() { let tf: (R, Result[String, R]) = (mk(5), Result[String, R].Err(mk(55)));
               let (rf, of) = tf; println(f"  r{rf.id}") }
fn consok()  { let (_, og) = (mk(22), Result[R, String].Ok(mk(122)));
               match og { Result.Ok(rg) => { println(f"  k{rg.id}") }, Result.Err(sg) => { println("  e") } } }
fn conserr() { let (_, oh) = (mk(24), Result[String, R].Err(mk(124)));
               match oh { Result.Ok(sh) => { println("  o") }, Result.Err(rh) => { println(f"  c{rh.id}") } } }
fn moved()   { let (_, oi) = (mk(26), Result[R, String].Ok(mk(126))); let qi = oi; println("  m") }
fn ret()     { let zj = give(); println("  t") }
fn arg()     { let (_, ok2) = (mk(28), Result[R, String].Ok(mk(128))); println(f"  g{eat(ok2)}") }
fn field()   { let (_, ol) = (mk(30), Result[R, String].Ok(mk(130))); let wl = W { p: ol }; println("  f") }
fn loopr()   { let mut i = 0; while i < 2 { let (_, om) = (mk(32), Result[R, String].Ok(mk(132))); i = i + 1; } println("  l") }
fn nested()  { let ((_, on), nn) = ((mk(36), Result[R, String].Ok(mk(136))), 4); println(f"  n{nn}") }
fn wildres() { let (ro, _) = (mk(15), Result[R, String].Ok(mk(115))); println(f"  w{ro.id}") }
fn strres()  { let (rp, op) = (mk(17), Result[String, String].Ok(f"p17")); println(f"  s{rp.id}") }

fn main() {
  println("annres");  annres()
  println("unann");   unann()
  println("fresh");   fresh()
  println("errside"); errside()
  println("consok");  consok()
  println("conserr"); conserr()
  println("moved");   moved()
  println("ret");     ret()
  println("arg");     arg()
  println("field");   field()
  println("loopr");   loopr()
  println("nested");  nested()
  println("wildres"); wildres()
  println("strres");  strres()
  println("done")
}
"#;
    assert_eq!(
        run_program(src).as_deref(),
        Some(
            r#"annres
dR77:s77:2
  a7
dR7:s7:2
unann
dR11:s11:2
  u1
dR1:s1:2
fresh
dR33:s33:2
  h3
dR3:s3:2
errside
dR55:s55:2
  r5
dR5:s5:2
consok
dR22:s22:2
  k122
dR122:s122:2
conserr
dR24:s24:2
  c124
dR124:s124:2
moved
dR26:s26:2
dR126:s126:2
  m
ret
dR20:s20:2
dR120:s120:2
  t
arg
dR28:s28:2
  g128
dR128:s128:2
field
dR30:s30:2
dR130:s130:2
  f
loopr
dR32:s32:2
dR132:s132:2
dR32:s32:2
dR132:s132:2
  l
nested
dR36:s36:2
dR136:s136:2
  n4
wildres
dR115:s115:2
  w15
dR15:s15:2
strres
  s17
dR17:s17:2
done
"#
        )
    );
}

/// B-2026-09-04-10 — AN `Option[<struct>]` DESTRUCTURE LEAF THAT IS MOVED
/// WHOLE MUST DISARM ITS SOURCE.
///
/// B-2026-09-03-15 made a tuple destructure leaf the sole owner of its
/// boxed payload: it registers `track_inline_option_agg_payload_var` and
/// cap-zeroes the element in the source. A whole-value MOVE of that leaf
/// then gave the payload two owners, because
/// `suppress_inline_option_result_binding_move_impl` tests membership in
/// three sets — inline Option, inline Result, boxed enum — and the tracker
/// records the binding in a FOURTH, `inline_option_agg_payload_vars`, which
/// nothing there listed. Both slots stayed armed and both freed the box.
///
/// ASSERTED STRICTLY (`assert_eq!(run_program(..), Some(..))`) for the
/// reason `e2e_boxed_enum_param_escape_no_double_free` states: this program
/// does not misprint on the unfixed compiler, it DIES — `free(): double
/// free detected in tcache 2` on the two-cell form, a SIGSEGV on this
/// twelve-cell one, with stdout still buffered so not one line reaches the
/// terminal. `run_program` returns `None` for that, which the tolerant
/// `let Some(out) = .. else { return }` form would swallow as a skip.
///
/// TWO CELLS WERE RED and the rest are controls. `rebind` is the row's
/// `let q = o;` and `ret` its `return o` (through `giveback`, so the move
/// crosses a function boundary). `twohop` chains two rebinds, `loopreb`
/// runs one twice, and `slot0` puts the `Option` in element 0 — all three
/// reach the same disarm.
///
/// THE CONTROLS COVER THE OPPOSITE FAILURE, a body that stops running, and
/// one of them is the reason this fix is not simply "add the fourth set".
/// `arg` passes the leaf to a function: for THIS payload shape that is not
/// a transfer — the caller's slot keeps the payload and runs its body — so
/// admitting the set at the shared entry point silently loses `dR114`
/// (measured). Hence the separate `..._rebind` entry point, with only the
/// rebind and return sites calling it. `matched` (a consuming arm, which
/// has always had its own retraction), `stay` (the leaf never moves),
/// `plain` (a struct element, not an `Option`), `instr`
/// (`Option[String]` — an INLINE payload, so one of the three sets that
/// already worked), `fld` (the STRUCT-FIELD destructure sibling, which
/// registers into `boxed_enum_payload_vars`) and `loc` (a plain `Option`
/// local moved the same two ways) were all correct before and must stay so
/// — they are what isolate the defect to the one set nothing listed.
///
/// EVERY BINDING IN THIS PROGRAM HAS A DISTINCT NAME ON PURPOSE. The
/// payload-ownership side tables are keyed by bare binding name, and at
/// least one of them is not cleared between function bodies, so two
/// same-named bindings in different functions interfere — a defect of its
/// own, filed separately, which would otherwise crash this fixture for a
/// reason that has nothing to do with the row it pins.
///
/// Twin of `tests/interpreter.rs`'s
/// `test_option_agg_destructure_leaf_move_disarms_its_source`, pinned to
/// the same string.
#[test]
fn e2e_option_agg_destructure_leaf_move_disarms_its_source() {
    let src = r#"struct R { id: i64, s: String, xs: Vec[i64] }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}:{self.s}:{self.xs.len()}") } }
struct H { a: R, b: Option[R] }
fn mk(n: i64) -> R { return R { id: n, s: f"s{n}", xs: [n, n] }; }

fn eat(xa: Option[R]) -> i64 { match xa { Option.Some(ra) => { ra.id }, Option.None => { 0 } } }
fn giveback() -> Option[R] { let tb = (mk(3), Option.Some(mk(103))); let (_, ob) = tb; return ob }

fn rebind()  { let tc = (mk(2), Option.Some(mk(102))); let (_, oc) = tc; let qc = oc; println(f"  q{qc.is_some()}") }
fn ret()     { let zd = giveback(); println(f"  z{zd.is_some()}") }
fn twohop()  { let te = (mk(6), Option.Some(mk(106))); let (_, oe) = te; let qe = oe; let we = qe; println(f"  w{we.is_some()}") }
fn loopreb() { let mut i = 0;
               while i < 2 { let tf = (mk(7), Option.Some(mk(107))); let (_, of) = tf; let qf = of; i = i + 1; }
               println("  lr") }
fn slot0()   { let tg = (Option.Some(mk(8)), 5); let (og, kg) = tg; let qg = og; println(f"  k{kg}{qg.is_some()}") }
fn matched() { let th = (mk(4), Option.Some(mk(104))); let (_, oh) = th; match oh { Option.Some(rh) => { println(f"  g{rh.id}") }, Option.None => { println("  n") } } }
fn arg()     { let ti = (mk(14), Option.Some(mk(114))); let (_, oi) = ti; println(f"  e{eat(oi)}") }
fn stay()    { let tj = (mk(5), Option.Some(mk(105))); let (_, oj) = tj; println(f"  y{oj.is_some()}") }
fn plain()   { let (_, ok) = (mk(9), mk(109)); let qk = ok; println(f"  p{qk.id}") }
fn instr()   { let tl = (mk(15), Option.Some(f"p15")); let (_, ol) = tl; let ql = ol; println(f"  i{ql.is_some()}") }
fn fld()     { let hm = H { a: mk(13), b: Option.Some(mk(113)) }; let H { a, b } = hm; let qm = b; println(f"  f{qm.is_some()}") }
fn loc()     { let on = Option.Some(mk(11)); let qn = on; println(f"  o{qn.is_some()}") }

fn main() {
  println("rebind");  rebind()
  println("ret");     ret()
  println("twohop");  twohop()
  println("loopreb"); loopreb()
  println("slot0");   slot0()
  println("matched"); matched()
  println("arg");     arg()
  println("stay");    stay()
  println("plain");   plain()
  println("instr");   instr()
  println("fld");     fld()
  println("loc");     loc()
  println("done")
}
"#;
    assert_eq!(
        run_program(src).as_deref(),
        Some(
            r#"rebind
dR2:s2:2
  qtrue
dR102:s102:2
ret
dR3:s3:2
  ztrue
dR103:s103:2
twohop
dR6:s6:2
  wtrue
dR106:s106:2
loopreb
dR7:s7:2
dR107:s107:2
dR7:s7:2
dR107:s107:2
  lr
slot0
  k5true
dR8:s8:2
matched
dR4:s4:2
  g104
dR104:s104:2
arg
dR14:s14:2
  e114
dR114:s114:2
stay
dR5:s5:2
  ytrue
dR105:s105:2
plain
dR9:s9:2
  p109
dR109:s109:2
instr
dR15:s15:2
  itrue
fld
dR13:s13:2
  ftrue
dR113:s113:2
loc
  otrue
dR11:s11:2
done
"#
        )
    );
}

/// B-2026-09-03-25 — A WILDCARD TUPLE LEAF OVER AN `Option`/`Result`
/// ELEMENT OWNS ITS PAYLOAD'S `Drop` BODY.
///
/// `run_discarded_leaf_user_drop_bodies` picks its walker by the type's
/// NAME, and the built-in `Option`/`Result` carry the payload in a generic
/// ARGUMENT instead. `Option` is even present in `enum_layouts`, so the
/// helper's `enum_payload_ok` answered true for it and then
/// `emit_enum_payload_user_drop_bodies_fn("Option")` found no variant
/// payload to walk and returned `None` — the leaf reported NOTHING and the
/// body was owned by nobody. Same defect B-2026-09-03-15 fixed one arm
/// over, and the same resolution: the walker keyed by the `TypeExpr`.
///
/// FOUR SPELLINGS SHARED THE DECLINE and the row recorded two of them.
/// `wr` and `wboth` are the row's own cells; `w0` puts the `Option` in slot
/// 0; `resw` is the `Result` twin; `nestw` reaches the same helper through
/// the nested-pattern recursion. Pre-fix, this exact program loses
/// `dR110`, `dR112`, `dR14`, `dR140`, `dR170` and both `dR180`s, and
/// nothing else.
///
/// `projw` IS THE CELL THAT LOOKS FIXED AND WAS NOT BROKEN. A projection
/// source printed the body pre-fix anyway — incidentally, out of the source
/// STRUCT's own walk, which is why `dR190` lands BEFORE the live read
/// rather than after it. Reading it as a passing cell pre-fix is the trap
/// the sibling row's `proj` cell documents; it is here so a fix that
/// re-routes it has to move this line deliberately.
///
/// `loopw` RUNS THE DESTRUCTURE TWICE, because a body emitted once for a
/// loop body is a distinct failure from a body emitted never, and a
/// single-iteration cell cannot tell them apart.
///
/// THE CONTROLS COVER THE OPPOSITE FAILURE, which in this family is a body
/// running TWICE rather than zero times: `bind` (the leaf B-2026-09-03-15
/// fixed, through different machinery), `bothst` (two plain struct
/// wildcards — proof the source's own walk does not double-fire here),
/// `undest`, `marm`, `nonew` (a `None` payload, nothing to run), `optstr`
/// (`Option[String]`, an inline payload with no user `Drop`, so nothing is
/// owed) and `i64opt` — `let (_, o) = t;` over `(i64, Option[R])`, the leaf
/// the wildcard arm's own comment cites as one this helper DECLINES. It
/// does not decline it any more, and it was already correct before this
/// fix because the `Option` there is BOUND rather than discarded.
///
/// Every `Drop` body renders `tag`, so a body run against a cap-zeroed husk
/// prints `dR110/` and fails on the transcript instead of passing as a bare
/// body count.
///
/// B-2026-09-03-39 — THE FRESH TUPLE SOURCE, half of the pair this fixture
/// originally recorded as knowingly absent. The WILDCARD leaf agrees on all
/// four surfaces now and is pinned here — the nine `fr*` cells. The BINDING
/// leaf of the same fresh literal is still split and is deliberately still
/// absent, for the reason that kept both out to begin with: the twins share
/// ONE string, so a cell whose backends disagree cannot live in them. Its
/// remedy is measured and rejected rather than merely unattempted — see
/// `tests/memory_sanitizer.rs`'s
/// `asan_fresh_tuple_source_optres_leaf_owns_its_payload_body`.
///
/// A DIFFERENT SITE FROM THE `w*` CELLS ABOVE, despite the identical
/// symptom. There the walker declined; here the walker was never given a
/// type to work from. `infer_arg_elem_te` types a tuple-literal element by
/// resolving its EXPRESSION, and an enum-constructor call falls through
/// every arm to a fallback that rebuilds a `Path` from the NAME alone —
/// `generic_args: None` — so the element reached both leaf walkers as a
/// bare `Option`, which is exactly the argument they key the payload off.
/// `refined_tuple_literal_elem_te` already rebuilt `Option[P]` for the
/// `let`-BINDING spelling of the same literal (B-2026-08-03-1); the
/// DESTRUCTURE spelling simply never asked it.
///
/// SIX CELLS WERE RED, and the row that filed this recorded two of them.
/// `frw` and `frres` are the row's own; `frw0` puts the `Option` in slot 0,
/// `frboth` discards both, `frnest` reaches the nested-pattern recursion,
/// and `frloop` runs it twice. Pre-fix, this exact program loses `dR131`,
/// `dR135`, `dR41`, `dR142`, `dR143` and both `dR144`s, and nothing else —
/// measured against a build of the parent commit.
///
/// `frres` NEEDS ITS ANNOTATION AND THAT IS THE POINT. `Result[R,
/// String].Ok(..)` parses as a METHOD CALL on a type path rather than as a
/// `Call`, and it is the only constructor spelling that names both type
/// arguments — `Ok(x)` alone says nothing about `E`, while the payload
/// walker's `Result` leg reads both. So the annotation is load-bearing
/// here, not decoration.
///
/// `frstr`, `frplain` and `frnone` ARE THE FRESH-SOURCE CONTROLS, and all
/// three were already correct: an inline `Option[String]` payload with no
/// user `Drop` (nothing owed), a plain struct element (proof the leaf arm
/// itself was always reached), and a `None` (nothing to run). They pin the
/// opposite failure — a body that starts running where none is due, or one
/// running twice.
///
/// Twin of `tests/interpreter.rs`'s
/// `test_wildcard_tuple_leaf_over_optres_owns_its_payload_body`, pinned to
/// the same string.
#[test]
fn e2e_wildcard_tuple_leaf_over_optres_owns_its_payload_body() {
    let Some(out) = run_program(
        r#"struct R { id: i64, tag: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}/{self.tag}") } }
struct Ho { pe: (R, Option[R]) }

fn mk(id: i64) -> R { return R { id: id, tag: f"t{id}" } }

fn wr()    { let t = (mk(10), Option.Some(mk(110))); let (r, _) = t; println(f"  rd{r.id}") }
fn wboth() { let t = (mk(12), Option.Some(mk(112))); let (_, _) = t; println("  x") }
fn w0()    { let t = (Option.Some(mk(14)), 5); let (_, k) = t; println(f"  k{k}") }
fn resw()  { let t: (R, Result[R, String]) = (mk(40), Result[R, String].Ok(mk(140)));
             let (r, _) = t; println(f"  rd{r.id}") }
fn nestw() { let t = ((mk(70), Option.Some(mk(170))), 3); let ((_, _), n) = t; println(f"  n{n}") }
fn projw() { let h = Ho { pe: (mk(90), Option.Some(mk(190))) }; let (r, _) = h.pe; println(f"  rd{r.id}") }
fn nonew() { let n: Option[R] = Option.None; let t = (mk(16), n); let (r, _) = t; println(f"  rd{r.id}") }
fn loopw() { let mut i = 0;
             while i < 2 { let t = (mk(80), Option.Some(mk(180))); let (_, _) = t; i = i + 1; }
             println("  x") }
fn bind()  { let t = (mk(21), Option.Some(mk(221))); let (a, b) = t; println(f"  rd{a.id}") }
fn bothst(){ let t = (mk(50), mk(150)); let (_, _) = t; println("  x") }
fn optstr(){ let t = (mk(60), Option.Some(f"x60")); let (r, _) = t; println(f"  rd{r.id}") }
fn undest(){ let t = (mk(6), Option.Some(mk(66))); println(f"  rd{t.0.id}") }
fn marm()  { let t = (mk(5), Option.Some(mk(105))); match t { (a, b) => { println("  m") } } }
fn i64opt(){ let t = (7, Option.Some(mk(77))); let (_, o) = t; println("  z") }

fn frw()   { let (r, _) = (mk(31), Option.Some(mk(131))); println(f"  rd{r.id}") }
fn frres() { let (r, _) = (mk(35), Result[R, String].Ok(mk(135))); println(f"  rd{r.id}") }
fn frw0()  { let (_, r) = (Option.Some(mk(41)), mk(141)); println(f"  rd{r.id}") }
fn frboth(){ let (_, _) = (mk(42), Option.Some(mk(142))); println("  x") }
fn frnest(){ let ((_, _), n) = ((mk(43), Option.Some(mk(143))), 4); println(f"  n{n}") }
fn frloop(){ let mut i = 0;
             while i < 2 { let (_, _) = (mk(44), Option.Some(mk(144))); i = i + 1; }
             println("  x") }
fn frstr() { let (r, _) = (mk(45), Option.Some(f"x45")); println(f"  rd{r.id}") }
fn frplain(){ let (r, _) = (mk(46), mk(146)); println(f"  rd{r.id}") }
fn frnone(){ let n: Option[R] = Option.None; let (r, _) = (mk(47), n); println(f"  rd{r.id}") }

fn main() {
    println("wr");     wr()
    println("wboth");  wboth()
    println("w0");     w0()
    println("resw");   resw()
    println("nestw");  nestw()
    println("projw");  projw()
    println("nonew");  nonew()
    println("loopw");  loopw()
    println("bind");   bind()
    println("bothst"); bothst()
    println("optstr"); optstr()
    println("undest"); undest()
    println("marm");   marm()
    println("i64opt"); i64opt()
    println("frw");     frw()
    println("frres");   frres()
    println("frw0");    frw0()
    println("frboth");  frboth()
    println("frnest");  frnest()
    println("frloop");  frloop()
    println("frstr");   frstr()
    println("frplain"); frplain()
    println("frnone");  frnone()
    println("done")
}
"#,
    ) else {
        return;
    };
    assert_eq!(
        out,
        r#"wr
dR110/t110
  rd10
dR10/t10
wboth
dR12/t12
dR112/t112
  x
w0
dR14/t14
  k5
resw
dR140/t140
  rd40
dR40/t40
nestw
dR70/t70
dR170/t170
  n3
projw
dR190/t190
  rd90
dR90/t90
nonew
  rd16
dR16/t16
loopw
dR80/t80
dR180/t180
dR80/t80
dR180/t180
  x
bind
dR221/t221
  rd21
dR21/t21
bothst
dR50/t50
dR150/t150
  x
optstr
  rd60
dR60/t60
undest
  rd6
dR6/t6
dR66/t66
marm
  m
dR5/t5
dR105/t105
i64opt
dR77/t77
  z
frw
dR131/t131
  rd31
dR31/t31
frres
dR135/t135
  rd35
dR35/t35
frw0
dR41/t41
  rd141
dR141/t141
frboth
dR42/t42
dR142/t142
  x
frnest
dR43/t43
dR143/t143
  n4
frloop
dR44/t44
dR144/t144
dR44/t44
dR144/t144
  x
frstr
  rd45
dR45/t45
frplain
dR146/t146
  rd46
dR46/t46
frnone
  rd47
dR47/t47
done
"#
    );
}

/// B-2026-09-03-33 — a `Result[<struct with a Drop body>, E]` STRUCT-FIELD
/// destructure leaf no longer leaves a HUSK body behind on the compiled
/// backends: `let HoRes { a, b } = h;` over `{ a: R, b: Result[R, String] }`
/// printed `dR0/` — `R`'s own `Drop` body running against a ZEROED object,
/// id 0 and empty tag — on jit/aot/AUTO_PAR=0 and nothing under `--interp`.
///
/// The two wrappers neutralize a moved-out field DIFFERENTLY, and that is
/// the whole defect. `zero_struct_field_move_cap` zeroes an `Option` field's
/// TAG, to `None`, so the source's field-bodies walk skips it; for a
/// `Result` field it can only zero the PAYLOAD AREA, because `Result` has no
/// empty tag to zero to — `Ok` and `Err` both name a live variant. The
/// source's walk therefore still visited the moved-out field, read
/// `tag == Ok`, and ran the payload's body over storage the zero had just
/// emptied. `finish_owned_struct_destructure`'s place-source hand-off could
/// not mask it either: that branch is gated on `leaf_struct_name`, which is
/// `None` for a field whose head is the wrapper rather than a user struct.
///
/// `box` is the control that made this look context-dependent when it is
/// not, and it is pinned for that reason. The discriminator is the PAYLOAD
/// STRUCT'S OWN WIDTH, not anything about the destructure: `R3`'s three
/// fields spill past `Result`'s 5-word inline area, so the payload is
/// heap-BOXED and the source's overlay walk never reached it. The first
/// probe of this shape used a three-field payload and came back agreed on
/// all four surfaces, which reads as "the `Result` gap is silent, as
/// documented" — the reason B-2026-09-03-15's fix prose recorded this
/// deferral as leaving "no body, no leak, agreed on all four surfaces",
/// true of the tuple leaf it measured and false of this one.
///
/// `err` pins that the phantom needed a payload to be PRESENT: an `Err`
/// half agreed on all four surfaces before the fix and still does.
///
/// THE REAL BODY IS STILL NOT RUN, and that is deliberate rather than
/// overlooked: `dR101` appears nowhere in the expected string. Giving the
/// leaf its payload's body is a separate question that has to be settled
/// across every source shape at once, and the three source controls here
/// show why they cannot be settled one at a time — `param` already runs it
/// (`dR107`, on all four surfaces, because the owner's own walk does),
/// while `call` and `lit` run it on none. What this fix removes is the
/// PHANTOM, which is the half that made the two backends disagree; the
/// missing body is agreed-and-absent on both and stays that way.
///
/// `wild` is the control that isolates the fix to the BOUND leaf: a
/// wildcard field is not moved out, the source's walk keeps it, and
/// `dR104` runs on every surface both before and after. `awild` is its
/// mirror — the `a` field wildcarded and `b` bound — which husked before
/// the fix exactly as `loc` did, so the phantom was never about which leaf
/// came first. `nest` moves the destructure into an inner block, where the
/// husk had drained at the block\'s exit rather than at the statement.
///
/// B-2026-09-04-1 — the "missing body is agreed-and-absent on both and stays
/// that way" sentence above is now history: the leaf owns the body on every
/// source shape here (`loc`, `box`, `awild`, `nest`, `call`, `lit` each gain
/// their `dR1xx` line, at the destructure — the unused leaf's last use). The
/// phantom this fixture was written for stays gone, which is what `wild` and
/// `param` (unchanged) still pin. `awild` was pinned per backend (B-2026-09-04-23):
/// both ran `a`'s discard body and `b5`'s body at the same statement, in
/// opposite sequence. B-2026-09-03-32 settled the tie — the discard is the
/// destructure's own destruction and precedes an unread leaf's NLL death —
/// so codegen now prints the interpreter's `dR5 dR105` and the cell is one string.
#[test]
fn e2e_struct_field_destructure_result_leaf_leaves_no_husk_body() {
    let Some(out) = run_program(
        r#"struct R  { id: i64, tag: String }
impl Drop for R  { fn drop(mut ref self) { println(f"dR{self.id}/{self.tag}") } }
fn mk(n: i64) -> R { return R { id: n, tag: f"t{n}" }; }

struct R3 { id: i64, tag: String, xs: Vec[i64] }
impl Drop for R3 { fn drop(mut ref self) { println(f"dQ{self.id}/{self.tag}") } }
fn mk3(n: i64) -> R3 { return R3 { id: n, tag: f"u{n}", xs: [n] }; }

struct HoRes  { a: R,  b: Result[R, String] }
struct HoRes3 { a: R3, b: Result[R3, String] }

fn mkho(n: i64) -> HoRes { return HoRes { a: mk(n), b: Result.Ok(mk(n + 100)) }; }
fn takes(h: HoRes) { let HoRes { a, b } = h; println(f"  p{a.id}") }

fn main() {
    println("loc")
    let h1 = HoRes { a: mk(1), b: Result.Ok(mk(101)) };
    let HoRes { a, b } = h1;
    println(f"  rd{a.id}")

    println("box")
    let h2 = HoRes3 { a: mk3(2), b: Result.Ok(mk3(102)) };
    let HoRes3 { a: a2, b: b2 } = h2;
    println(f"  rd{a2.id}")

    println("err")
    let h3 = HoRes { a: mk(3), b: Result.Err(f"e3") };
    let HoRes { a: a3, b: b3 } = h3;
    println(f"  rd{a3.id}")

    println("wild")
    let h4 = HoRes { a: mk(4), b: Result.Ok(mk(104)) };
    let HoRes { a: a4, b: _ } = h4;
    println(f"  rd{a4.id}")

    println("awild")
    let h5 = HoRes { a: mk(5), b: Result.Ok(mk(105)) };
    let HoRes { a: _, b: b5 } = h5;
    println("  rb5")

    println("nest")
    let h6 = HoRes { a: mk(6), b: Result.Ok(mk(106)) };
    { let HoRes { a: a6, b: b6 } = h6; println(f"  in{a6.id}") }
    println("  outer")

    println("param")
    takes(HoRes { a: mk(7), b: Result.Ok(mk(107)) })

    println("call")
    let HoRes { a: a8, b: b8 } = mkho(8);
    println(f"  rd{a8.id}")

    println("lit")
    let HoRes { a: a9, b: b9 } = HoRes { a: mk(9), b: Result.Ok(mk(109)) };
    println(f"  rd{a9.id}")

    println("done")
}
"#,
    ) else {
        return;
    };
    assert_eq!(
        out,
        r#"loc
dR101/t101
  rd1
dR1/t1
box
dQ102/u102
  rd2
dQ2/u2
err
  rd3
dR3/t3
wild
dR104/t104
  rd4
dR4/t4
awild
dR5/t5
dR105/t105
  rb5
nest
dR106/t106
  in6
dR6/t6
  outer
param
  p7
dR107/t107
dR7/t7
call
dR108/t108
  rd8
dR8/t8
lit
dR109/t109
  rd9
dR9/t9
done
"#
    );
}

/// B-2026-09-04-8 — a tuple destructure's `Result` element keeps its payload's
/// `Drop` body at EVERY projection depth, not just one.
///
/// `let (a, b) = g.h.inner;` ran no `dR103` under `--interp` while all three
/// compiled surfaces ran it. `eval_place_type_name` resolved only a ROOT
/// (`Identifier` / `SelfValue`), so asked about `g.h` it answered `None`,
/// `destructure_source_elem_tes` bailed before naming a single leaf, and
/// `optres_payload_bodies_tes` never learned the element's type. The one-hop
/// `w.inner` spelling was correct, which is what made the gap read as a
/// projection question rather than a DEPTH one.
///
/// DEPTH-INVARIANCE IS THE ORACLE: `named` (no projection), `one`, `two` and
/// `three` are one program at four depths and must print one body sequence
/// modulo ids. `nodest` and `live` keep the root alive past the destructure.
///
/// EVERY CELL BINDS ITS OWN LEAF NAMES (`a1`/`b1`, `a2`/`b2`, …) AND THAT IS
/// LOAD-BEARING, not style. `optres_payload_bodies_tes` is keyed by leaf NAME
/// with no function qualification, so a leaf registered in one function is still
/// registered for a same-named leaf in the next. A first version of this fixture
/// gave every cell `a`/`b`; `c_named` ran first, registered `b`, and `c_two`'s
/// own `b` inherited it — the interpreter then ran the payload body for the
/// wrong reason and the whole fixture PASSED WITHOUT THE FIX. Renaming the
/// leaves, or moving `c_two` first, makes it fail again. Verified both ways.
/// That cross-function inheritance is filed separately; here it is why a
/// regression fixture in this family must not share leaf names across cells.
///
/// The one-hop cell also guards someone else's fix: `6ab46d5` (B-2026-09-03-22,
/// a `Result` destructure leaf's boxed-payload memory owner) is what made `one`
/// agree, verified by bisect over `84cf064`, `ebe5821`, `5f3c3be` and `6ab46d5`.
///
/// Twin of `tests/interpreter.rs`'s
/// `test_tuple_destructure_result_payload_survives_every_depth`, pinned to the
/// SAME string. The compiled side was already correct — it is the oracle the
/// interpreter was moved onto — so this exists to stop the two being reconciled
/// later by moving codegen instead.
#[test]
fn e2e_tuple_destructure_result_payload_survives_every_depth() {
    let Some(out) = run_program(
        r#"struct R { id: i64, tag: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}/{self.tag}") } }
fn mk(n: i64) -> R { return R { id: n, tag: f"t{n}" }; }

struct WrapR { inner: (R, Result[R, String]) }
struct OuterR { h: WrapR }
struct DeepR { g: OuterR }

fn c_named()  { let t: (R, Result[R, String]) = (mk(1), Result.Ok(mk(101))); let (a1, b1) = t; println(f"  rd{a1.id}") }
fn c_one()    { let w = WrapR { inner: (mk(2), Result.Ok(mk(102))) }; let (a2, b2) = w.inner; println(f"  rd{a2.id}") }
fn c_two()    { let g = OuterR { h: WrapR { inner: (mk(3), Result.Ok(mk(103))) } }; let (a3, b3) = g.h.inner; println(f"  rd{a3.id}") }
fn c_three()  { let d = DeepR { g: OuterR { h: WrapR { inner: (mk(4), Result.Ok(mk(104))) } } }; let (a4, b4) = d.g.h.inner; println(f"  rd{a4.id}") }
fn c_nodest() { let w = WrapR { inner: (mk(5), Result.Ok(mk(105))) }; println(f"  rd{w.inner.0.id}") }
fn c_live()   { let g = OuterR { h: WrapR { inner: (mk(6), Result.Ok(mk(106))) } }; let (a6, b6) = g.h.inner; println(f"  rd{a6.id}"); println(f"  g{g.h.inner.0.id}") }

fn main() {
    println("named"); c_named(); println("named end")
    println("one");   c_one();   println("one end")
    println("two");   c_two();   println("two end")
    println("three"); c_three(); println("three end")
    println("nodest");c_nodest();println("nodest end")
    println("live");  c_live();  println("live end")
    println("done")
}
"#,
    ) else {
        return;
    };
    assert_eq!(
        out,
        r#"named
dR101/t101
  rd1
dR1/t1
named end
one
dR102/t102
  rd2
dR2/t2
one end
two
dR103/t103
  rd3
dR3/t3
two end
three
dR104/t104
  rd4
dR4/t4
three end
nodest
  rd5
dR5/t5
dR105/t105
nodest end
live
dR106/t106
  rd6
dR6/t6
  g6
live end
done
"#
    );
}

/// B-2026-09-05-9 — EVERY MOVING ARM over a heap-BOXED agg destructure
/// leaf payload prints correctly AND frees the box envelope, on the
/// `Option` and `Result` families alike. The memory half is the ASAN
/// twin (`asan_agg_leaf_boxed_payload_moving_arm_frees_envelope`); this
/// pins the output on every compiled surface against the interpreter's.
///
/// A moving arm hands the payload's CONTENTS to the binding, so the
/// leaf's own drop must not free them. The agg suppressors did that by
/// neutralizing the SLOT at runtime (`None` tag / zeroed payload words),
/// and for a boxed payload the box pointer went with the contents: the
/// leaf's tag-dispatch drop skipped the envelope and 56 B leaked per
/// arm, at -O0 and -O2. The plain-local family never had this because
/// its `BoxedEnumDrop` retracts only the INNER drop at compile time.
/// The agg families now do the same: when the leaf's payload is boxed,
/// the suppressor swaps the leaf's `EnumDrop` fn for a variant-masked
/// synthesis (`emit_{option,result}_drop_fn_envelope_only`) whose moved
/// variant frees only the box, and leaves the slot untouched.
///
/// `one`/`two`/`three` are the `Result` block-move, escape and `if let`
/// cells; `four`/`five`/`six` the same over `Option`; `seven` moves the
/// `Err` side (the other mask); `eight` is a struct-field leaf; `nine`
/// and `ten` take the NON-moved variant through the masked fn (its
/// other arm must still drop in full); `twelve` is the inline four-word
/// control that never boxed; `thirteen` moves BOTH sides of a
/// `Result[W, W]`, so the two arms' masks must compose (`OkErr`) — a
/// second swap that replaced the first would double-free the `Ok` arm.
#[test]
fn e2e_agg_leaf_boxed_payload_moving_arm_frees_envelope() {
    let Some(out) = run_program(
            "struct R { id: i64 }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\") } }\n\
             struct W { id: i64, x: String, y: String }\n\
             impl Drop for W { fn drop(mut ref self) { println(f\"dW{self.id}/{self.x}{self.y}\") } }\n\
             struct HoW { a: R, b: Result[W, String] }\n\
             fn mkw(k: i64) -> W { return W { id: k, x: f\"x{k}\", y: f\"y{k}\" } }\n\
             fn pickr(k: i64) -> W { let t: (R, Result[W, String]) = (R { id: k }, Result.Ok(mkw(k * 11))); let (a, b) = t; match b { Result.Ok(w) => w, Result.Err(e) => mkw(0) } }\n\
             fn picko(k: i64) -> W { let t: (R, Option[W]) = (R { id: k }, Option.Some(mkw(k * 11))); let (a, b) = t; match b { Option.Some(w) => w, Option.None => mkw(0) } }\n\
             fn pickboth(ok: bool) -> W { let t: (R, Result[W, W]) = (R { id: 13 }, if ok { Result.Ok(mkw(1313)) } else { Result.Err(mkw(1331)) }); let (a, b) = t; match b { Result.Ok(w) => w, Result.Err(w) => w } }\n\
             fn main() {\n\
             \x20   { let t: (R, Result[W, String]) = (R { id: 1 }, Result.Ok(mkw(11))); let (a, b) = t; match b { Result.Ok(w) => { let g: W = w; println(f\"g{g.id}\") }, Result.Err(e) => println(\"err\") } println(\"one\") }\n\
             \x20   { let w: W = pickr(2); println(f\"got{w.id}\"); println(\"two\") }\n\
             \x20   { let t: (R, Result[W, String]) = (R { id: 3 }, Result.Ok(mkw(33))); let (a, b) = t; if let Result.Ok(w) = b { let g: W = w; println(f\"g{g.id}\") } println(\"three\") }\n\
             \x20   { let t: (R, Option[W]) = (R { id: 4 }, Option.Some(mkw(44))); let (a, b) = t; match b { Option.Some(w) => { let g: W = w; println(f\"g{g.id}\") }, Option.None => println(\"none\") } println(\"four\") }\n\
             \x20   { let w: W = picko(5); println(f\"got{w.id}\"); println(\"five\") }\n\
             \x20   { let t: (R, Option[W]) = (R { id: 6 }, Option.Some(mkw(66))); let (a, b) = t; if let Option.Some(w) = b { let g: W = w; println(f\"g{g.id}\") } println(\"six\") }\n\
             \x20   { let t: (R, Result[String, W]) = (R { id: 7 }, Result.Err(mkw(77))); let (a, b) = t; match b { Result.Ok(s) => println(f\"ok{s}\"), Result.Err(w) => { let g: W = w; println(f\"g{g.id}\") } } println(\"seven\") }\n\
             \x20   { let h: HoW = HoW { a: R { id: 8 }, b: Result.Ok(mkw(88)) }; let HoW { a, b } = h; match b { Result.Ok(w) => { let g: W = w; println(f\"g{g.id}\") }, Result.Err(e) => println(\"err\") } println(\"eight\") }\n\
             \x20   { let t: (R, Result[W, String]) = (R { id: 9 }, Result.Err(\"e9\")); let (a, b) = t; match b { Result.Ok(w) => { let g: W = w; println(f\"g{g.id}\") }, Result.Err(e) => println(f\"err{e}\") } println(\"nine\") }\n\
             \x20   { let t: (R, Option[W]) = (R { id: 10 }, Option.None); let (a, b) = t; match b { Option.Some(w) => { let g: W = w; println(f\"g{g.id}\") }, Option.None => println(\"none\") } println(\"ten\") }\n\
             \x20   { let t: (R, Result[R, String]) = (R { id: 12 }, Result.Ok(R { id: 1212 })); let (a, b) = t; match b { Result.Ok(r) => { let g: R = r; println(f\"g{g.id}\") }, Result.Err(e) => println(\"err\") } println(\"twelve\") }\n\
             \x20   { let w: W = pickboth(true); let v: W = pickboth(false); println(f\"got{w.id}{v.id}\"); println(\"thirteen\") }\n\
             \x20   println(\"end\")\n\
             }\n\
             ",
        ) else {
            return;
        };
    assert_eq!(out, "dR1\ng11\ndW11/x11y11\none\ndR2\ngot22\ndW22/x22y22\ntwo\ndR3\ng33\ndW33/x33y33\nthree\ndR4\ng44\ndW44/x44y44\nfour\ndR5\ngot55\ndW55/x55y55\nfive\ndR6\ng66\ndW66/x66y66\nsix\ndR7\ng77\ndW77/x77y77\nseven\ndR8\ng88\ndW88/x88y88\neight\ndR9\nerre9\nnine\ndR10\nnone\nten\ndR12\ng1212\ndR1212\ntwelve\ndR13\ndR13\ngot13131331\ndW1331/x1331y1331\ndW1313/x1313y1313\nthirteen\nend\n");
}

/// B-2026-09-06-39 — AN OWNED ENUM RECEIVER'S PAYLOAD `Drop` BODY IS LOST
/// WHENEVER THE CALLEE NEVER DESTRUCTURES `self`.
///
/// `let a = E.A(mk(1)); a.none()` over `fn none(self) -> i64 { return 5 }` ran
/// the enum's SHELL body and never the payload's, on all four surfaces, with
/// memory balanced — so no A/B check and no ASAN fixture could see it. The
/// STRUCT receiver beside it was always correct (`S { r }.s_none()` prints
/// `dS dR`), which is what localises the fault: both registrars deliberately
/// "walk a STRUCT receiver's bodies caller-side and leave an ENUM receiver's to
/// the match-arm channel" — and with no match, that channel does not exist.
///
/// The disarm was a HAND-OFF written for the callee that matches on `self`
/// (B-2026-08-01-7's doubled body), applied unconditionally. It now fires only
/// when someone else really owns the payload: an arm channel does
/// (`fn_binds_self_part_out`, or the new `fn_matches_on_bare_self` for the
/// `match self` spelling a struct receiver treats as views), or the RESULT does
/// (a return that can carry the receiver).
///
/// `none` / `temp` / `nodrop` / `generic` are the fixed cells — local receiver,
/// fresh temp, an enum with NO `impl Drop` of its own, and a generic enum. The
/// rest are the controls that must not move, each covering one clause of the
/// gate: `matches` (the arm owns it — the shape whose double this disarm exists
/// to prevent), `ret_self` and `wrap` (the result owns it), `refm` (`ref self`,
/// already correct), `plain` (no call at all — the reference order, `dE` then
/// `dR`, per design.md § Part 8 "the user's `fn drop` body runs first, then the
/// compiler drops each field").
///
/// TWO PRE-EXISTING DEFECTS ARE PINNED AS-IS HERE RATHER THAN BLESSED, both
/// measured identical before and after this fix and filed separately: the
/// `dE … dE` in `ret_self` and `wrap` is a DOUBLED SHELL body on a receiver
/// that escapes via the return, and `E.A(mk(n)).ret_self().none()` (a chain)
/// runs NO body at all. Neither is this row's, and pinning them keeps this
/// fixture honest about what it measured.
///
/// B-2026-09-06-39 — REPINNED. `matches` reads `x5 dE dR5` for `dR5 x5 dE`: its
/// read-only arm binds a view now, so the caller runs the payload's body after
/// the shell's instead of the arm running it first. Every other cell, this row's
/// own included, is byte-identical.
///
/// Twin of `tests/interpreter.rs`'s
/// `test_owned_enum_receiver_runs_its_payload_body_when_no_arm_claims_it`, byte-identical source and expectation — the only fixture
/// shape that can hold an agreed gap closed.
#[test]
fn e2e_owned_enum_receiver_runs_its_payload_body_when_no_arm_claims_it() {
    let Some(out) = run_program(
        r#"struct R { id: i64, tag: String, xs: Vec[i64] }
    impl Drop for R { fn drop(mut ref self) { println(f"  dR{self.id}") } }
    fn mk(i: i64) -> R { return R { id: i, tag: f"t{i}", xs: [i] } }
    enum E { A(R), B }
    impl Drop for E { fn drop(mut ref self) { println("  dE") } }
    enum N { A(R), B }
    enum G[T] { X(T), Y }
    struct W { e: E }
    impl E {
        fn none(self) -> i64 { return 5 }
        fn matches(self) -> i64 { match self { E.A(r) => { return r.id; } E.B => { return 0; } } }
        fn ret_self(self) -> E { return self }
        fn wrap(self) -> W { return W { e: self } }
        fn refm(ref self) -> i64 { return 3 }
    }
    impl N { fn none(self) -> i64 { return 5 } }
    impl G[R] { fn none(self) -> i64 { return 5 } }
    fn main() {
        println("none");     { let a: E = E.A(mk(1)); println(f"  x{a.none()}") }
        println("temp");     { println(f"  x{E.A(mk(2)).none()}") }
        println("nodrop");   { let a: N = N.A(mk(3)); println(f"  x{a.none()}") }
        println("generic");  { let g: G[R] = G.X(mk(4)); println(f"  x{g.none()}") }
        println("matches");  { let a: E = E.A(mk(5)); println(f"  x{a.matches()}") }
        println("ret_self"); { let a: E = E.A(mk(6)); let b: E = a.ret_self(); println("  got") }
        println("wrap");     { let a: E = E.A(mk(7)); let w: W = a.wrap(); println("  got") }
        println("refm");     { let a: E = E.A(mk(8)); println(f"  x{a.refm()}") }
        println("plain");    { let a: E = E.A(mk(9)); println("  x9") }
        println("end");
    }
    "#,
    ) else {
        return;
    };
    assert_eq!(out, "none\n  x5\n  dE\n  dR1\ntemp\n  dE\n  dR2\n  x5\nnodrop\n  x5\n  dR3\ngeneric\n  x5\n  dR4\nmatches\n  x5\n  dE\n  dR5\nret_self\n  dE\n  dR6\n  dE\n  got\nwrap\n  dE\n  dR7\n  dE\n  got\nrefm\n  x3\n  dE\n  dR8\nplain\n  dE\n  dR9\n  x9\nend\n");
}

/// B-2026-09-10-14 — A WHOLE-PAYLOAD ARM BINDING OVER AN
/// `Option`/`Result` WHOSE PAYLOAD IS A TUPLE NOW RUNS THE ELEMENTS'
/// `Drop` BODIES.
///
/// `let o: Option[(R, R)] = Some(..); match o { Some(t) => { println("hit")
/// } .. }` printed `x hit` where `x hit dR1 dR2` is due — on `--interp`,
/// the JIT, `-O0` and `-O2` auto-par alike. BOTH backends were wrong the
/// same way, so no A/B rule saw it, and memory was balanced (0 valgrind
/// errors, nothing lost), so no sanitizer leg did either. The absent output
/// was the only observable.
///
/// THE DISARM WAS RIGHT AND ITS PREMISE WAS UNFUNDED.
/// `suppress_optres_payload_bodies_for_match` retracts the PLACE's
/// payload-bodies walk whenever an arm binds a Drop-bearing position, on
/// the premise that "the arm's binding owns the resource from then on".
/// Every other payload shape funds that at the bind site — a struct payload
/// registers a drop of its own (`struct` here), a destructure's leaves each
/// take an element — and a TUPLE-typed whole-value binding funds nothing,
/// because a tuple has no type NAME to key those arms on. So the walk went
/// to a holder that never ran it.
///
/// THE PLACE KEEPS THE WALK, rather than the binding being given one, and
/// that is measured rather than stylistic: the binding is a bit-copy VIEW
/// of a place the match does not consume, so `twice` binds it again a
/// statement later. Funding the binding ran the bodies once per arm
/// (`x a dR11 dR12 b dR11 dR12`); the place's single walk gives one set at
/// `o`'s real last use. It is also exactly what `wild` — correct all along
/// — already does.
///
/// The arm predicate lives in `binding_use`, SHARED with the interpreter
/// rather than reimplemented: two backends can only be moved off an agreed
/// gap together if they read the same AST through the same function. It is
/// the READ-THROUGH classifier and not a borrow one, which is the module's
/// own stated reason for existing.
///
/// TWO CELLS ARE PINNED AS-IS RATHER THAN BLESSED. `eat` (`x eat hit7`)
/// still loses the bodies: `eat(t)` materializes the binding by the
/// read-through classifier, so the disarm stands, and the agreed-and-wrong
/// answer is kept rather than traded for an A/B divergence — filed
/// separately. `ret` is the CONTROL for that decision: `return t` hands the
/// tuple to the caller's binding, which runs the bodies once, and leaving
/// the place armed there was measured at `dR30 dR31 dR30 dR31`.
///
/// Twin of `tests/interpreter.rs`'s
/// `test_whole_payload_arm_binding_over_a_tuple_payload_runs_element_bodies`,
/// byte-identical source and expectation — the only fixture shape that can
/// hold an agreed gap closed.
#[test]
fn e2e_whole_payload_arm_binding_over_a_tuple_payload_runs_element_bodies() {
    let Some(out) = run_program(
        r#"struct R { id: i64, name: String }
    impl Drop for R { fn drop(mut ref self) { println(f"  dR{self.id}") } }
    struct W { r: R, n: i64 }
    impl Drop for W { fn drop(mut ref self) { println(f"  dW{self.n}") } }
    fn mk(i: i64) -> R { return R { id: i, name: f"n{i}" } }
    fn eat(t: (R, R)) -> i64 { println("  eat"); return 7 }
    fn giveback() -> (R, R) {
        let o: Option[(R, R)] = Some((mk(30), mk(31)));
        match o { Some(t) => { return t } None => { return (mk(90), mk(91)) } }
    }

    fn main() {
        println("bind");    { let o: Option[(R, R)] = Some((mk(1), mk(2))); println("  x"); match o { Some(t) => { println("  hit") } None => { println("  n") } } }
        println("read");    { let o: Option[(R, R)] = Some((mk(3), mk(4))); println("  x"); match o { Some(t) => { println(f"  hit{t.0.id}") } None => { println("  n") } } }
        println("iflet");   { let o: Option[(R, R)] = Some((mk(5), mk(6))); println("  x"); if let Some(t) = o { println("  hit") } }
        println("whilet");  { let mut o: Option[(R, R)] = Some((mk(7), mk(8))); println("  x"); while let Some(t) = o { println("  hit"); o = None; } println("  end") }
        println("result");  { let o: Result[(R, R), i64] = Ok((mk(9), mk(10))); println("  x"); match o { Ok(t) => { println("  hit") } Err(e) => { println("  n") } } }
        println("twice");   { let o: Option[(R, R)] = Some((mk(11), mk(12))); println("  x"); match o { Some(t) => { println("  a") } None => { println("  n") } } match o { Some(t) => { println("  b") } None => { println("  n") } } }
        println("after");   { let o: Option[(R, R)] = Some((mk(13), mk(14))); println("  x"); match o { Some(t) => { println("  hit") } None => { println("  n") } } println("  end") }
        println("wild");    { let o: Option[(R, R)] = Some((mk(15), mk(16))); println("  x"); match o { Some(_) => { println("  hit") } None => { println("  n") } } }
        println("struct");  { let o: Option[W] = Some(W { r: mk(17), n: 18 }); println("  x"); match o { Some(t) => { println("  hit") } None => { println("  n") } } }
        println("single");  { let o: Option[R] = Some(mk(19)); println("  x"); match o { Some(t) => { println("  hit") } None => { println("  n") } } }
        println("ret");     { println("  x"); let v = giveback(); println("  hit") }
        println("eat");     { let o: Option[(R, R)] = Some((mk(20), mk(21))); println("  x"); match o { Some(t) => { println(f"  hit{eat(t)}") } None => { println("  n") } } }
        println("nomatch"); { let o: Option[(R, R)] = Some((mk(22), mk(23))); println("  x") }
        println("none");    { let o: Option[(R, R)] = None; println("  x"); match o { Some(t) => { println("  hit") } None => { println("  n") } } }
        println("tlocal");  { let t: (R, R) = (mk(24), mk(25)); println("  x") }
        println("end")
    }
"#,
    ) else {
        return;
    };
    assert_eq!(out, "bind\n  x\n  hit\n  dR1\n  dR2\nread\n  x\n  hit3\n  dR3\n  dR4\niflet\n  x\n  hit\n  dR5\n  dR6\nwhilet\n  x\n  hit\n  dR7\n  dR8\n  end\nresult\n  x\n  hit\n  dR9\n  dR10\ntwice\n  x\n  a\n  b\n  dR11\n  dR12\nafter\n  x\n  hit\n  dR13\n  dR14\n  end\nwild\n  x\n  hit\n  dR15\n  dR16\nstruct\n  x\n  hit\n  dW18\n  dR17\nsingle\n  x\n  hit\n  dR19\nret\n  x\n  dR30\n  dR31\n  hit\neat\n  x\n  eat\n  hit7\nnomatch\n  dR22\n  dR23\n  x\nnone\n  x\n  n\ntlocal\n  dR24\n  dR25\n  x\nend\n");
}

/// B-2026-09-14-18 — an UNMOVED part of an owned `Option`/`Result` payload
/// lost its `Drop` body when the arm destructured the payload and handed back
/// a DIFFERENT part.
///
/// `Some((a, b)) => return b` over `Option[(R, i64)]` printed `got:9 end`
/// where `dR5 got:9 end` is due, and the two-`Drop`-element cell lost `dR6`.
/// On ALL FOUR surfaces, so no comparison between the backends could see it —
/// which is why the row sat open through four sessions of this family's work,
/// each of which correctly found its own defect elsewhere.
///
/// The cause was one bit where a set was needed. The escape answer was per
/// VARIANT ("Some escapes"), so a caller holding a payload whose parts leave
/// SEPARATELY stood the whole payload down and the part that stayed behind was
/// owed a body by nobody. It is now per PART, on both backends: codegen through
/// `result_escape::optres_payload_escaping_param_variant_parts_with` feeding
/// `PayloadBodiesMask::TupleElems`, the interpreter through
/// `ast::fn_escaping_param_payload_destructured_elems` feeding the mask that
/// the projection spelling already used.
///
/// THE CELLS THAT MUST NOT MOVE are as much the point as the two that do.
/// `readonly` escapes nothing, `bothout` escapes everything, `wholeout` binds
/// the payload whole, and `wildkept` keeps a position the arm never names —
/// the first three are the boundaries of the narrowing and the fourth is the
/// case where a part cannot escape because nothing can refer to it.
///
/// `condfalse` is a KNOWN remaining gap, pinned here rather than fixed: the
/// escape predicate declines a CONDITIONAL hand-back by long-standing
/// convention on this channel (recording it would mask a part that really did
/// die on the other branch), so `dR19` is still owed and still lost. Measured
/// identical before this fix. Filed separately rather than left implicit.
///
/// The INTERPRETER twin is `tests/interpreter.rs`'s
/// `test_destructured_payload_keeps_the_unmoved_parts_body`, byte-identical
/// source and expectation — and byte-identical is the assertion here, since
/// an agreed defect can only be closed by moving both backends together.
#[test]
fn e2e_destructured_payload_keeps_the_unmoved_parts_body() {
    let Some(out) = run_program(
        r#"struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"  dR{self.id}") } }

struct Sink { n: i64 }
impl Sink {
    fn eat(ref self, o: Option[(R, i64)]) -> i64 { match o { Option.Some((a, b)) => { return b; } Option.None => { return 0; } } }
}

fn scalarOut(o: Option[(R, i64)]) -> i64 { match o { Option.Some((a, b)) => { return b; } Option.None => { return 0; } } }
fn dropOut(o: Option[(R, R)]) -> R { match o { Option.Some((a, b)) => { return a; } Option.None => { return R { id: 0 }; } } }
fn resultOut(o: Result[(R, i64), i64]) -> i64 { match o { Result.Ok((a, b)) => { return b; } Result.Err(e) => { return 0; } } }
fn wildKept(o: Option[(R, i64)]) -> i64 { match o { Option.Some((_, b)) => { return b; } Option.None => { return 0; } } }
fn middleOut(o: Option[(R, R, R)]) -> R { match o { Option.Some((a, b, c)) => { return b; } Option.None => { return R { id: 0 }; } } }
fn bothOut(o: Option[(R, R)]) -> (R, R) { match o { Option.Some((a, b)) => { return (a, b); } Option.None => { return (R { id: 0 }, R { id: 0 }); } } }
fn readOnly(o: Option[(R, R)]) -> i64 { match o { Option.Some((a, b)) => { return a.id + b.id; } Option.None => { return 0; } } }
fn wholeOut(o: Option[(R, i64)]) -> (R, i64) { match o { Option.Some(t) => { return t; } Option.None => { return (R { id: 0 }, 0); } } }
fn condOut(o: Option[(R, R)], k: bool) -> R { match o { Option.Some((a, b)) => { if k { return a; } return b; } Option.None => { return R { id: 0 }; } } }
fn genOut[T](o: Option[(R, T)], d: T) -> T { match o { Option.Some((a, b)) => { return b; } Option.None => { return d; } } }

fn main() {
    println("scalarout");  { let got: i64 = scalarOut(Option.Some((R { id: 5 }, 9))); println(f"  got:{got}") }
    println("dropout");    { let got = dropOut(Option.Some((R { id: 6 }, R { id: 7 }))); println(f"  got:{got.id}") }
    println("resultout");  { let got: i64 = resultOut(Result.Ok((R { id: 8 }, 9))); println(f"  got:{got}") }
    println("wildkept");   { let got: i64 = wildKept(Option.Some((R { id: 10 }, 9))); println(f"  got:{got}") }
    println("middleout");  { let got = middleOut(Option.Some((R { id: 11 }, R { id: 12 }, R { id: 13 }))); println(f"  got:{got.id}") }
    println("bothout");    { let got = bothOut(Option.Some((R { id: 14 }, R { id: 15 }))); println(f"  got:{got.0.id}") }
    println("readonly");   { let got: i64 = readOnly(Option.Some((R { id: 16 }, R { id: 17 }))); println(f"  got:{got}") }
    println("wholeout");   { let got = wholeOut(Option.Some((R { id: 18 }, 9))); println(f"  got:{got.1}") }
    println("condfalse");  { let got = condOut(Option.Some((R { id: 19 }, R { id: 20 })), false); println(f"  got:{got.id}") }
    println("genout");     { let got: i64 = genOut(Option.Some((R { id: 21 }, 9)), 0); println(f"  got:{got}") }
    println("method");     { let s = Sink { n: 1 }; let got: i64 = s.eat(Option.Some((R { id: 22 }, 9))); println(f"  got:{got}") }
    println("unused");     { dropOut(Option.Some((R { id: 23 }, R { id: 24 }))); println("  x") }
    println("end")
}
"#,
    ) else {
        return;
    };
    assert_eq!(out, "scalarout\n  dR5\n  got:9\ndropout\n  dR7\n  got:6\n  dR6\nresultout\n  dR8\n  got:9\nwildkept\n  dR10\n  got:9\nmiddleout\n  dR11\n  dR13\n  got:12\n  dR12\nbothout\n  got:14\n  dR14\n  dR15\nreadonly\n  dR16\n  dR17\n  got:33\nwholeout\n  got:9\n  dR18\ncondfalse\n  got:20\n  dR20\ngenout\n  dR21\n  got:9\nmethod\n  dR22\n  got:9\nunused\n  dR24\n  dR23\n  x\nend\n");
}

/// B-2026-09-19-30 — a WILDCARD leaf in a heap-BOXED destructured payload
/// made its NAMED siblings read from the wrong offset, returning a silently
/// wrong value on every compiled backend.
///
/// The row that filed this read the symptom as "the arm returns the `None`
/// arm's value", because the cell's `None` arm returned literal 0 and the
/// bug returned 0. It is not: with the `None` arm changed to return 77 the
/// wildcard spelling still returns 0, and in the FIRST position
/// (`Some((b, _))` over `Option[(i64, H)]`) it returns a pointer-shaped
/// integer. The arm is selected correctly — the payload's `Drop` body runs
/// — and only the binding is wrong.
///
/// The cause is that the payload's width was computed from the PATTERN.
/// `pattern_payload_word_count` sizes each leaf from the type the
/// typechecker recorded for it, and a `_` binds nothing so nothing was
/// recorded: it fell to the 1-word default. That sum is what the debox
/// predicate in `reconstruct_payload_value` tests
/// (`want > field_words.len()`), so one under-counted leaf made a boxed
/// payload look inline, and the arm rebuilt the tuple out of the ENVELOPE
/// words instead of loading through the box. The envelope's word 0 is the
/// box POINTER, which is where the pointer-shaped integer came from; word 1
/// is past the end of the area, which is the zero.
///
/// The fix records the wildcard's type in `check_pattern_against`, which is
/// handed it and used to walk past. The IR diff is the whole defect in two
/// lines: the named spelling emits `inttoptr` + `load` of the real tuple,
/// the wildcard spelling emitted `insertvalue { i64, i64 }` straight from
/// the envelope words.
///
/// `named` is the control that was always correct, `allwild` the one that
/// binds nothing and so never read an offset, and `narrow` the payload that
/// rides INLINE (`struct R { id: i64 }`, under the three-word area) and
/// therefore never took the boxed channel at all. `strwild` is a `String`
/// leaf with no user struct in the payload, `twoof3` and `twowild` are the
/// arities where the wildcard is not a lone prefix, and `okside` /
/// `errside` are the two `Result` channels — all measured wrong before the
/// fix, all agreeing after.
///
/// The `if let` spelling is deliberately ABSENT: it loses the payload's
/// `Drop` body on the compiled backends, which reproduces with this fix
/// reverted and is a separate defect with its own row. Its VALUE is fixed
/// here like the rest; including the cell would pin that missing body
/// instead.
///
/// Byte-identical to the interpreter twin, which is the assertion, and
/// identical on the JIT, `-O0` and `-O2`; valgrind reports no errors and no
/// leaks on the compiled program.
#[test]
fn e2e_wildcard_leaf_in_a_boxed_payload_binds_its_siblings_at_the_right_offset() {
    let Some(out) = run_program(
        r#"struct H { id: i64, s: String }
impl Drop for H { fn drop(mut ref self) { println(f"  dH{self.id}") } }
struct R { id: i64 }

fn second(o: Option[(H, i64)]) -> i64 { match o { Option.Some((_, b)) => { return b; } Option.None => { return 77; } } }
fn first(o: Option[(i64, H)]) -> i64 { match o { Option.Some((b, _)) => { return b; } Option.None => { return 77; } } }
fn named(o: Option[(H, i64)]) -> i64 { match o { Option.Some((a, b)) => { return b; } Option.None => { return 77; } } }
fn twoOfThree(o: Option[(H, i64, i64)]) -> i64 { match o { Option.Some((_, b, c)) => { return b + c; } Option.None => { return 77; } } }
fn allWild(o: Option[(H, i64)]) -> i64 { match o { Option.Some((_, _)) => { return 55; } Option.None => { return 77; } } }
fn twoWild(o: Option[(H, H, i64)]) -> i64 { match o { Option.Some((_, _, c)) => { return c; } Option.None => { return 77; } } }
fn strWild(o: Option[(String, i64)]) -> i64 { match o { Option.Some((_, b)) => { return b; } Option.None => { return 77; } } }
fn narrow(o: Option[(R, i64)]) -> i64 { match o { Option.Some((_, b)) => { return b; } Option.None => { return 77; } } }
fn okSide(r: Result[(H, i64), i64]) -> i64 { match r { Result.Ok((_, b)) => { return b; } Result.Err(e) => { return e; } } }
fn errSide(r: Result[i64, (H, i64)]) -> i64 { match r { Result.Ok(v) => { return v; } Result.Err((_, b)) => { return b; } } }

fn main() {
    println("second");  { let g = second(Option.Some((H { id: 1, s: "aaaaaaaaaaaa" }, 9))); println(f"  n{g}") }
    println("second2"); { let g = second(Option.Some((H { id: 2, s: "aaaaaaaaaaaa" }, 4242))); println(f"  n{g}") }
    println("first");   { let g = first(Option.Some((9, H { id: 3, s: "aaaaaaaaaaaa" }))); println(f"  n{g}") }
    println("named");   { let g = named(Option.Some((H { id: 4, s: "aaaaaaaaaaaa" }, 9))); println(f"  n{g}") }
    println("twoof3");  { let g = twoOfThree(Option.Some((H { id: 5, s: "aaaaaaaaaaaa" }, 9, 100))); println(f"  n{g}") }
    println("allwild"); { let g = allWild(Option.Some((H { id: 6, s: "aaaaaaaaaaaa" }, 9))); println(f"  n{g}") }
    println("twowild"); { let g = twoWild(Option.Some((H { id: 7, s: "aaaaaaaaaaaa" }, H { id: 8, s: "aaaaaaaaaaaa" }, 66))); println(f"  n{g}") }
    println("strwild"); { let g = strWild(Option.Some(("wwwwwwwwwwwwww", 88))); println(f"  n{g}") }
    println("narrow");  { let g = narrow(Option.Some((R { id: 9 }, 44))); println(f"  n{g}") }
    println("okside");  { let g = okSide(Result.Ok((H { id: 10, s: "aaaaaaaaaaaa" }, 11))); println(f"  n{g}") }
    println("errside"); { let g = errSide(Result.Err((H { id: 11, s: "aaaaaaaaaaaa" }, 22))); println(f"  n{g}") }
    println("nonearm"); { let g = second(Option.None); println(f"  n{g}") }
    println("end")
}
"#,
    ) else {
        return;
    };
    assert_eq!(out, "second\n  dH1\n  n9\nsecond2\n  dH2\n  n4242\nfirst\n  dH3\n  n9\nnamed\n  dH4\n  n9\ntwoof3\n  dH5\n  n109\nallwild\n  dH6\n  n55\ntwowild\n  dH7\n  dH8\n  n66\nstrwild\n  n88\nnarrow\n  n44\nokside\n  dH10\n  n11\nerrside\n  dH11\n  n22\nnonearm\n  n77\nend\n", "got:\n{out}");
}

/// B-2026-09-14-18 (BOXED leg) — the same defect on the OTHER channel, where
/// the payload is too wide to sit inline and the bodies belong to the CALLEE.
///
/// A payload of at most three words rides inside the envelope and the CALLER
/// keeps its `Drop` bodies, which is the channel the sibling fixture
/// (`e2e_destructured_payload_keeps_the_unmoved_parts_body`) pins. A wider one — `(H, i64)`
/// with `H { id: i64, s: String }` — heap-boxes, and the box's interior walk
/// becomes the ONLY holder of the bodies. Both channels had the same one-bit
/// answer and so the same defect, and fixing only the one that happened to be
/// measured first would have left the two backends disagreeing on the other.
///
/// Measured before the fix, identical on `--interp`, the JIT, `-O0` and `-O2`
/// auto-par: `scalarOut` printed `got:9` where `dH1 got:9` is due, and the
/// `(H, H)` cells lost whichever element the arm did not return. Agreed and
/// wrong, so no A/B gate saw it.
///
/// The cause here is not the escape answer but the DISARM:
/// `suppress_optres_payload_bodies_for_match_scoped` stands the whole walk
/// down as soon as an arm materializes ANY leaf of a destructure, on the
/// premise that a destructure's leaves each take an element and each register
/// a body of their own. True of the leaves the arm takes, false of the ones it
/// leaves behind. `narrow_callee_owned_tuple_payload_bodies_for_arm` now
/// re-homes the walk onto a `PayloadBodiesMask::TupleElems` walker that skips
/// only the taken indices, and declines — leaving every pre-existing path
/// byte-identical — for a whole-payload binding, a non-tuple payload, an arm
/// that takes every element, and an arm that takes none.
///
/// `noneout` and `bothtouched` are the boundaries: the first takes nothing and
/// must keep both bodies in the callee, the second reads one and returns the
/// other, so one body runs in the callee and one at the caller's `g`. `middle`
/// pins the three-element shape, where the mask has to name index 1 and not a
/// contiguous prefix. `resultout` is the `Result.Ok` spelling.
///
/// The WILDCARD spelling (`Some((_, b)) => return b`) is deliberately ABSENT,
/// and stays so now that it is fixed: it was a wrong VALUE rather than a
/// missing body — the wildcard leaf under-counted the payload's width, so the
/// arm read its named siblings from the envelope instead of through the box —
/// and it has its own fixture in
/// `e2e_wildcard_leaf_in_a_boxed_payload_binds_its_siblings_at_the_right_offset`
/// (B-2026-09-19-30). Keeping the two apart is what lets each keep measuring
/// one thing.
///
/// Byte-identical to the interpreter twin, which is the assertion, since an
/// agreed defect can only be closed by moving both backends together.
#[test]
fn e2e_boxed_destructured_payload_keeps_the_unmoved_parts_body() {
    let Some(out) = run_program(
        r#"struct H { id: i64, s: String }
impl Drop for H { fn drop(mut ref self) { println(f"  dH{self.id}") } }

fn scalarOut(o: Option[(H, i64)]) -> i64 { match o { Option.Some((a, b)) => { return b; } Option.None => { return 0; } } }
fn readAndScalarOut(o: Option[(H, i64)]) -> i64 { match o { Option.Some((a, b)) => { println(f"  in{a.id}"); return b; } Option.None => { return 0; } } }
fn firstOut(o: Option[(H, H)]) -> H { match o { Option.Some((a, b)) => { return a; } Option.None => { return H { id: 0, s: "zzzzzzzzzzzz" }; } } }
fn secondOut(o: Option[(H, H)]) -> H { match o { Option.Some((a, b)) => { return b; } Option.None => { return H { id: 0, s: "zzzzzzzzzzzz" }; } } }
fn middleOut(o: Option[(H, H, H)]) -> H { match o { Option.Some((a, b, c)) => { return b; } Option.None => { return H { id: 0, s: "zzzzzzzzzzzz" }; } } }
fn bothOut(o: Option[(H, H)]) -> H { match o { Option.Some((a, b)) => { println(f"  keep{b.id}"); return a; } Option.None => { return H { id: 0, s: "zzzzzzzzzzzz" }; } } }
fn noneOut(o: Option[(H, H)]) -> i64 { match o { Option.Some((a, b)) => { return a.id + b.id; } Option.None => { return 0; } } }
fn resultOut(r: Result[(H, i64), i64]) -> i64 { match r { Result.Ok((a, b)) => { return b; } Result.Err(e) => { return e; } } }

fn main() {
    println("scalar");
    { let g: i64 = scalarOut(Option.Some((H { id: 1, s: "aaaaaaaaaaaa" }, 9))); println(f"  got:{g}"); }
    println("readscalar");
    { let g: i64 = readAndScalarOut(Option.Some((H { id: 2, s: "bbbbbbbbbbbb" }, 8))); println(f"  got:{g}"); }
    println("first");
    { let g = firstOut(Option.Some((H { id: 3, s: "cccccccccccc" }, H { id: 4, s: "dddddddddddd" }))); println(f"  got:{g.id}"); }
    println("second");
    { let g = secondOut(Option.Some((H { id: 5, s: "eeeeeeeeeeee" }, H { id: 6, s: "ffffffffffff" }))); println(f"  got:{g.id}"); }
    println("middle");
    { let g = middleOut(Option.Some((H { id: 7, s: "gggggggggggg" }, H { id: 8, s: "hhhhhhhhhhhh" }, H { id: 9, s: "iiiiiiiiiiii" }))); println(f"  got:{g.id}"); }
    println("bothtouched");
    { let g = bothOut(Option.Some((H { id: 10, s: "jjjjjjjjjjjj" }, H { id: 11, s: "kkkkkkkkkkkk" }))); println(f"  got:{g.id}"); }
    println("noneout");
    { let g: i64 = noneOut(Option.Some((H { id: 12, s: "llllllllllll" }, H { id: 13, s: "mmmmmmmmmmmm" }))); println(f"  got:{g}"); }
    println("resultout");
    { let g: i64 = resultOut(Result.Ok((H { id: 14, s: "nnnnnnnnnnnn" }, 7))); println(f"  got:{g}"); }
    println("end");
}
"#,
    ) else {
        return;
    };
    assert_eq!(out, "scalar\n  dH1\n  got:9\nreadscalar\n  in2\n  dH2\n  got:8\nfirst\n  dH4\n  got:3\n  dH3\nsecond\n  dH5\n  got:6\n  dH6\nmiddle\n  dH7\n  dH9\n  got:8\n  dH8\nbothtouched\n  keep11\n  dH11\n  got:10\n  dH10\nnoneout\n  dH12\n  dH13\n  got:25\nresultout\n  dH14\n  got:7\nend\n");
}

/// B-2026-09-06-39 — A READ-ONLY ARM OVER AN OWNED ENUM RECEIVER NOW RUNS THE
/// PAYLOAD'S `Drop` BODY **AFTER** THE SHELL'S, the design.md § Part 8 order
/// ("the user's `fn drop` body runs first, then the compiler drops each field").
///
/// `let a = E.A(mk(1)); a.read()` over a read-only `match self` arm printed
/// `dR1 dE` on all four surfaces — the REVERSE of what the same arm prints over a
/// local scrutinee (`dE dR`, since B-2026-08-28-67) and over a by-value param
/// (`param` here). Agreed across the backends, so no A/B gate saw it, and memory
/// was balanced: it was the ORDER.
///
/// The cause was ownership, not sequencing, so the fix is a RE-HOMING and the
/// order falls out of it. A bare-`self` arm over an enum with its own `impl Drop`
/// whose bindings are only READ now binds VIEWS — design.md § Match Arm Binding
/// Modes' "bindings that are only read borrow from the already-owned value" —
/// exactly as a bare owned STRUCT receiver's arms have since B-2026-09-06-15, and
/// the CALLER keeps the payload's bodies. Three predicates decide it and all
/// three had to be there: `fn_bare_self_arms_bind_views` (every arm
/// projection-only, and no `let e = self` beside it), the receiver being a
/// non-shared value enum with its own `Drop`, and the callee-side
/// `bare_self_is_owned_drop_enum_receiver` reading a per-frame flag so caller and
/// callee cannot disagree about who owns the body.
///
/// `named/read`, `temp/read`, `named/wild` and `named/iflet` are the fixed cells
/// — the `match`, fresh-temp, wildcard and `if let` spellings. `chain/read` is
/// the chain-link receiver, whose payload walk had no owner at all once the arm
/// stopped claiming it (`x5` alone); the fresh-temp registrar now admits a
/// MethodCall receiver for that walk. Its missing `dE` is pre-existing and
/// untouched.
///
/// TWO RESIDUALS ARE PINNED AS THEY STAND rather than blessed. `named/call`
/// (`eat(r)`) still prints payload-then-shell: the read-only walk counts a bare
/// mention in ANY non-projection position as a take, which `Some(r)` really is
/// (it doubled the body without the clause) and `eat(r)` is not — the walk cannot
/// tell them apart syntactically and over-approximates, because an
/// over-approximation costs this mis-order while an under-approximation costs a
/// doubled body. That is B-2026-09-16-29. `ret_self` / `wrap` keep the doubled
/// `dE` of B-2026-09-16-22.
///
/// Controls that must not move: `named/none` (no arm at all, B-2026-09-16-21),
/// `named/letself` (a whole rebind — the callee owns it, so the arms must NOT
/// bind views), `refm` (`ref self`), `plain` (no call — the reference order),
/// `param` (the by-value twin), and `nodrop/read` / `nodrop/out` (an enum with no
/// `impl Drop`, which keeps transfer semantics because its arm may legally move
/// the payload out).
///
/// Twin of `tests/interpreter.rs`'s `test_read_only_arm_on_owned_enum_receiver_orders_payload_after_shell`, pinned to the same string.
#[test]
fn e2e_read_only_arm_on_owned_enum_receiver_orders_payload_after_shell() {
    let Some(out) = run_program(
        r#"struct R { id: i64, tag: String, xs: Vec[i64] }
impl Drop for R { fn drop(mut ref self) { println(f"  dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, tag: f"t{i}", xs: [i] } }
fn eat(r: R) -> i64 { return r.id; }
enum E { A(R), B }
impl Drop for E { fn drop(mut ref self) { println("  dE") } }
enum N { A(R), B }
struct W { e: E }
impl E {
    fn read(self) -> i64 { match self { E.A(r) => { return r.id; } E.B => { return 0; } } }
    fn wild(self) -> i64 { match self { E.A(_) => { return 1; } E.B => { return 0; } } }
    fn iflet(self) -> i64 { if let E.A(r) = self { return r.id; } return 0; }
    fn call(self) -> i64 { match self { E.A(r) => { return eat(r); } E.B => { return 0; } } }
    fn me(self) -> E { return self; }
    fn none(self) -> i64 { return 5 }
    fn letself(self) -> i64 { let e = self; match e { E.A(r) => { return r.id; } E.B => { return 0; } } }
    fn ret_self(self) -> E { return self }
    fn wrap(self) -> W { return W { e: self } }
    fn refm(ref self) -> i64 { match self { E.A(r) => { return r.id; } E.B => { return 0; } } }
}
impl N {
    fn read(self) -> i64 { match self { N.A(r) => { return r.id; } N.B => { return 0; } } }
    fn out(self) -> R { match self { N.A(r) => { return r; } N.B => { return mk(0); } } }
}
fn f_read(e: E) -> i64 { match e { E.A(r) => { return r.id; } E.B => { return 0; } } }
fn main() {
    println("named/read");   { let a: E = E.A(mk(1)); println(f"  x{a.read()}") }
    println("temp/read");    { println(f"  x{E.A(mk(2)).read()}") }
    println("named/wild");   { let a: E = E.A(mk(3)); println(f"  x{a.wild()}") }
    println("named/iflet");  { let a: E = E.A(mk(4)); println(f"  x{a.iflet()}") }
    println("chain/read");   { println(f"  x{E.A(mk(5)).me().read()}") }
    println("named/call");   { let a: E = E.A(mk(6)); println(f"  x{a.call()}") }
    println("named/none");   { let a: E = E.A(mk(7)); println(f"  x{a.none()}") }
    println("named/letself");{ let a: E = E.A(mk(8)); println(f"  x{a.letself()}") }
    println("ret_self");     { let a: E = E.A(mk(9)); let b: E = a.ret_self(); println("  got") }
    println("wrap");         { let a: E = E.A(mk(10)); let w: W = a.wrap(); println("  got") }
    println("refm");         { let a: E = E.A(mk(11)); println(f"  x{a.refm()}") }
    println("plain");        { let a: E = E.A(mk(12)); println("  x12") }
    println("param");        { let a: E = E.A(mk(13)); println(f"  x{f_read(a)}") }
    println("nodrop/read");  { let a: N = N.A(mk(14)); println(f"  x{a.read()}") }
    println("nodrop/out");   { let a: N = N.A(mk(15)); let r: R = a.out(); println(f"  x{r.id}") }
    println("end");
}
"#,
    ) else {
        return;
    };
    assert_eq!(
        out,
        r#"named/read
  x1
  dE
  dR1
temp/read
  dE
  dR2
  x2
named/wild
  x1
  dE
  dR3
named/iflet
  x4
  dE
  dR4
chain/read
  dR5
  x5
named/call
  dR6
  x6
  dE
named/none
  x5
  dE
  dR7
named/letself
  dE
  dR8
  x8
ret_self
  dE
  dR9
  dE
  got
wrap
  dE
  dR10
  dE
  got
refm
  x11
  dE
  dR11
plain
  dE
  dR12
  x12
param
  x13
  dE
  dR13
nodrop/read
  dR14
  x14
nodrop/out
  x15
  dR15
end
"#
    );
}

/// B-2026-09-03-34 — a match arm's STRUCT payload binding whose type has
/// Drop-bearing FIELDS (and no `Drop` of its own) runs those bodies on the
/// compiled backends, whatever the arm does with it.
///
/// The arm disarms the enum's payload walk in the binding's favour, and
/// the binding got `track_struct_var` — memory alone — so every field body
/// was lost: unread (`two`, `nine`), read (`seven`), destructured (`one`,
/// the row's cell, where the destructure transfers bodies only off a
/// source that owns a walk; `four`, `five`, `six`, `three` the `if let`
/// spelling). Only the `Option` field's payload body survived, through
/// B-2026-09-03-24's registrar, which is what made the row read as a
/// destructure defect. The binding now registers the field-bodies walk
/// beside its memory, as a local of the same type has, and every shape
/// prints the interpreter's lines. `eight` (rebind) and `ten` (`_` arm)
/// were always right and pin that the walk moves with a rebind and that
/// the enum's own walk still runs when nothing binds the payload.
#[test]
fn e2e_match_arm_struct_payload_binding_runs_its_field_bodies() {
    let Some(out) = run_program(
            "struct R { id: i64, tag: String, xs: Vec[i64] }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\") } }\n\
             struct Ho2 { a: R, b: Option[R] }\n\
             struct Two { a: R, b: R }\n\
             struct Nest { inner: Two, z: i64 }\n\
             enum Wrap { W(Ho2), T(Two), Nn(Nest), N }\n\
             fn mk(k: i64) -> R { return R { id: k, tag: f\"t{k}\", xs: [k] } }\n\
             fn main() {\n\
             \x20   { let w: Wrap = Wrap.W(Ho2 { a: mk(52), b: Option.Some(mk(152)) }); match w { Wrap.W(h) => { let Ho2 { a, b } = h; println(\"in\") }, _ => println(\"n\") } println(\"one\") }\n\
             \x20   { let w: Wrap = Wrap.W(Ho2 { a: mk(55), b: Option.Some(mk(155)) }); match w { Wrap.W(h) => { println(\"in\") }, _ => println(\"n\") } println(\"two\") }\n\
             \x20   { let w: Wrap = Wrap.W(Ho2 { a: mk(56), b: Option.Some(mk(156)) }); if let Wrap.W(h) = w { let Ho2 { a, b } = h; println(\"in\") } println(\"three\") }\n\
             \x20   { let w: Wrap = Wrap.T(Two { a: mk(57), b: mk(157) }); match w { Wrap.T(h) => { let Two { a, b } = h; println(\"in\") }, _ => println(\"n\") } println(\"four\") }\n\
             \x20   { let w: Wrap = Wrap.T(Two { a: mk(58), b: mk(158) }); match w { Wrap.T(h) => { let Two { a, b: _ } = h; println(\"in\") }, _ => println(\"n\") } println(\"five\") }\n\
             \x20   { let w: Wrap = Wrap.Nn(Nest { inner: Two { a: mk(59), b: mk(159) }, z: 1 }); match w { Wrap.Nn(h) => { let Nest { inner, z } = h; println(\"in\") }, _ => println(\"n\") } println(\"six\") }\n\
             \x20   { let w: Wrap = Wrap.T(Two { a: mk(60), b: mk(160) }); match w { Wrap.T(h) => { println(f\"use{h.a.id}\") }, _ => println(\"n\") } println(\"seven\") }\n\
             \x20   { let w: Wrap = Wrap.T(Two { a: mk(61), b: mk(161) }); match w { Wrap.T(h) => { let g: Two = h; println(\"in\") }, _ => println(\"n\") } println(\"eight\") }\n\
             \x20   { let w: Wrap = Wrap.T(Two { a: mk(62), b: mk(162) }); match w { Wrap.T(h) => { println(\"in\") }, _ => println(\"n\") } println(\"nine\") }\n\
             \x20   { let w: Wrap = Wrap.T(Two { a: mk(63), b: mk(163) }); match w { _ => println(\"n\") } println(\"ten\") }\n\
             \x20   println(\"end\")\n\
             }\n\
             ",
        ) else {
            return;
        };
    assert_eq!(out, "dR152\ndR52\nin\none\nin\ndR155\ndR55\ntwo\ndR156\ndR56\nin\nthree\ndR157\ndR57\nin\nfour\ndR158\ndR58\nin\nfive\ndR159\ndR59\nin\nsix\nuse60\ndR160\ndR60\nseven\ndR161\ndR61\nin\neight\nin\ndR162\ndR62\nnine\nn\ndR163\ndR63\nten\nend\n");
}

/// B-2026-09-07-33 — the VALUE half of
/// `asan_boxed_payload_handed_out_of_a_match_arm_is_not_written_after_free`.
///
/// This test PASSES on the parent, and that is the point worth stating
/// rather than hiding: the defect it guards was a write into freed memory
/// on a program whose printed answer was already right at both opt levels
/// and under `--interp`. So the ASAN twin is the pin that fails pre-fix,
/// and this one exists to catch the opposite regression — a future change
/// that silences the use-after-free by dropping the payload instead of
/// handing it over, which would show up here as a wrong value or an empty
/// string and nowhere else.
///
/// Covers both arms of the hand-out callee (the boxed payload and the
/// constructed fallback), the copy-supported twin, and the two neighbours
/// the fix must leave alone.
#[test]
fn e2e_boxed_payload_handed_out_of_a_match_arm_keeps_its_value() {
    let Some(out) = run_program(
            "struct X1 { a: Option[i64], s: String }\n\
             struct Ctl { s: String, n: i64 }\n\
             enum W { T(X1), U(i64) }\n\
             enum C { T(Ctl), U(i64) }\n\
             fn mkx(i: i64) -> X1 { return X1 { a: Option.Some(i), s: f\"s{i}\" }; }\n\
             fn mkc(i: i64) -> Ctl { return Ctl { s: f\"c{i}\", n: i }; }\n\
             fn payout(w: W) -> X1 { return match w { W.T(x) => x, W.U(n) => mkx(n) }; }\n\
             fn payoutc(c: C) -> Ctl { return match c { C.T(x) => x, C.U(n) => mkc(n) }; }\n\
             fn rebind(w: W) -> i64 { let v = w; return match v { W.T(x) => x.a.unwrap_or(0), W.U(n) => n }; }\n\
             fn store(w: W, out: mut ref Vec[W]) { out.push(w); }\n\
             fn main() {\n\
             \x20   let p = payout(W.T(mkx(23))); println(f\"a={p.a.unwrap_or(0)}/{p.s}\")\n\
             \x20   let q = payout(W.U(24)); println(f\"b={q.a.unwrap_or(0)}/{q.s}\")\n\
             \x20   let r = payoutc(C.T(mkc(25))); println(f\"c={r.n}/{r.s}\")\n\
             \x20   println(f\"d={rebind(W.T(mkx(26)))}\")\n\
             \x20   let mut v: Vec[W] = Vec.new(); store(W.T(mkx(27)), mut v); println(f\"e={v.len()}\")\n\
             \x20   println(\"end\")\n\
             }\n\
             ",
        ) else {
            return;
        };
    assert_eq!(out, "a=23/s23\nb=24/s24\nc=25/c25\nd=26\ne=1\nend\n");
}

/// B-2026-09-05-27 — a match arm handing a bare-tuple ELEMENT out of its
/// arm frees each buffer once. The arm's binding is a bit-copy of the
/// scrutinee's element; the hand-out zeroed the BINDING's caps and left
/// the scrutinee's element live, so the tuple drop at the merge freed the
/// buffers the handed-out value still held: two invalid frees per call on
/// every compiled backend (glibc aborts on the second call, valgrind
/// flags the first). The arm-tail and `return` hooks now cap-zero the
/// SOURCE element through `bare_tuple_elem_slots`, the map the rebind and
/// by-value-argument paths already consult.
///
/// `one`/`two` are the row's cells (one call, two calls), `three` the
/// explicit `return`, `four` a single-heap-field element, `five` the enum
/// spelling that was always clean, `six` named tuple locals, `seven` the
/// hand-out bound to a local (`let x = match ..`), `eight` a LOCAL
/// scrutinee, `nine` the rebind spelling that was always clean. The
/// bodies are unchanged from B-2026-09-02-24; the ASAN twin runs this
/// program under the sanitizer.
#[test]
fn e2e_match_arm_handing_out_a_tuple_element_frees_it_once() {
    let Some(out) = run_program(
            "struct R { id: i64, tag: String, xs: Vec[i64] }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\") } }\n\
             struct S1 { id: i64, tag: String }\n\
             impl Drop for S1 { fn drop(mut ref self) { println(f\"dS{self.id}\") } }\n\
             enum E { A(R), B }\n\
             fn mk(i: i64) -> R { return R { id: i, tag: f\"t{i}\", xs: [i] } }\n\
             fn mk1(i: i64) -> S1 { return S1 { id: i, tag: f\"t{i}\" } }\n\
             fn p4(t: (R, i64)) -> R { match t { (r, k) => { r } } }\n\
             fn p4r(t: (R, i64)) -> R { match t { (r, k) => { return r } } }\n\
             fn p1(t: (S1, i64)) -> S1 { match t { (r, k) => { r } } }\n\
             fn pe(e: E) -> R { match e { E.A(r) => { r }, E.B => mk(0) } }\n\
             fn pd(t: (R, i64)) -> R { let x: R = match t { (r, k) => r }; return x }\n\
             fn pl() -> R { let t: (R, i64) = (mk(13), 0); match t { (r, k) => { r } } }\n\
             fn pc(t: (R, i64)) -> R { match t { (r, k) => { let g: R = r; g } } }\n\
             fn main() {\n\
             \x20   { let a: R = p4((mk(3), 0)); println(f\"got{a.id}\"); println(\"one\") }\n\
             \x20   { let a: R = p4((mk(3), 0)); let b: R = p4((mk(6), 0)); println(f\"got{a.id}{b.id}\"); println(\"two\") }\n\
             \x20   { let a: R = p4r((mk(4), 0)); let b: R = p4r((mk(7), 0)); println(f\"got{a.id}{b.id}\"); println(\"three\") }\n\
             \x20   { let a: S1 = p1((mk1(5), 0)); let b: S1 = p1((mk1(8), 0)); println(f\"got{a.id}{b.id}\"); println(\"four\") }\n\
             \x20   { let a: R = pe(E.A(mk(9))); let b: R = pe(E.A(mk(10))); println(f\"got{a.id}{b.id}\"); println(\"five\") }\n\
             \x20   { let t: (R, i64) = (mk(11), 0); let a: R = p4(t); let u: (R, i64) = (mk(12), 0); let b: R = p4(u); println(f\"got{a.id}{b.id}\"); println(\"six\") }\n\
             \x20   { let a: R = pd((mk(14), 0)); println(f\"got{a.id}\"); println(\"seven\") }\n\
             \x20   { let a: R = pl(); println(f\"got{a.id}\"); println(\"eight\") }\n\
             \x20   { let a: R = pc((mk(15), 0)); println(f\"got{a.id}\"); println(\"nine\") }\n\
             \x20   println(\"end\")\n\
             }\n\
             ",
        ) else {
            return;
        };
    assert_eq!(out, "got3\ndR3\none\ngot36\ndR6\ndR3\ntwo\ngot47\ndR7\ndR4\nthree\ngot58\ndS8\ndS5\nfour\ngot910\ndR10\ndR9\nfive\ngot1112\ndR12\ndR11\nsix\ngot14\ndR14\nseven\ngot13\ndR13\neight\ngot15\ndR15\nnine\nend\n");
}

/// B-2026-09-10-16 — a MATCH-ARM PAYLOAD BINDING records its tuple's element
/// types, so a FIELD READ through it lowers instead of refusing.
///
/// `match o { Some(t) => t.0.id }` over an `Option[(R, R)]` failed `karac build`
/// with the loud "cannot resolve field 'id' on this receiver (its type was not
/// recorded for codegen)" while `karac check` accepted it and `--interp` answered
/// it. LOUD, so nothing was ever miscompiled — the build simply stopped, and the
/// natural spelling of "read a field of the arm-bound payload" was unavailable.
///
/// THE FOURTH SOURCE of a tuple's element types, and the last of the family: the
/// container element (B-2026-08-28-34), the struct field (B-2026-09-03-12) and the
/// `Array`-from-a-field (B-2026-09-04-28) were each taught to the place-chain
/// recorder one row at a time. The typechecker was ALREADY recording the whole
/// tuple `TypeExpr` for this binding, so the source existed and only codegen's
/// transcription of it was missing.
///
/// `u2` / `u3` / `u4` are the same binder reached three other ways — `Result`,
/// `if let`, `while let` — all of which failed identically before the fix and are
/// pinned here so a future narrowing to `match`-over-`Option` is caught.
///
/// `u5` / `u6` / `u7` are the over-reach controls, all three of which BUILT before
/// the fix and must not move: the destructuring arm (which binds elements rather
/// than the tuple), a bare `t.0` with no second hop (the tuple-index lowering reads
/// the LLVM aggregate directly and never needed a name — it is the SECOND hop that
/// had nothing to resolve against), and a METHOD call on the element, which
/// dispatches through a different table and so was never affected.
///
/// `u5` deliberately carries ONE `Drop`-bearing element rather than two. The
/// two-element spelling destructured out of a `match` arm dies in opposite orders
/// on the two backends (B-2026-09-06-21, open), which would pin a divergence here
/// that has nothing to do with this row.
///
/// THE ABSENT ELEMENT BODIES IN `u1`–`u4` AND `u7` ARE B-2026-09-10-14, NOT THIS
/// ROW. A whole-payload arm binding runs no element `Drop` body on EITHER backend
/// — both agree, memory is balanced, and that gap is filed and open. This test
/// pins the field READ resolving; it deliberately does not assert those bodies are
/// correct. Whoever fixes -14 should expect this string to need `dR1/t1`-shaped
/// additions and update it rather than treat the break as a regression.
///
/// Twin of `tests/interpreter.rs`'s
/// `test_arm_bound_tuple_payload_field_read_resolves`, pinned to the same string.
///
/// B-2026-09-10-14 — REPINNED, and the direction is the whole point: cells
/// u1 (`match`), u2 (`Result`), u3 (`if let`) and u7 (a method receiver and a
/// second element) GAINED the payload elements' `Drop` bodies, which this
/// string had been recording as absent. They were absent on BOTH backends, so
/// this fixture and its twin pinned the gap rather than a divergence, and the
/// new transcript is byte-identical on both again. u5 (a destructure, whose
/// leaves each own an element) and u6 (no `Drop` anywhere) are untouched,
/// which is what says the repin is the tuple whole-value binding and nothing
/// wider.
///
/// u4 stays bodiless and is NOT this row's: its scrutinee is a CALL
/// (`while let Some(t) = src(n)`), so the payload is a fresh temp with no
/// named place to keep a walk, and the disarm this row narrowed never runs
/// for it.
#[test]
fn e2e_arm_bound_tuple_payload_field_read_resolves() {
    let Some(out) = run_program(
        r#"struct R { id: i64, tag: String }
impl R { fn get(ref self) -> i64 { return self.id; } }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}/{self.tag}") } }
fn mk(i: i64) -> R { return R { id: i, tag: f"t{i}" }; }
fn src(i: i64) -> Option[(R, R)] { if i > 0 { return Some((mk(i), mk(i + 100))); } return None; }

fn u1() { let o: Option[(R, R)] = Some((mk(1), mk(101)));
          match o { Some(t) => { println(f"  a{t.0.id}/{t.0.tag}") } None => { println("  n") } } }
fn u2() { let o: Result[(R, R), i64] = Ok((mk(2), mk(102)));
          match o { Ok(t) => { println(f"  b{t.0.id}/{t.0.tag}") } Err(e) => { println(f"  e{e}") } } }
fn u3() { let o: Option[(R, R)] = Some((mk(3), mk(103)));
          if let Some(t) = o { println(f"  c{t.0.id}/{t.0.tag}") } else { println("  n") } }
fn u4() { let mut n = 4; while let Some(t) = src(n) { println(f"  d{t.0.id}/{t.0.tag}"); n = 0; } }
fn u5() { let o: Option[(R, i64)] = Some((mk(5), 50));
          match o { Some((a, b)) => { println(f"  e{a.id}/{a.tag}/{b}") } None => { println("  n") } } }
fn u6() { let o: Option[(i64, i64)] = Some((6, 60));
          match o { Some(t) => { println(f"  f{t.0}") } None => { println("  n") } } }
fn u7() { let o: Option[(R, R)] = Some((mk(7), mk(107)));
          match o { Some(t) => { println(f"  g{t.0.get()}/{t.1.tag}") } None => { println("  n") } } }

fn main() {
    println("u1"); u1(); println("u2"); u2(); println("u3"); u3();
    println("u4"); u4(); println("u5"); u5(); println("u6"); u6();
    println("u7"); u7(); println("end");
}
"#,
    ) else {
        return;
    };
    assert_eq!(
        out,
        r#"u1
  a1/t1
dR1/t1
dR101/t101
u2
  b2/t2
dR2/t2
dR102/t102
u3
  c3/t3
dR3/t3
dR103/t103
u4
  d4/t4
u5
  e5/t5/50
dR5/t5
u6
  f6
u7
  g7/t107
dR7/t7
dR107/t107
end
"#
    );
}

/// B-2026-09-10-21 — a NESTED tuple element inside a MATCH-ARM PAYLOAD
/// BINDING resolves, so `t.0.0.id` lowers instead of refusing.
///
/// The remainder of B-2026-09-10-16 (the ONE-hop read, `t.0.id`), and it failed
/// the same loud way one level in: `karac build` printed "cannot resolve field
/// 'id' on this receiver (its type was not recorded for codegen)" while `karac
/// check` accepted the program and `--interp` answered it. The cause is a data
/// shape, not a narrowing — that row's registry is name-valued
/// (`HashMap<String, Vec<Option<String>>>`) and a TUPLE element has no name, so
/// `((W, W), i64)` records `None` for element 0 BY CONSTRUCTION and the second
/// hop had nothing to resolve against.
///
/// STRICT `assert_eq!(…, Some(…))`, not the tolerant `let Some(out) = … else {
/// return }` its sibling uses, and that is deliberate: the pre-fix failure is a
/// CODEGEN refusal, so `run_program` returns `None` and the tolerant form
/// returns early and reports green. The one fixture shape that cannot see this
/// bug class is the one most of this file uses.
///
/// `n1`–`n3` are the three binders (`match` over `Option`, over `Result`, and
/// `if let`); `n5` is a THREE-level nest, which the row listed as not measured;
/// `n7` reads a METHOD through the nested element and a scalar sibling.
///
/// The controls are `n6` and `n8`, both of which BUILT before the fix and must
/// not move: a nested tuple of SCALARS (no field read to resolve, and the
/// fail-closed gate keeps it resolving to nothing), and the one-hop spelling the
/// parent row fixed.
///
/// `n4`'s missing bodies are B-2026-09-10-14's documented `while let` case — its
/// scrutinee is a CALL, so the payload is a fresh temp with no named place to
/// keep a walk.
///
/// THIS TRANSCRIPT IS NOT THE INTERPRETER'S, and the difference is
/// B-2026-09-17-20, not this row: the compiled backends run the nested payload
/// elements' `Drop` bodies (correct — the arm binding owns the payload) and the
/// interpreter runs NONE of them for a nested tuple, though it runs them for the
/// one-hop `n8`. The divergence was unobservable until this fix, because the
/// compiled side refused to build at all. Memory is balanced either way:
/// 48 allocs / 48 frees, 0 valgrind errors, "All heap blocks were freed" at
/// `KARAC_OPT_LEVEL=0`.
///
/// Twin: `tests/interpreter.rs`'s
/// `test_arm_bound_nested_tuple_payload_field_read_resolves`, pinned to the
/// INTERPRETER's transcript for the reason above.
#[test]
fn e2e_arm_bound_nested_tuple_payload_field_read_resolves() {
    assert_eq!(
        run_program(
            r#"struct W { id: i64, tag: String }
impl W { fn get(ref self) -> i64 { return self.id; } }
impl Drop for W { fn drop(mut ref self) { println(f"dW{self.id}/{self.tag}") } }
fn mk(i: i64) -> W { return W { id: i, tag: f"t{i}" }; }
fn src(i: i64) -> Option[((W, W), i64)] { if i > 0 { return Some(((mk(i), mk(i + 100)), 5)); } return None; }

fn n1() { let o: Option[((W, W), i64)] = Some(((mk(1), mk(101)), 5));
          match o { Some(t) => { println(f"  a{t.0.0.id}/{t.0.0.tag}") } None => { println("  n") } } }
fn n2() { let o: Result[((W, W), i64), i64] = Ok(((mk(2), mk(102)), 5));
          match o { Ok(t) => { println(f"  b{t.0.1.id}") } Err(e) => { println(f"  e{e}") } } }
fn n3() { let o: Option[((W, W), i64)] = Some(((mk(3), mk(103)), 5));
          if let Some(t) = o { println(f"  c{t.0.0.id}") } else { println("  n") } }
fn n4() { let mut k = 4; while let Some(t) = src(k) { println(f"  d{t.0.0.id}"); k = 0; } }
fn n5() { let o: Option[(((W, W), i64), i64)] = Some((((mk(5), mk(105)), 5), 6));
          match o { Some(t) => { println(f"  e{t.0.0.0.id}") } None => { println("  n") } } }
fn n6() { let o: Option[((i64, i64), i64)] = Some(((6, 60), 600));
          match o { Some(t) => { println(f"  f{t.0.0}/{t.0.1}/{t.1}") } None => { println("  n") } } }
fn n7() { let o: Option[((W, W), i64)] = Some(((mk(7), mk(107)), 5));
          match o { Some(t) => { println(f"  g{t.0.0.get()}/{t.1}") } None => { println("  n") } } }
fn n8() { let o: Option[(W, i64)] = Some((mk(8), 80));
          match o { Some(t) => { println(f"  h{t.0.id}") } None => { println("  n") } } }

fn main() {
    println("n1"); n1(); println("n2"); n2(); println("n3"); n3();
    println("n4"); n4(); println("n5"); n5(); println("n6"); n6();
    println("n7"); n7(); println("n8"); n8(); println("end");
}
"#
        ),
        Some(
            r#"n1
  a1/t1
dW1/t1
dW101/t101
n2
  b102
dW2/t2
dW102/t102
n3
  c3
dW3/t3
dW103/t103
n4
  d4
n5
  e5
dW5/t5
dW105/t105
n6
  f6/60/600
n7
  g7/5
dW7/t7
dW107/t107
n8
  h8
dW8/t8
end
"#
            .to_string()
        )
    );
}

#[test]
fn e2e_struct_pattern_destructure_of_owned_param_is_a_view() {
    let Some(out) = run_program(
        r#"struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
struct S  { r: R, k: i64 }
struct Hs { r: R, name: String }
struct In { r: R }
struct Ou { inner: In, k: i64 }

fn g1(s: S)  { let S { r, k } = s;      let m = r;  println(f"  b{m.id}"); println("  end") }
fn g2(s: S)  { let S { r: rr, k } = s;  let m = rr; println(f"  b{m.id}"); println("  end") }
fn g3(s: S)  { let S { r, .. } = s;     let m = r;  println(f"  b{m.id}"); println("  end") }
fn g4(h: Hs) { let Hs { r, name } = h;  let m = r;  println(f"  b{m.id} {name}"); println("  end") }
fn g5(o: Ou) { let Ou { inner, k } = o; let m = inner; println(f"  b{m.r.id}"); println("  end") }
fn g6(s: S)  { let S { r, k } = s;      println(f"  b{r.id}"); println("  end") }
fn g7(s: ref S) { println(f"  b{s.r.id}"); println("  end") }
fn g8()      { let s = S { r: R { id: 8 }, k: 0 }; let S { r, k } = s; let m = r; println(f"  b{m.id}"); println("  end") }

fn main() {
    println("plain");    g1(S { r: R { id: 1 }, k: 0 });  println("plain end")
    println("rename");   g2(S { r: R { id: 2 }, k: 0 });  println("rename end")
    println("rest");     g3(S { r: R { id: 3 }, k: 0 });  println("rest end")
    println("heapstr");  g4(Hs { r: R { id: 4 }, name: "nm" }); println("heapstr end")
    println("nested");   g5(Ou { inner: In { r: R { id: 5 } }, k: 0 }); println("nested end")
    println("norebind"); g6(S { r: R { id: 6 }, k: 0 });  println("norebind end")
    println("refparam"); let rs = S { r: R { id: 7 }, k: 0 }; g7(rs); println("refparam end")
    println("local");    g8();                             println("local end")
    println("done")
}
"#,
    ) else {
        return;
    };
    assert_eq!(
        out,
        r#"plain
  b1
  end
dR1
plain end
rename
  b2
  end
dR2
rename end
rest
  b3
  end
dR3
rest end
heapstr
  b4 nm
  end
dR4
heapstr end
nested
  b5
  end
dR5
nested end
norebind
  b6
  end
dR6
norebind end
refparam
  b7
  end
dR7
refparam end
local
  b8
dR8
  end
local end
done
"#
    );
}

#[test]
fn codegen_generic_destructured_payload_renders_at_its_instantiation() {
    let Some(out) = run_program(
        r#"fn show[T: Display](x: Option[T]) { match x { Some(t) => { println(f"{t}") } None => { println("n") } } }
struct H { }
impl H {
    fn m[T: Display](ref self, x: Option[T]) { match x { Some(t) => { println(f"m:{t}") } None => { println("n") } } }
}
fn showr[T: Display](x: Result[T, String]) { match x { Ok(t) => { println(f"r:{t}") } Err(e) => { println(e) } } }
fn main() {
    let a2: Array[i64, 2] = [1, 2];
    let a3: Array[i64, 3] = [7, 8, 9];
    let s2: Slice[i64] = a2[0..2];
    let mut v: Vec[i64] = Vec.new(); v.push(4); v.push(5);
    let vs: Vec[String] = ["ab", "cd"];
    let vs2: Vec[String] = ["ef"];
    let sv: Slice[String] = vs.as_slice();
    let t2: (i64, i64) = (5, 6);
    let v4: Vector[i64, 4] = Vector[i64, 4](1, 2, 3, 4);
    show(Some(a2));
    show(Some(a3));
    show(Some(s2));
    show(Some(v));
    show(Some(sv));
    show(Some(vs2));
    show(Some(t2));
    show(Some(v4));
    show(Some(7));
    show(Some("hi"));
    let h = H { };
    h.m(Some(a3));
    showr(Ok(a2));
    let n: Option[i64] = None;
    show(n);
}
"#,
    ) else {
        return;
    };
    assert_eq!(out, "[1, 2]\n[7, 8, 9]\n[1, 2]\n[4, 5]\n[ab, cd]\n[ef]\n(5, 6)\nVector(1, 2, 3, 4)\n7\nhi\nm:[7, 8, 9]\nr:[1, 2]\nn\n");
}

/// B-2026-08-31-39 — the SHADOW control for the fixture above.
///
/// The concrete payload type is recorded under the binding's NAME, because
/// the span-keyed sibling is keyed by the PATTERN and the display tables
/// want the span of each interpolation hole. A name-keyed record outlives
/// the arm that made it, so a later local of the same name must not render
/// at the payload's shape: without the slot-type guard,
/// `let t: i64 = 42; println(f"{t}")` after the match printed `[42, 1]`.
#[test]
fn codegen_generic_destructured_payload_record_does_not_leak_to_a_shadowing_local() {
    let Some(out) = run_program(
        r#"fn show[T: Display](x: Option[T]) {
    match x { Some(t) => { println(f"{t}") } None => { println("n") } }
    let t: i64 = 42;
    println(f"{t}")
}
fn main() { let a: Array[i64, 2] = [1, 2]; show(Some(a)); show(Some(9)) }
"#,
    ) else {
        return;
    };
    assert_eq!(out, "[1, 2]\n42\n9\n42\n");
}

/// B-2026-07-31-38 (enum sibling) — a plain value enum whose walker
/// action was retracted by a move (whole-value `let c = b;`, or a match
/// arm's payload move-out) gets it RE-ARMED on reassign, so the fresh
/// value's payload body fires at exit under `karac build` exactly as the
/// interpreter fires it (both `drop 4 s4` and `drop 6 s6` were
/// AOT-silent). Twin of `tests/interpreter.rs`'s
/// `test_enum_walker_rearm_after_move_reassign`.
#[test]
fn e2e_enum_walker_rearm_after_move_reassign() {
    let Some(out) = run_program(
        "struct Res { id: i64, name: String }\n\
             impl Drop for Res {\n\
             \x20   fn drop(mut ref self) {\n\
             \x20       println(f\"drop {self.id} {self.name}\")\n\
             \x20   }\n\
             }\n\
             enum SBox { Full(Res), Empty }\n\
             fn mk(n: i64) -> SBox {\n\
             \x20   return SBox.Full(Res { id: n, name: f\"s{n}\" });\n\
             }\n\
             fn use_res(r: Res) {\n\
             \x20   println(f\"took {r.id}\");\n\
             }\n\
             fn main() {\n\
             \x20   println(\"a\");\n\
             \x20   let mut b = mk(3);\n\
             \x20   let c = b;\n\
             \x20   b = mk(4);\n\
             \x20   println(\"m\");\n\
             \x20   let mut d = mk(5);\n\
             \x20   match d {\n\
             \x20       SBox.Full(r) => { use_res(r); }\n\
             \x20       SBox.Empty => {}\n\
             \x20   }\n\
             \x20   d = mk(6);\n\
             \x20   println(\"end\");\n\
             }\n",
    ) else {
        return;
    };
    assert_eq!(
        out,
        "a\ndrop 3 s3\ndrop 4 s4\nm\ntook 5\ndrop 5 s5\ndrop 6 s6\nend\n"
    );
}

/// B-2026-08-28-66 — a consuming `match` arm that hands a BOXED struct
/// payload on must not leave the source box's interior walk armed.
///
/// `Option[R]` with `R = { i64, String }` is 4 words, wider than the
/// `Option` inline payload area (3), so the payload is heap-BOXED. The
/// box's `BoxedEnumDrop` ran `__karac_drop_struct_R` over the interior,
/// freeing `name`, while the arm handed the whole payload to the match's
/// destination whose own `karac_drop_R` freed `name` too. Measured
/// pre-fix: `Invalid free()`, 11 allocs / 12 frees under valgrind at
/// `-O0`.
///
/// ASSERTED ON THE IR, and that choice is the row rather than a
/// convenience. Every runtime gate available in this tree is blind to it:
///
///   * At the DEFAULT `-O2` the optimizer erases the doubled malloc/free
///     pair outright — 9 allocs / 9 frees, clean — so `run_program`
///     comparisons and the ASAN suite both PASS against the broken
///     compiler. Adding a `String` read to keep the buffer live does not
///     change that (checked).
///   * The `-O0` ASAN leg does not flag it either (checked).
///   * Under the JIT it aborts with `free(): double free detected in
///     tcache 2` — but only sometimes: whether glibc's tcache NOTICES the
///     invalid free depends on the surrounding allocation pattern. The
///     same program with one extra interpolated `println` runs to
///     completion on the broken compiler. So an abort-based assertion is
///     allocator-state dependent and would be a flaky gate, not a gate.
///
/// The IR is where the defect is unambiguous and deterministic. The
/// hand-over does not DELETE the box's interior walk — the walk is still
/// called, and the box itself is still freed; what changes is that each
/// field's cap is zeroed inside the box first, so the walk frees nothing
/// and the destination's own drop is the single owner. So the assertion is
/// on the `boxwhole.suppress` blocks that emit those stores, which is the
/// hand-over's only signature in the IR.
///
/// The control is the one that fixing this got wrong first: an arm that
/// BINDS the payload and yields something else must KEEP the walk, since
/// the box still owns the fields. Disarming there leaked them (13 allocs /
/// 12 frees where 13 / 13 is right), which is why the gate is the positive
/// signal "the arm's value IS this binding, and someone downstream takes
/// it" rather than any consumption test.
#[test]
fn e2e_consuming_arm_boxed_payload_handed_on_disarms_source_box_walk() {
    // The arm's value IS the bound payload and `k` takes it: the
    // destination owns the fields, so the source box must not walk them.
    let handed_on = ir_for(
        "struct R { id: i64, name: String }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\"); } }\n\
             fn main() {\n\
               let o: Option[R] = Some(R { id: 1, name: f\"n1\" });\n\
               let k = match o { Some(r) => { r } None => { R { id: 9, name: f\"n9\" } } };\n\
               println(f\"kept {k.id}\");\n\
             }\n",
    );
    assert!(
        handed_on.contains("boxwhole.suppress"),
        "no hand-over emitted for a boxed payload the arm hands on:\n{handed_on}"
    );
    // CONTROL — the arm binds the payload but yields something else, so
    // nothing downstream took it and the box must still own the fields.
    let not_handed_on = ir_for(
        "struct R { id: i64, name: String }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\"); } }\n\
             fn main() {\n\
               let q: Option[R] = Some(R { id: 5, name: f\"n5\" });\n\
               let n = match q { Some(r) => { R { id: 7, name: f\"n7\" } } \
                                 None => { R { id: 8, name: f\"n8\" } } };\n\
               println(f\"other {n.id}\");\n\
             }\n",
    );
    assert!(
        !not_handed_on.contains("boxwhole.suppress"),
        "hand-over wrongly emitted for a payload the arm does NOT hand on:\n{not_handed_on}"
    );
}

/// B-2026-08-29-6 — a PASSTHROUGH CALL used DIRECTLY AS A MATCH SCRUTINEE,
/// with the result never bound, leaves the payload owned by the named
/// source alone.
///
/// Pre-fix the arm binding registered its own free on top of the source's
/// and both released the same buffer, aborting the process on all three
/// compiled backends — for a FREE FUNCTION and a method alike — so
/// `run_program` returns `None` here rather than wrong bytes. The memory
/// twin is `asan_passthrough_match_scrutinee_leaves_the_source_sole_owner`,
/// which also pins that the fix's two halves are each independently
/// necessary.
#[test]
fn e2e_passthrough_match_scrutinee_leaves_the_source_sole_owner() {
    const DECLS: &str = "struct Bx { n: i64 }\n\
             impl Bx {\n\
             fn take(ref self, o: Option[String]) -> Option[String] { o }\n\
             fn take_res(ref self, o: Result[String, i64]) -> Result[String, i64] { o }\n\
             fn fresh(ref self, o: Option[String]) -> Option[String] { Option.Some(f\"fresh{self.n}\") }\n\
             }\n\
             fn takef(o: Option[String]) -> Option[String] { o }\n";
    for (label, body, want) in [
        // FREE FUNCTION — the spelling that shows this is not the
        // method-path gap B-2026-08-29-4 closed.
        (
            "free-fn-scrutinee-passthrough",
            "fn main() { let n = 1; let s = Option.Some(f\"a{n}\"); \
                 match takef(s) { Option.Some(v) => { println(f\"got {v}\"); } \
                 Option.None => { println(\"none\"); } } }\n",
            "got a1\n",
        ),
        (
            "method-scrutinee-passthrough",
            "fn main() { let b = Bx { n: 1 }; let s = Option.Some(f\"a{b.n}\"); \
                 match b.take(s) { Option.Some(v) => { println(f\"got {v}\"); } \
                 Option.None => { println(\"none\"); } } }\n",
            "got a1\n",
        ),
        // The `Result` payload, which needs the SECOND half of the fix (the
        // fresh-temp inline-`Result` registrar declining); the `Option`
        // cases above are fixed by the scrutinee classification alone.
        (
            "result-scrutinee-passthrough",
            "fn main() { let b = Bx { n: 1 }; \
                 let s: Result[String, i64] = Result.Ok(f\"c{b.n}\"); \
                 match b.take_res(s) { Result.Ok(v) => { println(f\"got {v}\"); } \
                 Result.Err(e) => { println(f\"err {e}\"); } } }\n",
            "got c1\n",
        ),
        // CONTROL — no passthrough, so the scrutinee temp genuinely owns a
        // freshly produced payload and must KEEP its registration. A fix
        // that declined unconditionally would leak this one instead.
        (
            "no-passthrough-scrutinee-keeps-owner",
            "fn main() { let b = Bx { n: 1 }; let s = Option.Some(f\"h{b.n}\"); \
                 match b.fresh(s) { Option.Some(v) => { println(f\"got {v}\"); } \
                 Option.None => { println(\"none\"); } } }\n",
            "got fresh1\n",
        ),
        // CONTROL — the `let`-bound spelling, correct since B-2026-08-29-4.
        // Both routes now classify the same call; if they disagreed the
        // payload would end up with two owners again, or none.
        (
            "let-bound-spelling-still-correct",
            "fn main() { let b = Bx { n: 1 }; let s = Option.Some(f\"k{b.n}\"); \
                 let o = b.take(s); \
                 match o { Option.Some(v) => { println(f\"got {v}\"); } \
                 Option.None => { println(\"none\"); } } }\n",
            "got k1\n",
        ),
    ] {
        assert_eq!(
            run_program(&format!("{DECLS}{body}")).as_deref(),
            Some(want),
            "{label}"
        );
    }
}

/// B-2026-08-29-58, codegen leg — the compiled ORACLE for the interpreter's
/// `test_arm_payload_assigned_to_outer_local_drop_body_runs_once`.
///
/// Every expectation here is what `karac build` already produced BEFORE the
/// fix: the defect was interpreter-only, so this half is a pin against
/// regressing the side that was right. That is exactly why it is worth
/// carrying — the fix had two possible shapes reaching one body (suppress
/// the callee's slot, or disarm the caller's walk) and they differ in WHERE
/// the body fires. `assign-place-pinned` is the case that tells them apart,
/// and without this twin a future change could satisfy the interpreter test
/// by moving the compiled backends instead.
///
/// The interpreter file additionally carries an `arm-not-taken` case that is
/// deliberately NOT here: both compiled backends lose that body, tracked on
/// its own row, and encoding it as an expectation would freeze the defect.
#[test]
fn e2e_arm_payload_assigned_to_outer_local_drops_once() {
    const DROPPER: &str = "struct R { id: i64, tag: String }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\"); } }\n\
             enum E { A(R), B }\n\
             impl Drop for E { fn drop(mut ref self) { println(\"dE\"); } }\n";
    for (label, body, want) in [

            // The row's headline program. Pre-fix the interpreter printed an extra
            // `dR8` between `mid` and `dE` -- its own slot for `out` firing on top of
            // the caller's payload walk.
            (
                "assign-nonescaping",
                "#[allow(partial_move_of_drop_enum)]\nfn dies(b: E) -> i64 {\n\
                     let mut out: R = R { id: 0, tag: f\"t0\" };\n\
                     match b { E.A(r) => { out = r; } E.B => { } }\n\
                     println(\"mid\");\n\
                     return out.id;\n\
                 }\n\
                 #[allow(partial_move_of_drop_enum)]\nfn main() { let c1: E = E.A(R { id: 8, tag: f\"t8\" }); let v1: i64 = dies(c1); println(f\"v{v1}\"); }\n",
                "dR0\nmid\ndE\ndR8\nv8\n",
            ),
            // The `if let` spelling binds through a separate path in this backend and
            // needed confirming separately; it doubled identically pre-fix.
            (
                "assign-iflet",
                "#[allow(partial_move_of_drop_enum)]\nfn dies(b: E) -> i64 {\n\
                     let mut out: R = R { id: 0, tag: f\"t0\" };\n\
                     if let E.A(r) = b { out = r; }\n\
                     println(\"m\");\n\
                     return out.id;\n\
                 }\n\
                 #[allow(partial_move_of_drop_enum)]\nfn main() { let c: E = E.A(R { id: 8, tag: f\"t8\" }); let v: i64 = dies(c); println(f\"v{v}\"); }\n",
                "dR0\nm\ndE\ndR8\nv8\n",
            ),
            // PINS THE PLACE, not just the count, which is what separates this fix from
            // one that merely reaches the right total. `loc` dies at its last use and
            // `pre-ret` / `post-call` bracket the callee's exit, so a body fired at the
            // arm lands before `mid`, one at the callee's own slot between `pre-ret` and
            // `dE`, and the caller's walk after `dE`. Pre-fix there was a `dR8` in the
            // middle slot as well as the last one.
            (
                "assign-place-pinned",
                "#[allow(partial_move_of_drop_enum)]\nfn dies(b: E) -> i64 {\n\
                     let mut out: R = R { id: 0, tag: f\"t0\" };\n\
                     match b { E.A(r) => { out = r; } E.B => { } }\n\
                     println(\"mid\");\n\
                     let loc: R = R { id: 9, tag: f\"t9\" };\n\
                     let s: i64 = out.id + loc.id;\n\
                     println(\"pre-ret\");\n\
                     return s;\n\
                 }\n\
                 #[allow(partial_move_of_drop_enum)]\nfn main() { let c1: E = E.A(R { id: 8, tag: f\"t8\" }); let v1: i64 = dies(c1); println(\"post-call\"); println(f\"v{v1}\"); }\n",
                "dR0\nmid\ndR9\npre-ret\ndE\ndR8\npost-call\nv17\n",
            ),
            // Assigned TWICE. The displacement body must fire for the value the target
            // genuinely owned (`R{0}`) and NOT for the view it later held (`R{8}`), which
            // is the caller's to run -- so silencing through the whole-binding set has to
            // cover the displacement fire too, not only the slot.
            (
                "assign-repeated",
                "#[allow(partial_move_of_drop_enum)]\nfn dies(b1: E, b2: E) -> i64 {\n\
                     let mut out: R = R { id: 0, tag: f\"t0\" };\n\
                     match b1 { E.A(r) => { out = r; } E.B => { } }\n\
                     println(\"m1\");\n\
                     match b2 { E.A(r) => { out = r; } E.B => { } }\n\
                     println(\"m2\");\n\
                     return out.id;\n\
                 }\n\
                 #[allow(partial_move_of_drop_enum)]\nfn main() { let v: i64 = dies(E.A(R { id: 8, tag: f\"t8\" }), E.A(R { id: 9, tag: f\"t9\" })); println(f\"v{v}\"); }\n",
                "dR0\nm1\nm2\ndE\ndR9\ndE\ndR8\nv9\n",
            ),
            // A FRESH-TEMP argument rather than a named binding: the caller's fire is
            // `run_fresh_temp_arg_drops` instead of a binding's own NLL drop, and the
            // stand-down is correct against both.
            (
                "assign-fresh-temp-arg",
                "#[allow(partial_move_of_drop_enum)]\nfn dies(b: E) -> i64 {\n\
                     let mut out: R = R { id: 0, tag: f\"t0\" };\n\
                     match b { E.A(r) => { out = r; } E.B => { } }\n\
                     println(\"m\");\n\
                     return out.id;\n\
                 }\n\
                 #[allow(partial_move_of_drop_enum)]\nfn main() { let v: i64 = dies(E.A(R { id: 8, tag: f\"t8\" })); println(f\"v{v}\"); }\n",
                "dR0\nm\ndE\ndR8\nv8\n",
            ),
            // GUARD RAIL -- the arm assigns a FRESH value, not the payload, so nothing is
            // a view and every body stays armed. This is what keeps the gate from firing
            // on assignment generally.
            (
                "guard-fresh-value-assigned",
                "fn f(b: E) -> i64 {\n\
                     let mut out: R = R { id: 0, tag: f\"t0\" };\n\
                     match b { E.A(r) => { out = R { id: 7, tag: f\"t7\" }; } E.B => { } }\n\
                     println(\"m\");\n\
                     return out.id;\n\
                 }\n\
                 #[allow(partial_move_of_drop_enum)]\nfn main() { let c: E = E.A(R { id: 8, tag: f\"t8\" }); let v: i64 = f(c); println(f\"v{v}\"); }\n",
                "dR0\nm\ndR7\ndE\ndR8\nv7\n",
            ),
            // GUARD RAIL -- a LOCAL scrutinee has no caller behind it, so the assigned-into
            // local really is the only owner and MUST keep its body. Gating on the owned-
            // param view set is what buys this.
            (
                "guard-local-scrutinee",
                "#[allow(partial_move_of_drop_enum)]\nfn main() {\n\
                     let o: E = E.A(R { id: 8, tag: f\"t8\" });\n\
                     let mut out: R = R { id: 0, tag: f\"t0\" };\n\
                     match o { E.A(r) => { out = r; } E.B => { } }\n\
                     println(\"m\");\n\
                     println(f\"v{out.id}\");\n\
                     println(\"end\");\n\
                 }\n",
                "dR0\ndE\nm\nv8\ndR8\nend\n",
            ),
            // GUARD RAIL -- a fresh-temp SCRUTINEE is owned outright by the match, same
            // reasoning as the local one.
            (
                "guard-fresh-temp-scrutinee",
                "fn mk() -> E { return E.A(R { id: 8, tag: f\"t8\" }); }\n\
                 #[allow(partial_move_of_drop_enum)]\nfn main() {\n\
                     let mut out: R = R { id: 0, tag: f\"t0\" };\n\
                     match mk() { E.A(r) => { out = r; } E.B => { } }\n\
                     println(\"m\");\n\
                     println(f\"v{out.id}\");\n\
                     println(\"end\");\n\
                 }\n",
                "dR0\ndE\nm\nv8\ndR8\nend\n",
            ),
            // GUARD RAIL -- a second owned param that is never matched keeps its own
            // caller-side fire, so the view propagation has to be per-binding rather than
            // per-frame.
            (
                "guard-second-param-unmatched",
                "#[allow(partial_move_of_drop_enum)]\nfn take(b: E, p: R) -> i64 {\n\
                     let mut out: R = R { id: 0, tag: f\"t0\" };\n\
                     match b { E.A(r) => { out = r; } E.B => { } }\n\
                     println(\"m\");\n\
                     return out.id + p.id;\n\
                 }\n\
                 #[allow(partial_move_of_drop_enum)]\nfn main() { let c: E = E.A(R { id: 8, tag: f\"t8\" }); let v: i64 = take(c, R { id: 2, tag: f\"t2\" }); println(f\"v{v}\"); }\n",
                "dR0\nm\ndR2\ndE\ndR8\nv10\n",
            ),
            // GUARD RAIL -- B-2026-08-29-48's case, the opposite boundary: the assigned-into
            // local IS returned, so the value escapes and the caller's result binding owns
            // it. That row made the caller stand down here; this fix must not disturb it.
            (
                "guard-assigned-and-returned",
                "#[allow(partial_move_of_drop_enum)]\nfn takes(b: E) -> R {\n\
                     let mut out: R = R { id: 0, tag: f\"t0\" };\n\
                     match b { E.A(r) => { out = r; } E.B => { } }\n\
                     println(\"mid\");\n\
                     return out;\n\
                 }\n\
                 #[allow(partial_move_of_drop_enum)]\nfn main() { let c1: E = E.A(R { id: 8, tag: f\"t8\" }); let v1: R = takes(c1); println(\"post-call\"); println(f\"v{v1.id}\"); }\n",
                "dR0\nmid\ndE\npost-call\nv8\ndR8\n",
            ),
            // CONTROL -- the `let` spelling of the same move, correct since B-2026-08-29-17
            // and unchanged here. It is the oracle that made the assignment spelling the
            // anomaly rather than a matter of preference.
            (
                "control-let-twin",
                "#[allow(partial_move_of_drop_enum)]\nfn dies(b: E) -> i64 {\n\
                     let mut k: i64 = 0;\n\
                     match b { E.A(r) => { let inner: R = r; k = inner.id; } E.B => { } }\n\
                     println(\"mid\");\n\
                     return k;\n\
                 }\n\
                 #[allow(partial_move_of_drop_enum)]\nfn main() { let c1: E = E.A(R { id: 8, tag: f\"t8\" }); let v1: i64 = dies(c1); println(f\"v{v1}\"); }\n",
                "mid\ndE\ndR8\nv8\n",
            ),
        ] {
            assert_eq!(
                run_program(&format!("{DROPPER}{body}")).as_deref(),
                Some(want),
                "{label}"
            );
        }
}

/// B-2026-08-28-12 — a WILDCARD leaf of a `let` destructure runs the
/// discarded value's user `Drop` body exactly once.
///
/// Pre-fix it ran ZERO times, against design.md § Drop's "a value is
/// dropped exactly once, at the live-range end of its final owner". A
/// wildcard binds no name, so `push_drops_for_stmt` (interp) registered no
/// slot and codegen's leaf walkers — which key off a binding name — fell
/// through every arm. Codegen's tuple `Wildcard` arm did exist but only
/// freed a `{ptr,len,cap}` buffer; it never ran a body.
///
/// Backend-consistent on most rows, so no A/B gate could see it — except
/// `struct-place`, which was interp 0 / compiled 1. That single split is
/// why the family was reachable at all, and it is also why the fix is not
/// symmetric: the compiled side of that one spelling was ALREADY right, so
/// the struct arm fires for a fresh source only. Firing it for a place
/// source too would take the compiled backends to two and re-open the
/// split from the other side.
///
/// The three PARAM/MATCH rows are what constrain the fix rather than
/// merely passing. A by-value param source was already correct at one
/// body — the caller owns the entry copy under caller-retains and fires it
/// — so a leaf fire there would double it; both backends reuse the gate
/// they already apply to BINDING leaves (`let_destructures_owned_param` /
/// `owner_runs_bodies`) rather than introducing a second one. And
/// `match-arm-control` shows the defect was specific to `let`: the match
/// path ran a wildcard's body correctly all along.
///
/// `param-tuple-control` / `param-struct-control` carry COMPILED
/// expectations; the interpreter twin pins the same counts in the other
/// order. That order split is B-2026-08-28-19 (the caller-side walk fires
/// at the call in the interpreter and at scope exit compiled), predates
/// this fix, and is untouched by it — the gate above means neither row's
/// body is one this fix places. Twin: `tests/interpreter.rs`'s
/// `test_wildcard_destructure_leaf_user_drop_body_runs_once`.
///
/// One member of the family is deliberately absent: a wildcard field over
/// a struct LITERAL source (`let W { r: _, n } = W { .. }`) still runs
/// zero bodies compiled. It is blocked behind a larger, separate defect
/// (B-2026-08-28-29): a destructure of a FRESH STRUCT source loses even a
/// BOUND field's body, because `expr_yields_fresh_owned_temp` admits only
/// `Call`/`MethodCall` and a struct literal therefore reaches no leaf path
/// at all. Filed rather than pinned wrong here.
#[test]
fn e2e_wildcard_destructure_leaf_user_drop_body_runs_once() {
    const DROPPER: &str = "struct R { id: i64 }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"drop {self.id}\") } }\n";
    for (label, body, want) in [
            // The row's own repro — a tuple LOCAL (place) source.
            (
                "tuple-place",
                "fn main() { let p = (R { id: 41 }, 1); let (_, n) = p; println(f\"{n}\") }\n",
                "drop 41\n1\n",
            ),
            // Fresh tuple LITERAL source.
            (
                "tuple-fresh-literal",
                "fn main() { let (_, n) = (R { id: 41 }, 1); println(f\"{n}\") }\n",
                "drop 41\n1\n",
            ),
            // Fresh CALL source.
            (
                "tuple-fresh-call",
                "fn mk() -> (R, i64) { (R { id: 41 }, 1) }\n\
                 fn main() { let (_, n) = mk(); println(f\"{n}\") }\n",
                "drop 41\n1\n",
            ),
            // The STRUCT spelling over a fresh call source.
            (
                "struct-fresh-call",
                "struct W { r: R, n: i64 }\n\
                 fn mk() -> W { W { r: R { id: 41 }, n: 1 } }\n\
                 fn main() { let W { r: _, n } = mk(); println(f\"{n}\") }\n",
                "drop 41\n1\n",
            ),
            // The STRUCT spelling over a place source — the one row that was a
            // run-vs-build split, and where compiled was the correct side.
            (
                "struct-place",
                "struct W { r: R, n: i64 }\n\
                 fn main() { let w = W { r: R { id: 41 }, n: 1 };\n\
                 \x20            let W { r: _, n } = w; println(f\"{n}\") }\n",
                "drop 41\n1\n",
            ),
            // Two droppers, one wildcarded: the discarded one dies at the
            // destructure, the bound one at its last use. One body each.
            (
                "two-droppers-one-wildcard",
                "fn main() { let p = (R { id: 41 }, R { id: 42 });\n\
                 \x20            let (_, b) = p; println(f\"{b.id}\") }\n",
                "drop 41\n42\ndrop 42\n",
            ),
            // BOTH elements wildcarded — pre-fix this ran zero bodies for two
            // objects, so it pins that the fix is per-leaf and not "the first
            // wildcard wins".
            (
                "both-wildcards",
                "fn main() { let p = (R { id: 41 }, R { id: 42 });\n\
                 \x20            let (_, _) = p; println(\"x\") }\n",
                "drop 41\ndrop 42\nx\n",
            ),
            // CONTROL — a by-value TUPLE param source. Already correct at one
            // body (the caller owns the entry copy); a leaf fire would double it.
            (
            // ORDER CORRECTED by B-2026-08-29-55 -- the argument temp dies AS THE
            // CALL RETURNS (design.md's "Function/method call argument | After the
            // call returns"), which is BEFORE the enclosing `println` runs. The old
            // expectation held it to the statement's `;`; `--interp` printed the
            // body first all along, so that line recorded a run-vs-build divergence
            // rather than a decision.
                "param-tuple-control",
                "fn take(p: (R, i64)) -> i64 { let (_, n) = p; n }\n\
                 fn main() { println(f\"{take((R { id: 41 }, 1))}\") }\n",
                "drop 41\n1\n",
            ),
            // CONTROL — the same for a by-value STRUCT param.
            (
            // ORDER CORRECTED by B-2026-08-29-55 -- the argument temp dies AS THE
            // CALL RETURNS (design.md's "Function/method call argument | After the
            // call returns"), which is BEFORE the enclosing `println` runs. The old
            // expectation held it to the statement's `;`; `--interp` printed the
            // body first all along, so that line recorded a run-vs-build divergence
            // rather than a decision.
                "param-struct-control",
                "struct W { r: R, n: i64 }\n\
                 fn take(w: W) -> i64 { let W { r: _, n } = w; n }\n\
                 fn main() { println(f\"{take(W { r: R { id: 41 }, n: 1 })}\") }\n",
                "drop 41\n1\n",
            ),
            // An ENUM element's live-variant payload bodies, through the
            // discard. The bodies helper early-returned on any non-struct, so
            // every enum source ran no body compiled while the interpreter ran
            // one — a run-vs-build divergence left behind by this row's first
            // fix, on all three sources.
            (
                "enum-payload-place",
                "enum E { A(R), B }\n\
                 fn main() { let p = (E.A(R { id: 41 }), 1); let (_, n) = p; println(f\"{n}\") }\n",
                "drop 41\n1\n",
            ),
            (
                "enum-payload-fresh-literal",
                "enum E { A(R), B }\n\
                 fn main() { let (_, n) = (E.A(R { id: 41 }), 1); println(f\"{n}\") }\n",
                "drop 41\n1\n",
            ),
            (
                "enum-payload-fresh-call",
                "enum E { A(R), B }\n\
                 fn mk() -> (E, i64) { (E.A(R { id: 41 }), 1) }\n\
                 fn main() { let (_, n) = mk(); println(f\"{n}\") }\n",
                "drop 41\n1\n",
            ),
            // A wildcard inside a NESTED tuple pattern, both sources. The
            // place-source walker recursed and the fresh path did not, and the
            // interpreter stopped at the top level — so this shape diverged in
            // one direction from a place source and agreed on ZERO from a
            // literal. Both sides recurse now.
            (
                "nested-wildcard-place",
                "fn main() { let p = ((R { id: 41 }, 2), 1); let ((_, m), n) = p; println(f\"{m + n}\") }\n",
                "drop 41\n3\n",
            ),
            (
                "nested-wildcard-fresh-literal",
                "fn main() { let ((_, m), n) = ((R { id: 41 }, 2), 1); println(f\"{m + n}\") }\n",
                "drop 41\n3\n",
            ),
            // CONTROL — a `match` arm's wildcard, correct before and after.
            // This is what shows the defect was specific to `let`.
            (
                "match-arm-control",
                "fn main() { let p = (R { id: 41 }, 1);\n\
                 \x20            match p { (_, n) => { println(f\"{n}\") } } }\n",
                "1\ndrop 41\n",
            ),
            // CONTROL — the wildcard lands on the NON-dropping element.
            (
                "wildcard-on-non-dropper-control",
                "fn main() { let p = (R { id: 41 }, 1); let (r, _) = p; println(f\"{r.id}\") }\n",
                "41\ndrop 41\n",
            ),
            // CONTROL — the WHOLE-pattern wildcard, correct since
            // B-2026-07-30-11. The level this fix reaches one step down from.
            (
                "whole-pattern-wildcard-control",
                "fn main() { let _ = R { id: 41 }; println(\"x\") }\n",
                "drop 41\nx\n",
            ),
            // CONTROL — no destructure at all; the local's own drop runs it.
            (
                "no-destructure-control",
                "fn main() { let p = (R { id: 41 }, 1); println(f\"{p.1}\") }\n",
                "1\ndrop 41\n",
            ),
        ] {
            let prog = format!("{DROPPER}{body}");
            assert_eq!(run_program(&prog).as_deref(), Some(want), "{label}");
        }
}

/// B-2026-08-28-31 — an enum declaring its OWN `impl Drop`, discarded by a
/// wildcard destructure leaf, runs that body on the compiled backends.
///
/// Pre-fix the interpreter ran `drop E` and both compiled backends ran
/// nothing. B-2026-08-28-30 had wired a payload walker for payload-only
/// enums and deliberately EXCLUDED an own-`Drop` enum, because pointing the
/// payload walker at one prints the payload's body (`drop R41`) instead of
/// the enum's own — a different divergence, not a smaller one.
///
/// The right walker turns out to be the bodies-only one the STRUCT leaf
/// already uses: for an enum name it finds `owns_body` true and the
/// struct-field walk `None`, so it emits a call to `<E>.drop` and nothing
/// else. `payload-only-control` pins that the two walkers stay separate.
///
/// -31 stopped at ONE body — the enum's own — because the STRUCT-pattern
/// sibling did too, and matching the sibling was the smaller step.
/// B-2026-08-28-40 then closed the remaining gap in BOTH spellings at once:
/// the bodies-only walker gained an enum-payload leg, so an own-`Drop` enum
/// discarded through either pattern runs its own body and then its live
/// payload's. `struct-field-sibling` still holds the two spellings together;
/// it now expects two, like its tuple counterpart.
///
/// `bound-control` is what the target is measured against: a NON-discarded
/// enum of the same type, both bodies, on every backend. Each discard row
/// above now prints exactly that.
#[test]
fn e2e_own_drop_enum_discarded_by_wildcard_leaf_runs_its_body() {
    const H: &str = "enum E { A(R), B }\n\
             impl Drop for E { fn drop(mut ref self) { println(\"drop E\") } }\n\
             struct R { id: i64 }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"drop R{self.id}\") } }\n";
    for (label, body, want) in [
        // The row's own repro.
        (
            "place-source",
            "fn main() { let p = (E.A(R { id: 41 }), 1); let (_, n) = p;\n\
                 \x20            println(f\"{n}\"); }\n",
            "drop E\ndrop R41\n1\n",
        ),
        (
            "fresh-tuple-source",
            "fn main() { let (_, n) = (E.A(R { id: 41 }), 1); println(f\"{n}\"); }\n",
            "drop E\ndrop R41\n1\n",
        ),
        // A payloadless variant — the compiled zero was about the OWN body,
        // not about reaching a payload.
        (
            "no-payload-variant",
            "fn main() { let p = (E.B, 1); let (_, n) = p; println(f\"{n}\"); }\n",
            "drop E\n1\n",
        ),
        // CONTROL — the STRUCT-pattern sibling. It moved 1 -> 2 with the
        // tuple leaf under B-2026-08-28-40; the two spellings must not
        // drift apart again.
        (
            "struct-field-sibling",
            "struct W { e: E, n: i64 }\n\
                 fn main() { let w = W { e: E.A(R { id: 41 }), n: 1 };\n\
                 \x20            let W { e: _, n } = w; println(f\"{n}\"); }\n",
            "drop E\ndrop R41\n1\n",
        ),
        // CONTROL — a BOUND enum of the same type runs both bodies.
        (
            "bound-control",
            "fn main() { let e = E.A(R { id: 41 }); println(\"mid\"); }\n",
            "drop E\ndrop R41\nmid\n",
        ),
    ] {
        let prog = format!("{H}{body}");
        assert_eq!(run_program(&prog).as_deref(), Some(want), "{label}");
    }
}

/// B-2026-08-28-30's payload-only enum leg, kept beside its own-`Drop`
/// sibling: an enum with NO `impl Drop` of its own still runs its live
/// variant's payload body when discarded by a wildcard leaf. The two
/// walkers are selected by different predicates and must not collapse into
/// one — this row fails if the own-`Drop` arm ever swallows the
/// payload-only case.
#[test]
fn e2e_payload_only_enum_discarded_by_wildcard_leaf_runs_payload_body() {
    let prog = "enum E { A(R), B }\n\
             struct R { id: i64 }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"drop R{self.id}\") } }\n\
             fn main() { let p = (E.A(R { id: 41 }), 1); let (_, n) = p; println(f\"{n}\"); }\n";
    assert_eq!(run_program(prog).as_deref(), Some("drop R41\n1\n"));
}

#[test]
fn e2e_own_drop_enum_member_runs_its_body_when_never_destructured() {
    const H: &str = "enum E { A(R), B }\n\
             impl Drop for E { fn drop(mut ref self) { println(\"drop E\") } }\n\
             struct R { id: i64 }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"drop R{self.id}\") } }\n";
    for (label, body, want) in [
        // -47: the agree-on-zero half.
        (
            "tuple-elem-unit",
            "fn main() { let p = (E.B, 1); println(f\"{p.1}\"); }\n",
            "1\ndrop E\n",
        ),
        (
            "tuple-elem-payload",
            "fn main() { let p = (E.A(R { id: 7 }), 1); println(f\"{p.1}\"); }\n",
            "1\ndrop E\ndrop R7\n",
        ),
        // -46: the interpreter-silent half. Compiled already passed these
        // two, so they are the parity anchor rather than the RED rows.
        (
            "struct-field-unit",
            "struct W { e: E, n: i64 }\n\
                 fn main() { let w = W { e: E.B, n: 1 }; println(f\"{w.n}\"); }\n",
            "1\ndrop E\n",
        ),
        (
            "struct-field-payload",
            "struct W { e: E, n: i64 }\n\
                 fn main() { let w = W { e: E.A(R { id: 7 }), n: 1 }; println(f\"{w.n}\"); }\n",
            "1\ndrop E\ndrop R7\n",
        ),
        // The COMPOSITION of the two: a tuple, holding the enum, held in a
        // struct field. Reached through a third walker again, and silent on
        // every backend before the fix.
        (
            "tuple-inside-struct-field",
            "struct W { p: (E, i64) }\n\
                 fn main() { let w = W { p: (E.B, 1) }; println(\"hi\"); }\n",
            "drop E\nhi\n",
        ),
        // CONTROL — destructuring the same value already worked, on every
        // backend, before any of this.
        (
            "destructured-control",
            "fn main() { let p = (E.B, 1); let (_, n) = p; println(f\"{n}\"); }\n",
            "drop E\n1\n",
        ),
        // CONTROL — a plain struct element is the shape that always worked;
        // it pins that the widening did not disturb it.
        (
            "struct-elem-control",
            "struct S { id: i64 }\n\
                 impl Drop for S { fn drop(mut ref self) { println(\"drop S\") } }\n\
                 fn main() { let p = (S { id: 1 }, 1); println(f\"{p.1}\"); }\n",
            "1\ndrop S\n",
        ),
        // BOUNDARY — moving the tuple on must still run the body exactly
        // once, at the destination. A widening that fired per-owner rather
        // than per-value shows up here as two.
        (
            "moved-on",
            "fn main() { let p = (E.B, 1); let q = p; println(f\"{q.1}\"); }\n",
            "1\ndrop E\n",
        ),
    ] {
        let prog = format!("{H}{body}");
        assert_eq!(run_program(&prog).as_deref(), Some(want), "{label}");
    }
    // B-2026-08-28-55 — the `Vec` element, deliberately left out when
    // -46/-47 landed and closed one commit later. The walker was never
    // enum-blind (it reads `drop_method_keys` for `owns_body`); its
    // SELECTOR in `stmts.rs` was, so `Vec[E]` registered nothing and the
    // walker was never reached. All three spellings, since the element
    // type is derived differently in each.
    for (label, body) in [
        (
            "vec-inferred",
            "fn main() { let mut v = Vec.new(); v.push(E.B); println(f\"{v.len()}\"); }\n",
        ),
        (
            "vec-annotated",
            "fn main() { let mut v: Vec[E] = Vec.new(); v.push(E.B);\n\
                 \x20            println(f\"{v.len()}\"); }\n",
        ),
        (
            "vec-literal",
            "fn main() { let v = [E.B]; println(f\"{v.len()}\"); }\n",
        ),
    ] {
        assert_eq!(
            run_program(&format!("{H}{body}")).as_deref(),
            Some("1\ndrop E\n"),
            "{label}"
        );
    }
    // A payload variant in a Vec runs BOTH bodies. This row pins the
    // walker's new payload leg specifically: `owns_body` alone prints
    // `drop E` and stops, the one-body-for-two-objects asymmetry
    // B-2026-08-28-40 fixed for a struct field.
    assert_eq!(
        run_program(&format!(
            "{H}fn main() {{ let mut v = Vec.new(); v.push(E.A(R {{ id: 7 }}));\n\
                 \x20            println(f\"{{v.len()}}\"); }}\n"
        ))
        .as_deref(),
        Some("1\ndrop E\ndrop R7\n"),
        "vec-payload"
    );
    // B-2026-08-28-54 — an enum with NO own `Drop` but a Drop-bearing
    // variant payload. Pinned AS SILENCE by this very assertion when
    // -46/-47 landed, because every walker reached an enum through
    // `type_runs_user_drop`, whose legs all read struct field tables and
    // were blind to a variant payload. Widening that predicate through
    // `enum_variant_field_type_exprs` closed it in all three positions at
    // once, which is why one loop covers them here.
    for (label, body) in [
        (
            "payload-only-struct-field",
            "struct W3 { e: E2, n: i64 }\n\
                 fn main() { let w = W3 { e: E2.A(R { id: 5 }), n: 1 };\n\
                 \x20            println(f\"{w.n}\"); }\n",
        ),
        (
            "payload-only-tuple-elem",
            "fn main() { let p = (E2.A(R { id: 5 }), 1); println(f\"{p.1}\"); }\n",
        ),
        (
            "payload-only-vec-elem",
            "fn main() { let mut v = Vec.new(); v.push(E2.A(R { id: 5 }));\n\
                 \x20            println(f\"{v.len()}\"); }\n",
        ),
    ] {
        assert_eq!(
                run_program(&format!(
                    "enum E2 {{ A(R), B }}\n\
                     struct R {{ id: i64 }}\n\
                     impl Drop for R {{ fn drop(mut ref self) {{ println(f\"drop R{{self.id}}\") }} }}\n\
                     {body}"
                ))
                .as_deref(),
                Some("1\ndrop R5\n"),
                "{label}"
            );
    }
    // BOUNDARY — the payloadless variant of the SAME enum runs nothing.
    // `E2` has no own body, so there is nothing to run; this is what keeps
    // the widening from being read as "any enum member fires".
    assert_eq!(
        run_program(
            "enum E2 { A(R), B }\n\
                 struct R { id: i64 }\n\
                 impl Drop for R { fn drop(mut ref self) { println(f\"drop R{self.id}\") } }\n\
                 fn main() { let p = (E2.B, 1); println(f\"{p.1}\"); }\n"
        )
        .as_deref(),
        Some("1\n"),
        "payload-only-unit-variant"
    );
}

/// B-2026-08-28-73 — the OUTPUT half of the double free: a consuming arm
/// that hands its boxed enum payload out runs the body once, in the same
/// place, on every backend.
///
/// The memory claim lives in
/// `memory_sanitizer::asan_consuming_arm_handing_a_boxed_enum_payload_out_frees_it_once`;
/// this pins the observable side, because the two failure modes the fix sits
/// between print differently. Freeing twice aborts (so a row simply
/// disappears); neutralizing an arm whose result nobody takes leaks, and
/// leaks silently — `discarded` is here to hold that corner still.
///
/// `struct-control` is the axis, correct before the fix: the enum spelling
/// is what lacked the box-view registration the neutralizer reads.
#[test]
fn e2e_consuming_arm_handing_a_boxed_enum_payload_out_runs_one_body() {
    const H: &str = "enum G { A(String), B }\n\
             impl Drop for G { fn drop(mut ref self) {\n\
             \x20   match self { G.A(s) => { println(f\"dG{s}\") } G.B => { println(\"dGB\") } } } }\n\
             fn sink(g: G) -> i64 { println(\"sink\"); return 1; }\n";
    for (label, body, want) in [
        (
            "braced-tail",
            "fn main() { let o: Option[G] = Some(G.A(f\"z{9}\"));\n\
                 \x20            let k = match o { Some(g) => { g } None => { G.B } };\n\
                 \x20            println(\"kept\"); }\n",
            "dGz9\nkept\n",
        ),
        (
            "bare-tail",
            "fn main() { let o: Option[G] = Some(G.A(f\"z{9}\"));\n\
                 \x20            let k = match o { Some(g) => g, None => G.B };\n\
                 \x20            println(\"kept\"); }\n",
            "dGz9\nkept\n",
        ),
        (
            "result-twin",
            "fn main() { let r: Result[G, i64] = Ok(G.A(f\"z{9}\"));\n\
                 \x20            let k = match r { Ok(g) => { g } Err(n) => { G.B } };\n\
                 \x20            println(\"kept\"); }\n",
            "dGz9\nkept\n",
        ),
        (
            "into-call",
            "fn main() { let o: Option[G] = Some(G.A(f\"z{9}\"));\n\
                 \x20            match o { Some(g) => { let n = sink(g); println(f\"n{n}\") }\n\
                 \x20                      None => { println(\"none\") } } }\n",
            "sink\nn1\ndGz9\n",
        ),
        (
            "if-let-into-call",
            "fn main() { let o: Option[G] = Some(G.A(f\"z{9}\"));\n\
                 \x20            if let Some(g) = o { let n = sink(g); println(f\"n{n}\") } }\n",
            "sink\nn1\ndGz9\n",
        ),
        (
            "rebind",
            "fn main() { let o: Option[G] = Some(G.A(f\"z{9}\"));\n\
                 \x20            match o { Some(g) => { let q = g; println(\"bound\") }\n\
                 \x20                      None => { println(\"none\") } } }\n",
            "dGz9\nbound\n",
        ),
        (
            "read-only-arm",
            "fn main() { let o: Option[G] = Some(G.A(f\"z{9}\"));\n\
                 \x20            match o { Some(g) => { println(\"saw\") }\n\
                 \x20                      None => { println(\"none\") } } }\n",
            "saw\ndGz9\n",
        ),
        // The match RESULT is discarded, so nothing owns the value and the
        // box stays armed. NO body runs on any backend here — an agreed
        // silence that predates this row (the enum spelling of the
        // B-2026-08-28-69 neighbourhood) and is filed separately. Pinned as
        // measured so a change in either direction surfaces.
        (
            "discarded",
            "fn main() { let o: Option[G] = Some(G.A(f\"z{9}\"));\n\
                 \x20            match o { Some(g) => { g } None => { G.B } };\n\
                 \x20            println(\"kept\"); }\n",
            "kept\n",
        ),
    ] {
        assert_eq!(
            run_program(&format!("{H}{body}")).as_deref(),
            Some(want),
            "{label}"
        );
    }
    // THE AXIS — the struct payload, correct before the fix.
    assert_eq!(
        run_program(
            "struct R { id: i64, name: String }\n\
                 impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.name}\") } }\n\
                 fn main() { let o: Option[R] = Some(R { id: 1, name: f\"z{9}\" });\n\
                 \x20            let k = match o { Some(r) => { r }\n\
                 \x20                              None => { R { id: 0, name: f\"e{0}\" } } };\n\
                 \x20            println(\"kept\"); }\n"
        )
        .as_deref(),
        Some("dRz9\nkept\n"),
        "struct-control"
    );
}

/// B-2026-08-28-63 — a consuming `match` / `if let` arm that binds an ENUM
/// payload out runs that enum's `Drop` body, exactly as the same arm
/// binding a STRUCT payload already did.
///
/// `struct-control` is what localizes the bug: identical program one type
/// over, correct on every backend before the fix. The enum spelling ran
/// NOTHING anywhere — agreed silence, so no A/B gate could report it — and
/// the reason is that the arm's registration was struct-keyed on both
/// sides (`track_enum_var` registers the payload's MEMORY and no body).
///
/// `boxed-heap-payload` is the second registration site. An enum wider than
/// the `Option` payload area is heap-boxed, so the inline arm declines on
/// word count and a different branch has to carry it. It is here because
/// fixing only the inline one made the interpreter fire where codegen
/// stayed silent — turning agreed silence into a real divergence, which is
/// worse than the bug.
///
/// `escaping-enum-tail` is what keeps the new registration from
/// double-firing: an arm whose VALUE is the binding hands it to the match's
/// result, which registers its own drop, so registering here as well would
/// run the body twice. The gate is the arm's consumption classifier.
///
/// The STRUCT spelling of that shape is NOT pinned here, deliberately: it
/// already printed the body twice on both compiled backends against the
/// interpreter's one, long before any enum registration existed, and it is
/// filed on its own row. Applying this same gate to the struct arm was
/// tried and reverted — it suppressed three bodies that must run
/// (`e2e_let_else_moved_payload_single_drop`,
/// `e2e_owned_self_enum_receiver_single_fire`,
/// `test_e2e_result_struct_payload_field_freed_and_single_body`), because
/// the classifier calls "consuming" several shapes that transfer nothing,
/// and `let ... else` never populates the set at all. That is a separate
/// slice, not a rider on this one.
#[test]
fn e2e_consuming_arm_runs_the_bound_enum_payloads_drop_body() {
    const H: &str = "enum E { A(R), B }\n\
             impl Drop for E { fn drop(mut ref self) { println(\"drop E\") } }\n\
             enum G { A(String), B }\n\
             impl Drop for G { fn drop(mut ref self) { println(\"drop G\") } }\n\
             enum H { A(R), B }\n\
             enum J { A(i64), B }\n\
             struct R { id: i64 }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"drop R{self.id}\") } }\n\
             fn sink(e: E) { println(\"sank\") }\n";
    for (label, body, want) in [
        (
            "match-enum-payload",
            "fn main() { let o: Option[E] = Some(E.A(R { id: 1 }));\n\
                 \x20            match o { Some(e) => { println(\"got\") }\n\
                 \x20                      None => { println(\"none\") } } }\n",
            "got\ndrop E\ndrop R1\n",
        ),
        (
            "if-let-enum-payload",
            "fn main() { let o: Option[E] = Some(E.B);\n\
                 \x20            if let Some(e) = o { println(\"got\") } }\n",
            "got\ndrop E\n",
        ),
        (
            "result-enum-payload",
            "fn main() { let o: Result[E, i64] = Ok(E.B);\n\
                 \x20            match o { Ok(e) => { println(\"got\") }\n\
                 \x20                      Err(n) => { println(\"err\") } } }\n",
            "got\ndrop E\n",
        ),
        // -54's predicate at this position: no own `Drop`, Drop-bearing
        // payload.
        (
            "payload-only-enum",
            "fn main() { let o: Option[H] = Some(H.A(R { id: 4 }));\n\
                 \x20            match o { Some(h) => { println(\"got\") }\n\
                 \x20                      None => { println(\"none\") } } }\n",
            "got\ndrop R4\n",
        ),
        // The BOXED registration site — payload wider than the area.
        (
            "boxed-heap-payload",
            "fn main() { let o: Option[G] = Some(G.A(f\"z{9}\"));\n\
                 \x20            match o { Some(g) => { println(\"got\") }\n\
                 \x20                      None => { println(\"none\") } } }\n",
            "got\ndrop G\n",
        ),
        // CONTROL — the struct spelling, correct before the fix, pinned so
        // the two cannot drift apart.
        (
            "struct-control",
            "fn main() { let o: Option[R] = Some(R { id: 2 });\n\
                 \x20            match o { Some(r) => { println(\"got\") }\n\
                 \x20                      None => { println(\"none\") } } }\n",
            "got\ndrop R2\n",
        ),
        // ESCAPING — the arm's value IS the binding, so the match result
        // owns it and only the destination fires. The struct spelling of
        // this printed the body twice on both compiled backends before the
        // consumption gate went in.
        (
            "escaping-enum-tail",
            "fn main() { let o: Option[E] = Some(E.B);\n\
                 \x20            let k = match o { Some(e) => { e } None => { E.B } };\n\
                 \x20            println(\"kept\"); }\n",
            "drop E\nkept\n",
        ),
        // MOVED ON to a sink — the callee owns it and fires once.
        (
            "moved-to-sink",
            "fn main() { let o: Option[E] = Some(E.B);\n\
                 \x20            match o { Some(e) => { sink(e) }\n\
                 \x20                      None => { println(\"none\") } } }\n",
            "sank\ndrop E\n",
        ),
        // NO-BIND arm — the scrutinee still owns the payload, so its own
        // walk fires and the arm registers nothing.
        (
            "no-bind-wildcard-arm",
            "fn main() { let o: Option[E] = Some(E.A(R { id: 1 }));\n\
                 \x20            match o { Some(_) => { println(\"some\") }\n\
                 \x20                      None => { println(\"none\") } } }\n",
            "some\ndrop E\ndrop R1\n",
        ),
        // BOUNDARY — an enum with no `Drop` anywhere registers nothing.
        (
            "no-drop-enum-payload",
            "fn main() { let o: Option[J] = Some(J.A(2));\n\
                 \x20            match o { Some(x) => { println(\"got\") }\n\
                 \x20                      None => { println(\"none\") } } }\n",
            "got\n",
        ),
    ] {
        assert_eq!(
            run_program(&format!("{H}{body}")).as_deref(),
            Some(want),
            "{label}"
        );
    }
}

/// B-2026-08-28-67, compiled leg — the PARITY PIN for the interpreter fix.
///
/// These rows were GREEN before the fix and after it: the compiled order
/// (the enum's own `Drop` body, then its payload's) was already the correct
/// one, and the whole divergence lived on the interpreter side. The fixture
/// exists because its interpreter twin
/// (`interpreter::readthrough_arm_leaves_the_payload_with_its_enum`)
/// asserts these same strings, so a future change that "reconciles" the two
/// backends by moving the COMPILED half has to break this test to do it.
///
/// The row it closes was a pure sequence divergence — one body each, on
/// both sides — which is exactly what a count-based fixture cannot see.
#[test]
fn e2e_readthrough_arm_leaves_the_payload_with_its_enum() {
    const H: &str = "struct R { id: i64 }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\") } }\n\
             impl R { fn get(ref self) -> i64 { self.id }\n\
             \x20         fn eat(self) -> i64 { self.id } }\n\
             enum E { A(R), B }\n\
             impl Drop for E { fn drop(mut ref self) { println(\"dE\") } }\n\
             enum H { A(R), B }\n\
             struct W { r: R }\n\
             fn keep(r: R) -> i64 { r.id }\n";
    for (label, body, want) in [
            // ── read-through: the scrutinee keeps the payload ──────────────
            (
                "read-field",
                "let e = E.A(R { id: 5 });\n\
                 \x20 match e { E.A(r) => { println(f\"v{r.id}\") } E.B => {} }\n",
                "v5\ndE\ndR5\npost\n",
            ),
            (
                "never-mentioned",
                "let e = E.A(R { id: 5 });\n\
                 \x20 match e { E.A(r) => { println(\"got\") } E.B => {} }\n",
                "got\ndE\ndR5\npost\n",
            ),
            (
                "method-ref-self",
                "let e = E.A(R { id: 5 });\n\
                 \x20 match e { E.A(r) => { println(f\"v{r.get()}\") } E.B => {} }\n",
                "v5\ndE\ndR5\npost\n",
            ),
            (
                "method-owned-self",
                "let e = E.A(R { id: 5 });\n\
                 \x20 match e { E.A(r) => { println(f\"v{r.eat()}\") } E.B => {} }\n",
                "v5\ndE\ndR5\npost\n",
            ),
            (
                "guard-reads-only",
                "let e = E.A(R { id: 5 });\n\
                 \x20 match e { E.A(r) if r.id == 5i64 => { println(\"g\") }\n\
                 \x20           E.A(r) => { println(\"o\") } E.B => {} }\n",
                "g\ndE\ndR5\npost\n",
            ),
            (
                "if-let",
                "let e = E.A(R { id: 5 });\n\
                 \x20 if let E.A(r) = e { println(f\"v{r.id}\") }\n",
                "v5\ndE\ndR5\npost\n",
            ),
            (
                "nested-in-option",
                "let o: Option[E] = Some(E.A(R { id: 3 }));\n\
                 \x20 match o { Some(x) => { match x { E.A(r) => { println(\"inner\") }\n\
                 \x20                                  E.B => {} } } None => {} }\n",
                "inner\ndE\ndR3\npost\n",
            ),
            // ── materialized: the arm owns the payload ─────────────────────
            (
                "interpolation-hole",
                "let e = E.A(R { id: 5 });\n\
                 \x20 match e { E.A(r) => { println(f\"h{keep(r)}\") } E.B => {} }\n",
                "h5\ndR5\ndE\npost\n",
            ),
            (
                "free-fn-argument",
                "let e = E.A(R { id: 5 });\n\
                 \x20 match e { E.A(r) => { let k = keep(r); println(f\"k{k}\") } E.B => {} }\n",
                "k5\ndR5\ndE\npost\n",
            ),
            (
                "let-rebind",
                "let e = E.A(R { id: 5 });\n\
                 \x20 match e { E.A(r) => { let m = r; println(f\"m{m.id}\") } E.B => {} }\n",
                "m5\ndR5\ndE\npost\n",
            ),
            (
                "struct-literal-field",
                "let e = E.A(R { id: 5 });\n\
                 \x20 match e { E.A(r) => { let w = W { r: r }; println(f\"w{w.r.id}\") } E.B => {} }\n",
                "w5\ndR5\ndE\npost\n",
            ),
            (
                // MIXED ARMS — the shape that proves the disarm and the stash are
                // ONE decision. The disarm is a whole-match retraction (matching
                // codegen's compile-time one, which cannot be path-sensitive), so
                // the materializing FIRST arm retracts the walk even when the
                // read-through SECOND arm is the one taken. A cut of this fix that
                // asked the stash only about the taken arm stood it down anyway and
                // printed `v5 dE`, losing `dR5`.
                "mixed-arms-one-materializes",
                "let e = E.A(R { id: 5 });\n\
                 \x20 match e { E.A(r) if r.id == 1i64 => { let m = r; println(f\"m{m.id}\") }\n\
                 \x20           E.A(r) => { println(f\"v{r.id}\") } E.B => {} }\n",
                "v5\ndR5\ndE\npost\n",
            ),
            // ── boundary: no own `Drop` ────────────────────────────────────
            (
                "no-own-drop-enum",
                "let e = H.A(R { id: 5 });\n\
                 \x20 match e { H.A(r) => { println(f\"v{r.id}\") } H.B => {} }\n",
                "v5\ndR5\npost\n",
            ),
        ] {
            let src = format!("{H}#[allow(partial_move_of_drop_enum)]\nfn main() {{ {body} println(\"post\") }}\n");
            assert_eq!(run_program(&src).as_deref(), Some(want), "{label}");
        }
}

/// B-2026-08-30-17 — on an `if let` MISS, the scrutinee temporary's `Drop`
/// body fires BEFORE the `else` arm, not after it.
///
/// design.md § `if let` and `let...else` > "Scrutinee temporary scope",
/// second bullet: "In the **`else` arm of `if let` / `else if let`**:
/// scrutinee temporaries have already been dropped *before* the arm body
/// begins. The else arm does not see the scrutinee's temporaries — they
/// fired the moment the match decision routed control to the else path."
/// § Temporary Lifetime Rules repeats it in table form. The section states
/// that this is what "closes the lock-held-during-else-branch footgun", so
/// the ORDER is the feature: a `Lease` whose `Drop` returns a pooled
/// connection must be back in the pool before the else arm logs and
/// retries, which is exactly the worked example given there.
///
/// WHY THIS NEEDED AN ABSOLUTE EXPECTATION rather than an A/B one: both
/// backends printed `els dE`, so the four-surface parity rule reported
/// green, nothing leaked or double-freed, and the deviation was from a
/// SPEC SENTENCE that no test asserted. Fixing it moved BOTH backends.
///
/// The `let…else` rows are here because the same bullet's THIRD entry
/// covers them ("same rule — scrutinee temporaries are dropped before the
/// divergent else block runs") and they were wrong in the same way; the
/// row named only `if let`. The HIT rows are unchanged boundaries — the
/// spec asks for the opposite placement there ("scrutinee temporaries live
/// through the entire arm body, because pattern-bound names may borrow
/// into them"), so a fix that simply moved the drop earlier everywhere
/// would break them.
#[test]
fn e2e_if_let_miss_drops_the_scrutinee_temp_before_the_else_arm() {
    const H: &str = "struct R { id: i64 }\n\
             enum E { A(R), B }\n\
             impl Drop for E { fn drop(mut ref self) { println(\"dE\") } }\n\
             fn mkA(n: i64) -> E { return E.A(R { id: n }) }\n\
             fn mkB() -> E { return E.B }\n";
    for (label, body, want) in [
            // The row's own repro.
            (
                "if-let-miss",
                "if let E.A(r) = mkB() { println(f\"v{r.id}\") } else { println(\"els\") }\n",
                "dE\nels\npost\n",
            ),
            // The opposite edge, unchanged: through the arm body, then out.
            (
                "if-let-hit",
                "if let E.A(r) = mkA(7) { println(f\"v{r.id}\") } else { println(\"els\") }\n",
                "v7\ndE\npost\n",
            ),
            // No else arm at all — there is nothing for the drop to precede,
            // and this row was already correct. It is here so a later edit
            // cannot "fix" the miss edge by firing twice.
            (
                "if-let-miss-no-else",
                "if let E.A(r) = mkB() { println(f\"v{r.id}\") }\n",
                "dE\npost\n",
            ),
            // The then arm diverges: it fires on its OWN edge, so the else
            // emission cannot double it.
            (
                "if-let-hit-then-returns",
                "if let E.A(r) = mkA(9) { println(f\"v{r.id}\"); return } else { println(\"els\") }\n",
                "v9\ndE\n",
            ),
            // In a loop: once per iteration, still before the arm.
            (
                "if-let-miss-in-a-loop",
                "let mut i: i64 = 0;\n\
                 while i < 2 { if let E.A(r) = mkB() { println(f\"v{r.id}\") } else { println(\"els\") } i = i + 1; }\n",
                "dE\nels\ndE\nels\npost\n",
            ),
        ] {
            let src = format!("{H}#[allow(partial_move_of_drop_enum)]\nfn main() {{ {body} println(\"post\") }}\n");
            assert_eq!(run_program(&src).as_deref(), Some(want), "{label}");
        }
    // `let…else` — the divergent-else sibling, asserted separately because
    // its else block must terminate, so it cannot share the `post` tail.
    for (label, body, want) in [
        (
            "let-else-miss",
            "let E.A(r) = mkB() else { println(\"els\"); return };\n\
                 println(f\"v{r.id}\")\n",
            "dE\nels\n",
        ),
        (
            "let-else-hit",
            "let E.A(r) = mkA(4) else { println(\"els\"); return };\n\
                 println(f\"v{r.id}\")\n",
            "dE\nv4\n",
        ),
    ] {
        let src = format!("{H}#[allow(partial_move_of_drop_enum)]\nfn main() {{ {body} }}\n");
        assert_eq!(run_program(&src).as_deref(), Some(want), "{label}");
    }
}

/// B-2026-08-30-14, the compiled ORACLE — the twin of
/// `interpreter::diverging_arm_over_a_freshtemp_scrutinee_keeps_its_control_flow`.
///
/// The direction here is the mirror of B-2026-08-29-28's: there the
/// interpreter was the oracle and codegen was moved onto it; here BOTH
/// compiled backends were already right and the INTERPRETER was moved onto
/// them. So this half was green before the fix and after it, and that is
/// the point — it is what the interpreter was reconciled against, so a
/// later change that "fixes" the pair by moving codegen has to break these
/// assertions first.
///
/// Every row is a spelling of one thing: an arm that diverges over a
/// fresh-temp scrutinee with its own `Drop` body. Nine of the twelve
/// silently DISCARDED the signal on the interpreter (`return` skipped,
/// loop never broken, `?` turned an `Err` into `Ok(0)`); codegen emits the
/// arm's cleanup on that arm's own edge and has always been correct, which
/// is why this half needed no change. The other three — `while-let` and
/// the two boundary rows — were already green on both sides and are
/// carried here so the pair stays symmetric.
///
/// The struct-scrutinee row from the interpreter twin is deliberately
/// ABSENT: both compiled backends run no body at all for a fresh-temp
/// struct scrutinee (B-2026-08-30-15), so it cannot be asserted here
/// without encoding that separate defect. When -15 closes, that row moves
/// in and the interpreter-only test above collapses into its table.
#[test]
fn e2e_diverging_arm_over_a_freshtemp_scrutinee_keeps_its_control_flow() {
    const H: &str = "enum E { A(i64), B }\n\
             impl Drop for E { fn drop(mut ref self) { println(\"dE\") } }\n\
             enum H { A(i64), B }\n\
             fn mk(n: i64) -> E { return E.A(n) }\n\
             fn mkH(n: i64) -> H { return H.A(n) }\n";
    for (label, prog, want) in [
            (
                "return",
                "fn f() { match mk(7) { E.A(n) => { println(f\"v{n}\"); return } E.B => {} }\n\
                 \x20 println(\"AFTER\") }\n\
                 fn main() { f(); println(\"done\") }\n",
                "v7\ndE\ndone\n",
            ),
            (
                "return-value",
                "fn f() -> i64 { match mk(7) { E.A(n) => { return n } E.B => { return 0 } } }\n\
                 fn main() { println(f\"t{f()}\") }\n",
                "dE\nt7\n",
            ),
            (
                "break",
                "fn main() {\n\
                 \x20 let mut i = 0;\n\
                 \x20 while i < 3 {\n\
                 \x20   match mk(7) { E.A(n) => { println(f\"v{n}\"); break } E.B => {} }\n\
                 \x20   println(\"AFTER\"); i = i + 1;\n\
                 \x20 }\n\
                 \x20 println(\"done\")\n\
                 }\n",
                "v7\ndE\ndone\n",
            ),
            (
                "continue",
                "fn main() {\n\
                 \x20 let mut i = 0;\n\
                 \x20 while i < 2 {\n\
                 \x20   i = i + 1;\n\
                 \x20   match mk(7) { E.A(n) => { println(f\"v{n}\"); continue } E.B => {} }\n\
                 \x20   println(\"AFTER\");\n\
                 \x20 }\n\
                 \x20 println(\"done\")\n\
                 }\n",
                "v7\ndE\nv7\ndE\ndone\n",
            ),
            (
                "labeled-break",
                "fn main() {\n\
                 \x20 let mut i = 0;\n\
                 \x20 outer: while i < 3 {\n\
                 \x20   let mut j = 0;\n\
                 \x20   while j < 3 {\n\
                 \x20     match mk(7) { E.A(n) => { println(f\"v{n}\"); break outer; } E.B => {} };\n\
                 \x20     j = j + 1;\n\
                 \x20   }\n\
                 \x20   i = i + 1;\n\
                 \x20 }\n\
                 \x20 println(\"done\")\n\
                 }\n",
                "v7\ndE\ndone\n",
            ),
            (
                "question-mark",
                "fn g(x: i64) -> Result[i64, String] { if x > 5 { return Err(\"big\") } return Ok(x) }\n\
                 fn f() -> Result[i64, String] {\n\
                 \x20 match mk(7) { E.A(n) => { println(f\"v{n}\"); let q = g(n)?; return Ok(q) } E.B => {} }\n\
                 \x20 println(\"AFTER\");\n\
                 \x20 return Ok(0)\n\
                 }\n\
                 fn main() { match f() { Ok(v) => { println(f\"ok{v}\") } Err(e) => { println(f\"err{e}\") } } }\n",
                "v7\ndE\nerrbig\n",
            ),
            (
                "nested-match",
                "fn f() {\n\
                 \x20 match mk(1) { E.A(a) => { match mk(2) { E.A(b) => { println(f\"v{a}{b}\"); return } E.B => {} } } E.B => {} }\n\
                 \x20 println(\"AFTER\")\n\
                 }\n\
                 fn main() { f(); println(\"done\") }\n",
                "v12\ndE\ndE\ndone\n",
            ),
            (
                "return-inside-nested-block",
                "fn f() { match mk(7) { E.A(n) => { if n > 0 { println(f\"v{n}\"); return } } E.B => {} }\n\
                 \x20 println(\"AFTER\") }\n\
                 fn main() { f(); println(\"done\") }\n",
                "v7\ndE\ndone\n",
            ),
            (
                "if-let",
                "fn f() { if let E.A(n) = mk(7) { println(f\"v{n}\"); return } println(\"AFTER\") }\n\
                 fn main() { f() }\n",
                "v7\ndE\n",
            ),
            (
                "while-let",
                "fn f() { while let E.A(n) = mk(7) { println(f\"v{n}\"); return } println(\"AFTER\") }\n\
                 fn main() { f(); println(\"done\") }\n",
                "v7\ndE\ndone\n",
            ),
            (
                "named-scrutinee-control",
                "fn f() { let e = mk(7);\n\
                 \x20 match e { E.A(n) => { println(f\"v{n}\"); return } E.B => {} }\n\
                 \x20 println(\"AFTER\") }\n\
                 fn main() { f() }\n",
                "v7\ndE\n",
            ),
            (
                "no-own-drop-control",
                "fn f() { match mkH(7) { H.A(n) => { println(f\"v{n}\"); return } H.B => {} }\n\
                 \x20 println(\"AFTER\") }\n\
                 fn main() { f() }\n",
                "v7\n",
            ),
        ] {
            assert_eq!(
                run_program(&format!("{H}{prog}")).as_deref(),
                Some(want),
                "{label}"
            );
        }
}

/// B-2026-08-29-29 — a READ-THROUGH arm over a PROJECTION-PLACE enum
/// scrutinee (`match s.e`, `match t.0`) leaves the payload with the value
/// that owns it, and runs its `Drop` body exactly ONCE.
///
/// B-2026-08-28-67 settled the read-through rule for an identifier
/// scrutinee and gated it there, so a projection place kept the old path:
/// the arm registered the payload's body AND the owner's bodies walk ran
/// it, so both compiled backends fired it TWICE — the second time on the
/// slot the arm's move-out had cap-zeroed. `struct-field` measured
/// `v8 dR8 dE dR0` where `v8 dE dR8` is due, and `dR0` is a user body
/// reading its own fields on a zeroed object.
///
/// The identifier spelling is the ORACLE for every row here: each one
/// prints exactly what `match e { … }` prints for the same arm, which is
/// the point — the place a value is reached through must not change how
/// many times its destructor runs.
///
/// `self-field-in-ref-method` is a PARITY PIN, green before and after: a
/// `ref self` receiver was already classified as borrowed, so its field
/// projection never took the doubling path. It is here so a later edit
/// cannot reconcile the family by moving that half instead.
///
/// NOT PINNED, and deliberately: a MATERIALIZING arm over a projection
/// (`E.A(r) => { let m = r; … }`) still runs the body twice on every
/// backend. That needs a payload-ONLY retraction of the owner's walk,
/// which neither backend can express today — a struct's field-bodies walk
/// is one emitted function running the enum's own body and its payload's
/// together — so pinning it here would pin a bug as the contract. Filed
/// separately.
#[test]
fn e2e_readthrough_arm_over_a_projection_leaves_the_payload_with_its_owner() {
    const H: &str = "struct R { id: i64 }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\") } }\n\
             enum E { A(R), B }\n\
             impl Drop for E { fn drop(mut ref self) { println(\"dE\") } }\n\
             enum H { A(R), B }\n\
             struct S { e: E }\n\
             struct Sh { h: H }\n\
             struct W { s: S }\n\
             impl S { fn look(ref self) -> i64 {\n\
             \x20   match self.e { E.A(r) => { return r.id } E.B => { return 0i64 } } } }\n";
    for (label, body, want) in [
        (
            "struct-field",
            "let s = S { e: E.A(R { id: 8 }) };\n\
                 \x20 match s.e { E.A(r) => { println(f\"v{r.id}\") } E.B => {} }\n",
            "v8\ndE\ndR8\npost\n",
        ),
        (
            "struct-field-never-mentioned",
            "let s = S { e: E.A(R { id: 8 }) };\n\
                 \x20 match s.e { E.A(r) => { println(\"got\") } E.B => {} }\n",
            "got\ndE\ndR8\npost\n",
        ),
        (
            "tuple-element",
            "let t = (E.A(R { id: 8 }), 1i64);\n\
                 \x20 match t.0 { E.A(r) => { println(f\"v{r.id}\") } E.B => {} }\n",
            "v8\ndE\ndR8\npost\n",
        ),
        (
            "tuple-element-never-mentioned",
            "let t = (E.A(R { id: 8 }), 1i64);\n\
                 \x20 match t.0 { E.A(r) => { println(\"got\") } E.B => {} }\n",
            "got\ndE\ndR8\npost\n",
        ),
        (
            // A DEEPER chain resolves through the same place walker, so the
            // classifier must not stop at one hop.
            "nested-field-chain",
            "let w = W { s: S { e: E.A(R { id: 8 }) } };\n\
                 \x20 match w.s.e { E.A(r) => { println(f\"v{r.id}\") } E.B => {} }\n",
            "v8\ndE\ndR8\npost\n",
        ),
        (
            // B-2026-08-28-67's rule that the two spellings answer the SAME
            // question: a `match`-only cut of this fix left both `if let`
            // rows doubling.
            "if-let-struct-field",
            "let s = S { e: E.A(R { id: 8 }) };\n\
                 \x20 if let E.A(r) = s.e { println(f\"v{r.id}\") }\n",
            "v8\ndE\ndR8\npost\n",
        ),
        (
            "if-let-tuple-element",
            "let t = (E.A(R { id: 8 }), 1i64);\n\
                 \x20 if let E.A(r) = t.0 { println(f\"v{r.id}\") }\n",
            "v8\ndE\ndR8\npost\n",
        ),
        (
            // PARITY PIN — green before and after; see the doc comment.
            "self-field-in-ref-method",
            "let s = S { e: E.A(R { id: 8 }) };\n\
                 \x20 println(f\"v{s.look()}\");\n",
            "v8\ndE\ndR8\npost\n",
        ),
        (
            // The guard is part of the read-through question, exactly as it
            // is for the identifier spelling.
            "guard-reads-only",
            "let s = S { e: E.A(R { id: 8 }) };\n\
                 \x20 match s.e { E.A(r) if r.id == 8i64 => { println(\"g\") }\n\
                 \x20           E.A(r) => { println(\"o\") } E.B => {} }\n",
            "g\ndE\ndR8\npost\n",
        ),
        (
            // The enum having NO own `Drop` is not a carve-out here, unlike
            // B-2026-08-28-67's gate: with no own body the two orders
            // coincide, but the payload's body was still doubled, so these
            // are RED rows too (`v8 dR8 dR0` unfixed).
            "no-own-drop-field",
            "let s = Sh { h: H.A(R { id: 8 }) };\n\
                 \x20 match s.h { H.A(r) => { println(f\"v{r.id}\") } H.B => {} }\n",
            "v8\ndR8\npost\n",
        ),
        (
            "no-own-drop-tuple",
            "let t = (H.A(R { id: 8 }), 1i64);\n\
                 \x20 match t.0 { H.A(r) => { println(f\"v{r.id}\") } H.B => {} }\n",
            "v8\ndR8\npost\n",
        ),
    ] {
        let src = format!("{H}fn main() {{ {body} println(\"post\") }}\n");
        assert_eq!(run_program(&src).as_deref(), Some(want), "{label}");
    }
}

/// B-2026-08-29-33 — a MATERIALIZING arm over a PROJECTION-PLACE enum
/// scrutinee runs the payload's `Drop` body exactly as often as the
/// identifier spelling does, and never on a zeroed slot.
///
/// B-2026-08-29-29 fixed the READ-THROUGH half by classifying a projection
/// arm as a borrow. A materializing arm (`E.A(r) => { let m = r; … }`)
/// cannot take that route — the binding really does own the payload — so it
/// kept firing the body twice on both compiled backends, the second time
/// over the slot the move-out had cap-zeroed: `m8 dR8 dE dR0` where
/// `m8 dR8 dE` is due.
///
/// The retraction that fixes it is a PAYLOAD-ONLY mask on the owner's walk
/// (`FieldSkipTree::payload_here`, and the tuple walker's `payload_skip`),
/// which is expressible only because the walkers emit the enum's own body
/// and its payload's as two separate calls. Masking the whole field or
/// element — the granularity that already existed — loses `dE`.
///
/// `heap-payload-*` are the memory half, and they were a
/// USE-AFTER-FREE, not just a doubled body: the three `let`-form legs
/// (`if let` / `while let` / `let … else`) never ran the projection-place
/// suppressor at all, so the source struct kept a populated payload while
/// the binding took the buffer and both freed it. The binary produced NO
/// output under valgrind (invalid read of a freed block) while the `match`
/// spelling of the same code was clean.
///
/// `ident-oracle-let-rebind` is the control: every projection row above
/// prints exactly what it prints.
#[test]
fn e2e_materializing_arm_over_a_projection_owns_the_payload_once() {
    const H: &str = "struct R { id: i64 }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\") } }\n\
             struct Rh { id: i64, s: String }\n\
             impl Drop for Rh { fn drop(mut ref self) { println(f\"dH{self.id}:{self.s}\") } }\n\
             enum E { A(R), B }\n\
             impl Drop for E { fn drop(mut ref self) { println(\"dE\") } }\n\
             enum Eh { A(Rh), B }\n\
             impl Drop for Eh { fn drop(mut ref self) { println(\"dEh\") } }\n\
             enum N { A(R), B }\n\
             struct S { e: E }\n\
             struct Sh { e: Eh }\n\
             struct Sn { n: N }\n\
             fn take(x: R) -> i64 { x.id }\n\
             fn keep(x: R) -> R { x }\n";
    for (label, body, want) in [
            (
                "field-let-rebind",
                "let s = S { e: E.A(R { id: 8 }) };\n\
                 \x20 match s.e { E.A(r) => { let m = r; println(f\"m{m.id}\") } E.B => {} }\n",
                "m8\ndR8\ndE\npost\n",
            ),
            (
                "field-free-fn-argument",
                "let s = S { e: E.A(R { id: 8 }) };\n\
                 \x20 match s.e { E.A(r) => { let k = take(r); println(f\"k{k}\") } E.B => {} }\n",
                "k8\ndR8\ndE\npost\n",
            ),
            (
                // ONE body, the same count the `-let-rebind` sibling above
                // prints for the identical object flow. This read `dR8` twice
                // until B-2026-08-29-15, justified as "under the entry-copy
                // model `keep` returns an independent value, so `k` and the arm
                // binding both own one". There is no entry copy: measured with
                // a heap payload, the call form and the bare-rebind form are
                // both valgrind-clean and balanced (12/12 and 11/11), and the
                // second body had no second buffer behind it. The rebind
                // sibling is the oracle — `let m = r` and `let k = keep(r)`
                // move the same object to the same kind of owner.
                "field-returning-free-fn",
                "let s = S { e: E.A(R { id: 8 }) };\n\
                 \x20 match s.e { E.A(r) => { let k = keep(r); println(f\"k{k.id}\") } E.B => {} }\n",
                "k8\ndR8\ndE\npost\n",
            ),
            (
                "tuple-let-rebind",
                "let t = (E.A(R { id: 8 }), 1i64);\n\
                 \x20 match t.0 { E.A(r) => { let m = r; println(f\"m{m.id}\") } E.B => {} }\n",
                "m8\ndR8\ndE\npost\n",
            ),
            (
                // Same correction as `field-returning-free-fn` above, one
                // container over: matches `tuple-let-rebind`.
                "tuple-returning-free-fn",
                "let t = (E.A(R { id: 8 }), 1i64);\n\
                 \x20 match t.0 { E.A(r) => { let k = keep(r); println(f\"k{k.id}\") } E.B => {} }\n",
                "k8\ndR8\ndE\npost\n",
            ),
            (
                "if-let-field-let-rebind",
                "let s = S { e: E.A(R { id: 8 }) };\n\
                 \x20 if let E.A(r) = s.e { let m = r; println(f\"m{m.id}\") }\n",
                "m8\ndR8\ndE\npost\n",
            ),
            (
                "if-let-tuple-let-rebind",
                "let t = (E.A(R { id: 8 }), 1i64);\n\
                 \x20 if let E.A(r) = t.0 { let m = r; println(f\"m{m.id}\") }\n",
                "m8\ndR8\ndE\npost\n",
            ),
            (
                // The USE-AFTER-FREE rows — see the doc comment. A heap payload
                // makes the same defect a memory error rather than a doubled
                // line, so these fail by producing nothing at all.
                "heap-payload-field",
                "let s = Sh { e: Eh.A(Rh { id: 8, s: f\"n{8}\" }) };\n\
                 \x20 match s.e { Eh.A(r) => { let m = r; println(f\"m{m.id}:{m.s}\") } Eh.B => {} }\n",
                "m8:n8\ndH8:n8\ndEh\npost\n",
            ),
            (
                "heap-payload-if-let-field",
                "let s = Sh { e: Eh.A(Rh { id: 8, s: f\"n{8}\" }) };\n\
                 \x20 if let Eh.A(r) = s.e { let m = r; println(f\"m{m.id}:{m.s}\") }\n",
                "m8:n8\ndH8:n8\ndEh\npost\n",
            ),
            (
                // No own `Drop` is not a carve-out: with no own body to
                // preserve the two mask granularities coincide, but the
                // payload's body was still doubled (`m8 dR8 dR0` unfixed).
                "no-own-drop-field",
                "let s = Sn { n: N.A(R { id: 8 }) };\n\
                 \x20 match s.n { N.A(r) => { let m = r; println(f\"m{m.id}\") } N.B => {} }\n",
                "m8\ndR8\npost\n",
            ),
            (
                "no-own-drop-tuple",
                "let t = (N.A(R { id: 8 }), 1i64);\n\
                 \x20 match t.0 { N.A(r) => { let m = r; println(f\"m{m.id}\") } N.B => {} }\n",
                "m8\ndR8\npost\n",
            ),
            (
                // The control every row above is measured against.
                "ident-oracle-let-rebind",
                "let e = E.A(R { id: 8 });\n\
                 \x20 match e { E.A(r) => { let m = r; println(f\"m{m.id}\") } E.B => {} }\n",
                "m8\ndR8\ndE\npost\n",
            ),
        ] {
            let src = format!("{H}#[allow(partial_move_of_drop_enum)]\nfn main() {{ {body} println(\"post\") }}\n");
            assert_eq!(run_program(&src).as_deref(), Some(want), "{label}");
        }
}

/// B-2026-08-29-48 — a `match` arm that ASSIGNS the param's payload to an
/// outer binding, which the callee then returns, hands the value to the
/// CALLER exactly as `return r` does. One `Drop` body is due, at the
/// caller's result binding, and the assign spelling must print what the
/// direct spelling prints.
///
/// It ran the body TWICE: once caller-side at the argument's live-range end
/// and again at the result's. On all four surfaces, so no A/B gate could see
/// it — it was found by asking whether the COUNT was right rather than
/// whether the two backends agreed.
///
/// The escape predicate is the whole story. `fn_returns_param_payload`
/// already follows a payload through a `let` (`let k = r; return k;`), and
/// could not follow one through an assignment: with `out = r` the
/// destination is declared outside the arm and the `return` that carries it
/// out sits outside the arm too, so both ends of the route are invisible
/// from inside the arm body — which is all its return-walk ever sees. The
/// arm's assignment targets now come back out and the FUNCTION body is
/// asked whether their roots leave.
///
/// Both backends read that one predicate (codegen through
/// `callee_returns_enum_arg_payload`), which is why a single change moves
/// all four of method/free × user-enum/`Option` together.
///
/// `*-into-field` pins the place-root walk: `h.inner = r` escapes when `h`
/// does. `*-iflet-*` pins the `if let` leg, which has the same shape as the
/// `match` one and would otherwise be left behind.
///
/// `mid9` / `dR9` is a callee LOCAL, so the rows pin the PLACE and not just
/// the count — a body fired at the arm lands before `mid9`, one at callee
/// scope exit between `mid9` and `dR9`, and the caller-side fire after
/// `dR9`.
///
/// `control-assigned-but-not-returned` is the boundary in the other
/// direction: the payload is assigned to an outer local the callee does NOT
/// return, so it dies inside and the callee's own scope drop is the single
/// body. The predicate must stay FALSE there, and compiled output is
/// correct today. It has no interpreter peer in `tests/interpreter.rs`
/// because the interpreter runs an extra body on that shape — a
/// pre-existing divergence this fix does not touch, filed separately rather
/// than pinned wrong here.
#[test]
fn e2e_arm_assigned_payload_that_is_returned_runs_one_drop_body() {
    const H: &str = "struct R { id: i64, tag: String }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\") } }\n\
             enum E { A(R), B }\n\
             impl Drop for E { fn drop(mut ref self) { println(\"dE\") } }\n\
             struct Holder { inner: R }\n\
             struct T { n: i64 }\n\
             impl T {\n\
             \x20   #[allow(partial_move_of_drop_enum)]\nfn assign_enum(ref self, b: E) -> R {\n\
             \x20       let mut out: R = R { id: 0, tag: f\"t0\" };\n\
             \x20       match b { E.A(r) => { out = r; } E.B => { } }\n\
             \x20       let loc = R { id: 9, tag: f\"t9\" }; println(f\"mid{loc.id}\"); return out; }\n\
             \x20   fn assign_opt(ref self, b: Option[R]) -> R {\n\
             \x20       let mut out: R = R { id: 0, tag: f\"t0\" };\n\
             \x20       match b { Some(r) => { out = r; } None => { } }\n\
             \x20       let loc = R { id: 9, tag: f\"t9\" }; println(f\"mid{loc.id}\"); return out; }\n\
             \x20   #[allow(partial_move_of_drop_enum)]\nfn direct_enum(ref self, b: E) -> R {\n\
             \x20       match b { E.A(r) => { return r } E.B => { return R { id: 0, tag: f\"t0\" } } } }\n\
             \x20   #[allow(partial_move_of_drop_enum)]\nfn field_enum(ref self, b: E) -> Holder {\n\
             \x20       let mut h: Holder = Holder { inner: R { id: 0, tag: f\"t0\" } };\n\
             \x20       match b { E.A(r) => { h.inner = r; } E.B => { } }\n\
             \x20       let loc = R { id: 9, tag: f\"t9\" }; println(f\"mid{loc.id}\"); return h; }\n\
             \x20   fn iflet_opt(ref self, b: Option[R]) -> R {\n\
             \x20       let mut out: R = R { id: 0, tag: f\"t0\" };\n\
             \x20       if let Some(r) = b { out = r; }\n\
             \x20       let loc = R { id: 9, tag: f\"t9\" }; println(f\"mid{loc.id}\"); return out; } }\n\
             #[allow(partial_move_of_drop_enum)]\nfn f_assign_enum(b: E) -> R {\n\
             \x20   let mut out: R = R { id: 0, tag: f\"t0\" };\n\
             \x20   match b { E.A(r) => { out = r; } E.B => { } }\n\
             \x20   let loc = R { id: 9, tag: f\"t9\" }; println(f\"mid{loc.id}\"); return out; }\n\
             fn f_assign_opt(b: Option[R]) -> R {\n\
             \x20   let mut out: R = R { id: 0, tag: f\"t0\" };\n\
             \x20   match b { Some(r) => { out = r; } None => { } }\n\
             \x20   let loc = R { id: 9, tag: f\"t9\" }; println(f\"mid{loc.id}\"); return out; }\n\
             #[allow(partial_move_of_drop_enum)]\nfn f_direct_enum(b: E) -> R {\n\
             \x20   match b { E.A(r) => { return r } E.B => { return R { id: 0, tag: f\"t0\" } } } }\n\
             #[allow(partial_move_of_drop_enum)]\nfn f_dies(b: E) -> i64 {\n\
             \x20   let mut out: R = R { id: 0, tag: f\"t0\" };\n\
             \x20   match b { E.A(r) => { out = r; } E.B => { } }\n\
             \x20   println(\"mid\"); return out.id; }\n";
    for (label, body, want) in [
        // THE ROW. The second `dR8` — after `v8` and again before `post` —
        // is what this fix removes.
        (
            "method-enum-assign",
            "let t = T { n: 1 }; let carg: E = E.A(R { id: 8, tag: f\"t8\" });\n\
                 \x20 let v: R = t.assign_enum(carg); println(f\"v{v.id}\");\n",
            "dR0\nmid9\ndR9\ndE\nv8\ndR8\npost\n",
        ),
        (
            "free-enum-assign",
            "let carg: E = E.A(R { id: 8, tag: f\"t8\" });\n\
                 \x20 let v: R = f_assign_enum(carg); println(f\"v{v.id}\");\n",
            "dR0\nmid9\ndR9\ndE\nv8\ndR8\npost\n",
        ),
        (
            "method-option-assign",
            "let t = T { n: 1 }; let carg: Option[R] = Some(R { id: 8, tag: f\"t8\" });\n\
                 \x20 let v: R = t.assign_opt(carg); println(f\"v{v.id}\");\n",
            "dR0\nmid9\ndR9\nv8\ndR8\npost\n",
        ),
        (
            "free-option-assign",
            "let carg: Option[R] = Some(R { id: 8, tag: f\"t8\" });\n\
                 \x20 let v: R = f_assign_opt(carg); println(f\"v{v.id}\");\n",
            "dR0\nmid9\ndR9\nv8\ndR8\npost\n",
        ),
        // The spelling that was already right, kept beside the four above:
        // one body, at the caller's result binding, either way.
        (
            "method-enum-direct",
            "let t = T { n: 1 }; let carg: E = E.A(R { id: 8, tag: f\"t8\" });\n\
                 \x20 let v: R = t.direct_enum(carg); println(f\"v{v.id}\");\n",
            "dE\nv8\ndR8\npost\n",
        ),
        (
            "free-enum-direct",
            "let carg: E = E.A(R { id: 8, tag: f\"t8\" });\n\
                 \x20 let v: R = f_direct_enum(carg); println(f\"v{v.id}\");\n",
            "dE\nv8\ndR8\npost\n",
        ),
        (
            "method-enum-assign-into-field",
            "let t = T { n: 1 }; let carg: E = E.A(R { id: 8, tag: f\"t8\" });\n\
                 \x20 let v: Holder = t.field_enum(carg); println(f\"v{v.inner.id}\");\n",
            "dR0\nmid9\ndR9\ndE\nv8\ndR8\npost\n",
        ),
        (
            "method-option-iflet-assign",
            "let t = T { n: 1 }; let carg: Option[R] = Some(R { id: 8, tag: f\"t8\" });\n\
                 \x20 let v: R = t.iflet_opt(carg); println(f\"v{v.id}\");\n",
            "dR0\nmid9\ndR9\nv8\ndR8\npost\n",
        ),
        // The caller's binding spelled as the callee's parameter: a rename
        // is not a semantic change.
        (
            "free-enum-assign-shadowed-name",
            "let b: E = E.A(R { id: 8, tag: f\"t8\" });\n\
                 \x20 let v: R = f_assign_enum(b); println(f\"v{v.id}\");\n",
            "dR0\nmid9\ndR9\ndE\nv8\ndR8\npost\n",
        ),
        // The boundary: assigned to an outer local the callee does NOT
        // return, so it dies inside and `dR8` lands between `mid` and the
        // result. The predicate must stay false here.
        (
            "control-assigned-but-not-returned",
            "let carg: E = E.A(R { id: 8, tag: f\"t8\" });\n\
                 \x20 let v: i64 = f_dies(carg); println(f\"v{v}\");\n",
            "dR0\nmid\ndE\ndR8\nv8\npost\n",
        ),
    ] {
        let src = format!("{H}fn main() {{ {body} println(\"post\") }}\n");
        assert_eq!(run_program(&src).as_deref(), Some(want), "{label}");
    }
}

/// B-2026-08-28-40 — an own-`impl Drop` enum discarded by a wildcard
/// destructure leaf runs its LIVE PAYLOAD's body too, not just its own.
///
/// B-2026-08-28-31 had brought the discard from zero bodies to one on the
/// compiled backends by pointing the bodies-only walker at the enum name,
/// which finds `owns_body` true and emits `<E>.drop`. That walker had no
/// payload leg at all, so it stopped there — one body against the two the
/// same value runs when BOUND. Every backend agreed on the one, which is
/// why no run-vs-build gate could report it.
///
/// The fix gives the bodies-only walker an enum-payload leg, and gives the
/// STRUCT-field walker the same leg for an enum-typed field, because the
/// two spellings reach the payload through different code. Fixing only the
/// first is what the first cut did, and it turned the previously consistent
/// pair into an interp-2 / compiled-1 divergence — the reason `struct-field`
/// is a row here and not a footnote.
///
/// `second-variant` is the row that proves the walk reads the LIVE tag
/// rather than the first payload-bearing variant: the value is `E.B(..)` in
/// an enum whose `A` also carries an `R`, and only `R7` prints.
///
/// Each row is stated as the same value's BOUND spelling would print it —
/// that equivalence is the whole claim, and `no-payload-variant` is the
/// boundary where the two legitimately agree at one body.
#[test]
fn e2e_wildcard_discard_of_own_drop_enum_runs_its_payload_body() {
    const H: &str = "enum E { A(R), B }\n\
             impl Drop for E { fn drop(mut ref self) { println(\"drop E\") } }\n\
             struct R { id: i64 }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"drop R{self.id}\") } }\n";
    for (label, body, want) in [
        (
            "tuple-leaf",
            "fn main() { let p = (E.A(R { id: 41 }), 1); let (_, n) = p;\n\
                 \x20            println(f\"{n}\"); }\n",
            "drop E\ndrop R41\n1\n",
        ),
        (
            "struct-field",
            "struct W { e: E, n: i64 }\n\
                 fn main() { let w = W { e: E.A(R { id: 41 }), n: 1 };\n\
                 \x20            let W { e: _, n } = w; println(f\"{n}\"); }\n",
            "drop E\ndrop R41\n1\n",
        ),
        // BOUNDARY — a payloadless variant. One body is correct here, and
        // it is the same one body the pre-fix rows above printed, so this
        // row is what keeps the fix from being read as "always two".
        (
            "no-payload-variant",
            "fn main() { let p = (E.B, 1); let (_, n) = p; println(f\"{n}\"); }\n",
            "drop E\n1\n",
        ),
    ] {
        let prog = format!("{H}{body}");
        assert_eq!(run_program(&prog).as_deref(), Some(want), "{label}");
    }
    // The walk follows the LIVE variant, not the first one carrying a
    // payload: `A` and `B` both hold an `R` and only the constructed one
    // prints. Carries its own enum, hence a standalone assert.
    assert_eq!(
        run_program(
            "enum E { A(R), B(R) }\n\
                 impl Drop for E { fn drop(mut ref self) { println(\"drop E\") } }\n\
                 struct R { id: i64 }\n\
                 impl Drop for R { fn drop(mut ref self) { println(f\"drop R{self.id}\") } }\n\
                 fn main() { let p = (E.B(R { id: 7 }), 1); let (_, n) = p; println(f\"{n}\"); }\n"
        )
        .as_deref(),
        Some("drop E\ndrop R7\n1\n"),
        "second-variant"
    );
    // DEPTH — the payload's own fields keep walking, and the discard still
    // matches the bound spelling exactly.
    const D: &str = "struct S { tag: i64 }\n\
             impl Drop for S { fn drop(mut ref self) { println(f\"drop S{self.tag}\") } }\n\
             struct R { id: i64, s: S }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"drop R{self.id}\") } }\n\
             enum E { A(R), B }\n\
             impl Drop for E { fn drop(mut ref self) { println(\"drop E\") } }\n";
    assert_eq!(
        run_program(&format!(
            "{D}fn main() {{ let p = (E.A(R {{ id: 41, s: S {{ tag: 9 }} }}), 1);\n\
                 \x20            let (_, n) = p; println(f\"{{n}}\"); }}\n"
        ))
        .as_deref(),
        Some("drop E\ndrop R41\ndrop S9\n1\n"),
        "nested-payload"
    );
    assert_eq!(
        run_program(&format!(
            "{D}fn main() {{ let e = E.A(R {{ id: 41, s: S {{ tag: 9 }} }});\n\
                 \x20            println(\"mid\"); }}\n"
        ))
        .as_deref(),
        Some("drop E\ndrop R41\ndrop S9\nmid\n"),
        "nested-bound-control"
    );
}

/// B-2026-09-02-28 — A WILDCARD TUPLE-DESTRUCTURE OF A CALL-PRODUCED
/// AGGREGATE RUNS ITS `Drop` BODIES. `let (_, _) = (mk(1), 5);` ran NOTHING
/// on the compiled backends -- not the payload walk, and not the enum's own
/// `karac_drop_<Bx>` wrapper either -- against the interpreter's correct
/// `dB dW7`.
///
/// ALREADY FIXED when this fixture landed, by `1bfd07e` ("name a call
/// element's type so a tuple argument's Vec elements are freed"), which was
/// filed against a different payload family. Bisected: the bug reproduces at
/// its parent `1bd23f1` and not at `1bfd07e`. The shape had no test of its
/// own, so nothing pinned it -- which is exactly how it came to be fixed by
/// accident, and why it gets one now.
///
/// THE CALL IS THE DISCRIMINATOR, not the destructure. Measured at the buggy
/// parent, `inline-ctor-element` is CORRECT while `call-enum-element` is
/// not, so the hole is in resolving an owner for an element whose value came
/// from a CALL -- the same distinction that mattered for B-2026-09-02-13,
/// the parent row.
///
/// A STRUCT ELEMENT BEHAVES THE SAME as an enum one, and BOTH elements of a
/// two-call tuple were lost, so the defect was per-element rather than
/// per-statement. The row listed both as NOT MEASURED.
///
/// THE STRUCT-PATTERN WILDCARD LEAF DOES NOT have this hole, on EITHER
/// source -- which keeps this fixture's subject to the TUPLE leaf. The row
/// asked about its own place-sourced spelling (`let St { w: _, n } = w;`)
/// and that was correct at the buggy parent; so was the call-sourced twin
/// the row did not name (`let St { w: _, n } = mks();`), measured in the
/// same build where `call-enum-element` reproduces. So a call producer
/// broke the tuple leaf's element-type derivation specifically -- there is
/// no shared "call-produced element" notion that both leaves consult.
///
/// NOTHING LEAKED, at either revision: valgrind reports 0 errors and 0 bytes
/// lost for the heap-carrying payload too. The storage was always reclaimed;
/// only the observable body went missing, which is why no leak gate saw it
/// and why an absolute expected-output assertion is the only thing that can.
#[test]
fn e2e_wildcard_tuple_destructure_of_a_call_runs_its_drop_bodies() {
    const H: &str = "struct W { id: i64 }\n\
             impl Drop for W { fn drop(mut ref self) { println(f\"dW{self.id}\") } }\n\
             enum Bx { Full(W), Empty(W) }\n\
             impl Drop for Bx { fn drop(mut ref self) { println(\"dB\") } }\n\
             fn mk(n: i64) -> Bx {\n\
             \x20   if n < 1 { return Bx.Full(W { id: n }) }\n\
             \x20   return Bx.Empty(W { id: 7 });\n\
             }\n\
             struct P { s: String }\n\
             impl Drop for P { fn drop(mut ref self) { println(f\"dP{self.s}\") } }\n\
             enum Hx { One(P), Zero }\n\
             impl Drop for Hx { fn drop(mut ref self) { println(\"dH\") } }\n\
             fn mkh(n: i64) -> Hx { return Hx.One(P { s: f\"payloadpayload{n}\" }); }\n\
             struct St { w: W, n: i64 }\n\
             fn mks() -> St { return St { w: W { id: 3 }, n: 1 }; }\n";
    for (label, body, want) in [
        // THE ROW: a call-produced ENUM element behind a wildcard tuple leaf.
        (
            "call-enum-element",
            "let (_, _) = (mk(1), 5);\n",
            "dB\ndW7\nmid\n",
        ),
        // A call-produced STRUCT element -- the row's first NOT-MEASURED.
        (
            "call-struct-element",
            "let (_, _) = (mks(), 5);\n",
            "dW3\nmid\n",
        ),
        // BOTH elements from calls: the loss was per-element.
        (
            "two-call-elements",
            "let (_, _) = (mk(1), mk(0));\n",
            "dB\ndW7\ndB\ndW0\nmid\n",
        ),
        // A HEAP-carrying payload. Correct output AND no leak, at either
        // revision -- the row asked whether this one additionally leaked.
        (
            "heap-carrying-payload",
            "let (_, _) = (mkh(1), 5);\n",
            "dH\ndPpayloadpayload1\nmid\n",
        ),
        // CONTROLS, each correct at the buggy parent and here.
        (
            "ctl-inline-ctor-element",
            "let (_, _) = (Bx.Empty(W { id: 7 }), 5);\n",
            "dB\ndW7\nmid\n",
        ),
        ("ctl-single-wildcard", "let _ = mk(1);\n", "dB\ndW7\nmid\n"),
        ("ctl-bound-binding", "let x = mk(1);\n", "dB\ndW7\nmid\n"),
        (
            "ctl-struct-pattern-wildcard-leaf",
            "let w: St = St { w: W { id: 4 }, n: 2 };\n\
                 \x20 let St { w: _, n } = w; println(f\"n{n}\");\n",
            "dW4\nn2\nmid\n",
        ),
        // The CALL-sourced struct-pattern twin, which the row did not name.
        // Also correct at the buggy parent, so the hole was the TUPLE
        // leaf's derivation and not a shared call-producer blind spot.
        (
            "ctl-call-source-struct-pattern",
            "let St { w: _, n } = mks(); println(f\"n{n}\");\n",
            "dW3\nn1\nmid\n",
        ),
    ] {
        let src = format!("{H}fn main() {{ {body} println(\"mid\") }}\n");
        assert_eq!(run_program(&src).as_deref(), Some(want), "{label}");
    }
}

/// B-2026-08-30-8 — a returned `Option`/`Result` local that a READ-ONLY
/// `match` arm borrowed from is freed twice.
///
/// `bc1c37c` (B-2026-08-29-56) hooked the escape disarm at the two return
/// positions, but reused the CONSUMING-position helper and so inherited its
/// veto: a source that a read-only arm left owning its payload sits in
/// `inline_optres_retained_sources` and is skipped. That veto is right where
/// it was written — the premise of a consuming-position disarm is "a callee
/// already took the buffer", which a borrowing arm never did, so zeroing the
/// sole owner's slot would leak (B-2026-08-08-25 leg 1). A return has the
/// opposite premise: handing the header out IS the transfer, so the caller
/// owns the buffer and the callee's scope-exit free is one owner too many.
///
/// The correlation is exact and was measured per shape: every body whose
/// source hit the veto aborted, every body that reached the disarm was
/// clean, and nothing else moved.
///
/// Two of the ledger row's three "required ingredients" are refuted here and
/// are rows for that reason. A REASSIGNMENT of the local is not needed
/// (`readonly-arm-returned` has none and aborted pre-fix), and the arm need
/// not be read-only in the source-level sense (`arm-escapes-payload` moves
/// the payload out and still aborted). What is actually required is only
/// that a `Some(prev)`-shaped arm put the local on the retained list, and
/// that the local then escape.
///
/// A double free aborts the process, so `run_program` returns `None` and
/// each pre-fix row failed against its expected string outright. Ten of the
/// thirteen were RED; the three `*-control` rows were green on both sides
/// and guard the two directions this fix could have gone wrong in — losing a
/// free that is still needed, and disarming a chain whose source kept its
/// buffer.
#[test]
fn e2e_returned_option_local_borrowed_by_a_readonly_arm_is_freed_once() {
    const H: &str = "fn r_plain() -> Option[String] { let mut buf: Option[String] = Some(f\"a\"); match buf { Some(prev) => { println(f\"saw {prev}\"); } None => {} } buf }\n\
             fn r_reassign() -> Option[String] { let mut buf: Option[String] = Some(f\"a\"); match buf { Some(prev) => { println(f\"saw {prev}\"); buf = Some(f\"z\"); } None => { buf = None; } } buf }\n\
             fn r_after() -> Option[String] { let mut buf: Option[String] = Some(f\"a\"); match buf { Some(prev) => { println(f\"saw {prev}\"); } None => {} } buf = Some(f\"z\"); buf }\n\
             fn r_escapes() -> Option[String] { let mut buf: Option[String] = Some(f\"a\"); match buf { Some(prev) => { let mut j = prev; j.push_str(\"!\"); buf = Some(j); } None => { buf = Some(f\"n\"); } } buf }\n\
             fn r_ret_if(flag: bool) -> Option[String] { let mut buf: Option[String] = Some(f\"a\"); match buf { Some(prev) => { println(f\"saw {prev}\"); } None => {} } if flag { return buf; } buf = Some(f\"z\"); buf }\n\
             fn r_result() -> Result[String, i64] { let mut r: Result[String, i64] = Ok(f\"a\"); match r { Ok(prev) => { println(f\"saw {prev}\"); } Err(_) => {} } r }\n\
             fn r_vec() -> Option[Vec[i64]] { let mut v: Vec[i64] = Vec.new(); v.push(7i64); let mut buf: Option[Vec[i64]] = Some(v); match buf { Some(prev) => { println(f\"len {prev.len()}\"); } None => {} } buf }\n\
             fn r_loop(n: i64) -> Option[String] { let mut buf: Option[String] = None; let mut i: i64 = 0i64; while i < n { let line = f\"L{i}\"; match buf { Some(prev) => { let mut j = prev; j.push_str(\"-\"); j.push_str(line); buf = Some(j); } None => { buf = Some(line); } } i = i + 1i64; } buf }\n\
             fn r_wild() -> Option[String] { let mut buf: Option[String] = Some(f\"a\"); match buf { Some(_) => { buf = Some(f\"z\"); } None => {} } buf }\n\
             fn r_local() -> i64 { let mut buf: Option[String] = Some(f\"a\"); match buf { Some(prev) => { println(f\"saw {prev}\"); buf = Some(f\"z\"); } None => {} } match buf { Some(x) => { println(f\"[{x}]\"); 1i64 } None => { 0i64 } } }\n\
             fn r_chain(flag: bool) -> i64 { let o: Option[String] = Some(f\"hi\"); match o { Some(s) => { println(f\"saw {s}\"); } None => {} } if flag { return o.map(|x| x.len()).unwrap_or(0i64); } o.map(|x| x.len()).unwrap_or(0i64) }\n";
    for (label, body, want) in [
            // THE ROW, minimised: no reassignment anywhere. The ledger row
            // listed one as a required ingredient; it is not.
            (
                "readonly-arm-returned",
                "match r_plain() { Some(s) => println(f\"[{s}]\"), None => println(\"none\"), }\n",
                "saw a\n[a]\npost\n",
            ),
            // The row's own repro — reassignment inside the arm.
            (
                "arm-then-reassign",
                "match r_reassign() { Some(s) => println(f\"[{s}]\"), None => println(\"none\"), }\n",
                "saw a\n[z]\npost\n",
            ),
            // ... and outside it. Neither spelling is load-bearing.
            (
                "reassign-after-match",
                "match r_after() { Some(s) => println(f\"[{s}]\"), None => println(\"none\"), }\n",
                "saw a\n[z]\npost\n",
            ),
            // The arm MOVES the payload out and puts a new one back. The
            // classifier declines to call this read-only, yet the source is
            // still on the retained list from the `Some(prev)` binding.
            (
                "arm-escapes-payload",
                "match r_escapes() { Some(s) => println(f\"[{s}]\"), None => println(\"none\"), }\n",
                "[a!]\npost\n",
            ),
            // The `exprs.rs` hook: a `return` nested in an `if` is not the
            // body's tail, so the tail walk never sees it.
            (
                "explicit-return-in-if",
                "match r_ret_if(true) { Some(s) => println(f\"[{s}]\"), None => println(\"none\"), }\n",
                "saw a\n[a]\npost\n",
            ),
            // ... and the same function's tail on the other path, which is the
            // `call_dispatch.rs` hook. One function, both sites.
            (
                "tail-when-return-not-taken",
                "match r_ret_if(false) { Some(s) => println(f\"[{s}]\"), None => println(\"none\"), }\n",
                "saw a\n[z]\npost\n",
            ),
            // `Result`'s `Ok` payload takes the sibling `inline_result` set.
            (
                "result-twin",
                "match r_result() { Ok(s) => println(f\"[{s}]\"), Err(e) => println(f\"e{e}\"), }\n",
                "saw a\n[a]\npost\n",
            ),
            // A `Vec` payload is the other direct `{ptr,len,cap}` shape the
            // read-only classifier admits.
            (
                "vec-payload",
                "match r_vec() { Some(v) => println(f\"[{v[0]}]\"), None => println(\"none\"), }\n",
                "len 1\n[7]\npost\n",
            ),
            // `Parser.collect_leading_doc_comments` condensed: accumulate into
            // an `Option[String]` across a loop, then return it. This is what
            // `selfhost_parser_matches_rust_parser_items` was dying on, via a
            // `///` doc comment on any item.
            (
                "selfhost-doc-comment-shape",
                "match r_loop(3i64) { Some(s) => println(f\"[{s}]\"), None => println(\"none\"), }\n",
                "[L0-L1-L2]\npost\n",
            ),
            // CONTROL, clean on both sides and load-bearing in the LEAK
            // direction: the caller drops the value, so exactly one free must
            // happen. Pre-fix the callee's; post-fix the caller's.
            (
                "caller-discards",
                "let _ = r_reassign();\n",
                "saw a\npost\n",
            ),
            // CONTROL: `Some(_)` binds nothing, so the source never joins the
            // retained set and the escape disarm was already reaching it.
            (
                "wildcard-arm-control",
                "match r_wild() { Some(s) => println(f\"[{s}]\"), None => println(\"none\"), }\n",
                "[z]\npost\n",
            ),
            // CONTROL: consumed locally instead of returned. Nothing escapes,
            // so the source must KEEP its scope-exit free.
            (
                "consumed-locally-control",
                "println(f\"{r_local()}\");\n",
                "saw a\n[z]\n1\npost\n",
            ),
            // CONTROL for the narrowing, and the reason the veto override is
            // limited to a bare identifier: `<src>.map(f).unwrap_or(d)` in
            // return position is a CONSUMING expression whose read-only source
            // still owns its buffer. Disarming `o` here would leak it, which is
            // B-2026-08-08-25 leg 1 exactly. LSan is the gate that sees it.
            (
                "map-chain-return-control",
                "println(f\"{r_chain(true)}\"); println(f\"{r_chain(false)}\");\n",
                "saw hi\n2\nsaw hi\n2\npost\n",
            ),
        ] {
            let src = format!("{H}fn main() {{ {body} println(\"post\") }}\n");
            assert_eq!(run_program(&src).as_deref(), Some(want), "{label}");
        }
}

/// B-2026-08-01-6 — codegen parity pin for `tests/interpreter.rs`'s
/// `test_ref_self_match_borrowed_payload_silent` (same source and
/// expected string): a `ref self` method matching on `self` binds
/// borrowed views, so no payload body fires INSIDE the method — the
/// named-binding shape fires exactly once via the binding's own walk.
/// This side was already correct; the interpreter's stash was the
/// diverging half. The fresh-receiver shape was silent on every surface
/// when this landed (the borrowed temp's body had no owner at all);
/// since B-2026-09-06-38 the caller's receiver-temp registrar runs it
/// once at the statement's end (`drop 5 e5` before `t=1`), still
/// outside the arm.
#[test]
fn e2e_ref_self_match_borrowed_payload_silent() {
    let Some(out) = run_program(
        "struct Res { id: i64, name: String }\n\
             impl Drop for Res {\n\
             \x20   fn drop(mut ref self) {\n\
             \x20       println(f\"drop {self.id} {self.name}\")\n\
             \x20   }\n\
             }\n\
             enum Box2 { Full(Res), Empty }\n\
             impl Box2 {\n\
             \x20   fn tag(ref self) -> i64 {\n\
             \x20       match self {\n\
             \x20           Box2.Full(r) => { return 1; }\n\
             \x20           Box2.Empty => { return 0; }\n\
             \x20       }\n\
             \x20   }\n\
             }\n\
             fn mk_e(n: i64) -> Box2 {\n\
             \x20   return Box2.Full(Res { id: n, name: f\"e{n}\" });\n\
             }\n\
             fn main() {\n\
             \x20   println(\"a: ref-self match on fresh receiver\");\n\
             \x20   let t = mk_e(5).tag();\n\
             \x20   println(f\"t={t}\");\n\
             \x20   println(\"b: ref-self match on named binding\");\n\
             \x20   let bx = mk_e(8);\n\
             \x20   let u = bx.tag();\n\
             \x20   println(f\"u={u}\");\n\
             \x20   println(\"end\");\n\
             }\n",
    ) else {
        return;
    };
    assert_eq!(
        out,
        "a: ref-self match on fresh receiver\ndrop 5 e5\nt=1\n\
             b: ref-self match on named binding\ndrop 8 e8\nu=1\nend\n"
    );
}

/// B-2026-08-01-7 — an OWNED-`self` method consumes its named value-enum
/// receiver: the binding's payload-bodies walk disarms at the call
/// (exactly like `let c = b;`), leaving the method-internal arm channel
/// as the payload's sole owner. Pre-fix `b.into_id()` printed the body
/// TWICE on both backends (arm + still-armed walk); the non-consuming
/// owned-self shape is silent (the consumed value's drop is the
/// documented owned-self residual). Twin of `tests/interpreter.rs`'s
/// `test_owned_self_enum_receiver_single_fire`.
/// B-2026-08-28-69 — the compiled twin of
/// `test_discarded_match_arm_value_runs_its_drop_body_once`.
///
/// A DISCARDED `match` whose arm value is an owned Drop-bearing temp runs
/// that body exactly once. The two halves landed together because either
/// alone MOVES the divergence: the interpreter under-fired when an arm
/// handed on a bound payload, and BOTH backends were silent when an arm
/// minted a fresh value. The fresh-value cells are the ones this file
/// gained — `try_track_discarded_user_drop_temp` resolved a type name only
/// from a call, so a `Match` tail registered nothing even though
/// `discarded_match_value_tail` already admitted the shape.
///
/// Memory is balanced in every cell either way, so no ASAN/LSan corpus can
/// reach this class — only an A/B output comparison can, which is why the
/// interpreter twin is the other half of the pin.
#[test]
fn e2e_discarded_match_arm_value_runs_its_drop_body_once() {
    let hdr = "struct R { id: i64 }\n\
                   impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\") } }\n\
                   fn mk(i: i64) -> R { return R { id: i }; }\n";
    let rows: [(&str, &str, &str); 6] = [
        (
            "let o: Option[R] = Some(R { id: 1 });\n\
                 match o { Some(r) => { r } None => { R { id: 0 } } };\n\
                 println(\"dropped\");",
            "dR1\ndropped\n",
            "braced arm yields the bound payload",
        ),
        (
            "let o: Option[R] = Some(R { id: 1 });\n\
                 match o { Some(r) => r, None => R { id: 0 } };\n\
                 println(\"dropped\");",
            "dR1\ndropped\n",
            "bare arm yields the bound payload",
        ),
        (
            "let n = 1;\n\
                 match n { 1 => { R { id: 7 } } _ => { R { id: 0 } } };\n\
                 println(\"dropped\");",
            "dR7\ndropped\n",
            "braced arm yields a fresh literal",
        ),
        (
            "let n = 1;\n\
                 match n { 1 => R { id: 7 }, _ => R { id: 0 } };\n\
                 println(\"dropped\");",
            "dR7\ndropped\n",
            "bare arm yields a fresh literal",
        ),
        (
            "let n = 1;\n\
                 match n { 1 => mk(7), _ => mk(0) };\n\
                 println(\"dropped\");",
            "dR7\ndropped\n",
            "arm yields a call result",
        ),
        (
            "let r = R { id: 41 };\n\
                 let n = 0;\n\
                 match n { 0 => r, _ => R { id: 9 } };\n\
                 println(\"end\");",
            "dR41\nend\n",
            "BOUNDARY: arm hands out a LIVE enclosing local",
        ),
    ];
    for (body, expected, label) in rows {
        let src = format!("{hdr}fn main() {{\n{body}\n}}\n");
        assert_eq!(run_program(&src).as_deref(), Some(expected), "[{label}]");
    }
    // The two controls: both already ran exactly one body and must keep to
    // it — they are what prove the change did not widen into discarded
    // temporaries generally or into arm bindings generally.
    let call = format!("{hdr}fn main() {{ mk(1); println(\"dropped\"); }}\n");
    assert_eq!(
        run_program(&call).as_deref(),
        Some("dR1\ndropped\n"),
        "[discarded call result]"
    );
    let read = format!(
            "{hdr}fn main() {{\n\
             let o: Option[R] = Some(R {{ id: 1 }});\n\
             match o {{ Some(r) => {{ println(f\"saw{{r.id}}\") }} None => {{ println(\"none\") }} }};\n\
             println(\"dropped\");\n}}\n"
        );
    assert_eq!(
        run_program(&read).as_deref(),
        Some("saw1\ndR1\ndropped\n"),
        "[read-only arm]"
    );
}

/// B-2026-08-29-20 — the `let _ = <match>` SIBLING of
/// `e2e_discarded_match_arm_value_runs_its_drop_body_once`, row for row.
///
/// The bare-statement arm of `compile_stmt` has chained
/// `discarded_match_value_tail` since B-2026-08-28-69, and its comment says
/// the two discard spellings "cannot drift apart" because they share the
/// same `&self` siblings. They had: the WILDCARD-LET arm chained only
/// `discarded_owned_temp_tail` and `discarded_unit_variant_tail`, neither of
/// which admits a `Match`, so `let _ = match n { 1 => { R { .. } } .. };`
/// ran NO `Drop` body on any backend while `match n { .. };` ran one. The
/// fix is the missing `.or_else` leg plus its guard disjunct.
///
/// ROWS 3-5 ARE THE ONES THIS MOVED (0 bodies -> 1, on all three backends).
/// Rows 1, 2 and 6 are pinned AS MEASURED, not as preferred, and every one
/// of them is still wrong:
///
///  * 1 and 6 are agreed-silent, and 2 is a live run-vs-build DIVERGENCE —
///    compiled fires through the general match lowering (an arm-scope drop
///    of the binding), the interpreter does not. All three are arms that
///    HAND OUT A BINDING rather than mint a value. B-2026-08-29-5 fixed
///    the LEAK for that population ("a discarded branch construct strands
///    whatever its taken arm hands out"); the missing BODY, which is what
///    these rows measure, is B-2026-08-29-31.
///
/// Row 2 is why the interpreter twin EXCLUDES `Identifier` at an arm tail
/// rather than recursing whole: its predicate admits a bare identifier
/// unconditionally, so inheriting that would fire this backend on row 1 as
/// well — where compiled is silent — moving the divergence one row over
/// instead of removing it. Making rows 1 and 2 agree needs the arm-binding
/// ownership question answered for both backends at once, which is the
/// separate design decision B-2026-08-29-31 carries.
///
/// Also still silent and NOT in this table, measured while bounding the
/// fix: the `if` spelling of the same discard (`let _ = if c { R { .. } }
/// else { .. };` AND the bare-statement `if c { R { .. } } else { .. };`,
/// so that gap predates this row and sits at the parent's site too), and a
/// block-WRAPPED match (`let _ = { match .. };`, which neither gate
/// recurses into). Both are B-2026-08-29-25.
///
/// Memory is balanced in every cell, so no ASAN/LSan corpus reaches this
/// class — only an absolute output expectation can see it.
#[test]
fn e2e_discarded_let_wildcard_match_runs_its_drop_body_once() {
    let hdr = "struct R { id: i64 }\n\
                   impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\") } }\n\
                   fn mk(i: i64) -> R { return R { id: i }; }\n";
    let rows: [(&str, &str, &str); 6] = [
        (
            "let o: Option[R] = Some(R { id: 1 });\n\
                 let _ = match o { Some(r) => { r } None => { R { id: 0 } } };\n\
                 println(\"dropped\");",
            "dR1\ndropped\n",
            "FIXED (B-2026-08-29-31): braced arm hands out the bound payload",
        ),
        (
            "let o: Option[R] = Some(R { id: 1 });\n\
                 let _ = match o { Some(r) => r, None => R { id: 0 } };\n\
                 println(\"dropped\");",
            "dR1\ndropped\n",
            "FIXED (B-2026-08-29-31): bare arm hands out the bound payload — was \
                 a live divergence, compiled firing here while the interpreter did not",
        ),
        (
            "let n = 1;\n\
                 let _ = match n { 1 => { R { id: 7 } } _ => { R { id: 0 } } };\n\
                 println(\"dropped\");",
            "dR7\ndropped\n",
            "FIXED: braced arm yields a fresh literal",
        ),
        (
            "let n = 1;\n\
                 let _ = match n { 1 => R { id: 7 }, _ => R { id: 0 } };\n\
                 println(\"dropped\");",
            "dR7\ndropped\n",
            "FIXED: bare arm yields a fresh literal",
        ),
        (
            "let n = 1;\n\
                 let _ = match n { 1 => mk(7), _ => mk(0) };\n\
                 println(\"dropped\");",
            "dR7\ndropped\n",
            "FIXED: arm yields a call result",
        ),
        (
            "let r = R { id: 41 };\n\
                 let n = 0;\n\
                 let _ = match n { 0 => r, _ => R { id: 9 } };\n\
                 println(\"end\");",
            "dR41\nend\n",
            "FIXED (B-2026-08-29-31): arm hands out an enclosing local",
        ),
    ];
    for (body, expected, label) in rows {
        let src = format!("{hdr}fn main() {{\n{body}\n}}\n");
        assert_eq!(run_program(&src).as_deref(), Some(expected), "[{label}]");
    }
    // The control that localized the defect: the SAME discard site with a
    // non-match RHS was correct throughout, which is what identifies this
    // as a missing gate leg rather than anything about discards generally.
    let call = format!("{hdr}fn main() {{ let _ = mk(1); println(\"dropped\"); }}\n");
    assert_eq!(
        run_program(&call).as_deref(),
        Some("dR1\ndropped\n"),
        "[control: let _ = call]"
    );
    // A read-only arm yields unit and must stay a no-op — the guard that
    // the widened gate did not start firing on arms that own nothing.
    let read = format!(
            "{hdr}fn main() {{\n\
             let o: Option[R] = Some(R {{ id: 1 }});\n\
             let _ = match o {{ Some(r) => {{ println(f\"saw{{r.id}}\") }} None => {{ println(\"none\") }} }};\n\
             println(\"dropped\");\n}}\n"
        );
    assert_eq!(
        run_program(&read).as_deref(),
        Some("saw1\ndR1\ndropped\n"),
        "[control: read-only arm]"
    );
}

/// B-2026-08-29-25 — a discarded `if` runs its arm value's `Drop` body,
/// in BOTH statement forms, and a discard gate sees through a block
/// wrapper.
///
/// The `if` half was silent on all three backends: the gate the sibling
/// `match` spelling goes through (`discarded_match_value_tail`) opened
/// with a `Match`-only destructure, and the type resolution the battery
/// needs (`try_track_discarded_user_drop_temp`) had no `If` arm either —
/// so both halves had to land for the body to fire. The BARE-STATEMENT
/// row is the one that shows the gap predates B-2026-08-29-20: it sits at
/// B-2026-08-28-69's site too.
///
/// The wrapper half was an interpreter-vs-compiled DIVERGENCE for the
/// shapes compiled already admitted (a wrapped call, a wrapped struct
/// literal) and an agreed silence for the wrapped `match`. Every row here
/// is one `R`, constructed once and taken by nobody, so one body is due.
#[test]
fn e2e_discarded_if_and_block_wrapped_match_run_their_drop_body_once() {
    let hdr = "struct R { id: i64 }\n\
                   impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\") } }\n\
                   fn mk(i: i64) -> R { return R { id: i }; }\n";
    let rows: [(&str, &str, &str); 9] = [
            (
                "let n = 1;\n\
                 let _ = if n == 1 { R { id: 7 } } else { R { id: 0 } };\n\
                 println(\"end\");",
                "dR7\nend\n",
                "FIXED: `let _ = if …`, struct-literal branches",
            ),
            (
                "let n = 1;\n\
                 if n == 1 { R { id: 7 } } else { R { id: 0 } };\n\
                 println(\"end\");",
                "dR7\nend\n",
                "FIXED: bare-statement `if …` — predates B-2026-08-29-20",
            ),
            (
                "let n = 1;\n\
                 let _ = if n == 1 { mk(7) } else { mk(0) };\n\
                 println(\"end\");",
                "dR7\nend\n",
                "FIXED: `let _ = if …`, call branches",
            ),
            (
                "let n = 1;\n\
                 if n == 1 { mk(7) } else { mk(0) };\n\
                 println(\"end\");",
                "dR7\nend\n",
                "FIXED: bare-statement `if …`, call branches",
            ),
            (
                "let n = 1;\n\
                 let _ = if n == 2 { R { id: 1 } } else if n == 1 { R { id: 7 } } else { R { id: 0 } };\n\
                 println(\"end\");",
                "dR7\nend\n",
                "FIXED: `else if` chain — the else branch is another `If`",
            ),
            (
                "let n = 1;\n\
                 let _ = if n == 1 { match n { 1 => { R { id: 7 } } _ => { R { id: 3 } } } } else { R { id: 0 } };\n\
                 println(\"end\");",
                "dR7\nend\n",
                "FIXED: a `match` nested at an `if` branch tail",
            ),
            (
                "let n = 1;\n\
                 let _ = { match n { 1 => { R { id: 7 } } _ => { R { id: 0 } } } };\n\
                 println(\"end\");",
                "dR7\nend\n",
                "FIXED: block-wrapped match, `let _ =` form",
            ),
            (
                "let n = 1;\n\
                 { match n { 1 => { R { id: 7 } } _ => { R { id: 0 } } } };\n\
                 println(\"end\");",
                "dR7\nend\n",
                "FIXED: block-wrapped match, bare-statement form",
            ),
            (
                "let _ = { mk(7) };\n\
                 println(\"end\");",
                "dR7\nend\n",
                "block-wrapped call — compiled fired all along; the interpreter twin \
                 is what this row moved",
            ),
        ];
    for (body, expected, label) in rows {
        let src = format!("{hdr}fn main() {{\n{body}\n}}\n");
        assert_eq!(run_program(&src).as_deref(), Some(expected), "[{label}]");
    }
    // AN `if` BRANCH THAT HANDS OUT AN ENCLOSING LOCAL, both statement
    // forms, measured against the `match` spelling of the same shape —
    // the two agree exactly, which is what says the widening did not give
    // `if` a different answer from its sibling.
    //
    // The bare form is the GUARD RAIL and the reason the gate excludes an
    // `Identifier` branch tail: `r` is still in scope, so its scope-exit
    // body already fires and is correct at ONE. Firing the discard walker
    // on top prints `dR41` twice — the failure the sibling `match` arm's
    // liveness gate was measured into existence by, and this row fails at
    // 2 if the `If` arm's gate is removed.
    //
    // The `let _ =` form is PINNED AT THE DEFECT: `r` is handed out and
    // nothing runs its body on any backend. B-2026-08-29-5 fixed the
    // LEAK for that population; the missing BODY is B-2026-08-29-31, and
    // its `match` twin is pinned identically in
    // `e2e_discarded_let_wildcard_match_runs_its_drop_body_once`.
    for (stmt, expected, label) in [
        (
            "if n == 0 { r } else { R { id: 9 } };",
            "dR41\nend\n",
            "guard: bare form — scope-exit body must stay at ONE",
        ),
        (
            "let _ = if n == 0 { r } else { R { id: 9 } };",
            "dR41\nend\n",
            "FIXED (B-2026-08-29-31): branch hands out an enclosing local",
        ),
    ] {
        let src = format!(
            "{hdr}fn main() {{\n\
                 let r = R {{ id: 41 }};\n\
                 let n = 0;\n\
                 {stmt}\n\
                 println(\"end\");\n}}\n"
        );
        assert_eq!(
            run_program(&src).as_deref(),
            Some(expected),
            "[{label} — {stmt}]"
        );
    }
    // FIXED (B-2026-08-29-30). These two were pinned at the DEFECT here —
    // `"end\n"`, no body on any backend — because with no `else` there is
    // no phi for the statement site to own, so freeing the arm's value
    // needed a registration INSIDE the arm rather than a wider gate at the
    // statement. It has one now (`discarded_arm_owned_aggregate_tail`), so
    // the pin flips to the body that was always due.
    for stmt in [
        "let _ = if n == 1 { R { id: 7 } };",
        "let _ = if n == 1 { mk(7) };",
    ] {
        let src = format!("{hdr}fn main() {{\nlet n = 1;\n{stmt}\nprintln(\"end\");\n}}\n");
        assert_eq!(
            run_program(&src).as_deref(),
            Some("dR7\nend\n"),
            "[FIXED: `if` with no `else` — {stmt}]"
        );
    }
    // FIXED (B-2026-08-29-31): a moved local reached through a wrapper.
    // This pinned the shared SILENCE — `let _ = r` was correct at one body
    // because the let-rebind hook retracts the local's own slot, and no
    // such hook fires through `{ … }`. The wrapper is now a recorded
    // discarding position in its own right, so the block stops handing the
    // buffer to a consumer that does not exist and the local keeps its own
    // scope-exit body. At `KARAC_OPT_LEVEL=0` this also went from 9 B
    // stranded to clean — the body had been firing over a zeroed `cap`.
    let wrapped_local = format!(
        "{hdr}fn main() {{\n\
             let r = R {{ id: 41 }};\n\
             let _ = {{ r }};\n\
             println(\"end\");\n}}\n"
    );
    assert_eq!(
        run_program(&wrapped_local).as_deref(),
        Some("dR41\nend\n"),
        "[FIXED: `let _ = {{ r }}` — moved local through a wrapper]"
    );
}

/// B-2026-08-29-30 (remaining half) — a no-`else` `if` whose taken arm
/// MINTS an owned value now owns it, on both compiled backends.
///
/// The value was reachable from neither discard site. `compile_if`'s merge
/// yields a const-0 placeholder when there is no `else`, so the STATEMENT
/// site never sees the arm's value at all — which is why no widening of
/// `discarded_match_value_tail` could have reached it, and why the fix is a
/// registration inside the arm's own frame instead. B-2026-08-29-5 put one
/// there and gated it on `expr_yields_fresh_owned_temp` (Call / MethodCall);
/// that leg owns a Vec/String buffer, a Map/Set handle and an RC box, and a
/// plain user struct is none of the three, so even the CALL spelling
/// registered nothing.
///
/// All four spellings were silent on all three backends and leaked one
/// allocation per evaluation — agreed, so invisible to every A/B parity
/// gate. Twin: `test_no_else_if_arm_owns_the_value_it_mints`, whose table
/// is these shapes in this order. Leak leg:
/// `asan_no_else_if_arm_owns_the_value_it_mints`.
///
/// The CONTROLS are the fix's safety argument, each a shape where an
/// over-eager owner is a DOUBLE free rather than a missing body: a tail
/// naming a live local, an `if` WITH an `else` (owned by the statement
/// site), a struct literal whose field names a live local, and the branch
/// not taken.
#[test]
fn e2e_no_else_if_arm_owns_the_value_it_mints() {
    let hdr = "struct R { id: i64, name: String }\n\
                   impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\") } }\n\
                   struct H { s: String }\n\
                   enum E { A(R), B }\n\
                   impl Drop for E { fn drop(mut ref self) { println(\"dE\") } }\n\
                   fn mk(i: i64) -> R { return R { id: i, name: f\"heap-{i}\" }; }\n";
    for (label, body, want) in [
        // ── the row's four spellings ──────────────────────────────
        (
            "wildcard-let-struct-literal",
            "let _ = if n == 1 { R { id: 7, name: f\"h\" } };",
            "dR7\nend\n",
        ),
        (
            "wildcard-let-call",
            "let _ = if n == 1 { mk(7) };",
            "dR7\nend\n",
        ),
        (
            "bare-statement-struct-literal",
            "if n == 1 { R { id: 7, name: f\"h\" } };",
            "dR7\nend\n",
        ),
        ("bare-statement-call", "if n == 1 { mk(7) };", "dR7\nend\n"),
        // ── shapes the widened admission gate brings with it ──────
        (
            "block-wrapped-rhs",
            "let _ = { if n == 1 { mk(19) } };",
            "dR19\nend\n",
        ),
        (
            "nested-branch-tail",
            "let d = 1;\nlet _ = if n == 1 { if d == 1 { mk(6) } else { mk(7) } };",
            "dR6\nend\n",
        ),
        (
            "nested-match-tail",
            "let d = 1;\nlet _ = if n == 1 { match d { 1 => mk(6), _ => mk(7) } };",
            "dR6\nend\n",
        ),
        (
            "tuple-literal-tail",
            "let _ = if n == 1 { (mk(12), 20) };",
            "dR12\nend\n",
        ),
        (
            "inline-enum-ctor-tail",
            "let _ = if n == 1 { E.A(mk(8)) };",
            "dE\ndR8\nend\n",
        ),
        (
            "unit-variant-tail",
            "let _ = if n == 1 { E.B };",
            "dE\nend\n",
        ),
        (
            "loop-body-fires-per-iteration",
            "for i in 0..3 { if n == 1 { mk(i) }; }",
            "dR0\ndR1\ndR2\nend\n",
        ),
        // ── controls: an over-eager owner here is a DOUBLE free ───
        (
            "control: place-tail-bare",
            "let r = mk(1);\nif n == 1 { r };",
            "dR1\nend\n",
        ),
        (
            // FIXED by B-2026-08-29-31, which stopped a wildcard `let`
            // marking its RHS as an escaping position. This pinned `end`
            // when it was written: the local was marked moved-out and
            // nothing ran its body. It now keeps its own scope-exit body,
            // at ONE — still a control, for the opposite direction.
            "control: place-tail-wildcard-let",
            "let r = mk(1);\nlet _ = if n == 1 { r };",
            "dR1\nend\n",
        ),
        (
            "control: else-struct-literal",
            "let _ = if n == 1 { R { id: 2, name: f\"a\" } } else { R { id: 3, name: f\"b\" } };",
            "dR2\nend\n",
        ),
        (
            "control: else-call",
            "let _ = if n == 1 { mk(4) } else { mk(5) };",
            "dR4\nend\n",
        ),
        (
            "control: else-if-chain",
            "let _ = if n == 0 { mk(20) } else if n == 1 { mk(21) } else { mk(22) };",
            "dR21\nend\n",
        ),
        (
            "control: field-is-a-place",
            "let s = f\"live\";\nlet _ = if n == 1 { H { s: s } };\nprintln(\"kept\");",
            "kept\nend\n",
        ),
        (
            "control: branch-not-taken",
            "let _ = if n == 2 { mk(9) };",
            "end\n",
        ),
        (
            "control: branch-not-taken-place-tail",
            "let r = mk(18);\nlet _ = if n == 2 { r };\nprintln(\"still\");",
            "dR18\nstill\nend\n",
        ),
    ] {
        let src = format!("{hdr}fn main() {{\nlet n = 1;\n{body}\nprintln(\"end\");\n}}\n");
        assert_eq!(run_program(&src).as_deref(), Some(want), "[{label}]");
    }
}

/// B-2026-09-19-44 — a DESTRUCTURING `if let` over a boxed `Option`/`Result`
/// tuple payload loses the `Drop` body of every element it does NOT take.
///
/// `if let Some((_, b)) = o { … }` moves element 1 out and leaves element 0
/// with the source, so the source's payload-bodies walk is the only holder
/// of element 0's body. `compile_if_let` called
/// `suppress_optres_payload_bodies_for_match_scoped`, which is
/// all-or-nothing: it stood the whole walk down and the body ran nowhere.
/// Measured `n9` on the JIT, AOT-`O0` and AOT-`O2` against `--interp`'s
/// `dH1 n9` — a run-vs-build divergence with the COMPILED side lagging, so
/// repairing codegen removes a divergence rather than manufacturing one.
///
/// The `match` spelling has been correct since B-2026-09-14-18, which put
/// the same per-element narrowing in front of the same disarm in the arm
/// loop. The IR names the difference exactly: the losing cell stores
/// `i1 true` then `i1 false` into `%optresbodies.o`, so `%cmrun.armed` is
/// false and the walker is emitted and never called; the `match` cell calls
/// the MASKED walker unconditionally.
///
/// THE FIX SHARES THE ARM VERSION'S BODY RATHER THAN COPYING IT. The two
/// callers differ only in how they answer "which elements did this scope
/// move out" — an arm asks it of an expression plus a guard, an `if let`
/// asks it of a block and has no guard — so that answer became an ARGUMENT
/// and everything below it is one function
/// (`narrow_callee_owned_tuple_payload_bodies_core`). The `Block` sibling
/// of the analyzer is `binding_use::optres_block_moved_destructured_elems`.
///
/// NINE CELLS, of which two are the repair, six are controls that must not
/// move, and one is a pinned gap belonging to another row. The controls
/// cover both channels (boxed and inline), both heads (`Some`, `Ok`), a
/// whole-value binding, a borrow-only arm and an arm that takes
/// everything — the four ways the narrowing is supposed to decline.
#[test]
fn e2e_destructuring_if_let_runs_the_untaken_payload_elements_drop_body() {
    // (label, program, AOT expectation, interpreter expectation)
    for (label, prog, want, interp_want) in [
            (
                // THE FIX. One element is taken out by the arm and the other is
                // left behind under a `_`, so the source is still the only
                // holder of element 0's body. The all-or-nothing disarm stood
                // the whole walk down and nothing ran it: `n9` on all three
                // compiled surfaces against `--interp`'s `dH1 n9`.
                "iflet-wildcard-elem",
                r#"struct H { id: i64, s: String }
impl Drop for H { fn drop(mut ref self) { println(f"dH{self.id}") } }
fn mkh(i: i64) -> H { return H { id: i, s: f"s{i}" } }
fn f(o: Option[(H, i64)]) -> i64 { if let Some((_, b)) = o { return b } return 0 }
fn main() { println(f"n{f(Option.Some((mkh(1), 9)))}") }
"#.to_string(),
                "dH1\nn9\n".to_string(),
                "dH1\nn9\n".to_string(),
            ),
            (
                // THE FIX, NAMED SPELLING. `a` is bound rather than wildcarded
                // and never used, which the `_` cell cannot distinguish for us
                // -- the moved-elements analyzer classifies a binding by how
                // the BLOCK uses it, so this cell is what proves it reads the
                // block and not just the pattern.
                "iflet-bound-unused-elem",
                r#"struct H { id: i64, s: String }
impl Drop for H { fn drop(mut ref self) { println(f"dH{self.id}") } }
fn mkh(i: i64) -> H { return H { id: i, s: f"s{i}" } }
fn f(o: Option[(H, i64)]) -> i64 { if let Some((a, b)) = o { return b } return 0 }
fn main() { println(f"n{f(Option.Some((mkh(1), 9)))}") }
"#.to_string(),
                "dH1\nn9\n".to_string(),
                "dH1\nn9\n".to_string(),
            ),
            (
                // CONTROL: the `match` spelling of the same program, correct on
                // every surface since B-2026-09-14-18 wired the narrowing into
                // the arm loop. It is the oracle the `if let` cells above were
                // moved onto, and it must not move.
                "match-wildcard-elem",
                r#"struct H { id: i64, s: String }
impl Drop for H { fn drop(mut ref self) { println(f"dH{self.id}") } }
fn mkh(i: i64) -> H { return H { id: i, s: f"s{i}" } }
fn f(o: Option[(H, i64)]) -> i64 { match o { Option.Some((_, b)) => { return b } Option.None => { return 0 } } }
fn main() { println(f"n{f(Option.Some((mkh(1), 9)))}") }
"#.to_string(),
                "dH1\nn9\n".to_string(),
                "dH1\nn9\n".to_string(),
            ),
            (
                // CONTROL: the payload bound WHOLE. No tuple pattern, so the
                // moved-elements analyzer declines by construction and the
                // existing path decides. Already correct.
                "iflet-whole-binding",
                r#"struct H { id: i64, s: String }
impl Drop for H { fn drop(mut ref self) { println(f"dH{self.id}") } }
fn mkh(i: i64) -> H { return H { id: i, s: f"s{i}" } }
fn f(o: Option[(H, i64)]) -> i64 { if let Some(t) = o { return t.1 } return 0 }
fn main() { println(f"n{f(Option.Some((mkh(1), 9)))}") }
"#.to_string(),
                "dH1\nn9\n".to_string(),
                "dH1\nn9\n".to_string(),
            ),
            (
                // CONTROL, THE OTHER CHANNEL: a one-word payload element rides
                // INLINE rather than boxed, so the bodies stay with the caller
                // and `callee_owned_payload_bodies_params` does not hold the
                // scrutinee. The narrowing returns false at its first gate.
                "iflet-inline-channel",
                r#"struct N { id: i64 }
impl Drop for N { fn drop(mut ref self) { println(f"dN{self.id}") } }
fn mkn(i: i64) -> N { return N { id: i } }
fn f(o: Option[(N, i64)]) -> i64 { if let Some((_, b)) = o { return b } return 0 }
fn main() { println(f"n{f(Option.Some((mkn(1), 9)))}") }
"#.to_string(),
                "dN1\nn9\n".to_string(),
                "dN1\nn9\n".to_string(),
            ),
            (
                // CONTROL: BOTH elements leave the source. `moved.len() == arity`,
                // so the narrowing declines and the all-or-nothing disarm runs
                // -- which is the case it was written for.
                "iflet-both-elems-moved",
                r#"struct H { id: i64, s: String }
impl Drop for H { fn drop(mut ref self) { println(f"dH{self.id}") } }
fn mkh(i: i64) -> H { return H { id: i, s: f"s{i}" } }
fn f(o: Option[(H, i64)]) -> i64 {
    if let Some((h, b)) = o {
        let g = h;
        return b;
    }
    return 0;
}
fn main() { println(f"n{f(Option.Some((mkh(1), 9)))}") }
"#.to_string(),
                "dH1\nn9\n".to_string(),
                "dH1\nn9\n".to_string(),
            ),
            (
                // CONTROL: the `Result` head. Same shape, same fix; here to pin
                // that the gate's `Some`/`Ok`/`Err` set is exercised by more
                // than one member.
                "iflet-result-head",
                r#"struct H { id: i64, s: String }
impl Drop for H { fn drop(mut ref self) { println(f"dH{self.id}") } }
fn mkh(i: i64) -> H { return H { id: i, s: f"s{i}" } }
fn f(o: Result[(H, i64), i64]) -> i64 { if let Ok((_, b)) = o { return b } return 0 }
fn main() { println(f"n{f(Result.Ok((mkh(1), 9)))}") }
"#.to_string(),
                "dH1\nn9\n".to_string(),
                "dH1\nn9\n".to_string(),
            ),
            (
                // CONTROL: the arm only READS the bound element (`h.id`), so
                // nothing leaves and the source owns both. Already correct, and
                // the cell that would redden if the analyzer called a read a
                // move.
                "iflet-borrow-only",
                r#"struct H { id: i64, s: String }
impl Drop for H { fn drop(mut ref self) { println(f"dH{self.id}") } }
fn mkh(i: i64) -> H { return H { id: i, s: f"s{i}" } }
fn f(o: Option[(H, i64)]) -> i64 { if let Some((h, b)) = o { return h.id + b } return 0 }
fn main() { println(f"n{f(Option.Some((mkh(1), 9)))}") }
"#.to_string(),
                "dH1\nn10\n".to_string(),
                "dH1\nn10\n".to_string(),
            ),
            (
                // PINNED GAP, filed as B-2026-09-20-65. The same divergence
                // as the two cells at the top (compiled loses the body,
                // `--interp` runs it) at a DIFFERENT call site:
                // `compile_let_else` calls
                // `suppress_optres_payload_bodies_for_match`, the
                // unconditional `takes_payload: true` wrapper. Measured on
                // this exact cell, before and after this fix: unmoved. Pinned
                // at the WRONG answer so its own fix has to flip it.
                //
                // IT IS NOT B-2026-09-17-16, though that row names the
                // `let ... else` form and the resemblance is close enough to
                // mis-cite. That row's cell binds the payload WHOLE
                // (`let Some(t) = o else`), where both bodies are lost because
                // the DESTINATION registers nothing and the repair is to fund
                // it. This cell DESTRUCTURES, so the untaken element never
                // leaves the source and there is no destination to fund: the
                // source's walk has to stay armed over it, which is this row's
                // mechanism and not that one's. What makes the let-else leg its
                // own row rather than a line in this fix is that its binding
                // ESCAPES the block, so the block-scoped "only read through
                // this block" question the analyzer here asks is the wrong one
                // there -- every non-wildcard leaf escapes and is moved.
                "letelse-wildcard-elem",
                r#"struct H { id: i64, s: String }
impl Drop for H { fn drop(mut ref self) { println(f"dH{self.id}") } }
fn mkh(i: i64) -> H { return H { id: i, s: f"s{i}" } }
fn f(o: Option[(H, i64)]) -> i64 { let Some((_, b)) = o else { return 0 } return b }
fn main() { println(f"n{f(Option.Some((mkh(1), 9)))}") }
"#.to_string(),
                "n9\n".to_string(),
                "dH1\nn9\n".to_string(),
            ),
        ] {
            let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(&prog);
            assert!(
                interp_errs.is_empty(),
                "[{label}] interp errored: {interp_errs:?}"
            );
            assert_eq!(interp_out.join(""), interp_want, "[{label}] interpreter");
            let Some(aot) = run_program(&prog) else {
                continue;
            };
            assert_eq!(aot, want, "[{label}] AOT");
        }
}

/// B-2026-09-19-57 — A HEAP-CARRYING `Option` PAYLOAD PART HANDED OUT OF AN
/// ARM RAN ITS `Drop` BODY AN EXTRA TIME BEFORE THE CONSUMING CALL.
///
/// `sink(fp(Some((P { name: "a" }, P { name: "b" }))))` over
/// `fn fp(o: Option[(P, P)]) -> P { match o { Some(t) => return t.0 } }`
/// and `struct P { name: String }` printed `dP[a] dP[b] sank[a] dP[a]` on
/// every compiled backend against `--interp`'s correct
/// `dP[b] sank[a] dP[a]` — two bodies for one construction of `a`, the
/// extra one running BEFORE the consuming call.
///
/// FIXED BY A COMMIT WRITTEN FOR A DIFFERENT ROW. `52602ba`,
/// B-2026-09-19-34's fix, narrowed a BOXED `Option`/`Result` payload's body
/// walk to the parts an arm did not take, and repaired this as a side
/// effect. Bisected over the night's commits: `52602ba^` and every earlier
/// tree print the extra body, every tree containing it does not, and
/// `a29ca2a` — the tree just BEFORE this family's other fix, `1aff971` —
/// is already correct, so none of it is owed to that one.
///
/// THE ROW'S STATED TRIGGER IS REFUTED BY THE `widescalar` CELL. The row
/// concluded "the trigger is the payload element CARRYING HEAP, not the
/// shape of the call", because its scalar twin `(S, S)` over
/// `struct S { id: i64 }` was correct on all four surfaces. The real
/// discriminator is WIDTH: a seeded `Option` has three payload words, so
/// `(S, S)` rides inline and `(P, P)` — two three-word `String`s — boxes.
/// `(W, W)` over `struct W { a: i64, b: i64 }` carries NO heap at all and
/// is four words, and it printed `dW5 dW6 sank5 dW5` on all
/// three compiled surfaces at `52602ba^`, with no `String` or `Vec`
/// anywhere in the program. Heap was a proxy for
/// width the whole time.
///
/// THE `method` CELL IS PINNED AT A WRONG ANSWER ON PURPOSE. It is
/// B-2026-09-19-56 — a method-call result consumed by a by-value free
/// function loses THAT function's own param body, agreed on all four
/// surfaces. That row pins the SCALAR spelling; this cell is the heap one,
/// which the fix above moved from `dP[a] dP[b] sank[a]` onto the
/// interpreter's `dP[b] sank[a]`. Only the EARLY body moved, so nothing
/// regressed — but the two spellings now agree on the wrong answer, which
/// is the shape no A/B cross-check can see.
///
/// THE REMAINING CELLS ARE THE ROW'S OWN "NOT MEASURED" LIST: the `Result`
/// head, a payload whose heap is a `Vec` rather than a `String`, and the
/// result BOUND to a local instead of consumed by a call.
///
/// THE DUE SEQUENCE IS HAND-DERIVED, not taken from the interpreter: `fp`
/// owns `o` by value, so the element it does not hand out dies in its
/// frame; the one it returns is owned by `sink`'s by-value param and dies
/// after the read. Two constructions, two bodies.
///
/// The leak half is `asan_heap_payload_part_handed_out_of_an_arm_no_leak`
/// (tests/memory_sanitizer.rs), which answers the row's last open question.
#[test]
fn e2e_heap_payload_part_handed_out_of_an_arm_runs_one_body() {
    const P: &str = "struct P { name: String }\n\
             impl Drop for P { fn drop(mut ref self) { println(f\"dP[{self.name}]\") } }\n";
    const PARG: &str = "Some((P { name: \"a\" }, P { name: \"b\" }))";
    const SINK: &str = "fn sink(r: P) { println(f\"sank[{r.name}]\") }\n";
    // (label, source, expectation -- both backends, all four surfaces)
    for (label, prog, want) in [
            (
                "the row's own cell: a free fn, boxed heap payload",
                format!(
                    "{P}fn fp(o: Option[(P, P)]) -> P {{ match o {{ Some(t) => {{ return t.0; }} None => {{ return P {{ name: \"z\" }}; }} }} }}\n\
                     {SINK}fn main() {{ sink(fp({PARG})); }}\n"
                ),
                "dP[b]\nsank[a]\ndP[a]\n",
            ),
            (
                "control: the SCALAR twin, which the row measured as correct",
                "struct S { id: i64 }\n\
                     impl Drop for S { fn drop(mut ref self) { println(f\"dS{self.id}\") } }\n\
                     fn fs(o: Option[(S, S)]) -> S { match o { Some(t) => { return t.0; } None => { return S { id: 0 }; } } }\n\
                     fn sink(r: S) { println(f\"sank{r.id}\") }\n\
                     fn main() { sink(fs(Some((S { id: 5 }, S { id: 6 })))); }\n".to_string(),
                "dS6\nsank5\ndS5\n",
            ),
            (
                "widescalar: FOUR scalar words, no heap at all -- the row's trigger, refuted",
                "struct W { a: i64, b: i64 }\n\
                     impl Drop for W { fn drop(mut ref self) { println(f\"dW{self.a}\") } }\n\
                     fn fw(o: Option[(W, W)]) -> W { match o { Some(t) => { return t.0; } None => { return W { a: 0, b: 0 }; } } }\n\
                     fn sink(r: W) { println(f\"sank{r.a}\") }\n\
                     fn main() { sink(fw(Some((W { a: 5, b: 50 }, W { a: 6, b: 60 })))); }\n".to_string(),
                "dW6\nsank5\ndW5\n",
            ),
            (
                "not measured by the row: the `Result` head",
                format!(
                    "{P}fn fp(o: Result[(P, P), i64]) -> P {{ match o {{ Ok(t) => {{ return t.0; }} Err(e) => {{ return P {{ name: \"z\" }}; }} }} }}\n\
                     {SINK}fn main() {{ sink(fp(Result.Ok((P {{ name: \"a\" }}, P {{ name: \"b\" }})))); }}\n"
                ),
                "dP[b]\nsank[a]\ndP[a]\n",
            ),
            (
                "not measured by the row: the heap is a `Vec`, and both lengths are read",
                "struct P { xs: Vec[i64], name: String }\n\
                     impl Drop for P { fn drop(mut ref self) { println(f\"dP[{self.name}]len{self.xs.len()}\") } }\n\
                     fn fp(o: Option[(P, P)]) -> P { match o { Some(t) => { return t.0; } None => { return P { xs: [0], name: \"z\" }; } } }\n\
                     fn sink(r: P) { println(f\"sank[{r.name}]len{r.xs.len()}\") }\n\
                     fn main() { sink(fp(Some((P { xs: [1, 2, 3], name: \"a\" }, P { xs: [4, 5], name: \"b\" })))); }\n".to_string(),
                "dP[b]len2\nsank[a]len3\ndP[a]len3\n",
            ),
            (
                "not measured by the row: the result is BOUND rather than consumed",
                format!(
                    "{P}fn fp(o: Option[(P, P)]) -> P {{ match o {{ Some(t) => {{ return t.0; }} None => {{ return P {{ name: \"z\" }}; }} }} }}\n\
                     fn main() {{ let g = fp({PARG}); println(f\"got[{{g.name}}]\"); println(\"end\") }}\n"
                ),
                "dP[b]\ngot[a]\ndP[a]\nend\n",
            ),
            (
                // This cell was pinned at `dP[b] sank[a]` for B-2026-09-19-56
                // -- the method-call result whose consumer's own param body
                // ran nowhere. 52602ba had removed the EARLY `dP[a]` along
                // with the free spelling's, leaving the two spellings agreed
                // on a wrong answer, which is what the pin was guarding.
                //
                // The pin did its job: the fix landed here rather than
                // silently. The HEAP payload is not what the row was about --
                // an instance method was simply not one of the producer shapes
                // either backend enumerates -- so this cell and the scalar one
                // in `e2e_method_call_keeps_an_unmoved_payload_parts_drop_body`
                // moved together.
                "the METHOD spelling, formerly B-2026-09-19-56's loss, now correct",
                format!(
                    "{P}struct H {{ n: i64 }}\n\
                     impl H {{ fn ep(ref self, o: Option[(P, P)]) -> P {{ match o {{ Some(t) => {{ return t.0; }} None => {{ return P {{ name: \"z\" }}; }} }} }} }}\n\
                     {SINK}fn main() {{ let h = H {{ n: 1 }}; sink(h.ep({PARG})); }}\n"
                ),
                "dP[b]\nsank[a]\ndP[a]\n",
            ),
        ] {
            let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(&prog);
            assert!(
                interp_errs.is_empty(),
                "[{label}] interp errored: {interp_errs:?}"
            );
            assert_eq!(interp_out.join(""), want, "[{label}] interpreter");
            if let Some(aot) = run_program(&prog) {
                assert_eq!(aot, want, "[{label}] AOT");
            }
        }
}

/// B-2026-09-13-11 — the BODY COUNT for a read-only destructure of an
/// own-`Drop` enum's payload: exactly one enclosing body, on the
/// interpreter and AOT alike.
///
/// That row was filed because the count DEPENDED ON THE ARM BODY. The same
/// `enum K { A(R2), B }` with `impl Drop for K` printed one `dK` when the
/// arm only read the leaf and NONE when the arm moved it on — agreed on all
/// four surfaces, so no A/B gate saw it, and as the row argued, the count is
/// arguable in both directions but cannot be arguable per arm body.
///
/// IT IS CLOSED BY REJECTION, not by choosing a count.
/// `partial_move_of_drop_enum` is `Deny` since B-2026-09-13-14, so the
/// moving arm no longer compiles and the dependence is unreachable; what is
/// left to pin is that the READING arm still runs its one body. This is
/// that pin, and it is a body-count assertion rather than a diagnostic one,
/// which is why it is here and not beside
/// `partial_move_of_drop_enum_fires_on_moves_and_not_on_reads`.
///
/// THE MOVING SPELLING IS DELIBERATELY NOT A CELL. It is a compile error
/// now, so `run_program` has nothing to return; the diagnostic is asserted
/// in the typechecker test, whose cells 10-15 also carry this row's five
/// closing measurements (a `Result` head, `while let`, a two-field variant
/// moving one field and reading the other, that variant read in full, and a
/// `Vec` element in both directions).
///
/// Behind an explicit `#[allow(partial_move_of_drop_enum)]` the moving
/// spelling still compiles and still prints NO `dK`, measured on all four
/// surfaces. That is the annotation doing its job — it exists to keep the
/// pre-rule shape reachable for the fixtures that pin drop behaviour — not
/// a residue of this row, and it is memory-clean (`-O0` valgrind: 13
/// allocs, 13 frees, 0 bytes in use at exit, 0 errors, no invalid access).
#[test]
fn e2e_read_only_enum_payload_destructure_runs_one_enclosing_body() {
    for (label, prog, want) in [
            (
                // The row's own read-only twin, `acc` left empty on purpose so
                // the `len:0` says the leaf really did not travel.
                "match arm reads the leaf",
                "struct R2 { s: String }\n\
                 enum K { A(R2), B }\n\
                 impl Drop for K { fn drop(mut ref self) { println(\"dK\") } }\n\
                 fn show(x: Option[K], acc: mut ref Vec[R2]) {\n\
                 match x { Option.Some(K.A(r)) => { println(f\"a:{r.s}\") } Option.Some(K.B) => {} Option.None => {} }\n\
                 }\n\
                 fn main() { let mut acc: Vec[R2] = []; show(Option.Some(K.A(R2 { s: f\"z\" })), mut acc); println(f\"len:{acc.len()}\"); println(\"end\") }\n",
                "a:z\ndK\nlen:0\nend\n",
            ),
            (
                // Two payload fields, both read: still ONE body, so the count
                // is per VALUE and not per bound field.
                "two-field variant, both fields read",
                "struct R2 { s: String }\n\
                 enum K2 { A(R2, R2), B }\n\
                 impl Drop for K2 { fn drop(mut ref self) { println(\"dK2\") } }\n\
                 fn show(x: Option[K2]) {\n\
                 match x { Option.Some(K2.A(p, q)) => { println(f\"{p.s}/{q.s}\") } Option.Some(K2.B) => {} Option.None => {} }\n\
                 }\n\
                 fn main() { show(Option.Some(K2.A(R2 { s: f\"p\" }, R2 { s: f\"q\" }))); println(\"end\") }\n",
                "p/q\ndK2\nend\n",
            ),
            (
                // A `Vec` ELEMENT of the same enum — the container-bodies
                // channel the row listed as unmeasured. One body per element.
                "Vec element, arm reads the leaf",
                "struct R2 { s: String }\n\
                 enum K { A(R2), B }\n\
                 impl Drop for K { fn drop(mut ref self) { println(\"dK\") } }\n\
                 fn main() { let v: Vec[K] = [K.A(R2 { s: f\"z\" })];\n\
                 for e in v { match e { K.A(r) => { println(f\"a:{r.s}\") } K.B => {} } }\n\
                 println(\"end\") }\n",
                "a:z\ndK\nend\n",
            ),
            (
                // A SCALAR projection is a read (the `copy_read` oracle's
                // cell), and the body is owed just the same.
                "scalar projection off the leaf",
                "struct R2 { s: String, id: i64 }\n\
                 enum K { A(R2), B }\n\
                 impl Drop for K { fn drop(mut ref self) { println(\"dK\") } }\n\
                 fn show(x: Option[K]) -> i64 {\n\
                 match x { Option.Some(K.A(r)) => { return r.id; } Option.Some(K.B) => { return 0; } Option.None => { return 0; } }\n\
                 }\n\
                 fn main() { println(f\"n:{show(Option.Some(K.A(R2 { s: f\"z\", id: 7 })))}\"); println(\"end\") }\n",
                "dK\nn:7\nend\n",
            ),
        ] {
            let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(prog);
            assert!(
                interp_errs.is_empty(),
                "[{label}] interp errored: {interp_errs:?}"
            );
            assert_eq!(interp_out.join(""), want, "[{label}] interpreter");
            if let Some(aot) = run_program(prog) {
                assert_eq!(aot, want, "[{label}] AOT");
            }
        }
}

/// B-2026-09-14-2 (arm-binding half) — a CONTAINER payload bound out by a
/// `match` / `if let` arm runs its elements' `Drop` bodies, once, for BOTH
/// container kinds.
///
/// THE DISCRIMINATOR IS THE SCRUTINEE'S POSITION, not the container kind and
/// not the payload's width — both of those were tried and measured wrong.
/// Holding the element type fixed and varying only where the envelope came
/// from: a LOCAL scrutinee ran no bodies on either backend, a by-value PARAM
/// one ran exactly one. Sweeping `Array[E, N]` across the inline/boxed
/// word-count boundary (2, 3, 4 words against the `Option` area's 3) changes
/// nothing — every width is silent for a local and correct for a param.
///
/// So the registration is gated to a LOCAL scrutinee of the SEEDED PAIR: the
/// callee-owned-param machinery already runs a param envelope's payload
/// bodies, and a USER enum's own payload walker already runs a local one —
/// registering beside either double-fires, measured
/// `dRa1 dRa2 dRa1 dRa2` on `e2e_boxed_array_payload_runs_its_element_drop_bodies`
/// (whose scrutinee is a param, despite the name) and on its
/// `mono-enum-array-payload-matched-out` case.
#[test]
fn e2e_arm_bound_container_payload_runs_element_bodies() {
    const PRE: &str = "struct E1 { id: i64 }\n\
                           impl Drop for E1 { fn drop(mut ref self) { println(f\"d{self.id}\") } }\n";
    for (label, body, want) in [
            (
                "local envelope, Array payload",
                "let a: Array[E1, 2] = [E1 { id: 1 }, E1 { id: 2 }];\n                 let o: Option[Array[E1, 2]] = Option.Some(a);\n                 match o { Option.Some(t) => { println(f\"s{t[0].id}\"); } Option.None => { println(\"n\"); } }",
                "s1\nd1\nd2\nmid\n",
            ),
            (
                "local envelope, Vec payload",
                "let o: Option[Vec[E1]] = Option.Some([E1 { id: 1 }, E1 { id: 2 }]);\n                 match o { Option.Some(t) => { println(f\"s{t.len()}\"); } Option.None => { println(\"n\"); } }",
                "s2\nd1\nd2\nmid\n",
            ),
            (
                "if let, local envelope, Array payload",
                "let a: Array[E1, 2] = [E1 { id: 1 }, E1 { id: 2 }];\n                 let o: Option[Array[E1, 2]] = Option.Some(a);\n                 if let Option.Some(t) = o { println(f\"s{t[0].id}\"); }",
                "s1\nd1\nd2\nmid\n",
            ),
            // Width sweep across the inline/boxed payload boundary: the answer
            // must not depend on it, which is what rules the word count out as
            // the discriminator.
            (
                "width 3 words (inline edge)",
                "let a: Array[E1, 3] = [E1 { id: 1 }, E1 { id: 2 }, E1 { id: 3 }];\n                 let o: Option[Array[E1, 3]] = Option.Some(a);\n                 match o { Option.Some(t) => { println(f\"s{t[0].id}\"); } Option.None => { println(\"n\"); } }",
                "s1\nd1\nd2\nd3\nmid\n",
            ),
            (
                "width 4 words (boxed edge)",
                "let a: Array[E1, 4] = [E1 { id: 1 }, E1 { id: 2 }, E1 { id: 3 }, E1 { id: 4 }];\n                 let o: Option[Array[E1, 4]] = Option.Some(a);\n                 match o { Option.Some(t) => { println(f\"s{t[0].id}\"); } Option.None => { println(\"n\"); } }",
                "s1\nd1\nd2\nd3\nd4\nmid\n",
            ),
            // Controls: the two positions that already had an owner must keep
            // exactly one body, not two.
            (
                "control: by-value PARAM envelope keeps one body",
                "let a: Array[E1, 2] = [E1 { id: 1 }, E1 { id: 2 }];\n                 takeA(Option.Some(a));",
                "s1\nd1\nd2\nmid\n",
            ),
            (
                "control: non-Drop element stays silent",
                "let o: Option[Vec[i64]] = Option.Some([1, 2]);\n                 match o { Option.Some(t) => { println(f\"s{t.len()}\"); } Option.None => { println(\"n\"); } }",
                "s2\nmid\n",
            ),
        ] {
            let src = format!(
                "{PRE}fn takeA(x: Option[Array[E1, 2]]) {{ match x {{ Option.Some(t) => {{ println(f\"s{{t[0].id}}\"); }} Option.None => {{ println(\"n\"); }} }} }}\n\
                 fn main() {{\n{body}\nprintln(\"mid\");\n}}\n"
            );
            let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(&src);
            assert!(interp_errs.is_empty(), "[{label}] interp errored: {interp_errs:?}");
            assert_eq!(interp_out.join(""), want, "[{label}] interpreter");
            if let Some(aot) = run_program(&src) {
                assert_eq!(aot, want, "[{label}] AOT");
            }
        }
}

/// B-2026-09-06-25 — the interpreter took NO copy for two materializations
/// of a view bound off a BORROW-projection scrutinee: an arm value / a
/// `return` forwarding the binding (and a whole `return h.r`) through a
/// named `ref` / `mut ref` param let the caller-side escape masks fire on
/// a borrowed argument, so the CALLER's struct died with an empty shell;
/// and a by-value call argument under `ref self` / `mut ref self` bound
/// `r` with no slot (the `self.e` root had no disarm-and-stash lockstep),
/// so the copy's body ran nowhere. Codegen was right on every cell; this
/// pins it against the interpreter twin, byte for byte. `p_read` /
/// `m_read` are the read-only controls (one body); `m_mixed/read` is the
/// user-enum whole-match trade (B-2026-09-02-14): the guard arm's hand-out
/// makes the untaken path pay for the copy too, on every backend.
///
/// Twin of `tests/interpreter.rs`'s `test_borrow_projection_view_materialized_by_arm_value_or_call_arg_copies`, pinned to the same string.
#[test]
fn e2e_borrow_projection_view_materialized_by_arm_value_or_call_arg_copies() {
    let Some(out) = run_program(
        r#"struct R { id: i64, tag: String, xs: Vec[i64] }
impl Drop for R { fn drop(mut ref self) { println(f"  dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, tag: f"t{i}", xs: [i] } }
enum E { A(R), B }
impl Drop for E { fn drop(mut ref self) { println("  dE") } }
struct H { e: E }
struct W { r: R }
fn consume(x: R) -> i64 { return x.id }

#[allow(partial_move_of_drop_enum)]
fn p_out(h: ref H) -> R { let r2 = match h.e { E.A(r) => r, E.B => mk(0) }; return r2; }
#[allow(partial_move_of_drop_enum)]
fn p_out_mut(h: mut ref H) -> R { match h.e { E.A(r) => { return r; } E.B => { return mk(0); } } }
fn p_field(w: ref W) -> R { return w.r; }
fn p_consume(h: ref H) -> i64 { match h.e { E.A(r) => { return consume(r); } E.B => { return 0; } } }
fn p_read(h: ref H) -> i64 { match h.e { E.A(r) => { return r.id; } E.B => { return 0; } } }
impl H {
    fn m_consume(ref self) -> i64 { match self.e { E.A(r) => { return consume(r); } E.B => { return 0; } } }
    fn m_consume_mut(mut ref self) -> i64 { match self.e { E.A(r) => { return consume(r); } E.B => { return 0; } } }
    fn m_consume_iflet(ref self) -> i64 { if let E.A(r) = self.e { return consume(r); } return 0; }
    #[allow(partial_move_of_drop_enum)]
    fn m_let(ref self) -> i64 { match self.e { E.A(r) => { let m = r; return consume(m); } E.B => { return 0; } } }
    #[allow(partial_move_of_drop_enum)]
    fn m_out(ref self) -> R { let r2 = match self.e { E.A(r) => r, E.B => mk(0) }; return r2; }
    fn m_read(ref self) -> i64 { match self.e { E.A(r) => { return r.id; } E.B => { return 0; } } }
    fn m_mixed(ref self, k: bool) -> i64 { match self.e { E.A(r) if k => { return consume(r); } E.A(r) => { return r.id; } E.B => { return 0; } } }
}

fn main() {
    println("p_out"); let a1 = H { e: E.A(mk(1)) }; let r1 = p_out(a1); println(f"  got{r1.id}");
    println("p_out_mut"); let mut a2 = H { e: E.A(mk(2)) }; let r2 = p_out_mut(mut a2); println(f"  got{r2.id}");
    println("p_field"); let w3 = W { r: mk(3) }; let r3 = p_field(w3); println(f"  got{r3.id}");
    println("p_consume"); let a4 = H { e: E.A(mk(4)) }; let x4 = p_consume(a4); println(f"  got{x4}");
    println("p_read"); let a5 = H { e: E.A(mk(5)) }; let x5 = p_read(a5); println(f"  got{x5}");
    println("m_consume"); let a6 = H { e: E.A(mk(6)) }; let x6 = a6.m_consume(); println(f"  got{x6}");
    println("m_consume_mut"); let mut a7 = H { e: E.A(mk(7)) }; let x7 = a7.m_consume_mut(); println(f"  got{x7}");
    println("m_consume_iflet"); let a8 = H { e: E.A(mk(8)) }; let x8 = a8.m_consume_iflet(); println(f"  got{x8}");
    println("m_let"); let a9 = H { e: E.A(mk(9)) }; let x9 = a9.m_let(); println(f"  got{x9}");
    println("m_out"); let a10 = H { e: E.A(mk(10)) }; let r10 = a10.m_out(); println(f"  got{r10.id}");
    println("m_read"); let a11 = H { e: E.A(mk(11)) }; let x11 = a11.m_read(); println(f"  got{x11}");
    println("m_mixed/taken"); let a12 = H { e: E.A(mk(12)) }; let x12 = a12.m_mixed(true); println(f"  got{x12}");
    println("m_mixed/read"); let a13 = H { e: E.A(mk(13)) }; let x13 = a13.m_mixed(false); println(f"  got{x13}");
    println("end");
}
"#,
    ) else {
        return;
    };
    assert_eq!(
        out,
        r#"p_out
  dE
  dR1
  got1
  dR1
p_out_mut
  dE
  dR2
  got2
  dR2
p_field
  dR3
  got3
  dR3
p_consume
  dR4
  dE
  dR4
  got4
p_read
  dE
  dR5
  got5
m_consume
  dR6
  dE
  dR6
  got6
m_consume_mut
  dR7
  dE
  dR7
  got7
m_consume_iflet
  dR8
  dE
  dR8
  got8
m_let
  dR9
  dE
  dR9
  got9
m_out
  dE
  dR10
  got10
  dR10
m_read
  dE
  dR11
  got11
m_mixed/taken
  dR12
  dE
  dR12
  got12
m_mixed/read
  dR13
  dE
  dR13
  got13
end
"#
    );
}

/// B-2026-09-06-24 — a read-only `if let` / `while let` over a projection
/// off a `ref` / `mut ref` param or a borrowed `self` bound its payload
/// with a UserDrop slot of its own and ran the body twice on every
/// compiled backend (`dR5 5 dE dR5`), where the same program's `match`
/// spelling and the interpreter bind a view (`5 dE dR5`). The `match`
/// path classed such a scrutinee as a borrow via
/// `scrutinee_is_readonly_borrowed_place`; the block spellings had no
/// twin. `p_iflet_move` / `p_iflet_consume` / `m_iflet_move` are the
/// escaping controls, which still take the stopgap copy (two bodies).
///
/// Twin of `tests/interpreter.rs`'s `test_readonly_if_let_over_borrow_projection_binds_a_view`, pinned to the same string.
#[test]
fn e2e_readonly_if_let_over_borrow_projection_binds_a_view() {
    let Some(out) = run_program(
        r#"struct R { id: i64, tag: String, xs: Vec[i64] }
impl Drop for R { fn drop(mut ref self) { println(f"  dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, tag: f"t{i}", xs: [i] } }
enum E { A(R), B }
impl Drop for E { fn drop(mut ref self) { println("  dE") } }
struct S { e: E }
struct H { e: E }
struct H2 { s: S }
fn consume(x: R) -> i64 { return x.id }

fn p_iflet(h: ref H) -> i64 { if let E.A(r) = h.e { return r.id; } else { return 0; } }
fn p_iflet_mut(h: mut ref H) -> i64 { if let E.A(r) = h.e { return r.id; } else { return 0; } }
fn p_iflet_assign(h: ref H) -> i64 { let mut t = 0; if let E.A(r) = h.e { t = r.id + 1; } return t; }
fn p_whilelet(h: ref H) -> i64 { while let E.A(r) = h.e { return r.id; } return 0; }
fn p_iflet2(h: ref H2) -> i64 { if let E.A(r) = h.s.e { return r.id; } else { return 0; } }
fn p_match(h: ref H) -> i64 { match h.e { E.A(r) => { return r.id; } E.B => { return 0; } } }
#[allow(partial_move_of_drop_enum)]
fn p_iflet_move(h: ref H) -> i64 { if let E.A(r) = h.e { let m = r; return m.id; } else { return 0; } }
fn p_iflet_consume(h: ref H) -> i64 { if let E.A(r) = h.e { return consume(r); } return 0; }
impl H {
    fn m_iflet(ref self) -> i64 { if let E.A(r) = self.e { return r.id; } else { return 0; } }
    fn m_iflet_mut(mut ref self) -> i64 { if let E.A(r) = self.e { return r.id; } else { return 0; } }
    fn m_whilelet(ref self) -> i64 { while let E.A(r) = self.e { return r.id; } return 0; }
    #[allow(partial_move_of_drop_enum)]
    fn m_iflet_move(ref self) -> i64 { if let E.A(r) = self.e { let m = r; return m.id; } else { return 0; } }
}

fn main() {
    println("p_iflet"); let a1 = H { e: E.A(mk(1)) }; let x1 = p_iflet(a1); println(f"  got{x1}");
    println("p_iflet_mut"); let mut a2 = H { e: E.A(mk(2)) }; let x2 = p_iflet_mut(mut a2); println(f"  got{x2}");
    println("p_iflet_assign"); let a3 = H { e: E.A(mk(3)) }; let x3 = p_iflet_assign(a3); println(f"  got{x3}");
    println("p_whilelet"); let a4 = H { e: E.A(mk(4)) }; let x4 = p_whilelet(a4); println(f"  got{x4}");
    println("p_iflet2"); let a5 = H2 { s: S { e: E.A(mk(5)) } }; let x5 = p_iflet2(a5); println(f"  got{x5}");
    println("p_match"); let a6 = H { e: E.A(mk(6)) }; let x6 = p_match(a6); println(f"  got{x6}");
    println("p_iflet_move"); let a7 = H { e: E.A(mk(7)) }; let x7 = p_iflet_move(a7); println(f"  got{x7}");
    println("p_iflet_consume"); let a8 = H { e: E.A(mk(8)) }; let x8 = p_iflet_consume(a8); println(f"  got{x8}");
    println("m_iflet"); let a9 = H { e: E.A(mk(9)) }; let x9 = a9.m_iflet(); println(f"  got{x9}");
    println("m_iflet_mut"); let mut a10 = H { e: E.A(mk(10)) }; let x10 = a10.m_iflet_mut(); println(f"  got{x10}");
    println("m_whilelet"); let a11 = H { e: E.A(mk(11)) }; let x11 = a11.m_whilelet(); println(f"  got{x11}");
    println("m_iflet_move"); let a12 = H { e: E.A(mk(12)) }; let x12 = a12.m_iflet_move(); println(f"  got{x12}");
    println("end");
}
"#,
    ) else {
        return;
    };
    assert_eq!(
        out,
        r#"p_iflet
  dE
  dR1
  got1
p_iflet_mut
  dE
  dR2
  got2
p_iflet_assign
  dE
  dR3
  got4
p_whilelet
  dE
  dR4
  got4
p_iflet2
  dE
  dR5
  got5
p_match
  dE
  dR6
  got6
p_iflet_move
  dR7
  dE
  dR7
  got7
p_iflet_consume
  dR8
  dE
  dR8
  got8
m_iflet
  dE
  dR9
  got9
m_iflet_mut
  dE
  dR10
  got10
m_whilelet
  dE
  dR11
  got11
m_iflet_move
  dR12
  dE
  dR12
  got12
end
"#
    );
}

/// B-2026-09-07-7 — a DISCARDED arm whose tail is an aggregate literal over a
/// NAMED LOCAL stranded that local's buffer: `let s = payload(); let _ = if
/// n == 1 { D { s: s } };` lost 38 B per evaluation on every compiled backend.
///
/// B-2026-08-29-5 established the rule — suppression hands a tail's buffer from
/// its source to whatever consumes the branch's value, and a discarded branch
/// has no consumer, so the source keeps its cleanup. That gate sits at the tail
/// hook in `compile_block_with_frame`, which covers a tail that IS an
/// identifier. A literal tail disarms its sources one level down, in
/// `compile_struct_init`'s field loop, and never reached it.
///
/// Cells: the named local into a discarded literal (`let _ =` and bare-statement
/// spellings), a `let`-bound literal STATEMENT inside the same arm — which must
/// keep its disarm, because there the binding owns the buffer and freeing it
/// twice is the failure this family exists to avoid — a minted field, and an
/// arm that is never taken.
///
/// Twin of `tests/interpreter.rs`'s `test_discarded_arm_literal_over_a_named_local`, pinned to the same string.
#[test]
fn e2e_discarded_arm_literal_over_a_named_local() {
    let Some(out) = run_program(
        r#"struct D { s: String }
struct R { id: i64, s: String }
impl Drop for R { fn drop(mut ref self) { println(f"  dR{self.id}") } }
fn seed() -> i64 { env.args().len() }
fn payload() -> String { f"p{seed()}-aaaaaaaaaaaaaaaaaaaaaaaaaaaa" }
fn main() {
  let n = seed();
  println("named_field"); let b = payload(); let _ = if n >= 0 { D { s: b } };
  println("stmt_in_arm"); let c = payload(); if n >= 0 { let q = D { s: c }; println(f"  q={q.s.len()}"); }
  println("mint_field"); let _ = if n >= 0 { R { id: 2, s: payload() } };
  println("bare_stmt_named"); let d = payload(); if n >= 0 { D { s: d } };
  println("not_taken"); let e = payload(); let _ = if n > 900 { D { s: e } };
  println("end");
}
"#,
    ) else {
        return;
    };
    assert_eq!(
        out,
        r#"named_field
stmt_in_arm
  q=31
mint_field
  dR2
bare_stmt_named
not_taken
end
"#
    );
}

/// B-2026-09-06-32 — a `Drop`-carrying ENUM leaf handed back out of a `let`
/// destructure of a by-value param (`let H2 { e, n } = h; return e;`) or out of a
/// bare-tuple `match` arm (`match t { (e, n) => { return e; } }`) aborted with
/// glibc's `free(): double free detected in tcache 2` under `karac run` and at
/// `KARAC_OPT_LEVEL=0`, clean at -O2 and under `--interp`. Two axes localised it:
/// the struct-leaf twins (`Hr { r, n } => r`, `(r, n) => r`) were clean, and so were
/// the `match h { H2 { e, n } => e }` and `return h.e` spellings. Both failing paths
/// left the enum leaf's payload live in the SOURCE: the `let` ladder's callee-owned
/// transfer listed "Vec/String/non-shared-struct fields" and kept an enum field on
/// the source-owns path, so the param's `StructDrop` freed the payload the returned
/// value's owner freed again; the bare-tuple hand-out neutralizer
/// (`zero_bare_tuple_elem_source_for_moved`) recognised struct elements only, so
/// the tuple drop at the merge freed the enum element the result still held. Both
/// now take the enum's own transfer: the `let` leaf registers an `EnumDrop`
/// (`track_enum_var`) and the source field's payload caps are zeroed through
/// `zero_struct_field_move_cap`'s enum arm; the tuple element's source words are
/// zeroed with `zero_enum_payload_caps`, the same cap-zero a moved enum local gets.
///
/// The neighbours pin that nothing else moved: an UNCONSUMED enum leaf now frees
/// itself once (`let_unused`, `tuple_unused` — the leaf owns the memory, the source
/// skips it), a rebound leaf (`let_rebind`) and one handed to a by-value callee
/// (`let_call`, `tuple_call`) compose through the existing move suppressors, the
/// struct-leaf twins are unchanged, and the `if let` / `let (e, n) = t` spellings of
/// the tuple agree. Bodies were never the question here — the interpreter's
/// transcript is what every compiled surface now prints — so the ASAN twin is the
/// load-bearing pin.
///
/// Twin of `tests/interpreter.rs`'s `test_enum_leaf_handed_back_out_of_a_destructure`, pinned to the same string.
#[test]
fn e2e_enum_leaf_handed_back_out_of_a_destructure() {
    let Some(out) = run_program(
        r#"struct R { id: i64, tag: String, xs: Vec[i64] }
impl Drop for R { fn drop(mut ref self) { println(f"  dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, tag: f"t{i}", xs: [i] } }
enum E { A(R), B }
impl Drop for E { fn drop(mut ref self) { println("  dE") } }
struct H2 { e: E, n: i64 }
struct Hr { r: R, n: i64 }
fn consume_e(x: E) -> i64 { match x { E.A(r) => { return r.id; } E.B => { return 0; } } }

fn p_let(h: H2) -> E { let H2 { e, n } = h; return e; }
fn p_let_unused(h: H2) -> i64 { let H2 { e, n } = h; return n; }
fn p_let_rebind(h: H2) -> E { let H2 { e, n } = h; let k = e; return k; }
fn p_let_call(h: H2) -> i64 { let H2 { e, n } = h; return consume_e(e) + n; }
fn p_let_r(h: Hr) -> R { let Hr { r, n } = h; return r; }
fn p_match(h: H2) -> E { match h { H2 { e, n } => { return e; } } }
fn p_tuple(t: (E, i64)) -> E { match t { (e, n) => { return e; } } }
fn p_tuple_unused(t: (E, i64)) -> i64 { match t { (e, n) => { return n; } } }
fn p_tuple_call(t: (E, i64)) -> i64 { match t { (e, n) => { return consume_e(e) + n; } } }
fn p_tuple_r(t: (R, i64)) -> R { match t { (r, n) => { return r; } } }
fn p_tuple_iflet(t: (E, i64)) -> E { if let (e, n) = t { return e; } else { return E.B; } }
fn p_tuple_let(t: (E, i64)) -> E { let (e, n) = t; return e; }

fn main() {
    println("let/local"); let a1 = H2 { e: E.A(mk(1)), n: 10 }; let x1 = p_let(a1); println("  got"); let _ = x1;
    println("let/temp"); let x2 = p_let(H2 { e: E.A(mk(2)), n: 10 }); println("  got"); let _ = x2;
    println("let_unused/local"); let a3 = H2 { e: E.A(mk(3)), n: 10 }; let x3 = p_let_unused(a3); println(f"  got{x3}");
    println("let_rebind/local"); let a4 = H2 { e: E.A(mk(4)), n: 10 }; let x4 = p_let_rebind(a4); println("  got"); let _ = x4;
    println("let_call/local"); let a5 = H2 { e: E.A(mk(5)), n: 10 }; let x5 = p_let_call(a5); println(f"  got{x5}");
    println("let_r/local"); let a6 = Hr { r: mk(6), n: 10 }; let x6 = p_let_r(a6); println(f"  got{x6.id}");
    println("match/local"); let a7 = H2 { e: E.A(mk(7)), n: 10 }; let x7 = p_match(a7); println("  got"); let _ = x7;
    println("tuple/local"); let b1 = (E.A(mk(11)), 10); let y1 = p_tuple(b1); println("  got"); let _ = y1;
    println("tuple/temp"); let y2 = p_tuple((E.A(mk(12)), 10)); println("  got"); let _ = y2;
    println("tuple_unused/local"); let b3 = (E.A(mk(13)), 10); let y3 = p_tuple_unused(b3); println(f"  got{y3}");
    println("tuple_call/local"); let b4 = (E.A(mk(14)), 10); let y4 = p_tuple_call(b4); println(f"  got{y4}");
    println("tuple_r/local"); let b5 = (mk(15), 10); let y5 = p_tuple_r(b5); println(f"  got{y5.id}");
    println("tuple_iflet/local"); let b6 = (E.A(mk(16)), 10); let y6 = p_tuple_iflet(b6); println("  got"); let _ = y6;
    println("tuple_let/local"); let b7 = (E.A(mk(17)), 10); let y7 = p_tuple_let(b7); println("  got"); let _ = y7;
    println("end");
}
"#,
    ) else {
        return;
    };
    assert_eq!(
        out,
        r#"let/local
  got
  dE
  dR1
let/temp
  got
  dE
  dR2
let_unused/local
  dE
  dR3
  got10
let_rebind/local
  got
  dE
  dR4
let_call/local
  dE
  dR5
  got15
let_r/local
  got6
  dR6
match/local
  got
  dE
  dR7
tuple/local
  got
  dE
  dR11
tuple/temp
  got
  dE
  dR12
tuple_unused/local
  dE
  dR13
  got10
tuple_call/local
  dE
  dR14
  got24
tuple_r/local
  got15
  dR15
tuple_iflet/local
  got
  dE
  dR16
tuple_let/local
  got
  dE
  dR17
end
"#
    );
}

/// B-2026-09-06-27 — a READ-ONLY arm on an OWNED ENUM receiver lost the payload's
/// `Drop` body under `--interp`: `impl E { fn m_read(self) -> i64 { match self {
/// E.A(r) => { return r.id; } E.B => { return 0; } } } }` printed `dE x1` for a
/// named local and `x2` for a temp, against `dR1 dE x1` / `dR2 x2` on jit / -O0 /
/// -O2. The read-through gate (B-2026-08-28-67) stands the arm stash down on the
/// premise that "the scrutinee's own walk runs the body after the enum's own" —
/// true of a local scrutinee, and false of an owned enum receiver on this backend:
/// the caller's walk over a named-local receiver runs only the shell's body (its
/// payload is masked at the call, as codegen masks it), a temp receiver has no
/// caller walk, and the frame registers nothing for `self`. So the body ran
/// nowhere. The arm channel owns an enum receiver's payload by design
/// (B-2026-08-01-6, B-2026-09-04-30's registrar), so a bare owned enum `self`
/// scrutinee now keeps its stash on a read-only arm in all three legs (`match`,
/// `if let`, `while let`; `bare_self_is_owned_enum_receiver`), and the body fires
/// at the arm's end — the compiled order for this receiver.
///
/// Controls and neighbours, all byte-identical on the four surfaces: the consuming
/// arm (`r/*`, hand-back) was already right; a guarded pair (`guard`), a
/// non-returning read (`print`), a shell-less enum (`noshell`), and the
/// free-function twin (`free`) agree. The WILDCARD arm (`E.A(_)`, `none/*`) ran
/// the payload body on no surface when this row closed; B-2026-09-06-37's lowering
/// rewrite now binds that position to a never-read name, so `none/*` fire `dR5` /
/// `dR6` at the arm's end like the bound cells. The other agreed gap this row
/// pinned as it stood — a TEMP enum receiver losing the shell's own `dE` on every
/// surface (B-2026-09-04-30's registrar declined enum receiver bodies) — closed as
/// B-2026-09-06-38: the temp cells now carry their `dE` at the statement's end.
///
/// B-2026-09-06-39 — REPINNED. Every cell whose arm only READS through its
/// binding — `read/*`, `none/*`, `print/*`, `iflet/*`, `whilelet`, `guard` —
/// now prints `dE` before `dR`, the design.md § Part 8 order this fixture's own
/// `free/*` and a local scrutinee always printed. The arms bind views and the
/// caller owns the payload's body; `r/*` (a hand-back) and `noshell/*` (no shell
/// body to order against) are unchanged.
///
/// Twin of `tests/interpreter.rs`'s `test_read_only_arm_on_owned_enum_receiver_runs_payload_body`, pinned to the same string.
#[test]
fn e2e_read_only_arm_on_owned_enum_receiver_runs_payload_body() {
    let Some(out) = run_program(
        r#"struct R { id: i64, tag: String, xs: Vec[i64] }
impl Drop for R { fn drop(mut ref self) { println(f"  dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, tag: f"t{i}", xs: [i] } }
enum E { A(R), B }
impl Drop for E { fn drop(mut ref self) { println("  dE") } }
enum P { A(R), B }
impl E {
    fn m_read(self) -> i64 { match self { E.A(r) => { return r.id; } E.B => { return 0; } } }
    #[allow(partial_move_of_drop_enum)]
    fn m_r(self) -> R { match self { E.A(r) => { return r; } E.B => { return mk(0); } } }
    fn m_none(self) -> i64 { match self { E.A(_) => { return 1; } E.B => { return 0; } } }
    fn m_print(self) { match self { E.A(r) => { println(f"  p{r.id}"); } E.B => { } } }
    fn m_iflet(self) -> i64 { if let E.A(r) = self { return r.id; } else { return 0; } }
    fn m_whilelet(self) -> i64 { while let E.A(r) = self { return r.id; } return 0; }
    fn m_guard(self) -> i64 { match self { E.A(r) if r.id > 100 => { return 1; } E.A(r) => { return r.id + 1; } E.B => { return 0; } } }
}
impl P {
    fn m_read(self) -> i64 { match self { P.A(r) => { return r.id; } P.B => { return 0; } } }
}
fn f_read(e: E) -> i64 { match e { E.A(r) => { return r.id; } E.B => { return 0; } } }
fn main() {
    println("read/local"); let a = E.A(mk(1)); let x = a.m_read(); println(f"  x{x}");
    println("read/temp"); let x2 = E.A(mk(2)).m_read(); println(f"  x{x2}");
    println("r/local"); let b = E.A(mk(3)); let y = b.m_r(); println(f"  y{y.id}");
    println("r/temp"); let y2 = E.A(mk(4)).m_r(); println(f"  y{y2.id}");
    println("none/local"); let c = E.A(mk(5)); let z = c.m_none(); println(f"  z{z}");
    println("none/temp"); let z2 = E.A(mk(6)).m_none(); println(f"  z{z2}");
    println("print/local"); let d = E.A(mk(7)); d.m_print();
    println("print/temp"); E.A(mk(8)).m_print();
    println("noshell/local"); let g = P.A(mk(9)); let w = g.m_read(); println(f"  w{w}");
    println("noshell/temp"); let w2 = P.A(mk(10)).m_read(); println(f"  w{w2}");
    println("free/local"); let h = E.A(mk(11)); let v = f_read(h); println(f"  v{v}");
    println("free/temp"); let v2 = f_read(E.A(mk(12))); println(f"  v{v2}");
    println("iflet/local"); let i1 = E.A(mk(21)); let q1 = i1.m_iflet(); println(f"  q{q1}");
    println("iflet/temp"); let q2 = E.A(mk(22)).m_iflet(); println(f"  q{q2}");
    println("whilelet/local"); let i3 = E.A(mk(23)); let q3 = i3.m_whilelet(); println(f"  q{q3}");
    println("guard/local"); let i4 = E.A(mk(24)); let q4 = i4.m_guard(); println(f"  q{q4}");
    println("end");
}
"#,
    ) else {
        return;
    };
    assert_eq!(
        out,
        r#"read/local
  dE
  dR1
  x1
read/temp
  dE
  dR2
  x2
r/local
  dE
  y3
  dR3
r/temp
  dE
  y4
  dR4
none/local
  dE
  dR5
  z1
none/temp
  dE
  dR6
  z1
print/local
  p7
  dE
  dR7
print/temp
  p8
  dE
  dR8
noshell/local
  dR9
  w9
noshell/temp
  dR10
  w10
free/local
  dE
  dR11
  v11
free/temp
  dE
  dR12
  v12
iflet/local
  dE
  dR21
  q21
iflet/temp
  dE
  dR22
  q22
whilelet/local
  dE
  dR23
  q23
guard/local
  dE
  dR24
  q25
end
"#
    );
}

/// B-2026-09-06-29 — a payload handed back out of a TWO-LEVEL owned-param
/// destructure ran its `Drop` body twice on every surface: `fn p_h(h: H1) -> R {
/// match h { H1 { e } => { match e { E.A(r) => { return r; } .. } } } }` printed
/// `dE dR2 r2 dR2` where the one-level `match h.e { E.A(r) => return r }` printed
/// `dE r6 dR6`. The caller masks a handed-back payload out of its retained walk
/// over the argument through the field-payload path channel
/// (`fn_escaping_param_field_payload_paths`, B-2026-09-06-17), whose scanner
/// denoted PROJECTION scrutinees only (`h.e`, `h.s.e`); the inner scrutinee here
/// is the bare leaf `e` that the outer destructure bound, so no path was reported
/// and the walk ran the payload's body under the result's owner. The scanner now
/// carries the destructure-alias table the part-path scanner has had since
/// B-2026-08-28-23 (`alias_destructure` / `set_alias` / `clear_alias`, hoisted to
/// module level and shared): a `match` / `if let` / `while let` / `let` /
/// `let … else` that destructures the param or one of its parts makes each leaf an
/// alias of that part, so `match e` denotes `["e"]`, `match s { S { e } => match e
/// {..} }` denotes `["s", "e"]`, and a rebind `let k = e` follows. Both backends
/// consume the one predicate, so both moved together.
///
/// Cells: `h/local`, `h/temp`, `let/local`, `iflet/local`, `deep/local` (three
/// levels), `two/local` (a sibling field whose bodies must keep running), and the
/// owned-`self` receiver `hand/local`; `r/local` and `proj/local` are the one-level
/// controls. `hand/temp` keeps losing the shell's `dE` — B-2026-09-04-30's
/// receiver-temp registrar declines a method whose return can carry the receiver,
/// the documented conservative direction — and is pinned as it stands.
///
/// Twin of `tests/interpreter.rs`'s `test_two_level_destructure_hand_back_runs_one_payload_body`, pinned to the same string.
#[test]
fn e2e_two_level_destructure_hand_back_runs_one_payload_body() {
    let Some(out) = run_program(
        r#"struct R { id: i64, tag: String, xs: Vec[i64] }
impl Drop for R { fn drop(mut ref self) { println(f"  dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, tag: f"t{i}", xs: [i] } }
enum E { A(R), B }
impl Drop for E { fn drop(mut ref self) { println("  dE") } }
struct H1 { e: E }
struct S { e: E }
struct H2 { s: S }
struct Hb { e: E, b: E }
#[allow(partial_move_of_drop_enum)]
fn p_r(e: E) -> R { match e { E.A(r) => { return r; } E.B => { return mk(0); } } }
#[allow(partial_move_of_drop_enum)]
fn p_h(h: H1) -> R { match h { H1 { e } => { match e { E.A(r) => { return r; } E.B => { return mk(0); } } } } }
#[allow(partial_move_of_drop_enum)]
fn p_let(h: H1) -> R { let H1 { e } = h; match e { E.A(r) => { return r; } E.B => { return mk(0); } } }
#[allow(partial_move_of_drop_enum)]
fn p_iflet(h: H1) -> R { match h { H1 { e } => { if let E.A(r) = e { return r; } else { return mk(0); } } } }
#[allow(partial_move_of_drop_enum)]
fn p_proj(h: H1) -> R { match h.e { E.A(r) => { return r; } E.B => { return mk(0); } } }
#[allow(partial_move_of_drop_enum)]
fn p_deep(h: H2) -> R { match h { H2 { s } => { match s { S { e } => { match e { E.A(r) => { return r; } E.B => { return mk(0); } } } } } } }
#[allow(partial_move_of_drop_enum)]
fn p_two(h: Hb) -> R { match h { Hb { e, b } => { match e { E.A(r) => { return r; } E.B => { return mk(0); } } } } }
impl H1 { #[allow(partial_move_of_drop_enum)] fn hand(self) -> R { match self { H1 { e } => { match e { E.A(r) => { return r; } E.B => { return mk(0); } } } } } }
fn main() {
    println("r/local"); let a1 = E.A(mk(1)); let x1 = p_r(a1); println(f"  r{x1.id}");
    println("h/local"); let a2 = H1 { e: E.A(mk(2)) }; let x2 = p_h(a2); println(f"  r{x2.id}");
    println("h/temp"); let x3 = p_h(H1 { e: E.A(mk(3)) }); println(f"  r{x3.id}");
    println("let/local"); let a4 = H1 { e: E.A(mk(4)) }; let x4 = p_let(a4); println(f"  r{x4.id}");
    println("iflet/local"); let a5 = H1 { e: E.A(mk(5)) }; let x5 = p_iflet(a5); println(f"  r{x5.id}");
    println("proj/local"); let a6 = H1 { e: E.A(mk(6)) }; let x6 = p_proj(a6); println(f"  r{x6.id}");
    println("deep/local"); let a7 = H2 { s: S { e: E.A(mk(7)) } }; let x7 = p_deep(a7); println(f"  r{x7.id}");
    println("two/local"); let a8 = Hb { e: E.A(mk(8)), b: E.A(mk(108)) }; let x8 = p_two(a8); println(f"  r{x8.id}");
    println("hand/local"); let a9 = H1 { e: E.A(mk(9)) }; let x9 = a9.hand(); println(f"  r{x9.id}");
    println("hand/temp"); let x10 = H1 { e: E.A(mk(10)) }.hand(); println(f"  r{x10.id}");
    println("end");
}
"#,
    ) else {
        return;
    };
    assert_eq!(
        out,
        r#"r/local
  dE
  r1
  dR1
h/local
  dE
  r2
  dR2
h/temp
  dE
  r3
  dR3
let/local
  dE
  r4
  dR4
iflet/local
  dE
  r5
  dR5
proj/local
  dE
  r6
  dR6
deep/local
  dE
  r7
  dR7
two/local
  dE
  dR108
  dE
  r8
  dR8
hand/local
  dE
  r9
  dR9
hand/temp
  r10
  dR10
end
"#
    );
}

/// B-2026-09-16-12 — a MIXED bind-and-wildcard arm lost the WILDCARDED payload
/// field's `Drop` body: `let w = W2.Two(mk(1), mk(2)); match w { W2.Two(a, _) =>
/// { return a.id } .. }` ran `dR1` alone on `--interp` while the compiled
/// backends ran `dR1 dR2`, and once the arm MOVED the bound field out
/// (`let m = a`, or `take(a)`) EVERY surface lost `dR2` — the agreed-silence
/// profile no A/B gate can report.
///
/// Both backends keyed the payload-BODIES disarm on the BINDING rather than on
/// the consumed POSITION: codegen asked the boolean
/// `enum_pattern_consumes_user_drop_payload` and then retracted the husk's
/// whole `ContainerElemBodies` action, and the interpreter asked the same
/// boolean through `match_disarms_payload_walk` and inserted the scrutinee NAME
/// into `moved_out_enum_payload_bindings`. The MEMORY half beside each was
/// already per-position — its own doc says a wildcard sub-pattern "doesn't
/// claim ownership, so the source's drop must still fire" — so this is that
/// sentence applied to the bodies channel. Memory was never affected: 52 allocs
/// / 52 frees, valgrind-clean at `KARAC_OPT_LEVEL=0`, before and after.
///
/// Both sides now mask the positions the arms TAKE and fall back to the
/// whole-binding disarm when that union covers every Drop-bearing position, so
/// a fully-consuming arm — every case any pre-existing fixture covers — is
/// byte-for-byte unchanged. The mask carries the VARIANT as well as the index,
/// because position 0 of `Two` is not position 0 of `Three`; `other_variant_live`
/// is the cell that pins it.
///
/// CELLS. `first_bound` / `second_bound` (bind one, wildcard the other, on each
/// side), `moved_out` and `rebound` (the arm hands the bound field on — the
/// every-surface half), `iflet` (the `if let` spelling, which had the same hole
/// in the interpreter and was fixed in lockstep to avoid the
/// spelling-dependent split this family has closed four times:
/// B-2026-08-28-63, B-2026-08-29-17, B-2026-08-31-32, B-2026-09-01-28),
/// `other_variant_live` (arm A takes a position, variant B is live — the mask
/// must not blank B's), `each_variant_takes` (both arms take, different
/// variants). Controls: `both_wild` (nothing consumed, always correct) and
/// `both_bound` (fully consuming, the totality fallback).
///
/// THE TWO BACKENDS STILL DIFFER ON TWO CELLS, and deliberately so: in
/// `second_bound` and `both_bound` the interpreter prints the BOUND position's
/// body before the husk's, the compiled backends print both from the husk in
/// field order. That is B-2026-09-06-21 — where and in what order the CONSUMED
/// positions fire — which this row does not touch. What this row fixes is the
/// SET, and the set is now identical on `--interp`, jit, `KARAC_AUTO_PAR=0` and
/// the default auto-par build (the three compiled surfaces are byte-identical
/// to each other). When -06-21 lands, one of these two pinned strings changes
/// and the other does not.
///
/// Twin of `tests/interpreter.rs`'s
/// `test_mixed_bind_and_wildcard_arm_keeps_the_unbound_payload_body`.
#[test]
fn e2e_mixed_bind_and_wildcard_arm_keeps_the_unbound_payload_body() {
    let Some(out) = run_program(
        r#"struct R { id: i64, tag: String }
impl Drop for R { fn drop(mut ref self) { println(f"  dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, tag: f"t{i}" } }
enum W2 { Two(R, R), None2 }
enum W3 { A(R, R), B(R), None3 }
fn take(r: R) -> i64 { return r.id; }
fn first_bound() -> i64 { let w: W2 = W2.Two(mk(1), mk(2)); match w { W2.Two(a, _) => { return a.id; } W2.None2 => { return 0; } } }
fn second_bound() -> i64 { let w: W2 = W2.Two(mk(3), mk(4)); match w { W2.Two(_, b) => { return b.id; } W2.None2 => { return 0; } } }
fn moved_out() -> i64 { let w: W2 = W2.Two(mk(5), mk(6)); match w { W2.Two(a, _) => { return take(a); } W2.None2 => { return 0; } } }
fn rebound() -> i64 { let w: W2 = W2.Two(mk(7), mk(8)); match w { W2.Two(a, _) => { let m: R = a; return m.id; } W2.None2 => { return 0; } } }
fn iflet() -> i64 { let w: W2 = W2.Two(mk(9), mk(10)); if let W2.Two(a, _) = w { return a.id; } return 0; }
fn both_wild() -> i64 { let w: W2 = W2.Two(mk(11), mk(12)); match w { W2.Two(_, _) => { return 1; } W2.None2 => { return 0; } } }
fn both_bound() -> i64 { let w: W2 = W2.Two(mk(13), mk(14)); match w { W2.Two(a, b) => { return a.id + b.id; } W2.None2 => { return 0; } } }
fn other_variant_live() -> i64 { let w: W3 = W3.B(mk(22)); match w { W3.A(a, _) => { return a.id; } W3.B(_) => { return 99; } W3.None3 => { return 0; } } }
fn each_variant_takes() -> i64 { let w: W3 = W3.A(mk(30), mk(31)); match w { W3.A(a, _) => { return a.id; } W3.B(c) => { return c.id; } W3.None3 => { return 0; } } }
fn main() {
    println("first_bound"); let a: i64 = first_bound(); println(f"  ={a}");
    println("second_bound"); let b: i64 = second_bound(); println(f"  ={b}");
    println("moved_out"); let c: i64 = moved_out(); println(f"  ={c}");
    println("rebound"); let d: i64 = rebound(); println(f"  ={d}");
    println("iflet"); let e: i64 = iflet(); println(f"  ={e}");
    println("both_wild"); let f: i64 = both_wild(); println(f"  ={f}");
    println("both_bound"); let g: i64 = both_bound(); println(f"  ={g}");
    println("other_variant_live"); let h: i64 = other_variant_live(); println(f"  ={h}");
    println("each_variant_takes"); let i: i64 = each_variant_takes(); println(f"  ={i}");
    println("end");
}
"#,
    ) else {
        return;
    };
    assert_eq!(
        out,
        r#"first_bound
  dR2
  dR1
  =1
second_bound
  dR4
  dR3
  =4
moved_out
  dR5
  dR6
  =5
rebound
  dR7
  dR8
  =7
iflet
  dR10
  dR9
  =9
both_wild
  dR12
  dR11
  =1
both_bound
  dR14
  dR13
  =27
other_variant_live
  dR22
  =99
each_variant_takes
  dR31
  dR30
  =30
end
"#
    );
}

/// B-2026-09-06-35 — a partial struct `match` pattern destroyed the `..` REST
/// fields BEFORE the bound leaf on the interpreter and AFTER it on every
/// compiled backend: `let s = S3 { a: mk(1), b: mk(2) }; match s { S3 { a, .. }
/// => .. }` printed `dR2 dR1` under `--interp` and `dR1 dR2` under jit / aot /
/// `KARAC_AUTO_PAR=0`. Count-correct on both, so only the sequence diverged.
///
/// THE ROW ASKED WHICH BACKEND IS WRONG AND design.md ANSWERS IT.
/// § "Interaction with move semantics": "Moving a value out of a binding ends
/// that binding's live range — the destination takes over responsibility for
/// running `Drop` when *its* live range ends". A whole-field binding in the
/// pattern moves that field into the ARM's binding, so the arm's binding is its
/// final owner and it dies at the arm's end; § "Field drop order is reverse
/// declaration order" then governs only the fields the scrutinee still owns.
/// The compiled backends already do exactly that, so the interpreter moves.
///
/// `bind_middle` is what makes it decisive, and it is not in the row: for
/// `S4 { b, .. }` the interpreter printed `c b a` — reverse declaration order
/// over ALL THREE fields, putting the moved-out `b` back inside the struct's
/// own sweep, which is precisely what the move rule forbids. The compiled
/// `b c a` is the bound leaf at the arm's end followed by the husk in reverse
/// declaration order. The row's two-field cells cannot tell those models apart,
/// which is why its own explanation of the `{ b, .. }` mirror ("both sequences
/// happen to read `b` then `a`") does not survive a third field.
///
/// The interpreter now stashes each WHOLE-field binding as an arm-scoped Drop
/// slot and masks that field out of the scrutinee's own walk in the same step —
/// through `moved_out_struct_field_bodies` for a flat field and the path-keyed
/// `moved_out_nested_field_bodies` for a leaf inside a nested sub-pattern,
/// where the outer field is NOT wholly moved and must keep every other body it
/// owes. Stash and mask are written together, per field, because the two must
/// agree or the body fires twice or not at all.
///
/// CELLS. `bind_first` (the row's own shape), `bind_last` (its mirror, which
/// agreed before and still does), `bind_middle` (the three-field discriminator),
/// `renamed` (`a: x`, where field and binding names differ), `nested_leaf`
/// (the path-masked case), `iflet_named` (the `if let` spelling, which had the
/// same divergence and moves in the same commit — this family has closed a
/// spelling-dependent split four times), `scalar_beside` (a non-Drop field in
/// the rest), `moved_on` (the arm hands the leaf to a call). Controls that must
/// not move: `bind_all` (nothing left in the rest), `bind_none` (nothing
/// bound), `wild_field` (`a: _` takes nothing).
///
/// All four surfaces now print this string byte-identically, so the twinned
/// pair is pinned to ONE expected output. valgrind at `KARAC_OPT_LEVEL=0`:
/// 68 allocs / 68 frees, ERROR SUMMARY 0.
///
/// Twin of `tests/interpreter.rs`'s
/// `test_partial_struct_match_pattern_drops_the_bound_leaf_first`.
#[test]
fn e2e_partial_struct_match_pattern_drops_the_bound_leaf_first() {
    let Some(out) = run_program(
        r#"struct R { id: i64, name: String }
impl Drop for R { fn drop(mut ref self) { println(f"  dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, name: f"n{i}" }; }
struct S3 { a: R, b: R }
struct S4 { a: R, b: R, c: R }
struct Mix { a: R, k: i64, c: R }
struct Inner { p: R }
struct Outer { i: Inner, z: R }
fn eat(r: R) -> i64 { return r.id; }

fn bind_first() -> i64 { let s: S3 = S3 { a: mk(1), b: mk(2) }; match s { S3 { a, .. } => { return a.id; } } }
fn bind_last() -> i64 { let s: S3 = S3 { a: mk(3), b: mk(4) }; match s { S3 { b, .. } => { return b.id; } } }
fn bind_middle() -> i64 { let s: S4 = S4 { a: mk(5), b: mk(6), c: mk(7) }; match s { S4 { b, .. } => { return b.id; } } }
fn bind_all() -> i64 { let s: S3 = S3 { a: mk(8), b: mk(9) }; match s { S3 { a, b } => { return a.id + b.id; } } }
fn bind_none() -> i64 { let s: S3 = S3 { a: mk(10), b: mk(11) }; match s { S3 { .. } => { return 1; } } }
fn renamed() -> i64 { let s: S3 = S3 { a: mk(12), b: mk(13) }; match s { S3 { a: x, .. } => { return x.id; } } }
fn wild_field() -> i64 { let s: S3 = S3 { a: mk(14), b: mk(15) }; match s { S3 { a: _, .. } => { return 1; } } }
fn nested_leaf() -> i64 { let o: Outer = Outer { i: Inner { p: mk(16) }, z: mk(17) }; match o { Outer { i: Inner { p }, .. } => { return p.id; } } }
fn scalar_beside() -> i64 { let s: Mix = Mix { a: mk(18), k: 5, c: mk(19) }; match s { Mix { a, .. } => { return a.id; } } }
fn iflet_named() -> i64 { let s: S3 = S3 { a: mk(20), b: mk(21) }; if let S3 { a, .. } = s { return a.id; } return 0; }
fn moved_on() -> i64 { let s: S4 = S4 { a: mk(22), b: mk(23), c: mk(24) }; match s { S4 { b, .. } => { return eat(b); } } }

fn main() {
    println("bind_first"); let a: i64 = bind_first(); println(f"  v={a}");
    println("bind_last"); let b: i64 = bind_last(); println(f"  v={b}");
    println("bind_middle"); let c: i64 = bind_middle(); println(f"  v={c}");
    println("bind_all"); let d: i64 = bind_all(); println(f"  v={d}");
    println("bind_none"); let e: i64 = bind_none(); println(f"  v={e}");
    println("renamed"); let f: i64 = renamed(); println(f"  v={f}");
    println("wild_field"); let g: i64 = wild_field(); println(f"  v={g}");
    println("nested_leaf"); let h: i64 = nested_leaf(); println(f"  v={h}");
    println("scalar_beside"); let i: i64 = scalar_beside(); println(f"  v={i}");
    println("iflet_named"); let j: i64 = iflet_named(); println(f"  v={j}");
    println("moved_on"); let k: i64 = moved_on(); println(f"  v={k}");
    println("end");
}
"#,
    ) else {
        return;
    };
    assert_eq!(
        out,
        r#"bind_first
  dR1
  dR2
  v=1
bind_last
  dR4
  dR3
  v=4
bind_middle
  dR6
  dR7
  dR5
  v=6
bind_all
  dR9
  dR8
  v=17
bind_none
  dR11
  dR10
  v=1
renamed
  dR12
  dR13
  v=12
wild_field
  dR15
  dR14
  v=1
nested_leaf
  dR16
  dR17
  v=16
scalar_beside
  dR18
  dR19
  v=18
iflet_named
  dR20
  dR21
  v=20
moved_on
  dR23
  dR24
  dR22
  v=23
end
"#
    );
}

/// B-2026-09-06-40 — a REORDERED struct `let` pattern drained its leaves in
/// reverse PATTERN order on the interpreter and reverse DECLARATION order on
/// every compiled backend: `let s = S3 { a: mk(3), b: mk(4) };
/// let S3 { b, a } = s;` printed `dR3 dR4` under `--interp` and `dR4 dR3` under
/// jit / aot / `KARAC_AUTO_PAR=0`. Every body ran once; only the sequence
/// diverged, and only a pattern that reorders fields shows it.
///
/// THE ROW ASKED WHICH READING IS RIGHT AND A CONTROL ANSWERS IT, which is why
/// `desugared_control` is a cell here rather than a remark. `let b = s.b;
/// let a = s.a;` is unambiguously two `let` bindings, and BOTH backends drain
/// it `a` then `b` — LIFO of binding order. The destructure is sugar for
/// exactly that, so the interpreter's reverse-pattern order is the one that
/// generalizes and the compiled side is what moved. design.md agrees twice
/// over: § "Interaction with move semantics" makes each moved-out leaf's own
/// binding its final owner rather than the struct's field, so § "Field drop
/// order is reverse declaration order" no longer governs it; and the
/// destructor rule drains "ordered by program-order of introduction", which
/// for a single `let` is the order the pattern writes its bindings.
///
/// The alternative reading — that a destructure is the struct's own field-drop
/// pass — is what the compiled side implemented. It explains the in-order
/// pattern (where the two orders coincide, so `in_order` and `in_order_three`
/// pin it unchanged) but not the desugared control, which it would have to
/// drain by declaration too. It does not.
///
/// The fix is a visit-order change in the struct-destructure loop of
/// `src/codegen/stmts.rs`: it still walks the DECLARED slot for extraction,
/// dispatch and the discard branch — `idx` is unchanged — but visits the
/// fields in the order the pattern binds them, so the cleanups it registers
/// land in that order and the frame's LIFO drain reverses it. Fields the
/// pattern does not BIND sort after the bound ones and keep declaration order
/// among themselves.
///
/// CELLS. `swapped` (the row's shape), `three_rotated` (`{c, a, b}`, which
/// distinguishes the orders more sharply than any two-field cell can),
/// `renamed_swap` (`{b: y, a: x}`, where binding and field names differ),
/// `desugared_control` (the cell that settles the reading), `rest_swapped`
/// (`..` alongside a reorder), `wild_mixed` (`b: _` between two bound fields),
/// `scalar_between` (a non-Drop field in the middle), `nested_swapped` (a
/// nested sub-pattern, which binds no leaf itself and must not move),
/// `moved_leaf` (a leaf handed to a call). Controls that must not move:
/// `in_order` and `in_order_three`.
///
/// All four surfaces print this string byte-identically; valgrind at
/// `KARAC_OPT_LEVEL=0` is 72 allocs / 72 frees, ERROR SUMMARY 0.
///
/// Twin of `tests/interpreter.rs`'s
/// `test_reordered_struct_let_pattern_drops_in_pattern_order`.
#[test]
fn e2e_reordered_struct_let_pattern_drops_in_pattern_order() {
    let Some(out) = run_program(
        r#"struct R { id: i64, name: String }
impl Drop for R { fn drop(mut ref self) { println(f"  dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, name: f"n{i}" }; }
struct S3 { a: R, b: R }
struct S4 { a: R, b: R, c: R }
struct Mix { a: R, k: i64, c: R }
struct Inner { p: R }
struct Outer { i: Inner, z: R }
fn eat(r: R) -> i64 { return r.id; }

fn in_order() -> i64 { let s: S3 = S3 { a: mk(1), b: mk(2) }; let S3 { a, b } = s; return a.id + b.id; }
fn swapped() -> i64 { let s: S3 = S3 { a: mk(3), b: mk(4) }; let S3 { b, a } = s; return a.id + b.id; }
fn three_rotated() -> i64 { let s: S4 = S4 { a: mk(5), b: mk(6), c: mk(7) }; let S4 { c, a, b } = s; return a.id + b.id + c.id; }
fn renamed_swap() -> i64 { let s: S3 = S3 { a: mk(8), b: mk(9) }; let S3 { b: y, a: x } = s; return x.id + y.id; }
fn desugared_control() -> i64 { let s: S3 = S3 { a: mk(10), b: mk(11) }; let b: R = s.b; let a: R = s.a; return a.id + b.id; }
fn rest_swapped() -> i64 { let s: S4 = S4 { a: mk(12), b: mk(13), c: mk(14) }; let S4 { c, a, .. } = s; return a.id + c.id; }
fn wild_mixed() -> i64 { let s: S4 = S4 { a: mk(15), b: mk(16), c: mk(17) }; let S4 { c, b: _, a } = s; return a.id + c.id; }
fn scalar_between() -> i64 { let s: Mix = Mix { a: mk(18), k: 9, c: mk(19) }; let Mix { c, a, k } = s; return a.id + c.id + k; }
fn nested_swapped() -> i64 { let o: Outer = Outer { i: Inner { p: mk(20) }, z: mk(21) }; let Outer { z, i } = o; return z.id + i.p.id; }
fn moved_leaf() -> i64 { let s: S3 = S3 { a: mk(22), b: mk(23) }; let S3 { b, a } = s; return eat(b) + a.id; }
fn in_order_three() -> i64 { let s: S4 = S4 { a: mk(24), b: mk(25), c: mk(26) }; let S4 { a, b, c } = s; return a.id + b.id + c.id; }

fn main() {
    println("in_order"); let v1: i64 = in_order(); println(f"  v={v1}");
    println("swapped"); let v2: i64 = swapped(); println(f"  v={v2}");
    println("three_rotated"); let v3: i64 = three_rotated(); println(f"  v={v3}");
    println("renamed_swap"); let v4: i64 = renamed_swap(); println(f"  v={v4}");
    println("desugared_control"); let v5: i64 = desugared_control(); println(f"  v={v5}");
    println("rest_swapped"); let v6: i64 = rest_swapped(); println(f"  v={v6}");
    println("wild_mixed"); let v7: i64 = wild_mixed(); println(f"  v={v7}");
    println("scalar_between"); let v8: i64 = scalar_between(); println(f"  v={v8}");
    println("nested_swapped"); let v9: i64 = nested_swapped(); println(f"  v={v9}");
    println("moved_leaf"); let v10: i64 = moved_leaf(); println(f"  v={v10}");
    println("in_order_three"); let v11: i64 = in_order_three(); println(f"  v={v11}");
    println("end");
}
"#,
    ) else {
        return;
    };
    assert_eq!(
        out,
        r#"in_order
  dR2
  dR1
  v=3
swapped
  dR3
  dR4
  v=7
three_rotated
  dR6
  dR5
  dR7
  v=18
renamed_swap
  dR8
  dR9
  v=17
desugared_control
  dR10
  dR11
  v=21
rest_swapped
  dR13
  dR12
  dR14
  v=26
wild_mixed
  dR16
  dR15
  dR17
  v=32
scalar_between
  dR18
  dR19
  v=46
nested_swapped
  dR20
  dR21
  v=41
moved_leaf
  dR22
  dR23
  v=45
in_order_three
  dR26
  dR25
  dR24
  v=75
end
"#
    );
}

/// B-2026-09-06-37 — a WILDCARD arm over an owned ENUM receiver ran the payload's
/// `Drop` body on no surface: `impl E { fn m_wild(self) -> i64 { match self {
/// E.A(_) => { return 1; } E.B => { return 0; } } } }` printed `dE x1` for a named
/// local and `x1` for a temp on --interp / jit / -O0 / -O2 alike, and the half-bound
/// `T.A(_, r) => r.id` ran only the bound half's. An owned enum receiver's payload
/// bodies belong to the match-ARM channel on both backends (B-2026-08-01-6,
/// B-2026-09-04-30), which fires the body of each payload the arm BINDS; a wildcard
/// binds nothing, and no other owner exists. The shared lowering pass
/// (`Lowerer::bind_receiver_wildcards`) now rewrites every wildcard payload
/// position whose declared type can carry a user `Drop` body — under a `match` /
/// `if let` / `while let` over a bare owned enum `self` — into a fresh, never-read
/// binding, recording its surface type for codegen's payload reconstitution. That
/// binding is exactly the read-only payload binding both backends already run once
/// at the arm's end (B-2026-09-06-27), with the same memory hand-off: the position
/// becomes a consumed one, the source's payload words are zeroed, the binding
/// frees them.
///
/// Cells: `wild` (local / temp), `ifwild` (local / temp), `noshell` (an enum with no
/// own `Drop`), `half` (one wildcard beside a bound payload), `both` (two
/// wildcards), against the controls `bound` (arm binds the payload), `free` (the
/// by-value param twin, whose caller walk was always right) and `localmatch` (a
/// local scrutinee, untouched by the rewrite). The temp cells lost the shell's
/// `dE` when this landed; B-2026-09-06-38 gave a temp enum receiver its shell body
/// at the statement's end, so `wild/temp` / `ifwild/temp` now carry it. The arm
/// channel still fires the payload before the shell (B-2026-09-06-39), pinned as
/// it stands.
///
/// B-2026-09-06-39 — REPINNED. `wild/*`, `bound/local` and `ifwild/*` print the
/// shell's body first now; `half/local` and `both/local` likewise put `dT` ahead
/// of the two payload bodies, which then fall in reverse position order. The
/// `free/*` and `localmatch` controls did not move, which is the point: those
/// were already in the design order the receiver spelling has now joined.
///
/// Twin of `tests/interpreter.rs`'s `test_wildcard_arm_over_owned_enum_receiver_runs_payload_body`, pinned to the same string.
#[test]
fn e2e_wildcard_arm_over_owned_enum_receiver_runs_payload_body() {
    let Some(out) = run_program(
        r#"struct R { id: i64, tag: String, xs: Vec[i64] }
impl Drop for R { fn drop(mut ref self) { println(f"  dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, tag: f"t{i}", xs: [i] } }
enum E { A(R), B }
impl Drop for E { fn drop(mut ref self) { println("  dE") } }
enum P { A(R), B }
enum T { A(R, R), B }
impl Drop for T { fn drop(mut ref self) { println("  dT") } }
impl E {
    fn m_wild(self) -> i64 { match self { E.A(_) => { return 1; } E.B => { return 0; } } }
    fn m_unit(self) -> i64 { match self { E.A(r) => { return r.id; } E.B => { return 0; } } }
    fn m_ifwild(self) -> i64 { if let E.A(_) = self { return 1; } else { return 0; } }
}
impl P { fn m_wild(self) -> i64 { match self { P.A(_) => { return 1; } P.B => { return 0; } } } }
impl T {
    fn m_half(self) -> i64 { match self { T.A(_, r) => { return r.id; } T.B => { return 0; } } }
    fn m_both(self) -> i64 { match self { T.A(_, _) => { return 2; } T.B => { return 0; } } }
}
fn f_wild(e: E) -> i64 { match e { E.A(_) => { return 1; } E.B => { return 0; } } }
fn main() {
    println("wild/local"); let a = E.A(mk(1)); let x = a.m_wild(); println(f"  x{x}");
    println("wild/temp"); let x2 = E.A(mk(2)).m_wild(); println(f"  x{x2}");
    println("bound/local"); let b = E.A(mk(3)); let y = b.m_unit(); println(f"  y{y}");
    println("ifwild/local"); let c = E.A(mk(4)); let z = c.m_ifwild(); println(f"  z{z}");
    println("ifwild/temp"); let z2 = E.A(mk(5)).m_ifwild(); println(f"  z{z2}");
    println("noshell/local"); let d = P.A(mk(6)); let w = d.m_wild(); println(f"  w{w}");
    println("noshell/temp"); let w2 = P.A(mk(7)).m_wild(); println(f"  w{w2}");
    println("half/local"); let g = T.A(mk(8), mk(108)); let v = g.m_half(); println(f"  v{v}");
    println("both/local"); let h = T.A(mk(9), mk(109)); let v2 = h.m_both(); println(f"  v{v2}");
    println("free/local"); let i = E.A(mk(10)); let u = f_wild(i); println(f"  u{u}");
    println("free/temp"); let u2 = f_wild(E.A(mk(11))); println(f"  u{u2}");
    println("localmatch"); let j = E.A(mk(12)); match j { E.A(_) => { println("  arm"); } E.B => { } }
    println("end");
}
"#,
    ) else {
        return;
    };
    assert_eq!(
        out,
        r#"wild/local
  dE
  dR1
  x1
wild/temp
  dE
  dR2
  x1
bound/local
  dE
  dR3
  y3
ifwild/local
  dE
  dR4
  z1
ifwild/temp
  dE
  dR5
  z1
noshell/local
  dR6
  w1
noshell/temp
  dR7
  w1
half/local
  dT
  dR108
  dR8
  v108
both/local
  dT
  dR109
  dR9
  v2
free/local
  dE
  dR10
  u1
free/temp
  dE
  dR11
  u1
localmatch
  arm
  dE
  dR12
end
"#
    );
}

#[test]
fn e2e_wildcard_let_discard_owns_what_its_arm_hands_out() {
    let hdr = "struct R { id: i64, name: String }\n\
                   impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\") } }\n\
                   enum OptR { Has(R), Non }\n\
                   fn mk(i: i64) -> R { return R { id: i, name: f\"heap-{i}\" }; }\n";
    for (label, body, want) in [
            // ── the tail names an ENCLOSING LOCAL ─────────────────────────
            (
                "if-hands-out-a-local",
                "let r = mk(41);\nlet _ = if n == 0 { r } else { mk(9) };",
                "dR41\nend\n",
            ),
            (
                "match-hands-out-a-local",
                "let r = mk(41);\nlet _ = match n { 0 => r, _ => mk(9) };",
                "dR41\nend\n",
            ),
            (
                "block-hands-out-a-local",
                "let r = mk(41);\nlet _ = { r };",
                "dR41\nend\n",
            ),
            // ── the tail names the arm's own PATTERN BINDING ──────────────
            (
                "payload-braced-arm",
                "let o: Option[R] = Some(mk(1));\n\
                 let _ = match o { Some(r) => { r } None => { mk(9) } };",
                "dR1\nend\n",
            ),
            // ── double-own guards: ONE body, not two ──────────────────────
            (
                "guard: block-wrapped match is owned by the statement site",
                "let _ = { match n { 0 => { mk(7) } _ => { mk(3) } } };",
                "dR7\nend\n",
            ),
            (
                "guard: block-wrapped call is owned by the statement site",
                "let _ = { mk(7) };",
                "dR7\nend\n",
            ),
            // ── controls, unchanged by this row ───────────────────────────
            (
                "control: bare-statement form (B-2026-08-29-5)",
                "let r = mk(41);\nif n == 0 { r } else { mk(9) };",
                "dR41\nend\n",
            ),
            (
                "control: all-mint if stays statement-owned",
                "let _ = if n == 0 { mk(2) } else { mk(3) };",
                "dR2\nend\n",
            ),
            (
                "control: all-mint match stays statement-owned",
                "let _ = match n { 0 => mk(2), _ => mk(3) };",
                "dR2\nend\n",
            ),
            (
                "control: else-if chain",
                "let _ = if n == 1 { mk(20) } else if n == 0 { mk(21) } else { mk(22) };",
                "dR21\nend\n",
            ),
            (
                // A MIXED branch: one arm names a live local, the other
                // mints. Both bodies are due and neither may double. Before
                // this row BOTH backends printed only `dR18` — the minted
                // value had no owner at all (B-2026-08-30-11's population).
                // `dR4` lands at the discard, `dR18` at the local's own NLL
                // end, which is that same statement.
                "control: mixed branch — minted arm and live local both fire once",
                "let r = mk(18);\nlet _ = if n == 9 { r } else { mk(4) };\nprintln(\"still\");",
                "dR4\ndR18\nstill\nend\n",
            ),
            // B-2026-09-01-10 — the `match` spellings of the two rows above.
            // `compile_match` never told a BLOCK-bodied arm that the
            // construct's value is discarded, so the braced spellings here
            // stranded whatever the taken arm named while the bare ones were
            // already correct; and the minted arm of a match the statement
            // site DECLINES (one arm yields a place) had no owner in the bare
            // spelling at all — the compiled backends were silent on `dR9`
            // where the interpreter fired it.
            (
                "match braced arm — mixed branch, both bodies fire once",
                "let r = mk(18);\nlet _ = match n { 9 => { r } _ => { mk(4) } };\nprintln(\"still\");",
                "dR4\ndR18\nstill\nend\n",
            ),
            (
                "match braced arm — declined match, minted arm taken",
                "let o: Option[R] = None;\nlet _ = match o { Some(r) => { r } None => { mk(9) } };",
                "dR9\nend\n",
            ),
            (
                "match bare arm — declined match, minted arm taken",
                "let o: Option[R] = None;\nlet _ = match o { Some(r) => r, None => mk(9) };",
                "dR9\nend\n",
            ),
            (
                "match braced arm — bare statement, payload binding handed out",
                "let o: Option[R] = Some(mk(1));\nmatch o { Some(r) => { r } None => { mk(9) } };",
                "dR1\nend\n",
            ),
            // B-2026-08-31-28 — the BARE-arm spellings of the two rows above,
            // which were pinned DIVERGENT at the bottom of this fixture until
            // that row closed. The bodies retraction two lines below the
            // memory one in `compile_match` never took the
            // `branch_value_is_owned` guard the memory half took in
            // B-2026-08-28-66, so a discarded match handed the payload's body
            // to a destination that does not exist and it ran nowhere.
            (
                "match bare arm — payload binding handed out [B-2026-08-31-28]",
                "let o: Option[R] = Some(mk(1));\nlet _ = match o { Some(r) => r, None => mk(9) };",
                "dR1\nend\n",
            ),
            (
                "match bare arm — bare statement, payload binding handed out [B-2026-08-31-28]",
                "let o: Option[R] = Some(mk(1));\nmatch o { Some(r) => r, None => mk(9) };",
                "dR1\nend\n",
            ),
            // The three controls that localized it to the intersection rather
            // than to any one axis: braces, a USER enum, and a payload that
            // carries no heap were each correct throughout.
            (
                "control: bare arm over a USER enum [B-2026-08-31-28]",
                "let e: OptR = OptR.Has(mk(1));\nlet _ = match e { OptR.Has(r) => r, OptR.Non => mk(9) };",
                "dR1\nend\n",
            ),
            (
                "control: bare arm, value CONSUMED rather than discarded",
                "let o: Option[R] = Some(mk(1));\nlet x = match o { Some(r) => r, None => mk(9) };\nprintln(f\"{x.id}\");",
                "1\ndR1\nend\n",
            ),
            (
                "control: bare arm binds the payload but yields something else",
                "let o: Option[R] = Some(mk(1));\nlet _ = match o { Some(r) => mk(5), None => mk(9) };",
                "dR1\ndR5\nend\n",
            ),
        ] {
            let src = format!("{hdr}fn main() {{\nlet n = 0;\n{body}\nprintln(\"end\");\n}}\n");
            assert_eq!(run_program(&src).as_deref(), Some(want), "[{label}]");
        }
    // The BARE-arm divergence this fixture used to pin here is CLOSED
    // (B-2026-08-31-28) and its shapes are asserted in the table above,
    // alongside the three controls that localized it. Nothing is pinned
    // divergent in this fixture any more.
}

/// B-2026-09-07-59, MATCH-ARM spelling — filed in the same row and
/// diverging identically (`2` interpreted, `0` compiled). Kept as its own
/// test because its outer is an ARM-LOCAL binding, a different resolution
/// path for the ownership half even though the value half is the one niche
/// unpack.
#[test]
fn test_e2e_clone_of_niche_option_field_via_match_arm_binding() {
    let src = r#"
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
    let root: Option[Node] = Some(Node { val: 5, left: Some(Node { val: 6, left: None, right: None }), right: None });
    let s = match root { None => None, Some(n) => n.left.clone() };
    let l0 = clone_offset(s, 10);
    let l1 = clone_offset(s, 20);
    println(f"{count_nodes(l0) + count_nodes(l1)}");
}
"#;
    assert_eq!(run_program(src).as_deref(), Some("2\n"));
}

/// B-2026-08-31-15 — the VALUE-receiver spelling of the six
/// operator-named comparisons had no codegen arm at all, so every one of
/// them was check-green, correct under `--interp`, and a hard `karac build`
/// failure: "no handler for method 'lt' on variable 'a'". 36 cells — six
/// methods across every receiver class that accepts them.
///
/// The TYPE half of this row (the `Type::Error` poison that let `let s:
/// String = n.cmp(m)` through) is asserted in tests/typechecker.rs by
/// `comparison_methods_on_a_primitive_receiver_are_typed_not_poisoned`;
/// this is the run-vs-build half, and the interpreter is the oracle.
#[test]
fn e2e_value_receiver_comparison_methods_match_the_interpreter() {
    let decls = [
        ("i64", "let a: i64 = 3; let b: i64 = 4;"),
        ("u32", "let a: u32 = 3u32; let b: u32 = 4u32;"),
        ("i8", "let a: i8 = 3i8; let b: i8 = 4i8;"),
        ("bool", "let a: bool = false; let b: bool = true;"),
        ("char", "let a: char = 'x'; let b: char = 'y';"),
        ("String", "let a: String = \"x\"; let b: String = \"y\";"),
    ];
    // a < b holds for every pair above, so the expected answers are the
    // same across the receiver classes — which is itself the point: these
    // six must not depend on the receiver's shape.
    for (recv, decl) in decls {
        for (method, want) in [
            ("eq", "false\n"),
            ("ne", "true\n"),
            ("lt", "true\n"),
            ("le", "true\n"),
            ("gt", "false\n"),
            ("ge", "false\n"),
        ] {
            let src = format!("fn main() {{ {decl} println(a.{method}(b)); }}");
            assert_eq!(run_program(&src).as_deref(), Some(want), "{recv}.{method}");
        }
    }

    // SIGNEDNESS, which is the half that goes wrong quietly. B-2026-08-28-5
    // is the record: `.cmp` was hardcoded signed while its `<` sibling was
    // not, so `(200u8).cmp(100u8)` answered `Less`. The new arms derive the
    // flag from `expr_is_unsigned_int`, the same helper `.cmp` uses, so the
    // two spellings cannot drift apart again — and these cases are where
    // that would show.
    assert_eq!(
        run_program(
            "fn main() { let a: u8 = 200u8; let b: u8 = 100u8;\n\
                 println(f\"{a.lt(b)} {a.gt(b)} {a.le(b)} {a.ge(b)}\"); }"
        )
        .as_deref(),
        Some("false true false true\n"),
        "u8 with the top bit set must compare UNSIGNED"
    );
    assert_eq!(
        run_program(
            "fn main() { let a: u32 = 4000000000u32; let b: u32 = 1u32;\n\
                 println(f\"{a.lt(b)} {a.gt(b)}\"); }"
        )
        .as_deref(),
        Some("false true\n"),
        "u32 above i32::MAX must compare UNSIGNED"
    );
    assert_eq!(
        run_program(
            "fn main() { let a: bool = false; let b: bool = true;\n\
                 println(f\"{a.lt(b)} {a.gt(b)} {b.lt(a)}\"); }"
        )
        .as_deref(),
        Some("true false false\n"),
        "`false < true`: a bool is i1 and must NOT read as signed"
    );
    assert_eq!(
        run_program(
            "fn main() { let a: i64 = -5; let b: i64 = 3;\n\
                 println(f\"{a.lt(b)} {a.gt(b)}\"); }"
        )
        .as_deref(),
        Some("true false\n"),
        "a signed receiver must still compare SIGNED"
    );

    // THE OPERATOR AND THE METHOD ARE ONE QUESTION, so they must give one
    // answer. This is the assertion that would catch a future drift
    // between the two lowerings even if both were individually plausible.
    assert_eq!(
        run_program(
            "fn main() {\n\
                     let a: u8 = 200u8; let b: u8 = 100u8;\n\
                     println(f\"{a < b} {a.lt(b)} {a > b} {a.gt(b)}\");\n\
                     let c: bool = false; let d: bool = true;\n\
                     println(f\"{c < d} {c.lt(d)}\");\n\
                     let s: String = \"abc\"; let t: String = \"abd\";\n\
                     println(f\"{s < t} {s.lt(t)} {s == t} {s.eq(t)}\");\n\
                 }"
        )
        .as_deref(),
        Some("false false true true\ntrue true\ntrue true false false\n"),
        "the operator and method spellings must agree pair-for-pair"
    );

    // A USER IMPL OWNING ONE OF THESE NAMES WINS. Removing the typechecker
    // exemption is what made such an impl reachable at all, and both
    // backends have to honour it: codegen's arm consults
    // `user_impl_method_exists`, and the interpreter reads the
    // typechecker's `method_impl_dispatch` record.
    //
    // The `u32` line is the CONTROL, and it is the one that pins the
    // interpreter's mechanism: the impl is on `i64`, so a `u32` receiver
    // must still get the builtin. A gate keyed on the interpreter's own
    // `value_type_name` cannot tell them apart — it answers `i64` for both,
    // because a `Value::Int` is type-erased — and answered `user` here
    // while the compiled side (and check, which types it `bool`) said
    // `false`.
    assert_eq!(
        run_program(
            "impl i64 { fn lt(self, other: i64) -> String { \"user\" } }\n\
                 fn main() {\n\
                     let a: i64 = 1; let b: i64 = 2; println(a.lt(b));\n\
                     let c: u32 = 5u32; let d: u32 = 2u32; println(c.lt(d));\n\
                 }"
        )
        .as_deref(),
        Some("user\nfalse\n"),
        "a user `impl i64` must win for an i64 receiver and NOT for a u32 one"
    );

    // `.cmp` was never build-broken (it has had an arm since before this
    // row) and must stay working, since the fix moves its typing.
    for (recv, decl) in decls {
        let src = format!(
            "fn main() {{ {decl} let r: Ordering = a.cmp(b);\n\
                 match r {{ Ordering.Less => {{ println(\"Less\"); }}\n\
                 Ordering.Equal => {{ println(\"Equal\"); }}\n\
                 Ordering.Greater => {{ println(\"Greater\"); }} }} }}"
        );
        assert_eq!(run_program(&src).as_deref(), Some("Less\n"), "{recv}.cmp");
    }
}

#[test]
fn test_e2e_freshtemp_call_match_option_shared_correct() {
    // B-2026-07-12-23 correctness pin (the ASAN/leak gate lives in
    // tests/memory_sanitizer.rs::asan_freshtemp_call_match_option_shared_no_leak).
    // A direct `match take()` on a call returning `Option[shared]` leaked the
    // node; the lowering fix rewrites the call scrutinee into a let-bound
    // scrutinee. This pins that the rewrite preserves the value — the bound
    // arm must still read the returned node's field. Non-ASAN so it guards
    // the value everywhere.
    let out = run_program(
        r#"
shared struct Node { val: i64, mut left: Option[Node], mut right: Option[Node] }
fn take() -> Option[Node] {
    let mut src: Vec[Option[Node]] = Vec.new();
    src.push(Some(Node { val: 7, left: None, right: None }));
    match src[0] {
        None => None,
        Some(n) => Some(n),
    }
}
fn main() {
    let mut i: i64 = 0;
    let mut t: i64 = 0;
    while i < 200 {
        match take() {
            None => {}
            Some(n) => { t = t + n.val; }
        }
        i = i + 1;
    }
    println(t);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "1400");
    }
}

#[test]
fn test_e2e_freshtemp_call_match_result_shared_correct() {
    // B-2026-07-12-24 correctness pin (the ASAN/leak gate lives in
    // tests/memory_sanitizer.rs::asan_freshtemp_call_match_result_shared_no_leak).
    // The `Result[shared]` sibling of the Option B-23 case: `match take()`
    // where `take` returns `Result[shared Node, i64]` via a returning arm
    // leaked the node (the B-21/B-23 lowering rewrite routes it through a
    // synthetic let-bound scrutinee, but Result had no rc cleanup for that
    // scrutinee — `track_rc_result_var` now supplies it). Pins that the
    // rewrite + release preserve the value.
    let out = run_program(
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
fn main() {
    let mut i: i64 = 0;
    let mut t: i64 = 0;
    while i < 200 {
        match take() {
            Err(e) => { t = t + e; }
            Ok(n) => { t = t + n.val; }
        }
        i = i + 1;
    }
    println(t);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "1400");
    }
}

#[test]
fn e2e_for_loop_enum_element_match_move_payload_no_double_free() {
    // B-2026-07-14-1: `for p in <owned Vec[enum]> { match p { V(t) => <move t> } }`.
    // The loop element bit-copy-aliases the container's slot, so moving a heap
    // payload out of it (into a call / a `let`) raw-moved it off that alias —
    // the consumer freed the buffer AND the container's per-element drop freed
    // it again (double-free abort). This is the f-string `render` loop in the
    // self-hosted lexer (`for p in parts { match p { Text(t) => …(t) } }` over
    // `Vec[InterpPart]`). Fixed by deep-copying the element's payload into an
    // INDEPENDENT buffer at the match (`clone_escaping_owned_agg_loop_var_enum`,
    // the loop-element sibling of the `v[i]` clone). A double-free aborts the
    // process, so a clean run with the correct sum IS the check. Mixed variants
    // + multiple heap elements exercise the tag switch and >1 free.
    let out = run_program(
        "enum Part { Text(String), Empty }\n\
             fn use_str(s: String) -> i64 { s.len() }\n\
             fn main() {\n\
                 let mut parts: Vec[Part] = Vec.new();\n\
                 let mut a = \"\".to_string(); a.push_str(\"hello\");\n\
                 let mut b = \"\".to_string(); b.push_str(\"world!!\");\n\
                 parts.push(Part.Text(a)); parts.push(Part.Empty); parts.push(Part.Text(b));\n\
                 let mut n = 0;\n\
                 for p in parts {\n\
                     match p {\n\
                         Part.Text(t) => { n = n + use_str(t); }\n\
                         Part.Empty => { n = n + 1; }\n\
                     }\n\
                 }\n\
                 println(n);\n\
             }",
    );
    if let Some(out) = out {
        assert_eq!(out, "13\n");
    }
}

#[test]
fn match_scrutinee_field_of_ref_struct_payload_binding() {
    // B-2026-07-11-12: `match h.w { Some(g) => ... }` where the scrutinee is a
    // FIELD read through a `ref` struct (`h: ref Holder`, `Holder.w:
    // Option[Wrap]`). Two independent halves were broken:
    //
    //   (1) Typechecker: field access through a `ref` struct returned
    //       `Type::Error` (a deliberate gap — only unions were peeled), so the
    //       match scrutinee had no type and the `Some(g)` payload binding never
    //       got its surface type recorded. Codegen then sized `g` to a single
    //       word and truncated the 3-word `Wrap` (a `Vec`) to just its data
    //       pointer → the loop over `g.items` read an empty collection (0).
    //
    //   (2) Ownership: once (1) sized `g` correctly, the payload aliases the
    //       borrowed buffer but got an owned scope-exit drop → double-free
    //       against the caller's `h`. A read-through-a-borrow scrutinee whose
    //       payloads never escape is a read-only VIEW, so its drop is
    //       suppressed (the escaping counterpart below stays owned).
    let non_escaping = "struct Wrap { items: Vec[i64] }\n\
            struct Holder { w: Option[Wrap] }\n\
            fn count(h: ref Holder) -> i64 {\n\
                let mut c = 0;\n\
                match h.w { Some(g) => { for x in g.items { c = c + 1; } } None => {} }\n\
                c\n\
            }\n\
            fn main() {\n\
                let mut items: Vec[i64] = Vec.new();\n\
                items.push(1); items.push(2); items.push(3);\n\
                let h = Holder { w: Some(Wrap { items: items }) };\n\
                println(count(h));\n\
            }\n";
    if let Some(out) = run_program(non_escaping) {
        assert_eq!(
            out.trim(),
            "3",
            "non-escaping struct payload through ref-field match; got:\n{out}"
        );
    }
    // Escaping counterpart: the bound String is RETURNED, so it must be owned
    // (move-out path), not aliased. Guards against the read-only-borrow
    // classification over-firing on an escaping payload.
    let escaping = "enum Tk { A, Id(String) }\n\
            struct Sp { tok: Tk }\n\
            struct P { toks: Vec[Sp], pos: i64 }\n\
            impl P {\n\
                fn take(mut ref self) -> String {\n\
                    match self.toks[self.pos].tok {\n\
                        Id(s) => { self.pos = self.pos + 1; s }\n\
                        A => { self.pos = self.pos + 1; \"a\".to_string() }\n\
                    }\n\
                }\n\
            }\n\
            fn main() {\n\
                let mut w: Vec[Sp] = Vec.new();\n\
                w.push(Sp { tok: Tk.Id(\"hello\".to_string()) });\n\
                let mut p = P { toks: w, pos: 0 };\n\
                println(p.take());\n\
            }\n";
    if let Some(out) = run_program(escaping) {
        assert_eq!(
            out.trim(),
            "hello",
            "escaping String payload through ref-field match; got:\n{out}"
        );
    }
}

#[test]
fn for_loop_enum_element_direct_match_payload_read() {
    // Direct `for it in items { match it { V(x) => … } }` on a heap-bearing
    // user-enum element — matching the loop var DIRECTLY (no `let x = it`
    // whole-move copy first, unlike the sibling above). The element is
    // registered under the deep-copy-on-whole-move model
    // (`for_loop_owned_agg_vars`), which `scrutinee_is_borrowed_binding`
    // does NOT consult, so the struct payload `x` bound out of the borrowed
    // element registered its own scope-exit drop and double-freed against
    // the container's per-element drop. `match` now treats a non-escaping
    // payload over such an element as a read-only view (drop suppressed).
    // Covers a String-field read AND iterating a Vec payload field (both
    // aliased through the element); the sink locks the value.
    let src = "struct Named { name: String, tags: Vec[i64] }\n\
                   enum Item { A(Named), B }\n\
                   fn total(items: ref Vec[Item]) -> i64 {\n\
                   \x20   let mut n = 0;\n\
                   \x20   for it in items {\n\
                   \x20       match it {\n\
                   \x20           A(x) => { n = n + x.name.len(); for t in x.tags { n = n + t; } }\n\
                   \x20           B => {}\n\
                   \x20       }\n\
                   \x20   }\n\
                   \x20   n\n\
                   }\n\
                   fn main() {\n\
                   \x20   let mut v: Vec[Item] = Vec.new();\n\
                   \x20   let mut tg: Vec[i64] = Vec.new(); tg.push(10); tg.push(20);\n\
                   \x20   v.push(Item.A(Named { name: \"hello\", tags: tg }));\n\
                   \x20   v.push(Item.B);\n\
                   \x20   println(total(v));\n\
                   \x20   println(v.len());\n\
                   }\n";
    // name.len()=5 + tags 10+20 = 35; v still intact (len 2).
    assert_eq!(run_program(src).as_deref(), Some("35\n2\n"));
}

// ── Enum struct-variant construction + scalar enum `==` (codegen) ──

#[test]
fn e2e_enum_struct_variant_construction_match_and_eq() {
    // Source-level `Enum.Variant { field: value }` lowers to the seeded
    // enum aggregate; match binds named fields by extracting payload words;
    // `==` is sound across unit/struct/mixed variants (zero-init keeps the
    // word-wise compare from reading undef payload words).
    if let Some(out) = run_program(
        "#[derive(Eq)]\n\
             enum Shape { Circle { r: i64 }, Square { side: i64 }, Unknown }\n\
             fn area(s: Shape) -> i64 {\n\
                 match s {\n\
                     Shape.Circle { r } => 3 * r * r,\n\
                     Shape.Square { side } => side * side,\n\
                     Shape.Unknown => 0,\n\
                 }\n\
             }\n\
             fn main() {\n\
                 let c = Shape.Circle { r: 2 };\n\
                 let c2 = Shape.Circle { r: 2 };\n\
                 let sq = Shape.Square { side: 3 };\n\
                 let u = Shape.Unknown;\n\
                 let ca = Shape.Circle { r: 2 };\n\
                 let sqa = Shape.Square { side: 3 };\n\
                 let ua = Shape.Unknown;\n\
                 println(f\"{area(ca)}\");\n\
                 println(f\"{area(sqa)}\");\n\
                 println(f\"{area(ua)}\");\n\
                 println(f\"{c == c2}\");\n\
                 println(f\"{c == sq}\");\n\
                 println(f\"{c == u}\");\n\
                 println(f\"{u == u}\");\n\
             }",
    ) {
        assert_eq!(out, "12\n9\n0\ntrue\nfalse\nfalse\ntrue\n");
    }
}

#[test]
fn e2e_unqualified_struct_variant_pattern_binds_fields() {
    // B-2026-06-13-7: an UNQUALIFIED struct-variant pattern (`A { n }`, no
    // `E.` qualifier) must bind its fields. Pre-fix the binding path only
    // ran for a qualified `Enum.Variant { .. }` (path.len() >= 2), so an
    // unqualified `A { n }` fell through to the plain-struct lookup (which
    // misses — the name is a variant, not a struct) and `n` stayed unbound:
    // "codegen failed: Undefined variable 'n'". The sibling test above only
    // ever uses the qualified `Shape.Circle { r }` form, which masked it.
    // Covers single + multi-field variants and a mix of struct/unit arms.
    if let Some(out) = run_program(
        "enum E { A { n: i64 }, C { x: i64, y: i64 }, B }\n\
             fn f(e: E) -> i64 {\n\
                 match e {\n\
                     A { n } => n,\n\
                     C { x, y } => x + y,\n\
                     B => 0,\n\
                 }\n\
             }\n\
             fn main() {\n\
                 println(f\"{f(E.A { n: 5 })}\");\n\
                 println(f\"{f(E.C { x: 3, y: 4 })}\");\n\
                 println(f\"{f(E.B)}\");\n\
             }",
    ) {
        assert_eq!(out, "5\n7\n0\n");
    }
}

#[test]
fn e2e_plain_struct_pattern_with_nested_enum_field_checks_discriminant() {
    // Regression: a PLAIN struct pattern whose field sub-pattern is an
    // enum-variant pattern (`Item { shape: Shape.Circle(r), .. }`) must
    // check the nested enum's discriminant. Pre-fix the plain struct
    // pattern fell through `compile_pattern_condition`'s `_ => true`
    // catch-all, dropping every field sub-pattern's test, so a `Rect` /
    // `Point` item matched the first `Circle(r)` arm and bound garbage
    // (`karac build` printed `circle r=3` / `circle r=0` where `karac run
    // --interp` printed `rect 3x6` / `point`). Struct sibling of the tuple
    // "no discriminating test" class (B-2026-07-12-13). Scrutinee is
    // `ref Item` so the fix's pointer-GEP-and-load path is exercised.
    if let Some(out) = run_program(
        "enum Shape { Circle(f64), Rect(f64, f64), Point }\n\
             struct Item { shape: Shape, count: i64 }\n\
             fn classify(it: ref Item) -> String {\n\
                 match it {\n\
                     Item { shape: Shape.Circle(r), count: _ } => f\"circle r={r}\",\n\
                     Item { shape: Shape.Rect(w, h), count: _ } => f\"rect {w}x{h}\",\n\
                     Item { shape: Shape.Point, count: _ } => \"point\",\n\
                 }\n\
             }\n\
             fn main() {\n\
                 println(classify(Item { shape: Shape.Circle(5.0), count: 1 }));\n\
                 println(classify(Item { shape: Shape.Rect(3.0, 6.0), count: 1 }));\n\
                 println(classify(Item { shape: Shape.Point, count: 9 }));\n\
             }",
    ) {
        assert_eq!(out, "circle r=5\nrect 3x6\npoint\n");
    }
}

#[test]
fn e2e_plain_struct_pattern_with_literal_field_checks_value() {
    // Companion: a plain struct pattern with a LITERAL field sub-pattern
    // (`P { x: 0, y }`) must test the literal, not fall through to always-
    // match. Owned (by-value) struct scrutinee → the extractvalue path.
    if let Some(out) = run_program(
        "struct P { x: i64, y: i64 }\n\
             fn name(p: P) -> String {\n\
                 match p {\n\
                     P { x: 0, y: 0 } => \"origin\",\n\
                     P { x: 0, y: _ } => \"y-axis\",\n\
                     P { x: _, y: 0 } => \"x-axis\",\n\
                     P { x: _, y: _ } => \"other\",\n\
                 }\n\
             }\n\
             fn main() {\n\
                 println(name(P { x: 0, y: 0 }));\n\
                 println(name(P { x: 0, y: 5 }));\n\
                 println(name(P { x: 5, y: 0 }));\n\
                 println(name(P { x: 5, y: 5 }));\n\
             }",
    ) {
        assert_eq!(out, "origin\ny-axis\nx-axis\nother\n");
    }
}

#[test]
fn e2e_128bit_literal_patterns_match_their_own_value() {
    // B-2026-08-20-4. `LiteralPattern::Integer` held an `i64`, so no
    // literal past `i64::MAX` could be a match pattern — the upper half of
    // `i128`, the whole top half of `u128`, and every `u128` value beyond
    // `i128::MAX` were reachable in EXPRESSION position and unreachable in
    // PATTERN position. A 128-bit scrutinee could be compared and bound but
    // never matched against its own literal, and a guard (`n if n == …`)
    // was the only spelling — one exhaustiveness cannot see through.
    //
    // Every value here is past `i64::MAX`, which is exactly where the old
    // carrier ran out. The MISS arms matter as much as the hits: the
    // pattern and the scrutinee have to agree on the wrapped encoding, so a
    // pattern that should not fire must not fire either. `…105728u128` is
    // one past `i128::MAX`, so its carrier value is NEGATIVE — it is
    // distinguished from `u128::MAX` and from `5u128` on the same run.
    if let Some(out) = run_program(
            "fn classify(n: i128) -> String {\n\
             match n {\n\
             0i128 => return \"zero\",\n\
             1267650600228229401496703205376i128 => return \"2^100\",\n\
             170141183460469231731687303715884105727i128 => return \"i128max\",\n\
             1000000000000000000000000000000i128..=2000000000000000000000000000000i128 => return \"band\",\n\
             _ => return \"other\",\n\
             }\n\
             }\n\
             fn uclassify(n: u128) -> String {\n\
             match n {\n\
             340282366920938463463374607431768211455u128 => return \"umax\",\n\
             170141183460469231731687303715884105728u128 => return \"just-past-imax\",\n\
             5u128 => return \"five\",\n\
             _ => return \"other\",\n\
             }\n\
             }\n\
             fn main() {\n\
             println(classify(0i128));\n\
             println(classify(1267650600228229401496703205376i128));\n\
             println(classify(170141183460469231731687303715884105727i128));\n\
             println(classify(1500000000000000000000000000000i128));\n\
             println(classify(7i128));\n\
             println(uclassify(340282366920938463463374607431768211455u128));\n\
             println(uclassify(170141183460469231731687303715884105728u128));\n\
             println(uclassify(5u128));\n\
             println(uclassify(9u128));\n\
             let b: u64 = 18446744073709551615u64;\n\
             match b { 18446744073709551615u64 => println(\"u64max\"), _ => println(\"no\") }\n\
             }",
        ) {
            assert_eq!(
                out,
                "zero\n\
                 2^100\n\
                 i128max\n\
                 band\n\
                 other\n\
                 umax\n\
                 just-past-imax\n\
                 five\n\
                 other\n\
                 u64max\n"
            );
        }
}

#[test]
fn e2e_negative_literal_patterns_match() {
    // B-2026-08-20-7. Parsing is only half of it — the folded literal has
    // to carry its sign all the way to the comparison, on both backends.
    //
    // MISS arms are asserted beside the hits throughout: a sign lost
    // anywhere between the fold and the compare would show up as an arm
    // that fires on the wrong value, not as one that fails to fire. The two
    // MIN magnitudes are the sharpest case — they have no positive form, so
    // they exist only as an already-negated literal.
    //
    // The `Some(5)` MISS was held out when this test was written: codegen
    // then compared only the variant TAG, so any `Some(<literal>)` arm
    // fired for every `Some(_)` (B-2026-08-20-11, found by this very
    // discipline of asserting misses). That is fixed, so the assertion is
    // restored — a negative literal in an enum payload now has to both
    // fire on its own value and stay quiet on another.
    if let Some(out) = run_program(
            "fn sign(n: i64) -> String {\n\
             match n {\n\
             -9223372036854775808 => return \"min\",\n\
             -10..=-1 => return \"small-neg\",\n\
             0 => return \"zero\",\n\
             1 | 2 => return \"one-or-two\",\n\
             _ => return \"other\",\n\
             }\n\
             }\n\
             fn main() {\n\
             println(sign(0i64 - 9223372036854775807i64 - 1i64));\n\
             println(sign(0i64 - 5i64));\n\
             println(sign(0i64));\n\
             println(sign(2i64));\n\
             println(sign(0i64 - 99i64));\n\
             let o: Option[i64] = Some(0i64 - 5i64);\n\
             match o { Some(-5) => println(\"payload\"), _ => println(\"no\") }\n\
             let q: Option[i64] = Some(5i64);\n\
             match q { Some(-5) => println(\"payload\"), _ => println(\"no\") }\n\
             let t = (0i64 - 1i64, 2i64);\n\
             match t { (-1, 2) => println(\"tuple\"), _ => println(\"no\") }\n\
             let f: f64 = 0.0 - 1.5;\n\
             match f { -1.5 => println(\"float\"), _ => println(\"no\") }\n\
             let g: f64 = 1.5;\n\
             match g { -1.5 => println(\"float\"), _ => println(\"no\") }\n\
             let w: i128 = 0i128 - 170141183460469231731687303715884105727i128 - 1i128;\n\
             match w { -170141183460469231731687303715884105728i128 => println(\"i128min\"), _ => println(\"no\") }\n\
             let s: i8 = 0i8 - 5i8;\n\
             match s { -128i8..=-1i8 => println(\"neg\"), 0i8..=127i8 => println(\"nonneg\") }\n\
             }",
        ) {
            assert_eq!(
                out,
                "min\n\
                 small-neg\n\
                 zero\n\
                 one-or-two\n\
                 other\n\
                 payload\n\
                 no\n\
                 tuple\n\
                 float\n\
                 no\n\
                 i128min\n\
                 neg\n"
            );
        }
}

#[test]
fn e2e_let_init_match_all_arms_return() {
    if let Some(out) = run_program(
        "fn t(n: i64) -> i64 {\n\
                 let x = match n {\n\
                     0 => { return 10; }\n\
                     _ => { return 20; }\n\
                 };\n\
                 99\n\
             }\n\
             fn main() {\n\
                 println(f\"{t(0)}\");\n\
                 println(f\"{t(4)}\");\n\
             }",
    ) {
        assert_eq!(out, "10\n20\n");
    }
}

/// B-2026-08-08-25 leg 1, THE CONSUMING HALF — a match arm that MOVES a
/// heap payload out of a live `Option` local no longer leaves that local
/// dangling. This was the row's last memory-unsafety: valgrind reported 7
/// `Invalid read`/`Invalid free` errors on case 1, which printed garbage
/// where `--interp` printed the string twice.
///
/// The borrow-only half was already free — a read-only match is classified
/// as a borrow and the payload simply stays with the source. A consuming
/// arm cannot use that: the payload really does leave, so two live owners
/// need two buffers. `clone_escaping_live_local_option` is the bare-local
/// sibling of the existing `<refparam>.field` clone leg, reusing its whole
/// contract — tag-guarded type-directed clone, a `FreeInlineOptionPayload`
/// on the clone slot, and the consuming arm zeroing the clone's tag so the
/// buffer the binding now owns is not freed twice.
///
/// TWO EARLIER READINGS OF THIS LEG WERE WRONG, both recorded on the row:
///
/// - "A bespoke inline-payload clone must get element DEPTH right or a
///   `Vec[String]` payload becomes a double-free." Nothing bespoke was
///   needed — `emit_clone_fn_for_type_expr` is type-directed, so depth
///   follows the `TypeExpr`. Case 3 is that `Vec[String]` payload and it is
///   valgrind-clean. The `deep_copy_enum_heap_payload_in_place` trap the row
///   warned about is real but belongs to the user-ENUM legs, which key on
///   `field_drop_kinds` the erased `Option` layout does not carry.
///
/// - "The fix is a LIFETIME question, not a clone question" — suppress the
///   RESULT's free while the source is live, since an IR diff showed the
///   alias already balanced. That holds only for a result that dies in the
///   same scope; a returned, pushed, or stored one would dangle instead, so
///   it trades this use-after-free for a rarer one. Two live owners need two
///   buffers.
///
/// Case 4 is the load-bearing control: with the source DEAD there is only
/// one owner, so the clone must NOT fire and the zero-cost transfer stays.
/// That gate (`scrutinee_read_after_match`) is what keeps this off the
/// ~1000 memory fixtures that exercise the arm path.
///
/// The read-only half is deliberately NOT re-pinned here — it already has
/// two tests above, and its shape needs an ownership-gate grandfather entry
/// that these cases do not. Which is its own oddity, noted on the row: the
/// ownership checker calls the READ-ONLY `match o { … }` twice a
/// `UseAfterMove`, and says nothing at all about `o.map(|s| s)` followed by
/// a read — the shape that genuinely moves the payload out.
#[test]
fn test_e2e_consuming_match_over_a_live_option_local_keeps_the_source() {
    // 1. THE BUG: a consuming mapper, then a plain read of the source.
    assert_eq!(
        run_program(
            "fn main() {\n\
                     let o: Option[String] = Some(f\"hi\");\n\
                     match o.map(|s| s) { Some(v) => { println(v); } None => {} }\n\
                     match o { Some(v) => { println(v); } None => {} }\n\
                 }"
        )
        .as_deref(),
        Some("hi\nhi\n")
    );
    // 2. Chained consuming mappers over the same live source.
    assert_eq!(
        run_program(
            "fn main() {\n\
                     let o: Option[String] = Some(f\"hi\");\n\
                     match o.map(|s| s).map(|s| s) { Some(v) => { println(v); } None => {} }\n\
                     match o { Some(v) => { println(v); } None => {} }\n\
                 }"
        )
        .as_deref(),
        Some("hi\nhi\n")
    );
    // 3. The element-DEPTH case the row flagged as the double-free trap:
    // a shallow clone here would alias both element buffers.
    assert_eq!(
        run_program(
            "fn main() {\n\
                     let o: Option[Vec[String]] = Some([f\"aa\", f\"bb\"]);\n\
                     match o.map(|s| s) { Some(v) => { println(v[0]); } None => {} }\n\
                     match o { Some(v) => { println(v[1]); } None => {} }\n\
                 }"
        )
        .as_deref(),
        Some("aa\nbb\n")
    );
    // 4. CONTROL — source DEAD after the consuming match. One owner, so no
    // clone: this is the shape the liveness gate must keep on the old path.
    assert_eq!(
        run_program(
            "fn main() {\n\
                     let o: Option[String] = Some(f\"hi\");\n\
                     match o.map(|s| s) { Some(v) => { println(v); } None => {} }\n\
                 }"
        )
        .as_deref(),
        Some("hi\n")
    );
    // 5. `None` at runtime — the clone allocates nothing and the absent arm
    // leaves the clone's cleanup armed against an empty payload.
    assert_eq!(
            run_program(
                "fn main() {\n\
                     let o: Option[String] = None;\n\
                     match o.map(|s| s) { Some(v) => { println(v); } None => { println(\"-\"); } }\n\
                     match o { Some(v) => { println(v); } None => { println(\"-\"); } }\n\
                 }"
            )
            .as_deref(),
            Some("-\n-\n")
        );
    // 6. CONTROL — the `let`-bound spelling, correct before this fix
    // (the result's free lands after the re-read) and still correct.
    assert_eq!(
        run_program(
            "fn main() {\n\
                     let o: Option[String] = Some(f\"hi\");\n\
                     let a: Option[String] = o.map(|s| s);\n\
                     match a { Some(v) => { println(v); } None => {} }\n\
                     match o { Some(v) => { println(v); } None => {} }\n\
                 }"
        )
        .as_deref(),
        Some("hi\nhi\n")
    );
}

/// B-2026-08-08-25 LEGS 2 AND 3 — the `Result` and user-ENUM channels,
/// which leg 1's `Option`-scoped work did not reach. This test replaces the
/// `#[ignore]`d `test_e2e_consuming_map_over_live_optres_still_open`; its
/// two cases are cases 1 and 4 below, unchanged.
///
/// Each channel had the SAME two halves leg 1 had, and each half had its own
/// cause:
///
/// - READ-ONLY, `Result` (case 2). The classifier already covered `Result`;
///   what disqualified it was the co-arm. `scrutinee_is_readonly_inline_
///   optres_local` requires EVERY payload-consuming arm to be the direct-heap
///   shape, and `Err(e)` binding an `i64` is not — so the whole match fell
///   back to the transfer path that empties the source. That is the entire
///   reason the defect looked `Result`-specific: `Option`'s only co-arm is
///   `None`, which binds nothing and can never trip the gate. Cases 2a/2b are
///   the discriminator that found it — the same program with `Err(_)` or with
///   a heap `Err` was always correct; only a SCALAR co-arm broke it.
///
///   The row anticipated the trap in fixing this: "no recorded type" cannot
///   be read as "scalar", because a destructured tuple payload is untyped in
///   `pattern_binding_types` too and its bindings own themselves. Confirmed —
///   a plain `i64` records NO entry at all there (that table stores narrowed
///   widths and named types; 64-bit is the default and is omitted). So the
///   exemption is keyed on the scrutinee's own instantiation instead, which
///   is a positive witness rather than an absence.
///
/// - CONSUMING, `Result` (case 1). The heap-payload `.map` lowers via
///   `compile_map_via_match_synthesis`, whose synthesized match has the
///   RECEIVER as its scrutinee — so this is literally leg 1's bare-local
///   consuming match, gated to `inline_option_payload_vars` and never
///   looking at the `Result` set. `clone_escaping_live_local_result` is the
///   twin, on the ref-chain leg's clone-slot contract.
///
/// - READ-ONLY, user enum (cases 4-6). A user enum's payload rides
///   `field_drop_kinds`, not the inline Option/Result channel, so no
///   caller-retains classifier covered it at all;
///   `scrutinee_is_readonly_owned_enum_local` is that missing sibling.
///
/// - CONSUMING, user enum (case 7). Same clone shape, but the duplicator is
///   `deep_copy_enum_heap_payload_in_place` rather than a type-directed
///   clone — see case 8 for the depth limit that keeps it honest.
///
/// Cases 3 and 9 are the liveness controls, the gate that keeps all of this
/// off the common shape: with the source DEAD after the match there is one
/// owner, the clone must not fire, and the zero-cost transfer stays.
#[test]
fn test_e2e_live_local_match_keeps_the_source_for_result_and_user_enums() {
    // 1. LEG 2, consuming half — `Result.map` over a live source.
    assert_eq!(
        run_program(
            "fn main() {\n\
                     let r: Result[String, i64] = Ok(f\"hi\");\n\
                     match r.map(|x| x.to_uppercase()) { Ok(v) => { println(v); } Err(e) => {} }\n\
                     match r { Ok(v) => { println(v); } Err(e) => {} }\n\
                 }"
        )
        .as_deref(),
        Some("HI\nhi\n")
    );
    // 2. LEG 2, read-only half — the scalar `Err` co-arm is what broke it.
    assert_eq!(
        run_program(
            "fn main() {\n\
                     let r: Result[String, i64] = Ok(f\"hi\");\n\
                     match r { Ok(v) => { println(v); } Err(e) => { println(f\"E\"); } }\n\
                     match r { Ok(v) => { println(v); } Err(e) => { println(f\"E\"); } }\n\
                 }"
        )
        .as_deref(),
        Some("hi\nhi\n")
    );
    // 2a/2b. The discriminators: a co-arm that BINDS NOTHING, and one that
    // binds HEAP, were both always correct. Kept as pins so a future
    // narrowing of the exemption cannot silently re-break only the scalar
    // spelling — the asymmetry is the whole diagnosis.
    assert_eq!(
        run_program(
            "fn main() {\n\
                     let r: Result[String, i64] = Ok(f\"hi\");\n\
                     match r { Ok(v) => { println(v); } Err(_) => {} }\n\
                     match r { Ok(v) => { println(v); } Err(_) => {} }\n\
                 }"
        )
        .as_deref(),
        Some("hi\nhi\n")
    );
    assert_eq!(
        run_program(
            "fn main() {\n\
                     let r: Result[String, String] = Ok(f\"hi\");\n\
                     match r { Ok(v) => { println(v); } Err(e) => { println(e); } }\n\
                     match r { Ok(v) => { println(v); } Err(e) => { println(e); } }\n\
                 }"
        )
        .as_deref(),
        Some("hi\nhi\n")
    );
    // 3. CONTROL — `Result` source DEAD after the consuming match. One
    // owner, so the clone must not fire.
    assert_eq!(
        run_program(
            "fn main() {\n\
                     let r: Result[String, i64] = Ok(f\"hi\");\n\
                     match r.map(|s| s) { Ok(v) => { println(v); } Err(_) => {} }\n\
                 }"
        )
        .as_deref(),
        Some("hi\n")
    );
    // 4. LEG 3, read-only half — the user-enum channel.
    assert_eq!(
        run_program(
            "enum E { A(String), B }\n\
                 fn main() {\n\
                     let e: E = E.A(f\"hi\");\n\
                     match e { E.A(s) => { println(s); } E.B => {} }\n\
                     match e { E.A(s) => { println(s); } E.B => {} }\n\
                 }"
        )
        .as_deref(),
        Some("hi\nhi\n")
    );
    // 5. Three reads, and a MULTI-FIELD variant: the suppressor zeroes each
    // consumed field, so a single-field pin would not catch a per-field
    // regression.
    assert_eq!(
        run_program(
            "enum E { A(String, String), B }\n\
                 fn main() {\n\
                     let e: E = E.A(f\"x\", f\"y\");\n\
                     match e { E.A(p, q) => { println(p); println(q); } E.B => {} }\n\
                     match e { E.A(p, q) => { println(p); println(q); } E.B => {} }\n\
                     match e { E.A(p, q) => { println(p); println(q); } E.B => {} }\n\
                 }"
        )
        .as_deref(),
        Some("x\ny\nx\ny\nx\ny\n")
    );
    // 6. The ABSENT variant at runtime — nothing is bound, so the source
    // keeps everything and the arms are pure control flow.
    assert_eq!(
        run_program(
            "enum E { A(String), B }\n\
                 fn main() {\n\
                     let e: E = E.B;\n\
                     match e { E.A(s) => { println(s); } E.B => { println(f\"-\"); } }\n\
                     match e { E.A(s) => { println(s); } E.B => { println(f\"-\"); } }\n\
                 }"
        )
        .as_deref(),
        Some("-\n-\n")
    );
    // 7. LEG 3, consuming half — the arm MOVES the payload into a `let`,
    // and the source is read afterwards.
    assert_eq!(
        run_program(
            "enum E { A(String), B }\n\
                 fn main() {\n\
                     let e: E = E.A(f\"hi\");\n\
                     match e { E.A(v) => { let k: String = v; println(k); } E.B => {} }\n\
                     match e { E.A(v) => { println(v); } E.B => {} }\n\
                 }"
        )
        .as_deref(),
        Some("hi\nhi\n")
    );
    // 8. The same consuming shape with a `Vec` payload, whose `for` loop
    // moves it. `Vec[i64]` is depth-1 and the enum deep-copy is exact for
    // it. The `Vec[String]` sibling is NOT — that copy is outer-buffer only,
    // so the element `String`s would stay shared — and it is deliberately
    // left on the transfer path by `enum_payload_deep_copy_is_exact` rather
    // than handed a use-after-free. See the still-open pin below.
    assert_eq!(
        run_program(
            "enum E { A(Vec[i64]), B }\n\
                 fn main() {\n\
                     let e: E = E.A([11, 22]);\n\
                     match e { E.A(v) => { for s in v { println(s); } } E.B => {} }\n\
                     match e { E.A(v) => { for s in v { println(s); } } E.B => {} }\n\
                 }"
        )
        .as_deref(),
        Some("11\n22\n11\n22\n")
    );
    // 9. CONTROL — user-enum source DEAD after a consuming match.
    assert_eq!(
        run_program(
            "enum E { A(String), B }\n\
                 fn main() {\n\
                     let e: E = E.A(f\"hi\");\n\
                     match e { E.A(v) => { let k: String = v; println(k); } E.B => {} }\n\
                 }"
        )
        .as_deref(),
        Some("hi\n")
    );
    // 10. The `if let` SPELLING of leg 3's read-only half. `control_flow.rs`
    // calls the same `suppress_destructured_enum_payload_cleanup`, so this
    // emptied the source exactly as the `match` spelling did — and only the
    // user-enum channel was affected here, because `Option`/`Result` already
    // had a block classifier (`scrutinee_is_readonly_inline_optres_local_
    // block`) and this one did not exist. Fixing the match form alone would
    // have left a measured hole under a closed row.
    assert_eq!(
        run_program(
            "enum E { A(String), B }\n\
                 fn main() {\n\
                     let e: E = E.A(f\"hi\");\n\
                     if let E.A(v) = e { println(v); }\n\
                     if let E.A(v) = e { println(v); }\n\
                 }"
        )
        .as_deref(),
        Some("hi\nhi\n")
    );
    // 11. Same, re-entered from a loop — the `while let` site takes the
    // identical classifier, and the body frame must not accumulate owners.
    assert_eq!(
        run_program(
            "enum E { A(String), B }\n\
                 fn main() {\n\
                     let e: E = E.A(f\"hi\");\n\
                     let mut i: i64 = 0;\n\
                     while i < 3 {\n\
                         if let E.A(v) = e { println(v); }\n\
                         i = i + 1;\n\
                     }\n\
                 }"
        )
        .as_deref(),
        Some("hi\nhi\nhi\n")
    );
}

/// B-2026-08-09-14 — a CONSUMING `while let` arm over a plain enum local
/// whose source is DEAD after the loop double-freed: the arm moved the
/// payload into the binding but never zeroed the SOURCE's cap, so `e`'s
/// `__karac_drop_<E>` re-freed the buffer the binding had already freed.
///
/// The missing WHILE-LET leg of B-2026-07-23-13, which gave the
/// owned-enum-local suppression to `if let` and left the loop form on the
/// raw transfer path. `match` and `if let` were both already correct,
/// which is what made this look while-let-specific rather than a plain
/// missing mirror.
///
/// It surfaced only for a DEAD source because B-2026-08-09-11's live-local
/// clone hands a LIVE source's arm its own buffer, so the source's free is
/// not a double one (case 3 of the test above is exactly that program).
/// Dead sources decline that clone by design and land back on the transfer
/// path, where this lived.
///
/// Case 2 is the `break` spelling, which pins that the defect never
/// depended on the reassignment — the reasoning that first ruled out the
/// assignment's drop of the old value as the cause.
#[test]
fn test_e2e_consuming_while_let_over_dead_enum_local() {
    assert_eq!(
        run_program(
            "enum E { A(String), B }\n\
                 fn main() {\n\
                     let mut e: E = E.A(f\"hi\");\n\
                     while let E.A(v) = e { let k: String = v; println(k); e = E.B; }\n\
                     println(f\"done\");\n\
                 }"
        )
        .as_deref(),
        Some("hi\ndone\n")
    );
    // Same, without the reassignment — terminates via `break`.
    assert_eq!(
            run_program(
                "enum E { A(String), B }\n\
                 fn main() {\n\
                     let mut e: E = E.A(f\"hi\");\n\
                     let mut n: i64 = 0;\n\
                     while let E.A(v) = e { let k: String = v; println(k); n = n + 1; if n > 0 { break } }\n\
                     println(f\"done\");\n\
                 }"
            )
            .as_deref(),
            Some("hi\ndone\n")
        );
    // MULTI-ITERATION, each iteration re-populating the source with a
    // FRESH payload. The suppression stores a zero cap in the matched
    // path, which runs every iteration, so this is the case that pins the
    // re-arm: iteration n's assignment drops the old value against the
    // zeroed cap (skipping the payload the binding owns), stores a new
    // payload, and iteration n+1's store re-fires against that. A
    // suppression hoisted out of the loop, or one that permanently
    // disarmed the slot, would free a later iteration's payload twice or
    // leak it — a single-iteration test cannot tell those apart.
    assert_eq!(
        run_program(
            "enum E { A(String), B }\n\
                 fn main() {\n\
                     let mut e: E = E.A(f\"a1\");\n\
                     let mut n: i64 = 0;\n\
                     while let E.A(v) = e {\n\
                         let k: String = v; println(k); n = n + 1;\n\
                         if n < 3 { e = E.A(f\"a{n + 1}\"); } else { e = E.B; }\n\
                     }\n\
                     println(f\"done\");\n\
                 }"
        )
        .as_deref(),
        Some("a1\na2\na3\ndone\n")
    );
}

/// B-2026-08-09-10 — the interpreter's moved-out tracking is keyed by NAME
/// with no frame scoping, so a callee that moves a payload out of its own
/// binding silently disarmed an UNRELATED CALLER binding spelled the same
/// way, and that binding's `Drop` never ran.
///
/// Case 1 is the row's own reproduction, whose caller local and callee
/// param are both `b` — as ordinary code routinely does. Case 2 is the
/// SAME program with the names made distinct: it was already correct
/// before the fix, and is the measurement that identified the collision
/// (renaming either side fixed it with no compiler change). Keeping both
/// is the point — if the fix regresses, case 1 goes silent again while
/// case 2 stays green, which localises it immediately.
///
/// Cases 3 and 4 are the shapes that already worked and must keep working:
/// an owned param never matched at all, and a local matched with an early
/// `return` in the arm. They rule out "params don't drop" and "an early
/// return skips cleanup", both of which the shape superficially suggests.
#[test]
fn test_e2e_callee_move_does_not_disarm_a_caller_binding_of_the_same_name() {
    let prog = |caller: &str, param: &str| {
        format!(
            "struct Res {{ id: i64, name: String }}\n\
                 impl Drop for Res {{\n\
                 \x20   fn drop(mut ref self) {{ println(f\"drop {{self.id}}\") }}\n\
                 }}\n\
                 enum Box2 {{ Full(Res), Empty }}\n\
                 fn into_id({param}: Box2) -> i64 {{\n\
                 \x20   match {param} {{\n\
                 \x20       Box2.Full(r) => {{ return r.id; }}\n\
                 \x20       Box2.Empty => {{ return 0; }}\n\
                 \x20   }}\n\
                 }}\n\
                 fn main() {{\n\
                 \x20   let {caller}: Box2 = Box2.Full(Res {{ id: 7, name: f\"e7\" }});\n\
                 \x20   let v: i64 = into_id({caller});\n\
                 \x20   println(f\"v={{v}}\");\n\
                 }}"
        )
    };
    // 1. Names COLLIDE — the row's reproduction. Was silent on `--interp`.
    assert_eq!(
        run_program(&prog("b", "b")).as_deref(),
        Some("drop 7\nv=7\n")
    );
    // 2. Names DISTINCT — the control that identified the collision.
    assert_eq!(
        run_program(&prog("outer", "bp")).as_deref(),
        Some("drop 7\nv=7\n")
    );
    // 3. Owned param never matched — already fired; must still.
    assert_eq!(
        run_program(
            "struct Res { id: i64, name: String }\n\
                 impl Drop for Res {\n\
                 \x20   fn drop(mut ref self) { println(f\"drop {self.id}\") }\n\
                 }\n\
                 enum Box2 { Full(Res), Empty }\n\
                 fn eat(b: Box2) { println(\"in\"); }\n\
                 fn main() {\n\
                 \x20   let b: Box2 = Box2.Full(Res { id: 7, name: f\"e7\" });\n\
                 \x20   eat(b);\n\
                 \x20   println(\"end\");\n\
                 }"
        )
        .as_deref(),
        Some("in\ndrop 7\nend\n")
    );
    // 4. LOCAL (not a param) with an early return in the arm — rules out
    //    the early return as the discriminator.
    assert_eq!(
        run_program(
            "struct Res { id: i64, name: String }\n\
                 impl Drop for Res {\n\
                 \x20   fn drop(mut ref self) { println(f\"drop {self.id}\") }\n\
                 }\n\
                 enum Box2 { Full(Res), Empty }\n\
                 fn eat() -> i64 {\n\
                 \x20   let b: Box2 = Box2.Full(Res { id: 7, name: f\"e7\" });\n\
                 \x20   match b {\n\
                 \x20       Box2.Full(r) => { return r.id; }\n\
                 \x20       Box2.Empty => { return 0; }\n\
                 \x20   }\n\
                 }\n\
                 fn main() { let v: i64 = eat(); println(f\"v={v}\"); }"
        )
        .as_deref(),
        Some("drop 7\nv=7\n")
    );
}

/// B-2026-08-29-8 — a `match` arm that REBINDS its payload to a local and
/// yields that local as the arm's BLOCK TAIL ran the payload's `Drop` body
/// twice on both compiled backends, against the interpreter's single fire.
///
/// The value-position block-tail suppressor (`suppress_block_tail_cleanup`,
/// stmts.rs) neutralized the memory and boxed-payload channels for an
/// Identifier tail but never the USER-DROP one, which its function-body
/// sibling `suppress_cleanup_for_tail_return` has called since
/// B-2026-07-22-2. Same rule, two sites, one channel missing at one of them.
///
/// THE THREE CONTROLS ARE WHAT NAME THE MECHANISM, and each is pinned below:
/// the `return` spelling of the identical arm was already correct (it routes
/// through the function sibling), and the arm WITHOUT the rebind was too (a
/// payload bound out of an owned-param scrutinee registers memory-only under
/// caller-retains, so there is no `UserDrop` to retract). It is specifically
/// the rebind — which creates a genuinely owned local — combined with the
/// block tail that hands it out.
///
/// Memory stays balanced on both sides of this fix (`suppress_user_drop_for_var`
/// frees nothing), so neither the ASAN corpus nor the parity oracle's memory
/// half can see this class. The output pin is the only signal.
#[test]
fn test_e2e_rebound_arm_payload_as_block_tail_drops_once() {
    let hdr = "struct R { id: i64 }\n\
                   impl Drop for R {\n\
                   \x20   fn drop(mut ref self) { println(f\"dR{self.id}\") }\n\
                   }\n\
                   enum Box2 { Full(R), Empty }\n";
    // The defect: rebind + block tail, over a user value enum.
    assert_eq!(
            run_program(&format!(
                "{hdr}\
                 fn take(o: Box2) -> R {{\n\
                 \x20   match o {{ Box2.Full(r) => {{ let m = r; m }} Box2.Empty => {{ R {{ id: 0 }} }} }}\n\
                 }}\n\
                 fn main() {{\n\
                 \x20   let o: Box2 = Box2.Full(R {{ id: 1 }});\n\
                 \x20   let k = take(o);\n\
                 \x20   println(f\"C k={{k.id}}\");\n\
                 }}"
            ))
            .as_deref(),
            Some("C k=1\ndR1\n")
        );
    // `Option` carries it identically — the suppressor is keyed on the tail
    // being an Identifier, not on the scrutinee's type.
    assert_eq!(
        run_program(
            "struct R { id: i64 }\n\
                 impl Drop for R {\n\
                 \x20   fn drop(mut ref self) { println(f\"dR{self.id}\") }\n\
                 }\n\
                 fn take(o: Option[R]) -> R {\n\
                 \x20   match o { Some(r) => { let m = r; m } None => { R { id: 0 } } }\n\
                 }\n\
                 fn main() {\n\
                 \x20   let o: Option[R] = Some(R { id: 1 });\n\
                 \x20   let k = take(o);\n\
                 \x20   println(f\"C k={k.id}\");\n\
                 }"
        )
        .as_deref(),
        Some("C k=1\ndR1\n")
    );
    // CONTROL — the `return` spelling was already correct and must stay so.
    assert_eq!(
        run_program(&format!(
            "{hdr}\
                 fn take(o: Box2) -> R {{\n\
                 \x20   match o {{\n\
                 \x20       Box2.Full(r) => {{ let m = r; return m; }}\n\
                 \x20       Box2.Empty => {{ return R {{ id: 0 }}; }}\n\
                 \x20   }}\n\
                 }}\n\
                 fn main() {{\n\
                 \x20   let o: Box2 = Box2.Full(R {{ id: 1 }});\n\
                 \x20   let k = take(o);\n\
                 \x20   println(f\"C k={{k.id}}\");\n\
                 }}"
        ))
        .as_deref(),
        Some("C k=1\ndR1\n")
    );
    // CONTROL — no rebind. The payload binding itself has no `UserDrop`
    // under caller-retains, so this shape never had the defect and the new
    // call must not disturb it.
    assert_eq!(
        run_program(&format!(
            "{hdr}\
                 fn take(o: Box2) -> R {{\n\
                 \x20   match o {{ Box2.Full(r) => {{ r }} Box2.Empty => {{ R {{ id: 0 }} }} }}\n\
                 }}\n\
                 fn main() {{\n\
                 \x20   let o: Box2 = Box2.Full(R {{ id: 1 }});\n\
                 \x20   let k = take(o);\n\
                 \x20   println(f\"C k={{k.id}}\");\n\
                 }}"
        ))
        .as_deref(),
        Some("C k=1\ndR1\n")
    );
    // GUARD RAIL, and the one that matters: a BRANCH LEAF naming an OUTER
    // binding must keep its body on the path where it dies in place.
    //
    // This is not a hypothetical. The first version of this fix called
    // `suppress_user_drop_for_var`, which walks EVERY live frame, and it
    // silenced `b` here — `k=1 / dR1 / end` against the correct
    // `dR2 / k=1 / dR1 / end` the interpreter prints, a fresh divergence
    // introduced by the fix. Only the top-frame retraction is correct, and
    // this assertion is what distinguishes the two.
    //
    // `a` and `b` live in an ENCLOSING frame, so they are B-2026-08-28-51's
    // runtime per-path flag's business, not this static site's.
    assert_eq!(
        run_program(&format!(
            "{hdr}\
                 fn main() {{\n\
                 \x20   let a = R {{ id: 1 }};\n\
                 \x20   let b = R {{ id: 2 }};\n\
                 \x20   let k = if a.id > 0 {{ a }} else {{ b }};\n\
                 \x20   println(f\"k={{k.id}}\");\n\
                 \x20   println(\"end\");\n\
                 }}"
        ))
        .as_deref(),
        Some("dR2\nk=1\ndR1\nend\n")
    );
    // GUARD RAIL — a block-local that does NOT escape still fires inside
    // the block. The retraction must key on the tail, not on locality.
    assert_eq!(
        run_program(&format!(
            "{hdr}\
                 fn main() {{\n\
                 \x20   let v = {{ let t = R {{ id: 3 }}; t.id }};\n\
                 \x20   println(f\"v={{v}}\");\n\
                 }}"
        ))
        .as_deref(),
        Some("dR3\nv=3\n")
    );
}

#[test]
fn test_e2e_match_arm_guard() {
    // B-2026-07-12-9 — a `match` arm GUARD (`pat if cond => ..`) was SILENTLY
    // IGNORED under codegen: the arm fired whenever its pattern matched,
    // regardless of the `if` condition, so the first pattern-matching arm
    // won even when its guard was false. Now the guard is evaluated after
    // the pattern binds and the arm falls through to the next arm's test on
    // guard-false (Rust semantics). Covers: (a) scalar binding guards
    // (`x if x > N`), (b) an enum-pattern guard (`Some(x) if x > 5`), and
    // (c) a guard that reads a heap payload binding (`Some(s) if s.len() > 3`)
    // — the guard-false edge must not double-free or leak the aliased
    // payload (valgrind-clean; the source retains ownership).
    if let Some(out) = run_program(
            "fn classify(n: i64) -> String {\n\
                 match n { 0 => f\"zero\", x if x < 0 => f\"neg\", x if x < 10 => f\"small\", _ => f\"big\" }\n\
             }\n\
             fn g(o: Option[i64]) -> i64 { match o { Some(x) if x > 5 => 1, Some(_) => 2, None => 3 } }\n\
             fn h(n: i64) -> i64 { match n { x if x > 100 => 1, x if x > 10 => 2, x => x } }\n\
             fn pick(o: Option[String]) -> String {\n\
                 match o { Some(s) if s.len() > 3 => s, Some(_) => f\"short\", None => f\"none\" }\n\
             }\n\
             fn main() {\n\
                 println(classify(0));\n\
                 println(classify(0 - 3));\n\
                 println(classify(5));\n\
                 println(classify(100));\n\
                 println(f\"{g(Some(10))} {g(Some(2))} {g(None)}\");\n\
                 println(f\"{h(500)} {h(50)} {h(5)}\");\n\
                 println(pick(Some(f\"hello\")));\n\
                 println(pick(Some(f\"hi\")));\n\
             }",
        ) {
            assert_eq!(
                out,
                "zero\nneg\nsmall\nbig\n1 2 3\n1 2 5\nhello\nshort\n"
            );
        }
}

#[test]
fn test_e2e_match_tuple_pattern_discriminated() {
    // B-2026-07-12-13 — a `match` on a TUPLE scrutinee was not DISCRIMINATED
    // under codegen: the tuple pattern fell through to the catch-all
    // always-match, so the FIRST tuple-pattern arm always fired regardless
    // of the element values (`match (a,b) { (0,0)=>10, (1,2)=>20, _=>30 }`
    // returned 10 for every input). Now each element emits its own equality
    // test, AND'd together. Covers: i64 tuples, bool tuples, mixed
    // binding+literal elements (`(0, y)` / `(x, 0)`), and a nested tuple
    // (`(0, (1, 2))`) whose inner elements are tested recursively.
    if let Some(out) = run_program(
        "fn f(a: i64, b: i64) -> i64 { match (a, b) { (0, 0) => 10, (1, 2) => 20, _ => 30 } }\n\
             fn bt(a: bool, b: bool) -> i64 { match (a, b) { (true, true) => 1, _ => 0 } }\n\
             fn mixed(a: i64, b: i64) -> i64 {\n\
                 match (a, b) { (0, y) => y + 100, (x, 0) => x + 200, (1, 2) => 20, _ => 30 }\n\
             }\n\
             fn nested(a: i64, b: i64, c: i64) -> i64 {\n\
                 match (a, (b, c)) { (0, (1, 2)) => 1, (0, (_, _)) => 2, _ => 3 }\n\
             }\n\
             fn main() {\n\
                 println(f\"{f(0, 0)} {f(1, 2)} {f(5, 5)}\");\n\
                 println(f\"{bt(true, true)} {bt(true, false)} {bt(false, false)}\");\n\
                 println(f\"{mixed(0, 5)} {mixed(7, 0)} {mixed(1, 2)} {mixed(9, 9)}\");\n\
                 println(f\"{nested(0, 1, 2)} {nested(0, 4, 5)} {nested(8, 1, 2)}\");\n\
             }",
    ) {
        assert_eq!(out, "10 20 30\n1 0 0\n105 207 20 30\n1 2 3\n");
    }
}

#[test]
fn test_e2e_match_variant_name_shared_across_enums_resolves_to_scrutinee() {
    // #39 (phase-12 self-hosting, parser stage): a bare variant name that
    // exists in MORE THAN ONE enum (`Float` in both a value `Tok` and a
    // shared `Expr`) was resolved against whichever enum the unordered
    // `enum_layouts` map yielded first, not the match scrutinee's own enum.
    // Two distinct failures, both fixed by pinning resolution to the
    // scrutinee's enum (`match_scrutinee_enum_hint`):
    //   (a) value-enum match — the arm's TAG came from the wrong enum, so
    //       a `match tok { Float(v, s) => … }` never matched (tags differ:
    //       `Tok.Float` is variant 0, `Expr.Float` is variant 2) and fell
    //       through to the wrong arm / default.
    //   (b) shared-enum match — the bound payload's WORD OFFSETS came from
    //       the wrong enum. `Tok.Float` is a 2-field `(f64, String)` whose
    //       first slot is 1 word; binding `Expr.Float(n)` (one whole
    //       multi-word `FLit` struct) off that layout read a single word
    //       and reconstructed `n` with a garbage `Sp` pointer → SIGSEGV
    //       reading `n.span.offset`. This is the parser's
    //       `Token.Float`/`Expr.Float` shape exactly.
    if let Some(out) = run_program(
            "struct Sp { line: i64, column: i64, offset: i64, length: i64 }\n\
             struct FLit { value: f64, suffix: String, span: Sp }\n\
             enum Tok { Float(f64, String), Int(i64, String) }\n\
             shared enum Expr { Int(i64), Boolish(i64), Float(FLit) }\n\
             fn tok_kind(t: Tok) -> i64 {\n\
                 match t {\n\
                     Float(v, s) => 100,\n\
                     Int(v, s) => 200,\n\
                 }\n\
             }\n\
             fn expr_off(e: Expr) -> i64 {\n\
                 match e {\n\
                     Int(v) => v,\n\
                     Boolish(v) => v,\n\
                     Float(n) => n.span.offset,\n\
                 }\n\
             }\n\
             fn main() {\n\
                 let t = Tok.Float(1.5, \"f64\");\n\
                 println(tok_kind(t).to_string());\n\
                 let n = FLit { value: 2.5, suffix: \"\", span: Sp { line: 1, column: 1, offset: 7, length: 3 } };\n\
                 let e = Expr.Float(n);\n\
                 println(expr_off(e).to_string());\n\
             }",
        ) {
            assert_eq!(out, "100\n7\n");
        }
}

/// B-2026-09-01-42 — THE `while let` LOOP-EXIT MISS NEVER DROPPED THE
/// SCRUTINEE TEMPORARY THAT ENDED THE LOOP, on all four surfaces.
///
///     let mut i: i64 = 0;
///     while let E.A(r) = mk(i) { println(f"v{r.id}"); i = i + 1; }
///
///     was:      v0 dE after      <- two temporaries, ONE body
///     correct:  v0 dE dE after
///
/// design.md § `if let` and `let...else` > "Scrutinee temporary scope",
/// fourth bullet: "After the loop terminates (the pattern stopped
/// matching), the final iteration's scrutinee temporaries are already
/// dropped." They were not dropped at all.
///
/// WHY IT SURVIVED EVERY GATE, and why this fixture is the only thing that
/// can hold it: all four surfaces AGREED on the wrong answer, so the A/B
/// parity rule reported green, and the missing body is a lost SIDE EFFECT
/// rather than a leak of memory — the temporary's storage was always
/// reclaimed (measured: 11 allocs / 11 frees, 0 valgrind errors, before
/// the fix as well as after), so no valgrind/LSan check fired either. Only
/// an absolute expected-output assertion catches it.
///
/// THE DEFECT WAS THE MEMORY/BODIES SPLIT AGAIN.
/// `drop_freshtemp_enum_scrutinee_on_miss` emitted only the payload-walking
/// drop SWITCH — the memory half — so the enum's own user `Drop` body and
/// its payload's bodies never ran. It also returned early when
/// `emit_enum_drop_switch` answered `None`, which is the case for an enum
/// with no heap-bearing variant, so the row's own repro (all-scalar
/// payload) registered nothing whatsoever.
///
/// THE ORDER IS MEASURED, NOT ASSUMED. A bound local of the very value
/// that ends the loop prints `dB dW7` — the enum's own body, then the
/// payload's — on every surface. That transcript is the oracle this
/// fixture asserts the exit temporary against, which is why the
/// `payload-carrying enum` row spells both bodies out in that order.
///
/// THE ROW LEFT THREE SHAPES UNMEASURED AND ALL THREE ARE ANSWERED HERE:
/// the enum STRUCT-variant flavour was broken the same way and is fixed;
/// `break` out of the body was already correct (it creates no further
/// temporary) and is pinned as a control so the fix cannot start
/// over-firing on it; and a `Result` scrutinee is NOT fixed by this — see
/// the pinned row at the end.
#[test]
fn e2e_while_let_loop_exit_drops_the_temporary_that_ended_it() {
    const PRELUDE: &str = "struct R { id: i64 }\n\
             struct H { s: String }\n\
             enum E { A(R), B }\n\
             impl Drop for E { fn drop(mut ref self) { println(\"dE\") } }\n\
             enum Eh { A(H), B(H) }\n\
             impl Drop for Eh { fn drop(mut ref self) { println(\"dEH\") } }\n\
             enum Es { A { r: R }, B }\n\
             impl Drop for Es { fn drop(mut ref self) { println(\"dES\") } }\n\
             struct W { id: i64 }\n\
             impl Drop for W { fn drop(mut ref self) { println(f\"dW{self.id}\") } }\n\
             enum Bx { Full(W), Empty(W) }\n\
             impl Drop for Bx { fn drop(mut ref self) { println(\"dB\") } }\n\
             fn pad(n: i64) -> String { return f\"payload-{n}-aaaaaaaaaaaaaaaaaaaaaaaaaaaa\"; }\n\
             fn mk(n: i64) -> E { if n < 1 { return E.A(R { id: n }) } return E.B }\n\
             fn mkh(n: i64) -> Eh { if n < 1 { return Eh.A(H { s: pad(n) }) } return Eh.B(H { s: pad(7) }) }\n\
             fn mks(n: i64) -> Es { if n < 1 { return Es.A { r: R { id: n } } } return Es.B }\n\
             fn mkb(n: i64) -> Bx { if n < 1 { return Bx.Full(W { id: n }) } return Bx.Empty(W { id: 7 }) }\n";
    let cases: &[(&str, &str, &str)] = &[
        (
            "all-scalar payload — the row's own repro [B-2026-09-01-42]",
            "let mut i: i64 = 0;\n \
                 while let E.A(r) = mk(i) { println(f\"v{r.id}\"); i = i + 1; }",
            "v0\ndE\ndE\nafter\n",
        ),
        (
            "HEAP-carrying payload [B-2026-09-01-42]",
            "let mut i: i64 = 0;\n \
                 while let Eh.A(h) = mkh(i) { println(f\"v{h.s.len()}\"); i = i + 1; }",
            "v38\ndEH\ndEH\nafter\n",
        ),
        (
            "enum STRUCT-variant scrutinee [B-2026-09-01-42, row: NOT MEASURED]",
            "let mut i: i64 = 0;\n \
                 while let Es.A { r } = mks(i) { println(f\"v{r.id}\"); i = i + 1; }",
            "v0\ndES\ndES\nafter\n",
        ),
        (
            // The oracle: a bound local of the exit value prints `dB dW7`
            // on every surface, so the exit temporary must too.
            "payload-carrying enum — own body THEN payload body [B-2026-09-01-42]",
            "let mut i: i64 = 0;\n \
                 while let Bx.Full(w) = mkb(i) { println(f\"v{w.id}\"); i = i + 1; }",
            "v0\ndW0\ndB\ndB\ndW7\nafter\n",
        ),
        (
            // THE ORACLE ITSELF, asserted so the row above cannot drift
            // away from what a plain binding of the same value produces.
            // `x`'s NLL last use is its own `let`, so both bodies land
            // before the marker.
            "control: a bound local of the exit value is the oracle",
            "let x = mkb(1);\n println(\"mid\");",
            "dB\ndW7\nmid\nafter\n",
        ),
        (
            "control: `break` creates no further temporary [row: NOT MEASURED]",
            "let mut i: i64 = 0;\n \
                 while let E.A(r) = mk3(i) {\n \
                 println(f\"v{r.id}\");\n \
                 if r.id == 1 { break; }\n \
                 i = i + 1;\n \
                 }",
            "v0\ndE\nv1\ndE\nafter\n",
        ),
        (
            "control: the `if let` miss, correct throughout",
            "if let E.A(r) = mk(9) { println(f\"v{r.id}\") }",
            "dE\nafter\n",
        ),
        (
            "control: a loop that never matches drops its one temporary",
            "while let E.A(r) = mk(9) { println(f\"v{r.id}\") }",
            "dE\nafter\n",
        ),
    ];
    for (label, body, want) in cases {
        let src = format!(
            "{PRELUDE}\
                 fn mk3(n: i64) -> E {{ if n < 3 {{ return E.A(R {{ id: n }}) }} return E.B }}\n\
                 fn main() {{\n    {body}\n    println(\"after\");\n}}\n"
        );
        let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(&src);
        assert!(
            interp_errs.is_empty(),
            "{label}: interpreter errored: {interp_errs:?}"
        );
        assert_eq!(interp_out.join(""), *want, "{label}: interpreter");
        if let Some(aot) = run_program(&src) {
            assert_eq!(aot, *want, "{label}: run and build must agree");
        }
    }
}

#[test]
fn e2e_freshtemp_enum_scrutinee_while_let_runs_user_drop_per_iter() {
    // The while-let leg: the user Drop fires once per matched iteration.
    //
    // B-2026-09-01-42 — AND ONCE FOR THE TEMPORARY THAT ENDS THE LOOP,
    // which is the fourth `DROP` below. `next` is called FOUR times
    // (i = 0, 1, 2 yield; i = 3 stops), so four temporaries exist and four
    // bodies are due. This fixture asserted three: it was pinning the
    // loop-exit residual as expected output, which is what made that
    // residual look intentional. design.md § `if let` and `let...else` >
    // "Scrutinee temporary scope", fourth bullet, requires the last one to
    // be dropped too.
    if let Some(out) = run_program(
        "enum Step { Yield(i64), Stop }\n\
             impl Drop for Step { fn drop(mut ref self) { println(\"DROP\"); } }\n\
             fn next(i: i64) -> Step { if i < 3 { Step.Yield(i) } else { Step.Stop } }\n\
             fn main() {\n\
                 let mut i: i64 = 0;\n\
                 while let Step.Yield(v) = next(i) { println(f\"I {v}\"); i = i + 1; }\n\
                 println(\"DONE\");\n\
             }",
    ) {
        assert_eq!(out, "I 0\nDROP\nI 1\nDROP\nI 2\nDROP\nDROP\nDONE\n");
    }
}

#[test]
fn e2e_vecdeque_payload_in_option_match_codegen() {
    // B-2026-06-10-3 general fix: a VecDeque payload bound out of an
    // Option/Result via `match` was reconstructed with the 1-word default
    // (the payload word-count/type gates handled `Vec`/`String` but not
    // `VecDeque`) → malformed value, SIGTRAP at the binding's scope-exit
    // free. VecDeque shares Vec's 3-word layout; the binding now dispatches
    // and frees cleanly.
    if let Some(out) = run_program(
            "fn mk() -> VecDeque[i64] {\n\
                 let mut q: VecDeque[i64] = VecDeque.new();\n\
                 q.push_back(5_i64);\n\
                 q.push_back(6_i64);\n\
                 q\n\
             }\n\
             fn main() {\n\
                 let o: Option[VecDeque[i64]] = Some(mk());\n\
                 match o { Some(v) => { println(v.len()); println(v[0]); } None => println(\"n\") }\n\
             }",
        ) {
            assert_eq!(out, "2\n5\n");
        }
}

#[test]
fn test_e2e_borrow_return_match_multi_source() {
    // Tier sibling of the `if` arm (B-2026-06-07-5): a `match` over a
    // scalar selector returns a borrow from whichever arm runs — per-arm
    // pointer phi'd at the merge, deref'd correctly at the let-bound
    // caller. Exercises literal arms + the wildcard catch-all.
    let out = run_program(
        "fn pick(a: ref String, b: ref String, c: ref String, which: i32) -> ref String {\n\
             \x20   match which {\n\
             \x20       0 => a,\n\
             \x20       1 => b,\n\
             \x20       _ => c,\n\
             \x20   }\n\
             }\n\
             fn main() {\n\
             \x20   let x = \"alpha\"; let y = \"beta\"; let z = \"gamma\";\n\
             \x20   let p0 = pick(x, y, z, 0); println(p0);\n\
             \x20   let p1 = pick(x, y, z, 1); println(p1);\n\
             \x20   let p2 = pick(x, y, z, 2); println(p2);\n\
             }\n",
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "alpha\nbeta\ngamma");
    }
}

#[test]
fn test_e2e_borrow_return_match_field_of_ref() {
    // `match` arms returning a field reached through a `ref` param —
    // the per-arm borrow pointer is a struct-GEP, not a forwarded param.
    let out = run_program(
        "struct Pair { first: String, second: String }\n\
             fn choose(p: ref Pair, which: i32) -> ref String {\n\
             \x20   match which {\n\
             \x20       0 => p.first,\n\
             \x20       _ => p.second,\n\
             \x20   }\n\
             }\n\
             fn main() {\n\
             \x20   let pr = Pair { first: \"L\", second: \"R\" };\n\
             \x20   let a = choose(pr, 0); println(a);\n\
             \x20   let b = choose(pr, 9); println(b);\n\
             }\n",
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "L\nR");
    }
}

#[test]
fn test_e2e_borrow_return_match_tuple_variant() {
    // Tier 2c (B-2026-06-07-5): borrow-return `match` over a non-scalar
    // identifier scrutinee with a binding-free tuple-variant arm
    // (`Ok(_)`) + wildcard catch-all. Per-arm pointer phi'd at the merge;
    // the enum tag drives arm selection (verified both branches).
    let out = run_program(
        "fn pick(res: ref Result[i64, String], a: ref String, b: ref String) -> ref String {\n\
             \x20   match res {\n\
             \x20       Ok(_) => a,\n\
             \x20       _ => b,\n\
             \x20   }\n\
             }\n\
             fn main() {\n\
             \x20   let ok: Result[i64, String] = Result.Ok(1);\n\
             \x20   let er: Result[i64, String] = Result.Err(\"e\");\n\
             \x20   let x = \"yes\"; let y = \"no\";\n\
             \x20   println(pick(ok, x, y));\n\
             \x20   println(pick(er, x, y));\n\
             }\n",
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "yes\nno");
    }
}

#[test]
fn test_e2e_borrow_return_match_dotted_unit_variant() {
    // Tier 2c: dotted unit-variant arms (`Side.Left`/`Side.Right`) over an
    // enum identifier scrutinee, returning a field reached through a `ref`
    // param. Exercises the no-`_`-catch-all exhaustive enum form (the
    // trailing block is unreachable because the value is always Left/Right).
    let out = run_program(
        "enum Side { Left, Right }\n\
             struct Pair { first: String, second: String }\n\
             fn choose(s: ref Side, p: ref Pair) -> ref String {\n\
             \x20   match s {\n\
             \x20       Side.Left => p.first,\n\
             \x20       Side.Right => p.second,\n\
             \x20   }\n\
             }\n\
             fn main() {\n\
             \x20   let pr = Pair { first: \"FST\", second: \"SND\" };\n\
             \x20   println(choose(Side.Left, pr));\n\
             \x20   println(choose(Side.Right, pr));\n\
             }\n",
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "FST\nSND");
    }
}

/// gap-d: same for a `match` with a diverging arm — the single live arm's
/// value flows through (one-incoming phi), and the struct-returning fn
/// compiles (proving the `unreachable()` arm emitted a terminator, not a
/// `ret` mismatch).
#[test]
fn test_e2e_diverge_match_arm_in_struct_returning_fn() {
    let out = run_program(
        "struct Point { x: i64 }\n\
             fn make(n: i64) -> Point {\n\
             \x20   match n {\n\
             \x20       0 => Point { x: 42 },\n\
             \x20       _ => unreachable(),\n\
             \x20   }\n\
             }\n\
             fn main() { let p = make(0); println(p.x); }",
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "42");
    }
}

#[test]
fn test_e2e_match_integer() {
    let out = run_program(
        r#"
fn classify(n: i64) -> i64 {
    match n {
        0 => 0,
        1 => 1,
        _ => 2,
    }
}
fn main() {
    println(classify(0));
    println(classify(1));
    println(classify(42));
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["0", "1", "2"]);
    }
}

#[test]
fn test_e2e_match_at_binding_variant_inner() {
    // `whole @ Some(x)` must bind BOTH the inner payload `x` and the
    // outer alias `whole` (the entire Option), and the variant tag
    // condition must still select Some vs None. `whole` is forwarded
    // to a function taking `Option[i64]` to prove the alias holds the
    // whole scrutinee value, not garbage.
    let out = run_program(
        r#"
fn unwrap_or(o: Option[i64], d: i64) -> i64 {
    match o {
        Some(v) => v,
        None => d,
    }
}
fn describe(o: Option[i64]) -> i64 {
    match o {
        whole @ Some(x) => x + unwrap_or(whole, 0),
        None => -1,
    }
}
fn main() {
    println(describe(Some(7)));  // x=7, whole=Some(7) → 7 + 7 = 14
    println(describe(None));     // None arm → -1
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["14", "-1"]);
    }
}

#[test]
fn test_e2e_match_at_binding_struct_alias_field_access() {
    // Pre-existing silent-zero gap surfaced by the slice-4 E2E
    // work: the `@` alias's surface type was never recorded
    // (`check_pattern_against`'s AtBinding arm skipped the
    // `pattern_binding_types` write the Binding leaf arm does),
    // so `x.n` on the alias compiled against an unknown type and
    // read 0. Copy fields keep this clear of the
    // double-consume rejection.
    let out = run_program(
        r#"
struct Foo { a: i64, n: i64 }
fn main() {
    let foo = Foo { a: 1, n: 7 };
    match foo {
        x @ Foo { a, n } => {
            println(a);
            println(n);
            println(x.n);
        }
    }
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["1", "7", "7"]);
    }
}

#[test]
fn test_e2e_match_ref_at_binding_struct_heap_field() {
    // `ref x @ Foo { a, n }` under an OWNED scrutinee (design.md
    // § @ Bindings, "Explicit `ref` on the `@` binding"): the whole
    // subtree borrows — `a` reads the String without taking
    // ownership, `x` aliases the whole struct, and `foo` stays
    // live (and droppable exactly once) after the match. The
    // by_ref bind path suppresses the pattern bindings' heap
    // cleanup registration (`pattern_binding_is_borrow`) while
    // `pattern_consumes_field` keeps the source's drop intact —
    // a double-free here means one of the two halves regressed.
    let out = run_program(
        r#"
struct Foo { a: String, n: i64 }
fn main() {
    let foo = Foo { a: "hi", n: 7 };
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
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["hi", "7", "7", "hi"]);
    }
}

#[test]
fn test_e2e_match_at_binding_or() {
    // `@` + or-pattern composition: the alias binds from the first
    // alternative and the condition ORs each alternative's test.
    let out = run_program(
        r#"
fn f(n: i64) -> i64 {
    match n {
        x @ 1 | x @ 2 => x * 100,
        other => other,
    }
}
fn main() {
    println(f(1));  // matches → x=1 → 100
    println(f(2));  // matches → x=2 → 200
    println(f(9));  // catch-all → 9
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["100", "200", "9"]);
    }
}

#[test]
fn test_e2e_match_at_binding_parenthesized_or() {
    // B-2026-07-23-19: the GROUPED form `x @ (1 | 2 | 3)` (vs the
    // distributed `x @ 1 | x @ 2` above). Pre-fix the parser read
    // `(1 | 2 | 3)` as a 1-element tuple `Tuple([Or(...)])`, which can't
    // match a scalar — codegen over-matched (always-true tuple-on-scalar
    // branch, so `9` wrongly hit the arm) and the interpreter under-matched
    // (a tuple pattern never matches a scalar, so `2` fell through). The
    // parser now treats `(P)` as a grouping, so both backends agree.
    let out = run_program(
        r#"
fn f(n: i64) -> i64 {
    match n {
        x @ (1 | 2 | 3) => x * 100,
        other => other,
    }
}
fn main() {
    println(f(2));  // grouped or matches → 200
    println(f(9));  // must NOT match the grouped or → 9
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["200", "9"]);
    }
}

#[test]
fn test_e2e_match_top_level_parenthesized_or() {
    // The `@`-free grouped or-pattern `(1 | 2)` must also match by value,
    // not fall through the (pre-fix) tuple-on-scalar always-true branch.
    let out = run_program(
        r#"
fn f(n: i64) -> i64 {
    match n {
        (1 | 2) => 10,
        _ => 20,
    }
}
fn main() {
    println(f(1));
    println(f(2));
    println(f(3));
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["10", "10", "20"]);
    }
}

#[test]
fn test_e2e_match_byte_literal() {
    // Byte-literal patterns (`b'I'`) desugar to integer patterns with
    // a U8 suffix; previously the parser rejected them outright.
    let out = run_program(
        r#"
fn value(b: u8) -> i64 {
    match b {
        b'I' => 1,
        b'V' => 5,
        b'X' => 10,
        _ => 0,
    }
}
fn main() {
    println(value(b'I'));
    println(value(b'X'));
    println(value(b'?'));
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["1", "10", "0"]);
    }
}

#[test]
fn test_e2e_if_let_shared_binding_survives_field_displacement() {
    // Regression for the kata-#24 (swap-nodes-in-pairs) UAF
    // (2026-06-07): an `if let Some(second) = first.next` binding
    // was a NON-retained alias of the field's payload, so the
    // store `first.next = second.next` — which releases the
    // field's (only) ref to that node — freed it while the binding
    // was still live; the following read printed freed memory.
    // `bind_pattern_values` now gives pattern-bound shared aliases
    // their own +1 (`emit_refcount_inc` + `track_rc_var`, the
    // pattern sibling of the let-path receive-inc), drained at the
    // binding scope's end. Pre-fix: prints 3 (the freed chunk's
    // neighbor) instead of 2.
    let out = run_program(
        r#"
shared struct ListNode { val: i64, mut next: Option[ListNode] }
fn from3() -> Option[ListNode] {
    let head = ListNode { val: 1, next: None };
    let n2 = ListNode { val: 2, next: None };
    let n3 = ListNode { val: 3, next: None };
    n2.next = Some(n3);
    head.next = Some(n2);
    Some(head)
}
fn poke(head: Option[ListNode]) {
    if let Some(first) = head {
        if let Some(second) = first.next {
            first.next = second.next;
            println(second.val);
        }
    }
}
fn main() {
    poke(from3());
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "2");
    }
}

#[test]
fn test_ir_bug8_match_tail_call_rhs_no_double_inc() {
    // Parallel coverage for the `Match` tail-shape case. Every
    // arm body is a `Call` returning +1 — the receive site must
    // recurse into the arms and suppress the inc when ALL arms
    // are fresh-ref sources. Same expected counts as the if-case.
    let ir = ir_for(
        r#"
shared struct S { val: i64 }
fn make_a() -> S { let s = S { val: 1 }; s }
fn make_b() -> S { let s = S { val: 2 }; s }
fn use_it(tag: i64) {
    let x = match tag {
        0 => make_a(),
        _ => make_b(),
    };
}
"#,
    );
    let inc_count = ir.matches("add i64 %rc").count();
    let dec_count = ir.matches("sub i64 %rc").count();
    assert_eq!(
        inc_count, 2,
        "match-tail Call RHS must not double-inc; expected 2 \
             callee-side move-out incs only. Found {} in:\n{}",
        inc_count, ir
    );
    assert_eq!(
        dec_count, 3,
        "expected 3 rc_decs; found {} in:\n{}",
        dec_count, ir
    );
}

/// B-2026-08-27-43 — the ARM-LOCAL half of the branch-leaf family, split
/// out of B-2026-08-27-34 and unchanged in either direction by its fix.
///
/// The delta from the sibling test above is one line per leg: the arm binds
/// a LOCAL and hands the local out (`if c { let u = make(1); u }`) instead
/// of handing out a binding from the enclosing scope. That moves the
/// failure boundary from the THIRD consumption to the FIRST: there the leaf
/// retain survived and the box reached the uses at rc 2, paying for two of
/// them; here the arm-local's own scope-exit dec drains with the arm's frame
/// and cancels the retain, so the box arrives at rc 1 with nobody owning it
/// and the first callee's param dec already frees it.
///
/// Both halves were individually right, which is why the sibling fix moved
/// nothing: the leaf hook fires, and case (g)'s `Identifier` arm correctly
/// DECLINES to register a binding whose retain was cancelled — it reads
/// `var_option_shared_heap`, and `compile_block_with_frame` reverted the
/// arm's name env before the `let` classified its RHS, so the name is
/// simply gone. The fix gives case (g) a second source of truth that
/// survives the revert: the emission RECORD each leaf retain writes,
/// keyed by the leaf expression's span.
///
/// Eleven arm-local legs plus the enclosing-binding control. Every leg
/// consumes its ONE binding three times (four on leg 4, five on leg 5), so
/// no count can be balanced by accident. Measured against the unfixed
/// compiler: legs 1-11 all RED and all exiting 0 — silent wrong answers,
/// `/ 0 0` and garbage words — and leg 12 GREEN, which is what attributes
/// the failure to the arm-LOCAL shape rather than to branch leaves at large.
///
/// Leg 4 is load-bearing beyond the reported shape: the arm-local is itself
/// an ALIAS of an enclosing binding (`let u = d; u`), so `d` must still read
/// correctly AFTER the branch's value has been consumed three times. It
/// pins that the escaping `+1` the new binding adopts is the arm-local's
/// own, not the source's.
///
/// Twinned against the interpreter, which was correct throughout.
#[test]
fn option_shared_from_an_arm_local_leaf_survives_repeated_consumption() {
    let src = "shared struct Node { val: i64 }\n\
                   fn make(n: i64) -> Option[Node] { return Some(Node { val: n }); }\n\
                   fn show(t: Option[Node]) -> i64 {\n\
                       match t { None => { return 0; } Some(n) => { return n.val; } }\n\
                   }\n\
                   fn main() {\n\
                       let b = make(2);\n\
                       let t1 = if true { let u = make(1); u } else { b };\n\
                       println(f\"{show(t1)} {show(t1)} {show(t1)}\");\n\
                       let c = make(3);\n\
                       let t2 = if false { c } else { let u = make(4); u };\n\
                       println(f\"{show(t2)} {show(t2)} {show(t2)}\");\n\
                       let t3 = if true { let u = make(5); u } else { let v = make(6); v };\n\
                       println(f\"{show(t3)} {show(t3)} {show(t3)}\");\n\
                       let d = make(7);\n\
                       let t4 = if true { let u = d; u } else { b };\n\
                       println(f\"{show(t4)} {show(t4)} {show(t4)} {show(d)}\");\n\
                       let t5 = if true { let u = make(8); u } else { b };\n\
                       println(f\"{show(t5)} {show(t5)} {show(t5)} {show(t5)} {show(t5)}\");\n\
                       let k = 1;\n\
                       let t6 = match k { 1 => { let u = make(9); u } _ => { b } };\n\
                       println(f\"{show(t6)} {show(t6)} {show(t6)}\");\n\
                       let t7 = if true { { let u = make(10); u } } else { b };\n\
                       println(f\"{show(t7)} {show(t7)} {show(t7)}\");\n\
                       let t8 = if false { b } else if true { let u = make(11); u } else { b };\n\
                       println(f\"{show(t8)} {show(t8)} {show(t8)}\");\n\
                       let t9 = if true { let u = make(12); u } else { None };\n\
                       println(f\"{show(t9)} {show(t9)} {show(t9)}\");\n\
                       let src2 = make(13);\n\
                       let t10 = if let Some(n) = src2 { let u = make(n.val); u } else { b };\n\
                       println(f\"{show(t10)} {show(t10)} {show(t10)}\");\n\
                       let mut i = 0;\n\
                       while i < 3 {\n\
                           let t11 = if true { let u = make(20 + i); u } else { b };\n\
                           println(f\"{show(t11)} {show(t11)} {show(t11)}\");\n\
                           i = i + 1;\n\
                       }\n\
                       let t12 = if true { b } else { b };\n\
                       println(f\"{show(t12)} {show(t12)} {show(t12)}\");\n\
                   }";
    let expected = "1 1 1\n4 4 4\n5 5 5\n7 7 7 7\n8 8 8 8 8\n9 9 9\n\
                        10 10 10\n11 11 11\n12 12 12\n13 13 13\n20 20 20\n\
                        21 21 21\n22 22 22\n2 2 2\n";
    if let Some(c) = run_program_capturing(src) {
        assert_eq!(
            c.stdout, expected,
            "an ARM-LOCAL branch leaf must hand its `Option[shared]` to the \
                 binding with an owner, on the first consumption as on the third"
        );
        assert!(
            c.status.success(),
            "process died ({:?}) — stderr: {}",
            c.status,
            c.stderr
        );
    }
    let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
    assert!(
        interp_errs.is_empty(),
        "interpreter errored on the twin: {interp_errs:?}"
    );
    assert_eq!(
        interp_out.join(""),
        expected,
        "interpreter twin must agree with the compiled backend"
    );
}

#[test]
fn test_e2e_struct_pattern_destructure_heap_field() {
    // #16 (phase-12 self-hosting): a plain struct-pattern match destructure of
    // an OWNED local struct (`match v { S { a, b: _ } => … }`) moves each
    // consumed field's heap payload into the new binding. Without
    // `suppress_destructured_struct_pattern_cleanup` the source struct's drop
    // re-frees the moved buffer (double-free; exit 134 under guardmalloc / ASAN
    // — see `tests/memory_sanitizer.rs::asan_struct_pattern_destructure_no_
    // double_free`). Output correctness is the codegen-lane guard: the
    // partial-bind field (`b: _`) is left to the source drop, the bound fields
    // (including a nested-struct field and an enum field moved whole) read back
    // intact.
    let out = run_program(
        r#"
struct Inner { s: String }
enum Tok { Id(String), Eof }
struct S { a: String, b: String, inner: Inner, tok: Tok, n: i64 }
fn main() {
    let v = S {
        a: "aa".to_string(),
        b: "bb".to_string(),
        inner: Inner { s: "deep".to_string() },
        tok: Tok.Id("tok".to_string()),
        n: 7,
    };
    match v {
        S { a, b: _, inner, tok, n } => {
            let Inner { s } = inner;
            println(a);
            println(s);
            match tok { Id(t) => println(t), Eof => println("eof") }
            println(n.to_string());
        }
    }
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "aa\ndeep\ntok\n7");
    }
}

#[test]
fn test_e2e_match_some_node_let_destructure_tuple_payload() {
    // The kata's canonical BFS shape: `Some(node) => let (i, d) = node`
    // where `node: (i64, i64)` is reconstituted as a tuple struct
    // value from the multi-word Option payload. The typechecker
    // records the tuple `TypeExpr` in `pattern_binding_inner_types`
    // (tagged "Tuple" in `pattern_binding_types`); codegen's
    // `reconstruct_payload_value` Binding arm walks the recorded
    // element types and builds a tuple struct from `field_words`.
    let out = run_program(
        r#"
fn main() {
    let mut q: VecDeque[(i64, i64)] = VecDeque.new();
    q.push_back((3, 30));
    q.push_back((7, 70));
    loop {
        match q.pop_front() {
            None => { break; },
            Some(node) => {
                let (a, b) = node;
                println(a);
                println(b);
            },
        }
    }
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "3\n30\n7\n70");
    }
}

#[test]
fn test_e2e_byvalue_enum_param_wrapped_into_struct_and_destructured() {
    // #14: the bootstrap shape — an enum param moved into a returned struct
    // literal (`Spanned { tok: t, .. }`, mirroring the self-hosted lexer's
    // `make_spanned(token)`), then destructured by the caller. The flowed
    // copy and the caller's source must be independent.
    if let Some(out) = run_program(
        r#"
enum Token { Ident(String), Int(i64) }
struct Spanned { tok: Token, off: i64 }
fn make_spanned(t: Token, o: i64) -> Spanned { Spanned { tok: t, off: o } }
fn main() {
    let t = Token.Ident("hello".to_string());
    let s = make_spanned(t, 3);
    match s.tok {
        Ident(name) => println(name),
        Int(n) => println(n.to_string()),
    }
}
"#,
    ) {
        assert_eq!(out, "hello\n");
    }
}

#[test]
fn test_ir_while_let_emits_loop_not_noop() {
    // phase-6-runtime.md line 489: `while let` lowers to a real loop
    // (header re-tests the pattern, body runs on a match). Regression
    // against the prior silent no-op (fall-through to a constant `0`,
    // loop body dropped). Archive-independent — asserts on the IR.
    let src = r#"
fn pop(v: mut ref Vec[i64]) -> Option[i64] {
    if v.len() == 0 {
        return Option.None;
    }
    let last = v.len() - 1;
    let x = v[last];
    v.remove(last);
    return Option.Some(x);
}

fn main() {
    let mut v: Vec[i64] = Vec.new();
    v.push(10_i64);
    while let Some(x) = pop(mut v) {
        println(f"got {x}");
    }
}
"#;
    let ir = ir_for(src);
    assert!(
        ir.contains("whilelet.cond") && ir.contains("whilelet.body"),
        "expected while-let loop blocks in IR; got:\n{}",
        ir
    );
}

#[test]
fn test_ir_structpat_boxed_destructure_suppresses_box_fields() {
    // Slice 3t: `match o { Some(Holder { name, id }) => … }` over a named
    // boxed `Option[Holder]` binding — the consumed fields' caps are
    // zeroed INSIDE the box (`boxfld.suppress`) so the let-site
    // BoxedEnumDrop inner walk frees only unbound fields.
    //
    // B-2026-08-30-52 RETARGETED THIS ARM TO A CONSUMING ONE. It used to
    // read `println(name.len() + id)` — a READ-ONLY destructuring arm,
    // which is now classified a BORROW: the source keeps the payload and
    // frees it at scope exit, so there is no transfer and `boxfld.suppress`
    // is correctly absent. Both strategies free exactly once, so the
    // behaviour this test protects is unchanged; what moved is WHICH
    // mechanism a read-only arm uses, which is the row's whole subject.
    // `let owned = name;` restores a genuine consume, so the suppression is
    // required again and this test still guards the thing it was written
    // for. The read-only spelling is asserted just below.
    let src = r#"
struct Holder { name: String, id: i64 }
fn main() {
    let o: Option[Holder] = Some(Holder { name: "a heap string padded out beyond thirty-six bytes!", id: 1 });
    match o {
        Some(Holder { name, id }) => { let owned: String = name; println(owned.len() + id); },
        None => { println("missing"); },
    }
}
"#;
    let ir = ir_for(src);
    assert!(
        ir.contains("boxfld.suppress"),
        "expected the consumed-field cap-zero inside the boxed payload \
             (boxfld.suppress blocks); got:\n{}",
        ir
    );
}

#[test]
fn test_ir_let_wildcard_block_tail_temp_freed() {
    // Slice 5: `let _ = { make_vec() };` discards the block's tail temp
    // through the wildcard-`let` path. The early Wildcard arm routes the
    // peeled tail (`make_vec()`) through the chokepoint. The hint table is
    // keyed on the tail call's span, so a `Vec[String]` would recover its
    // element type — here a `Vec[i64]` only needs the outer-buffer free.
    let src = r#"
fn make_vec() -> Vec[i64] {
    let mut v: Vec[i64] = Vec.new();
    v.push(1_i64);
    return v;
}

fn main() {
    let _ = { make_vec() };
    println(0);
}
"#;
    let ir = ir_for(src);
    assert!(
        ir.contains("__owned_tmp"),
        "expected the wildcard-let block-tail Vec temp materialized; got:\n{}",
        ir
    );
    assert!(
        ir.contains("cleanup.free"),
        "expected a FreeVecBuffer drain for the wildcard-let discarded temp; got:\n{}",
        ir
    );
}

#[test]
fn test_ir_iflet_freshtemp_enum_unbound_field_freed() {
    // `if let Full(_, n) = make()` — the Vec field is `_` (unbound). Before
    // the fix the payload words were extracted with no `free` (IR-proven
    // leak). Now the temp is materialized and the enum drop walk frees the
    // unbound Vec.
    let src = format!(
        "{B_ENUM_PRELUDE}\nfn main() {{ if let Holder.Full(_, n) = make() {{ println(n); }} }}\n"
    );
    let ir = ir_for_with_ownership(&src);
    assert!(
        ir.contains("__freshtemp_enum_scrut"),
        "expected the fresh-temp enum scrutinee materialized; got:\n{ir}"
    );
    assert!(
        ir.contains("@__karac_drop_Holder("),
        "expected the enum drop walk to free the unbound Vec field; got:\n{ir}"
    );
}

#[test]
fn test_ir_match_freshtemp_enum_unbound_field_freed() {
    // `match make() { Full(_, n) => …, Empty => … }` — same leak, match
    // surface. Confirms the fresh-temp materialization fires in
    // compile_match (the bound-variable path already worked via the source
    // binding's EnumDrop).
    let src = format!(
            "{B_ENUM_PRELUDE}\nfn main() {{ match make() {{ Holder.Full(_, n) => println(n), Holder.Empty => println(0) }} }}\n"
        );
    let ir = ir_for_with_ownership(&src);
    assert!(
        ir.contains("__freshtemp_enum_scrut") && ir.contains("@__karac_drop_Holder("),
        "expected fresh-temp match scrutinee materialized + enum-dropped; got:\n{ir}"
    );
}

#[test]
fn test_ir_iflet_freshtemp_enum_bound_field_suppressed() {
    // `if let Full(v, n) = make()` — the Vec is MOVED into `v`. The
    // materialized temp gets an EnumDrop, but the suppression must zero the
    // moved field's payload words (`match.dest.suppress.wp` ← 0) so the drop
    // walk skips it — `v`'s own cleanup frees it exactly once (no
    // double-free). (B-2026-06-13-13 generalized the suppression from
    // cap-only to all payload words, so nested-struct payloads' inner caps
    // are zeroed too; the label changed from `match.dest.cap.suppress.p`.)
    let src = format!(
            "{B_ENUM_PRELUDE}\nfn main() {{ if let Holder.Full(v, n) = make() {{ println(v.len() + n); }} }}\n"
        );
    let ir = ir_for_with_ownership(&src);
    assert!(
        ir.contains("__freshtemp_enum_scrut"),
        "expected the fresh-temp enum scrutinee materialized; got:\n{ir}"
    );
    assert!(
        ir.contains("match.dest.suppress.wp"),
        "expected the moved Vec field's payload words zeroed (suppression) to \
             avoid a double-free against the enum drop walk; got:\n{ir}"
    );
}

#[test]
fn test_ir_iflet_place_scrutinee_not_materialized() {
    // Negative / double-free guard: a *place* scrutinee (a let-bound enum
    // `x`) already owns its `EnumDrop`. Materializing a second one for it
    // would double-free, so `materialize_freshtemp_enum_scrutinee` gates to
    // fresh Call/MethodCall temps only — `if let Full(_, n) = x` must NOT
    // emit a `__freshtemp_enum_scrut`.
    let src = format!(
            "{B_ENUM_PRELUDE}\nfn main() {{ let x = make(); if let Holder.Full(_, n) = x {{ println(n); }} }}\n"
        );
    let ir = ir_for_with_ownership(&src);
    assert!(
        !ir.contains("__freshtemp_enum_scrut"),
        "a place (bound-variable) scrutinee must not materialize a second \
             enum temp (would double-free against its own drop); got:\n{ir}"
    );
}

#[test]
fn test_ir_whilelet_freshtemp_enum_unbound_field_freed() {
    // `while let Full(_, n) = next(i) { … }` — per-iteration fresh-temp enum
    // scrutinee. The materialize + `track_enum_var` register in the body's
    // per-iteration frame (not an enclosing one), so the enum drop walk
    // frees each iteration's unbound Vec before the next scrutinee eval.
    let src = r#"
enum Holder { Full(Vec[i64], i64), Empty }
fn next(i: i64) -> Holder {
    if i < 3 {
        let mut v: Vec[i64] = Vec.new();
        v.push(1_i64);
        return Holder.Full(v, i);
    }
    return Holder.Empty;
}
fn main() {
    let mut i = 0;
    while let Holder.Full(_, n) = next(i) {
        println(n);
        i = i + 1;
    }
}
"#;
    let ir = ir_for_with_ownership(src);
    assert!(
        ir.contains("__freshtemp_enum_scrut") && ir.contains("@__karac_drop_Holder("),
        "expected fresh-temp while-let scrutinee materialized + enum-dropped; got:\n{ir}"
    );
}

#[test]
fn test_ir_whilelet_miss_variant_freed() {
    // B follow-up #2: the final non-matching fresh-temp enum scrutinee at
    // loop exit (here `Item.Stop(vec)` — heap-bearing, does not match
    // `Go`) is freed wholesale on the dedicated `whilelet.miss` edge. Pre-
    // fix the miss branched straight to exit and that Vec leaked.
    let src = r#"
enum Item { Go(Vec[i64]), Stop(Vec[i64]) }
fn mk() -> Vec[i64] {
    let mut v: Vec[i64] = Vec.new();
    v.push(1_i64);
    return v;
}
fn step(c: i64) -> Item {
    if c < 2 {
        return Item.Go(mk());
    }
    return Item.Stop(mk());
}
fn main() {
    let mut c: i64 = 0;
    while let Go(xs) = step(c) {
        c = c + 1;
    }
    println(c);
}
"#;
    let ir = ir_for_with_ownership(src);
    assert!(
        ir.contains("whilelet.miss"),
        "expected a dedicated whilelet.miss block; got:\n{ir}"
    );
    assert!(
        ir.contains("__whilelet_miss_scrut")
            && ir.contains("@__karac_drop_Item(ptr %__whilelet_miss_scrut"),
        "expected the final non-matching scrutinee dropped wholesale on the \
             miss edge; got:\n{ir}"
    );
}

#[test]
fn test_ir_whilelet_place_scrutinee_miss_not_dropped() {
    // A *place* scrutinee (a binding, owned by its own scope) must NOT be
    // wholesale-dropped on the miss edge — that would double-free against
    // the binding's own scope-exit cleanup. The miss helper is gated to
    // fresh temps, so no `__whilelet_miss_scrut` is emitted here.
    let src = r#"
enum Item { Go(Vec[i64]), Stop(Vec[i64]) }
fn mk() -> Vec[i64] {
    let mut v: Vec[i64] = Vec.new();
    v.push(1_i64);
    return v;
}
fn main() {
    let item = Item.Stop(mk());
    while let Go(_) = item {
        break;
    }
}
"#;
    let ir = ir_for_with_ownership(src);
    assert!(
        !ir.contains("__whilelet_miss_scrut"),
        "place scrutinee must not be materialized/dropped on the miss edge; got:\n{ir}"
    );
}

#[test]
fn test_e2e_while_let_drains_and_binds() {
    // The loop body runs once per match with the correct per-iteration
    // binding, and terminates when the scrutinee stops matching (`None`).
    let src = r#"
fn pop(v: mut ref Vec[i64]) -> Option[i64] {
    if v.len() == 0 {
        return Option.None;
    }
    let last = v.len() - 1;
    let x = v[last];
    v.remove(last);
    return Option.Some(x);
}

fn main() {
    let mut v: Vec[i64] = Vec.new();
    v.push(10_i64);
    v.push(20_i64);
    v.push(30_i64);
    let mut sum = 0_i64;
    while let Some(x) = pop(mut v) {
        sum = sum + x;
        println(f"got {x}");
    }
    println(f"sum={sum}");
    println("done");
}
"#;
    if let Some(out) = run_program(src) {
        assert_eq!(out.trim(), "got 30\ngot 20\ngot 10\nsum=60\ndone");
    }
}

#[test]
fn test_e2e_into_at_struct_field_if_and_match_tails() {
    // `.into()` threads the expected type through a struct-literal field
    // value, an if/else tail, and a match-arm tail — the three positions
    // the let/return/call-arg codegen tests above don't reach. Each flows
    // through `check_expr` with the contextual type, so the user
    // `impl From[Celsius] for Kelvin` lowers to `Kelvin.from(...)` at
    // every one. Build==run parity with the interpreter sibling
    // (`test_into_at_struct_field_if_and_match_tails`).
    let out = run_program(
        r#"
struct Celsius { deg: i64 }
struct Kelvin { k: i64 }
impl From for Kelvin {
    fn from(c: Celsius) -> Kelvin { Kelvin { k: c.deg + 273 } }
}
struct Reading { temp: Kelvin }
fn pick(hot: bool) -> Kelvin {
    if hot { (Celsius { deg: 100 }).into() }
    else { (Celsius { deg: 0 }).into() }
}
fn classify(n: i64) -> Kelvin {
    match n {
        0 => (Celsius { deg: 0 }).into(),
        _ => (Celsius { deg: n }).into(),
    }
}
fn main() {
    let r: Reading = Reading { temp: (Celsius { deg: 27 }).into() };
    println(r.temp.k);
    println(pick(true).k);
    println(pick(false).k);
    println(classify(0).k);
    println(classify(50).k);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "300\n373\n273\n273\n323");
    }
}

#[test]
fn test_e2e_generic_enum_heap_payload_match_return() {
    // B-2026-07-13-3: a generic enum `enum Opt[T] { Yes(T), No }` matched in
    // a generic fn whose value is a heap `T` (`match o { Opt.Yes(v) => v,
    // Opt.No => d }`). The payload AREA is sized for the erased `T` (1 word)
    // at declare, so a String monomorph is BOXED — the match-bind must debox
    // `v` at the concrete `{ptr,i64,i64}` width so the arm value agrees with
    // the `d: String` arm. Before the fix this failed module verification
    // (`ret i64 0` vs `{ptr,i64,i64}`); the interpreter was the only backend.
    let out = run_program(
        "enum Opt[T] { Yes(T), No }\n\
             fn get[T](o: Opt[T], d: T) -> T { match o { Opt.Yes(v) => v, Opt.No => d } }\n\
             fn main() {\n\
             \x20   let r: String = get(Opt.No, f\"def\");\n\
             \x20   println(r);\n\
             \x20   let s: String = get(Opt.Yes(f\"data\"), f\"x\");\n\
             \x20   println(s);\n\
             }",
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "def\ndata");
    }
}

#[test]
fn test_e2e_generic_enum_struct_heap_payload_match_return() {
    // B-2026-07-13-3, user-struct payload: `enum Opt[T] { Yes(T) }` at
    // `T = struct Box { s: String }` boxes the wider-than-erased-area
    // payload; the debox must rebuild at the struct's exact aggregate
    // `{ {ptr,i64,i64} }`, not the 3-word vec heuristic, or the arm value
    // disagrees with the `-> Box` return (`ret i64 0`).
    let out = run_program(
        "struct Box { s: String }\n\
             enum Opt[T] { Yes(T), No }\n\
             fn get[T](o: Opt[T], d: T) -> T { match o { Opt.Yes(v) => v, Opt.No => d } }\n\
             fn main() {\n\
             \x20   let b: Box = Box { s: f\"inside\" };\n\
             \x20   let db: Box = Box { s: f\"dflt\" };\n\
             \x20   let r: Box = get(Opt.Yes(b), db);\n\
             \x20   println(r.s);\n\
             \x20   let db2: Box = Box { s: f\"fb\" };\n\
             \x20   let r2: Box = get(Opt.No, db2);\n\
             \x20   println(r2.s);\n\
             }",
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "inside\nfb");
    }
}

#[test]
fn test_e2e_adopted_builders_match_and_walk() {
    // Phase C1c end-to-end: two adopted families in one fn (one
    // SomeRoot builder result read through the sanctioned match,
    // one RootLink builder result walked by non-owning cursors),
    // repeated. A free-walk miscount is a deterministic UAF
    // (double-free against the walk) or a wrong sum (leak reuses
    // garbage).
    let out = run_program_with_ownership(
        r#"
shared struct ListNode { val: i64, mut next: Option[ListNode] }
fn build_someroot(n: i64) -> Option[ListNode] {
    let head = ListNode { val: 1, next: None };
    let mut tail = head;
    let mut i = 2;
    while i <= n {
        let node = ListNode { val: i, next: None };
        tail.next = Some(node);
        tail = node;
        i = i + 1;
    }
    Some(head)
}
fn build_rootlink(n: i64) -> Option[ListNode] {
    let dummy = ListNode { val: 0, next: None };
    let mut tail = dummy;
    let mut i = 1;
    while i <= n {
        let node = ListNode { val: i, next: None };
        tail.next = Some(node);
        tail = node;
        i = i + 1;
    }
    dummy.next
}
fn main() {
    let mut total = 0;
    let mut iter = 0;
    while iter < 64 {
        let a = build_someroot(50);
        match a {
            Some(node) => { total = total + node.val; }
            None => {}
        }
        let b = build_rootlink(50);
        let mut cur = b;
        while cur.is_some() {
            let x = cur.unwrap();
            total = total + x.val;
            cur = x.next;
        }
        iter = iter + 1;
    }
    println(total);
}
"#,
    );
    // 64 * (1 + 1275) = 81664.
    assert_eq!(out.as_deref(), Some("81664\n"));
}

#[test]
fn test_e2e_shadow_match_arm_pattern_reverts() {
    // A `match` arm's PAYLOAD binding (`Some(v)`) shadowing an outer `v`
    // must not leak to the code after the match, NOR to a sibling arm that
    // references the outer `v` (`None => v`).
    let out = run_program(
        "fn describe(x: Option[i64]) -> i64 {\n\
             let v = 100;\n\
             match x { Some(v) => v, None => v }\n\
             }\n\
             fn main() {\n\
             println(describe(Some(7)).to_string());\n\
             println(describe(None).to_string());\n\
             }",
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "7\n100");
    }
}

#[test]
fn test_e2e_shadow_if_let_pattern_reverts() {
    let out = run_program(
        "fn main() {\n\
             let v = 100;\n\
             if let Some(v) = Some(42) { println(v.to_string()); }\n\
             println(v.to_string());\n\
             }",
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "42\n100");
    }
}

#[test]
fn test_e2e_question_result_match_interop() {
    // Built-in Result construction + ? + match against built-in Result variants
    // all interoperate using the same enum layout. Pins Step 6 (pattern-match interop).
    let out = run_program(
        r#"
fn check(n: i64) -> Result[i64, i64] {
    if n > 0 { Ok(n) } else { Err(0_i64 - n) }
}
fn double(n: i64) -> Result[i64, i64] {
    let x = check(n)?;
    Ok(x * 2)
}
fn main() {
    match double(7_i64) {
        Ok(v) => println(v),
        Err(e) => println(e),
    }
    match double(-5_i64) {
        Ok(v) => println(v),
        Err(e) => println(e),
    }
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "14\n5");
    }
}

#[test]
fn test_e2e_enum_match_simple_singleton() {
    let out = run_program(
        r#"
#[derive(Hash, Eq)]
enum Color { Red, Green, Blue }

fn main() {
    let g: Color = Color.Green;
    match g {
        Color.Green => println(11_i64),
        _ => println(99_i64),
    }
}
"#,
    );
    let out = out.expect("simple enum match should not bail");
    assert_eq!(out.trim(), "11");
}

#[test]
fn test_e2e_enum_variant_match_codegen_sanity() {
    // Sanity: distinct unit-enum variants codegen to distinct values
    // (different tags). Pre-requisite for `Map[Color, V]` to work.
    let out = run_program(
        r#"
#[derive(Hash, Eq)]
enum Color { Red, Green, Blue }

fn main() {
    let r: Color = Color.Red;
    let g: Color = Color.Green;
    let b: Color = Color.Blue;
    match r {
        Color.Red => println(0_i64),
        _ => println(99_i64),
    }
    match g {
        Color.Green => println(1_i64),
        _ => println(99_i64),
    }
    match b {
        Color.Blue => println(2_i64),
        _ => println(99_i64),
    }
}
"#,
    );
    let out = out.expect("enum variant match codegen sanity");
    let lines: Vec<&str> = out.trim().lines().collect();
    assert_eq!(lines, vec!["0", "1", "2"]);
}

/// Oversized-enum-payload §1 (fresh-temp scrutinee box-free): a
/// `match v.pop() { … }` over a boxed `Option[Entity]` (4 words > 3 area)
/// has no named binding, so before §1 the box leaked. The fresh-temp
/// materialization must now queue a `BoxedEnumDrop` — the `boxdrop`
/// cleanup block — so the box is freed at scope exit. (Leak gate; macOS
/// has no LeakSanitizer.)
#[test]
fn test_ir_freshtemp_boxed_option_match_frees_box() {
    let src = r#"
struct Entity { x: i64, y: i64, hp: i64, label: i64 }
fn main() {
    let mut v: Vec[Entity] = Vec.new();
    v.push(Entity { x: 1, y: 2, hp: 3, label: 5000 });
    match v.pop() {
        Some(e) => println(e.label),
        None => println(-1),
    }
}
"#;
    let ir = ir_for_with_ownership(src);
    assert!(
        ir.contains("boxdrop"),
        "fresh-temp boxed Option scrutinee must free its box (boxdrop block); got:\n{ir}"
    );
}

/// Oversized-enum-payload §1: same fix for the `if let` construct over a
/// fresh-temp boxed `Option[Wide]` returned by a call.
#[test]
fn test_ir_freshtemp_boxed_option_iflet_frees_box() {
    let src = r#"
struct Wide { a: i64, b: i64, c: i64, d: i64 }
fn make() -> Option[Wide] {
    return Some(Wide { a: 1, b: 2, c: 3, d: 4 });
}
fn main() {
    if let Some(e) = make() {
        println(e.d);
    }
}
"#;
    let ir = ir_for_with_ownership(src);
    assert!(
        ir.contains("boxdrop"),
        "fresh-temp boxed Option if-let scrutinee must free its box; got:\n{ir}"
    );
}

// Match-arm struct destructure for an OWNED scrutinee. Predecessor
// for the ref-scrutinee shape below — `bind_pattern_values` had no
// `PatternKind::Struct` arm at all before slice 3a, so well-typed
// `match p { Point { x, y } => x + y }` errored at codegen with
// `Undefined variable 'x'`.
#[test]
fn test_e2e_match_owned_struct_destructure_smoke() {
    let out = run_program(
        r#"
struct Point { x: i64, y: i64 }
fn show(p: Point) -> i64 {
    match p {
        Point { x, y } => x + y * 100,
    }
}
fn main() {
    let p = Point { x: 3, y: 5 };
    println(show(p));
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out, "503\n");
    }
}

// ── Ref-scrutinee match-arm leaf-binding ABI parity (slice 3a) ──
//
// Each test below typechecked correctly post-slice-1 (the ref
// scrutinee binding-form propagation landed 2026-05-12) but
// miscompiled at codegen until slice 3a — the leaf binding was
// emitted as a value-typed alloca, then passed to a `ref T` /
// `mut ref T` parameter as a value rather than a pointer (ABI
// mismatch). Slice 3a wraps each leaf in a ref-shim alloca so the
// call-site ABI matches the typechecker's view.

#[test]
fn test_e2e_match_ref_struct_field_passes_to_ref_param() {
    // `Foo { age }` under a `ref Foo` scrutinee binds `age` as
    // `ref i64`; passing it to `read_int(n: ref i64)` rounds the
    // pointer back to the value via the runtime's println path.
    let out = run_program(
        r#"
struct Foo { age: i64 }
fn read_int(n: ref i64) -> i64 { n + 1 }
fn show(f: ref Foo) -> i64 {
    match f {
        Foo { age } => read_int(age),
    }
}
fn main() {
    let f = Foo { age: 41 };
    println(show(f));
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out, "42\n");
    }
}

#[test]
fn test_e2e_match_ref_option_payload_passes_to_ref_param() {
    // Tuple-variant payload binding: `Option.Some(n)` under a
    // `ref Option[i64]` scrutinee binds `n` as `ref i64`.
    let out = run_program(
        r#"
fn read_int(n: ref i64) -> i64 { n * 2 }
fn show(opt: ref Option[i64]) -> i64 {
    match opt {
        Option.Some(n) => read_int(n),
        Option.None => -1,
    }
}
fn main() {
    let some = Option.Some(7);
    let none: Option[i64] = Option.None;
    println(show(some));
    println(show(none));
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out, "14\n-1\n");
    }
}

#[test]
fn test_e2e_match_owned_struct_field_owned_call_unaffected() {
    // Sanity: under an OWNED scrutinee, the leaf binding stays
    // value-typed; passing it to a value-taking function works
    // exactly as before slice 3a (the borrow_modes table is empty
    // for owned scrutinees, so the shim never fires).
    let out = run_program(
        r#"
struct Foo { age: i64 }
fn read_int(n: i64) -> i64 { n + 100 }
fn show(f: Foo) -> i64 {
    match f {
        Foo { age } => read_int(age),
    }
}
fn main() {
    let f = Foo { age: 1 };
    println(show(f));
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out, "101\n");
    }
}

#[test]
fn test_e2e_match_mut_ref_struct_field_passes_to_mut_ref_param() {
    // `mut ref Foo` scrutinee → leaf binding is `mut ref i64`;
    // passes ABI-shape parity check at the call site. Mutation
    // propagation is NOT exercised here — slice 3a's shim aliases
    // a copy, not the scrutinee storage; that's the deferred GEP
    // sub-slice (see phase-5 entry).
    let out = run_program(
        r#"
struct Bag { n: i64 }
fn bump(n: mut ref i64) -> i64 { n + 1 }
fn show(b: mut ref Bag) -> i64 {
    match b {
        Bag { n } => bump(n),
    }
}
fn main() {
    let mut b = Bag { n: 10 };
    println(show(mut b));
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out, "11\n");
    }
}

// ── Slice 3b probe tests: mut-ref scrutinee write-through ──
//
// These tests assert that mutation through a `mut ref` leaf binding
// *propagates back* to the original scrutinee storage. Slice 3a's
// ref-shim aliases a copy — these tests will FAIL until slice 3b's
// GEP-into-scrutinee lowering lands. The probe converts the silent
// miscompile into a known-failing pin so the gap can't persist
// unnoticed.
//
// Each test follows the same shape: mutate via a `mut ref` arg
// inside a match arm under a `mut ref` scrutinee, then observe the
// scrutinee's state from the outer scope. If the shim's copy
// semantics dominate, the outer observation reads the pre-mutation
// value (test fails). If true GEP aliasing is in place, the outer
// observation reads the post-mutation value (test passes).

#[test]
fn test_e2e_match_mut_ref_struct_field_write_through_propagates() {
    // The match arm returns `set_to`'s i64 (Kara represents unit as
    // i64 zero), so the match-as-statement form would trip an
    // unrelated "non-void return in void function" codegen bug.
    // Returning the propagation result through the function value
    // sidesteps that orthogonal gap and isolates the write-through
    // semantic this test is probing.
    let out = run_program(
        r#"
struct Bag { n: i64 }
fn set_to(n: mut ref i64, v: i64) -> i64 { *n = v; v }
fn mutate(b: mut ref Bag) -> i64 {
    match b {
        Bag { n } => set_to(n, 99),
    }
}
fn main() {
    let mut b = Bag { n: 10 };
    let _ = mutate(mut b);
    println(b.n);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(
            out, "99\n",
            "mut-ref scrutinee leaf binding must write through to scrutinee storage"
        );
    }
}

#[test]
fn test_e2e_match_mut_ref_option_payload_write_through_propagates() {
    // Tuple-variant payload: under `mut ref Option[i64]`, the
    // `Option.Some(n)` binding `n` should be `mut ref i64` aliasing
    // the Some-payload's storage.
    let out = run_program(
        r#"
fn set_to(n: mut ref i64, v: i64) -> i64 { *n = v; v }
fn mutate(opt: mut ref Option[i64]) -> i64 {
    match opt {
        Option.Some(n) => set_to(n, 42),
        Option.None => 0,
    }
}
fn main() {
    let mut opt = Option.Some(7);
    let _ = mutate(mut opt);
    match opt {
        Option.Some(v) => println(v),
        Option.None => println(-1),
    }
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(
            out, "42\n",
            "mut-ref Option payload binding must write through to scrutinee storage"
        );
    }
}

#[test]
fn test_e2e_match_mut_ref_struct_two_fields_independent_write_through() {
    // Two leaf bindings under the same scrutinee: each should alias
    // its own field independently.
    let out = run_program(
        r#"
struct Pair { a: i64, b: i64 }
fn set_to(n: mut ref i64, v: i64) -> i64 { *n = v; v }
fn mutate(p: mut ref Pair) -> i64 {
    match p {
        Pair { a, b } => set_to(a, 100) + set_to(b, 200),
    }
}
fn main() {
    let mut p = Pair { a: 1, b: 2 };
    let _ = mutate(mut p);
    println(p.a);
    println(p.b);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(
            out, "100\n200\n",
            "two mut-ref leaf bindings must each write through to their own field"
        );
    }
}

#[test]
fn test_e2e_match_at_binding_outer_and_nested_write_through() {
    // `whole @ Pair { a, b }` under `mut ref Pair`: the outer alias
    // `whole: mut ref Pair` AND the nested leaf bindings `a` / `b`
    // (`mut ref i64`) must all alias the scrutinee storage, so writes
    // through any of them propagate back to the caller. Previously the
    // whole `@`-pattern fell back to the value-source copy path and
    // none of the writes propagated. `a` is set to 5 then overwritten
    // to 7 via the `whole` alias (proving `a` and `whole.a` alias the
    // same storage); `b` is set to 9 via its own leaf.
    let out = run_program(
        r#"
struct Pair { a: i64, b: i64 }
fn set_to(n: mut ref i64, v: i64) -> i64 { *n = v; v }
fn set_a(p: mut ref Pair, v: i64) -> i64 { p.a = v; v }
fn mutate(p: mut ref Pair) -> i64 {
    match p {
        whole @ Pair { a, b } => {
            let _ = set_to(a, 5);
            let _ = set_to(b, 9);
            set_a(whole, 7)
        }
    }
}
fn main() {
    let mut p = Pair { a: 1, b: 2 };
    let _ = mutate(mut p);
    println(p.a);
    println(p.b);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(
            out, "7\n9\n",
            "at-binding outer alias + nested leaves must all write through"
        );
    }
}

#[test]
fn test_e2e_match_at_binding_ref_read_through_alias() {
    // Read path: `whole @ Bag { n }` under `ref Bag`. The outer alias
    // `whole: ref Bag` passes to a `ref Bag` param, and the nested
    // `n: ref i64` auto-derefs in value position — both observe the
    // live scrutinee.
    let out = run_program(
        r#"
struct Bag { n: i64 }
fn read_n(b: ref Bag) -> i64 { b.n }
fn peek(b: ref Bag) -> i64 {
    match b {
        whole @ Bag { n } => read_n(whole) + n,
    }
}
fn main() {
    let b = Bag { n: 5 };
    println(peek(b));
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(
            out, "10\n",
            "at-binding outer alias + nested binding must read the live scrutinee"
        );
    }
}

#[test]
fn test_return_value_terminal_arm_stores_placeholder() {
    // The terminal arm writes a placeholder `i64 0` into the
    // terminal field via the named GEP `kara.return.field_ptr`
    // before the Ready return.
    let ir = ir_for_with_state_struct_layouts(
        "effect resource Network;
             pub fn fetch() with sends(Network) receives(Network) {}
             fn driver() -> i64 with sends(Network) receives(Network) { fetch(); 0 }",
    );
    let body = extract_fn_ir(&ir, "__kara_poll_driver");
    assert!(
        body.contains("store i64 0, ptr %kara.return.field_ptr"),
        "terminal arm must store placeholder i64 0 into terminal field:\n{body}"
    );
    // The store must precede the Ready return.
    let store_pos = body
        .find("store i64 0, ptr %kara.return.field_ptr")
        .unwrap();
    let ready_pos = body.find("ret i8 1").unwrap();
    assert!(
        store_pos < ready_pos,
        "terminal field store must precede the Ready return:\n{body}"
    );
}

#[test]
fn test_body_splitting_8p_assigns_in_terminal_arm_no_writeback() {
    // Assignment in the terminal arm — no yield follows, so no
    // writeback emits. The assignment still stores into the slot
    // (visible to the final-expression read in slice 8o).
    let ir = ir_for_with_state_struct_layouts(
        "effect resource Network;
             pub fn fetch() with sends(Network) receives(Network) {}
             fn driver(n: i64) -> i64 with sends(Network) receives(Network) {
                 fetch();
                 n = 42;
                 n
             }",
    );
    let body = extract_fn_ir(&ir, "__kara_poll_driver");
    assert!(
        body.contains("store i64 42, ptr %n.slot"),
        "terminal-arm assignment must store into n.slot:\n{body}"
    );
    // The slice-8o final-expression read should see the new value.
    assert!(
        body.contains("%n.return = load i64, ptr %n.slot"),
        "terminal final-expression must load from updated n.slot:\n{body}"
    );
}

#[test]
fn test_body_splitting_8r_terminal_arm_compound_assign() {
    // `fn driver(n: i64) -> i64 { fetch(); n += 41; n }` — terminal
    // arm compound-assign. No writeback follows (terminal arm), but
    // slice 8o's `%n.return` final-expression read sees the post-op
    // slot value.
    let ir = ir_for_with_state_struct_layouts(
        "effect resource Network;
             pub fn fetch() with sends(Network) receives(Network) {}
             fn driver(n: i64) -> i64 with sends(Network) receives(Network) {
                 fetch();
                 n += 41;
                 n
             }",
    );
    let body = extract_fn_ir(&ir, "__kara_poll_driver");
    assert!(
        body.contains("%binop.assign_rhs = add i64 %n.assign_rhs, 41"),
        "terminal-arm compound-assign must emit the binary add:\n{body}"
    );
    assert!(
        body.contains("store i64 %binop.assign_rhs, ptr %n.slot"),
        "terminal-arm compound-assign must store into n.slot:\n{body}"
    );
    // The slice-8o terminal-return reads the updated slot.
    assert!(
        body.contains("%n.return = load i64, ptr %n.slot"),
        "terminal-return must load from updated n.slot:\n{body}"
    );
}

// ── Phase 6 line 26 slice 8t: control flow inside arms ────────────────
//
// Body-splitting walker now descends into stmt-position CF expressions
// (if / while / for / match / loop / labeled-block) to count nested
// yield-point spans, advancing `cur_arm` so post-CF statements get
// queued into the post-yield arm. v1 scope: the CF expression itself
// is dropped from the IR — only `cur_arm` advancement is preserved.
// A follow-on slice rebuilds CF as branching codegen.

#[test]
fn test_body_splitting_8t_yield_inside_if_advances_arm_for_post_if_stmt() {
    // `fn driver() { if cond { fetch(); } take(42); }` — without
    // slice 8t, the if-stmt drops silently and `take(42)` lands in
    // state_0 (the pre-yield arm). With slice 8t, the descent finds
    // the yield inside the if-body, advances cur_arm to 1, and
    // `take(42)` lands in state_1 (post-yield / terminal). The IR
    // difference: state_0 has just reload + state-transition +
    // Pending; state_1 has reload + take(42) + terminal return.
    let ir = ir_for_with_state_struct_layouts(
        "effect resource Network;
             pub fn fetch() with sends(Network) receives(Network) {}
             fn take(n: i64) {}
             fn driver(cond: bool) with sends(Network) receives(Network) {
                 if cond { fetch(); }
                 take(42);
             }",
    );
    let body = extract_fn_ir(&ir, "__kara_poll_driver");
    // `take(42)` must be present (post-CF stmt is preserved).
    assert!(
        body.contains("call void @take(i64 42)"),
        "post-CF take(42) must be emitted:\n{body}"
    );
    // The call's position relative to the state-transition matters
    // — slice 8t puts the call in state_1 (post-yield arm). The
    // state-transition writes tag=1 then returns Pending; the call
    // must come AFTER the Pending return in IR text (since state_1
    // appears after state_0 in the switch arm sequence).
    let pending_return_pos = body
        .find("store i32 1, ptr %state_0.next_tag_ptr")
        .expect("state_0 transition store should be present");
    let take_call_pos = body
        .find("call void @take(i64 42)")
        .expect("take call should be present");
    assert!(
            take_call_pos > pending_return_pos,
            "take(42) must be in state_1 (after state_0's transition):\nstate_0_pos={pending_return_pos} take_pos={take_call_pos}\n{body}"
        );
}

#[test]
fn test_body_splitting_8t_yield_inside_match_advances_arm() {
    // `match cond { true => fetch(), false => {} }` followed by a
    // post-match call. Descent walks match arms in source order.
    let ir = ir_for_with_state_struct_layouts(
        "effect resource Network;
             pub fn fetch() with sends(Network) receives(Network) {}
             fn take(n: i64) {}
             fn driver(cond: bool) with sends(Network) receives(Network) {
                 match cond { true => fetch(), false => {} }
                 take(23);
             }",
    );
    let body = extract_fn_ir(&ir, "__kara_poll_driver");
    let transition_pos = body
        .find("store i32 1, ptr %state_0.next_tag_ptr")
        .expect("state_0 transition store should be present");
    let take_pos = body
        .find("call void @take(i64 23)")
        .expect("take call should be present");
    assert!(
        take_pos > transition_pos,
        "take(23) must land in state_1 (after the match-arm yield):\n{body}"
    );
}

#[test]
fn test_terminal_return_8o_uses_arm_local_let_final_expr() {
    // `fn driver() -> i64 ... { fetch(); let r = 99; r }` — slice
    // 8m emits the let into the terminal arm's slot_map; slice 8o
    // loads from that slot for the final-expression return.
    let ir = ir_for_with_state_struct_layouts(
        "effect resource Network;
             pub fn fetch() with sends(Network) receives(Network) {}
             fn driver() -> i64 with sends(Network) receives(Network) {
                 fetch();
                 let r = 99;
                 r
             }",
    );
    let body = extract_fn_ir(&ir, "__kara_poll_driver");
    assert!(
        body.contains("store i64 99, ptr %r.slot"),
        "let r = 99 must store into r.slot in terminal arm:\n{body}"
    );
    assert!(
        body.contains("%r.return = load i64, ptr %r.slot"),
        "terminal arm must load r.slot via .return:\n{body}"
    );
    assert!(
        body.contains("store i64 %r.return, ptr %kara.return.field_ptr"),
        "terminal arm must store r's loaded value into terminal field:\n{body}"
    );
}

// ──────────────────────────────────────────────────────────────────
// i64.parse(s: String) -> Option[i64] — shipped 2026-05-21.
//
// Calls `karac_runtime_parse_i64(data, len, *out)`; trims +
// i64::from_str on the runtime side; PHI-builds Option[i64] at
// the merge BB via `build_option_some_via_phis`.
// ──────────────────────────────────────────────────────────────────

#[test]
fn e2e_enum_f64_payload_match_codegen() {
    // Regression: enum float payloads were bound/printed as raw i64 bits
    // (the payload word was never bitcast back to f64, and the binding was
    // type-tracked as i64). Covers the single-word path (Option[f64]) and
    // the tuple-payload path (`Float(f64, i64)` — the self-hosting lexer's
    // Token::Float shape). Fixed via the float arm in
    // record_pattern_binding_surface_types + the FloatType arms in
    // reconstruct_payload_value.
    let output = run_program(
            "enum Tok { Float(f64, i64), Nil }\n\
             fn main() {\n\
                 match Some(3.14) { Some(x) => println(x), None => println(0.0) }\n\
                 let o: Option[f64] = Some(1.5);\n\
                 match o { Some(x) => println(x), None => println(0.0) }\n\
                 match Tok.Float(2.5, 7) { Float(x, y) => { println(x); println(y); } Nil => println(0.0) }\n\
             }",
        )
        .expect("compile + run failed");
    assert_eq!(output, "3.14\n1.5\n2.5\n7\n");
}

#[test]
fn test_e2e_ref_param_struct_field_struct_pattern_consume() {
    // B-2026-07-21-7: struct-PATTERN destructure of a struct-typed FIELD
    // reached through a `ref` param, with a binding the escape walk counts
    // as moved (string `+` desugars to `Call(String.add, ..)`; even the
    // scalar `s.len() + x` shape counts, because `iN.add`'s args are
    // conservative moves). The bound field bit-copy-aliased the CALLER's
    // String and both the binding's scope-exit free and the caller's
    // struct drop freed the same buffer — double-free abort under JIT and
    // AOT-O0 (O2 happened to survive), interp correct. Now the ESCAPING
    // ref-chain struct match deep-clones the scrutinee
    // (`clone_escaping_borrowed_ref_chain_struct`), registers the clone's
    // own StructDrop, and each arm's per-field suppression fires against
    // the CLONE slot — bindings own independent buffers, unbound fields
    // are freed by the clone drop, the caller's struct is untouched.
    // Covers: concat consume, the scalar-escape "read-only" shape,
    // identity move of the String field, an unbound second String field,
    // a two-hop chain, and caller reuse after every call. Sibling LSan
    // test guards the leak/double-free halves.
    let output = run_program(
        "struct Pt { s: String, x: i64 }\n\
             struct Pair { a: String, b: String, x: i64 }\n\
             struct Mid { p: Pt }\n\
             struct Holder { inner: Pt, pair: Pair, mid: Mid, n: i64 }\n\
             fn render(h: ref Holder) -> String {\n\
                 match h.inner {\n\
                     Pt { s, x } => { return \"p:\".to_string() + s + \":\" + x.to_string(); }\n\
                 }\n\
                 return \"?\".to_string();\n\
             }\n\
             fn peek(h: ref Holder) -> i64 {\n\
                 match h.inner {\n\
                     Pt { s, x } => { return s.len() + x; }\n\
                 }\n\
                 return -1;\n\
             }\n\
             fn take_a(h: ref Holder) -> String {\n\
                 match h.pair {\n\
                     Pair { a, b: _, x: _ } => { return a; }\n\
                 }\n\
                 return \"?\".to_string();\n\
             }\n\
             fn deep(h: ref Holder) -> String {\n\
                 match h.mid.p {\n\
                     Pt { s, x } => { return s + \":\" + x.to_string(); }\n\
                 }\n\
                 return \"?\".to_string();\n\
             }\n\
             fn main() {\n\
                 let a = Holder {\n\
                     inner: Pt { s: \"sp\".to_string(), x: 7 },\n\
                     pair: Pair { a: \"left\".to_string(), b: \"right\".to_string(), x: 9 },\n\
                     mid: Mid { p: Pt { s: \"dp\".to_string(), x: 2 } },\n\
                     n: 1,\n\
                 };\n\
                 println(render(a));\n\
                 println(render(a));\n\
                 println(peek(a));\n\
                 println(take_a(a));\n\
                 println(take_a(a));\n\
                 println(deep(a));\n\
             }",
    )
    .expect("compile + run failed");
    assert_eq!(output, "p:sp:7\np:sp:7\n9\nleft\nleft\ndp:2\n");
}

#[test]
fn test_e2e_ref_param_field_chain_iflet_whilelet_letelse() {
    // B-2026-07-21-8: the if-let / while-let / let-else ROUTES of the
    // ref-param field-chain consuming-read family. Each route binds a
    // heap payload out of `<refparam>.field` and consumes it (string `+`
    // desugars to `Call(String.add, ..)` — an escape), but none ran the
    // ref-chain clone legs the match route gained for B-2026-07-21-5/-6/-7
    // — the binding aliased the caller's buffer and both freed it
    // (double-free abort on every compiled backend; interp correct).
    // Now: if-let and let-else run both clone legs (enum leaf rides the
    // freshtemp channel, struct leaf carries its own StructDrop with the
    // suppression firing on the clone slot); while-let clones per header
    // evaluation, with the final non-matching copy freed wholesale on the
    // miss edge (`drop_freshtemp_enum_scrutinee_on_miss(force)`). Caller
    // reuse after every call pins that the caller's value survives.
    let output = run_program(
            "enum Tok { Plus, Ident(String) }\n\
             struct Pt { s: String, x: i64 }\n\
             struct Holder { tok: Tok, inner: Pt, n: i64 }\n\
             fn iflet_enum(h: ref Holder) -> String {\n\
                 if let Ident(name) = h.tok {\n\
                     return \"ie:\".to_string() + name;\n\
                 }\n\
                 return \"+\".to_string();\n\
             }\n\
             fn iflet_struct(h: ref Holder) -> String {\n\
                 if let Pt { s, x } = h.inner {\n\
                     return \"is:\".to_string() + s + \":\" + x.to_string();\n\
                 }\n\
                 return \"?\".to_string();\n\
             }\n\
             fn whilelet_enum(h: ref Holder) -> String {\n\
                 while let Ident(name) = h.tok {\n\
                     return \"we:\".to_string() + name;\n\
                 }\n\
                 return \"+\".to_string();\n\
             }\n\
             fn letelse_enum(h: ref Holder) -> String {\n\
                 let Ident(name) = h.tok else {\n\
                     return \"+\".to_string();\n\
                 }\n\
                 return \"le:\".to_string() + name;\n\
             }\n\
             fn letelse_struct(h: ref Holder) -> String {\n\
                 let Pt { s, x } = h.inner else {\n\
                     return \"?\".to_string();\n\
                 }\n\
                 return \"ls:\".to_string() + s + \":\" + x.to_string();\n\
             }\n\
             fn main() {\n\
                 let a = Holder {\n\
                     tok: Tok.Ident(\"tk\".to_string()),\n\
                     inner: Pt { s: \"pt\".to_string(), x: 5 },\n\
                     n: 1,\n\
                 };\n\
                 println(iflet_enum(a));\n\
                 println(iflet_enum(a));\n\
                 println(iflet_struct(a));\n\
                 println(whilelet_enum(a));\n\
                 println(whilelet_enum(a));\n\
                 println(letelse_enum(a));\n\
                 println(letelse_struct(a));\n\
                 let p = Holder { tok: Tok.Plus, inner: Pt { s: \"z\".to_string(), x: 0 }, n: 2 };\n\
                 println(iflet_enum(p));\n\
                 println(whilelet_enum(p));\n\
                 println(letelse_enum(p));\n\
             }",
        )
        .expect("compile + run failed");
    assert_eq!(
        output,
        "ie:tk\nie:tk\nis:pt:5\nwe:tk\nwe:tk\nle:tk\nls:pt:5\n+\n+\n+\n"
    );
}

/// B-2026-09-01-28 — compiled twin of `tests/interpreter.rs`'s
/// `pattern_spellings_agree_on_a_field_only_drop_payload`, same programs
/// and expectations.
///
/// This backend was correct on every row; the interpreter's `if let` and
/// `while let` lost the body. The twin is what makes "the three spellings
/// and the two backends all agree" a property a test can hold, rather than
/// something only the fixed side asserts.
#[test]
fn test_e2e_pattern_spellings_agree_on_a_field_only_drop_payload() {
    const H: &str = "struct R { s: String }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"dR[{self.s}]\") } }\n\
             struct P { r: R, n: i64 }\n\
             struct D { n: i64 }\n\
             impl Drop for D { fn drop(mut ref self) { println(f\"dD[{self.n}]\") } }\n\
             fn optp(t: String) -> Option[P] { return Option.Some(P { r: R { s: t }, n: 1 }); }\n\
             fn optd(k: i64) -> Option[D] { return Option.Some(D { n: k }); }\n";
    for (label, body, want) in [
        (
            "match",
            "match optp(\"m\") { Some(p) => { println(p.n) } None => { println(\"x\") } }",
            "1\ndR[m]\n",
        ),
        (
            "if-let",
            "if let Some(p) = optp(\"i\") { println(p.n) }",
            "1\ndR[i]\n",
        ),
        (
            "own-drop-payload, if-let",
            "if let Some(d) = optd(7) { println(d.n) }",
            "7\ndD[7]\n",
        ),
        (
            "own-drop-payload, match",
            "match optd(8) { Some(d) => { println(d.n) } None => { println(\"x\") } }",
            "8\ndD[8]\n",
        ),
    ] {
        assert_eq!(
            run_program(&format!("{H}fn main() {{\n{body}\n}}\n")),
            Some(want.to_string()),
            "{label}"
        );
    }
}

/// B-2026-08-31-32 — compiled twin of `tests/interpreter.rs`'s
/// `fresh_temp_struct_scrutinee_arm_binding_runs_its_body`, same programs
/// and expectations.
///
/// This backend was correct on every row; the interpreter ran no body for
/// the two fresh-temp ones. The twin exists because the property is that
/// the two AGREE — and because `bound-local` and `no-binding` are the rows a
/// later widening on either side would break silently.
///
/// B-2026-09-16-18 — AND `no-binding` WAS ITSELF WRONG ON BOTH BACKENDS,
/// which is what that last sentence could not see: it watches for a
/// DIVERGENCE, and the defect was an AGREEMENT. `P`'s husk was owned by
/// nobody once the arm bound nothing out of it, so `R`'s body never ran and
/// its buffer leaked on every surface. Flipped from `"nb\n"` to
/// `"nb\ndR[d]\n"` in lockstep with the interpreter twin.
#[test]
fn test_e2e_fresh_temp_struct_scrutinee_arm_binding_runs_its_body() {
    const H: &str = "struct R { s: String }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"dR[{self.s}]\") } }\n\
             struct P { r: R, n: i64 }\n\
             struct Q { r: R }\n\
             impl Drop for Q { fn drop(mut ref self) { println(\"dQ\") } }\n\
             fn mkp(t: String) -> P { return P { r: R { s: t }, n: 1 }; }\n";
    for (label, body, want) in [
        (
            "fresh-literal",
            "match P { r: R { s: \"a\" }, n: 4 } { P { r, .. } => println(r.s) }",
            "a\ndR[a]\n",
        ),
        (
            "fresh-call",
            "match mkp(\"c\") { P { r, .. } => println(r.s) }",
            "c\ndR[c]\n",
        ),
        (
            "bound-local",
            "let p = P { r: R { s: \"b\" }, n: 4 };\n\
                 match p { P { r, .. } => println(r.s) }",
            "b\ndR[b]\n",
        ),
        (
            // B-2026-09-16-18 — the husk of a fresh temp whose arm binds
            // nothing is still owned by the match: its fields' `Drop`
            // bodies run, in reverse declaration order, after the arm.
            // Flipped from `"nb\n"`.
            "no-binding",
            "match P { r: R { s: \"d\" }, n: 4 } { P { .. } => println(\"nb\") }",
            "nb\ndR[d]\n",
        ),
        (
            "own-drop-struct",
            "match Q { r: R { s: \"e\" } } { Q { r } => println(r.s) }",
            "e\ndR[e]\n",
        ),
    ] {
        // `own-drop-struct` binds `Q`'s field out of an own-`Drop`
        // scrutinee — denied since B-2026-09-01-43, kept under the designed
        // opt-out because the drop placement it pins stays reachable that
        // way. The other four cells use `P`, which declares no `Drop`, so
        // they keep `assert_check_clean`'s gate.
        let attr = match label {
            "own-drop-struct" => "#[allow(partial_move_of_drop_struct)]\n",
            _ => "",
        };
        assert_eq!(
            run_program(&format!("{H}{attr}fn main() {{\n{body}\n}}\n")),
            Some(want.to_string()),
            "{label}"
        );
    }
}

/// B-2026-09-21-1 — a fresh-temp struct scrutinee's UNBOUND fields run their
/// `Drop` bodies under `if let` and `let ... else` too, not only under `match`.
///
/// B-2026-09-16-18 gave the husk an owner in `eval_match` alone and gated the
/// compiled side to the `match` spelling, deliberately: arming codegen by itself
/// would have turned a gap all four surfaces shared into a run-vs-build
/// DIVERGENCE, which is strictly worse than the gap. The interpreter's other
/// three spellings now carry the same ownership, the gate is gone, and every
/// surface agrees.
///
/// THE TWO CONSTRUCTS DISAGREE ABOUT SEQUENCE AND BOTH ARE CORRECT, which is
/// the part worth reading before filing an ordering bug against this fixture.
/// `iflet-one-bound` runs `dR72 dR73` (binding, then husk) and
/// `letelse-one-bound` runs `dR75 dR74` (husk, then binding). One rule produces
/// both: design.md ties a destructor to its binding's LIVE-RANGE END, and scopes
/// a STATEMENT-POSITION temporary to its `;`. A `let ... else` binding escapes
/// into the enclosing block while the scrutinee temporary dies at the `;`, so
/// the husk goes first; an `if let` binding dies at the end of the block, inside
/// the construct, so it goes first and the husk follows. `match-control` pins
/// the third case and must not move at all.
///
/// `iflet-rebind` is the cell that catches a mask built from BINDING names
/// rather than FIELD names: `S3 { a: q, .. }` takes field `a` under the name
/// `q`, so a binding-keyed mask leaves `a` in the unbound set and walks it a
/// second time beside `q`'s own drop — `dR5 dR5 dR6` instead of `dR5 dR6`.
///
/// The two `*-named-scrutinee-control` cells pin the other direction: a NAMED
/// scrutinee has an owner already, so the husk channel must stay out of it or
/// each remaining field's body runs twice.
#[test]
fn test_e2e_iflet_letelse_fresh_temp_husk_fields_run_their_drop_bodies() {
    const H: &str = "struct R { id: i64, name: String }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\") } }\n\
             fn mk(i: i64) -> R { return R { id: i, name: f\"n{i}\" }; }\n\
             struct S3 { a: R, b: R }\n\
             struct S4 { a: R, b: R, c: R }\n\
             struct Inner { r: R }\n\
             struct Outer { i: Inner, b: R }\n";
    for (label, cell, want) in [
            (
                "iflet-one-bound",
                "fn c() -> i64 { if let S3 { a, .. } = S3 { a: mk(72), b: mk(73) } { return a.id; } return 0; }",
                "dR72\ndR73\nz=72\n",
            ),
            (
                "iflet-none-bound",
                "fn c() -> i64 { if let S3 { .. } = S3 { a: mk(1), b: mk(2) } { return 9; } return 0; }",
                "dR2\ndR1\nz=9\n",
            ),
            (
                "iflet-all-bound",
                "fn c() -> i64 { if let S3 { a, b } = S3 { a: mk(3), b: mk(4) } { return a.id + b.id; } return 0; }",
                "dR4\ndR3\nz=7\n",
            ),
            (
                "iflet-rebind",
                "fn c() -> i64 { if let S3 { a: q, .. } = S3 { a: mk(5), b: mk(6) } { return q.id; } return 0; }",
                "dR5\ndR6\nz=5\n",
            ),
            (
                "iflet-three-field-one-bound",
                "fn c() -> i64 { if let S4 { b, .. } = S4 { a: mk(7), b: mk(8), c: mk(9) } { return b.id; } return 0; }",
                "dR8\ndR9\ndR7\nz=8\n",
            ),
            (
                "iflet-nested",
                "fn c() -> i64 { if let Outer { i: Inner { r }, .. } = Outer { i: Inner { r: mk(10) }, b: mk(11) } { return r.id; } return 0; }",
                "dR10\ndR11\nz=10\n",
            ),
            (
                "letelse-one-bound",
                "fn c() -> i64 { let S3 { a, .. } = S3 { a: mk(74), b: mk(75) } else { return 0; }; return a.id; }",
                "dR75\ndR74\nz=74\n",
            ),
            (
                "letelse-all-bound",
                "fn c() -> i64 { let S3 { a, b } = S3 { a: mk(14), b: mk(15) } else { return 0; }; return a.id + b.id; }",
                "dR15\ndR14\nz=29\n",
            ),
            (
                "letelse-three-field-one-bound",
                "fn c() -> i64 { let S4 { b, .. } = S4 { a: mk(16), b: mk(17), c: mk(18) } else { return 0; }; return b.id; }",
                "dR18\ndR16\ndR17\nz=17\n",
            ),
            (
                "match-control",
                "fn c() -> i64 { match S3 { a: mk(70), b: mk(71) } { S3 { a, .. } => { return a.id; } } }",
                "dR70\ndR71\nz=70\n",
            ),
            (
                "iflet-named-scrutinee-control",
                "fn c() -> i64 { let s: S3 = S3 { a: mk(40), b: mk(41) }; if let S3 { a, .. } = s { return a.id; } return 0; }",
                "dR40\ndR41\nz=40\n",
            ),
            (
                "letelse-named-scrutinee-control",
                "fn c() -> i64 { let s: S3 = S3 { a: mk(42), b: mk(43) }; let S3 { a, .. } = s else { return 0; }; return a.id; }",
                "dR43\ndR42\nz=42\n",
            ),
        ] {
            let src = format!(
                "{H}{cell}\nfn main() {{ let z: i64 = c(); println(f\"z={{z}}\"); }}\n"
            );
            assert_eq!(run_program(&src), Some(want.to_string()), "{label}");
        }
}

/// B-2026-09-21-6 — compiled twin of `tests/interpreter.rs`'s
/// `freshtemp_own_drop_struct_scrutinee_runs_its_body`, same programs and
/// same expectations.
///
/// This backend was correct on every cell; the INTERPRETER ran nothing at all
/// for a fresh-temp struct scrutinee whose type declares its own `impl Drop`,
/// under `match`, `if let` and `let ... else` alike. The twin exists because
/// the property being pinned is that the two AGREE — the interpreter's fix
/// routes the body between two channels that overlap on a CALL scrutinee, so
/// `whilelet-call` and `match-call` are the cells a double-fire on that side
/// would trip, and they are only meaningful against an oracle here.
#[test]
fn test_e2e_freshtemp_own_drop_struct_scrutinee_runs_its_body() {
    const H: &str = "struct R { id: i64, name: String }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\") } }\n\
             fn mk(i: i64) -> R { return R { id: i, name: f\"n{i}\" }; }\n\
             struct Od { a: R, b: R }\n\
             impl Drop for Od { fn drop(mut ref self) { println(\"dOd\") } }\n\
             fn mkod(i: i64) -> Od { return Od { a: mk(i), b: mk(i + 1) }; }\n";
    for (label, cell, want) in [
        (
            "match-literal",
            "fn c() -> i64 { match Od { a: mk(50), b: mk(51) } { Od { .. } => { return 9; } } }",
            "dOd\ndR51\ndR50\nz=9\n",
        ),
        (
            "iflet-literal",
            "fn c() -> i64 { if let Od { .. } = Od { a: mk(12), b: mk(13) } { return 9; } return 0; }",
            "dOd\ndR13\ndR12\nz=9\n",
        ),
        (
            "letelse-literal",
            "fn c() -> i64 { let Od { .. } = Od { a: mk(20), b: mk(21) } else { return 0; }; return 9; }",
            "dOd\ndR21\ndR20\nz=9\n",
        ),
        (
            "whilelet-literal",
            "fn c() -> i64 { let mut n: i64 = 0;\n\
                 while let Od { .. } = Od { a: mk(35), b: mk(36) } { n = n + 1; if n > 0 { break; } }\n\
                 return n; }",
            "dOd\ndR36\ndR35\nz=1\n",
        ),
        (
            // The row's FOURTH spelling, and the only GUARDED shape this type
            // can legally take -- `partial_move_of_drop_struct` rejects an arm
            // binding a field out of an own-`Drop` struct.
            "match-guarded-literal",
            "fn c() -> i64 { match Od { a: mk(56), b: mk(57) } {\n\
                 Od { .. } if 1 > 900 => { return 1; }\n\
                 Od { .. } => { return 6; } } }",
            "dOd\ndR57\ndR56\nz=6\n",
        ),
        (
            "whilelet-call",
            "fn c() -> i64 { let mut n: i64 = 0;\n\
                 while let Od { .. } = mkod(30) { n = n + 1; if n > 0 { break; } }\n\
                 return n; }",
            "dOd\ndR31\ndR30\nz=1\n",
        ),
        (
            "match-call",
            "fn c() -> i64 { match mkod(80) { Od { .. } => { return 9; } } }",
            "dOd\ndR81\ndR80\nz=9\n",
        ),
        (
            "iflet-call",
            "fn c() -> i64 { if let Od { .. } = mkod(40) { return 9; } return 0; }",
            "dOd\ndR41\ndR40\nz=9\n",
        ),
        (
            "letelse-call",
            "fn c() -> i64 { let Od { .. } = mkod(45) else { return 0; }; return 9; }",
            "dOd\ndR46\ndR45\nz=9\n",
        ),
        (
            "named-control",
            "fn c() -> i64 { let s: Od = Od { a: mk(60), b: mk(61) };\n\
                 match s { Od { .. } => { return 9; } } }",
            "dOd\ndR61\ndR60\nz=9\n",
        ),
    ] {
        let src = format!("{H}{cell}\nfn main() {{ let z: i64 = c(); println(f\"z={{z}}\"); }}\n");
        assert_eq!(run_program(&src), Some(want.to_string()), "{label}");
    }
}

/// B-2026-09-21-3 — compiled twin of `tests/interpreter.rs`'s
/// `while_let_struct_scrutinee_binding_runs_its_drop_body`, same programs
/// and same expectations.
///
/// This backend was correct on every cell; the INTERPRETER ran no `Drop`
/// body at all for a `while let` over a struct scrutinee. The twin exists
/// because the property being pinned is that the two AGREE — a fixture on
/// the already-correct side alone would keep passing while the other
/// drifted, which is exactly how this defect reached a filed row.
///
/// `whilelet-none-bound-husk` WAS `whilelet-none-bound-agreed-gap`, pinned
/// at `z=3\n` — no body at all where two are due — and B-2026-09-21-1
/// flipped it here
/// too: the husk's unbound fields run nothing on any surface, and closing
/// that is B-2026-09-21-1's job on both backends together. See the
/// interpreter twin's doc for why arming one side alone is worse than the
/// gap.
#[test]
fn test_e2e_while_let_struct_scrutinee_binding_runs_its_drop_body() {
    const H: &str = "struct R { id: i64, name: String }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\") } }\n\
             fn mk(i: i64) -> R { return R { id: i, name: f\"n{i}\" }; }\n\
             struct S3 { a: R, b: R }\n\
             struct One { r: R }\n\
             struct Nest { o: One, z: i64 }\n\
             enum E { Full(R), Empty }\n";
    for (label, cell, want) in [
            (
                "whilelet-one-bound",
                "fn c() -> i64 { let mut s: i64 = 0; while let S3 { a, .. } = S3 { a: mk(76), b: mk(77) } { s = a.id; break; } return s; }",
                "dR76\ndR77\nz=76\n",
            ),
            (
                "whilelet-all-bound",
                "fn c() -> i64 { let mut s: i64 = 0; while let S3 { a, b } = S3 { a: mk(78), b: mk(79) } { s = a.id + b.id; break; } return s; }",
                "dR79\ndR78\nz=157\n",
            ),
            (
                "whilelet-none-bound-husk",
                "fn c() -> i64 { let mut s: i64 = 0; while let S3 { .. } = S3 { a: mk(82), b: mk(83) } { s = 3; break; } return s; }",
                "dR83\ndR82\nz=3\n",
            ),
            (
                "whilelet-named-scrutinee",
                "fn c() -> i64 { let n: One = One { r: mk(86) }; let mut s: i64 = 0; while let One { r } = n { s = r.id; break; } return s; }",
                "dR86\nz=86\n",
            ),
            (
                "whilelet-nested-pattern",
                "fn c() -> i64 { let mut s: i64 = 0; while let Nest { o: One { r }, .. } = Nest { o: One { r: mk(88) }, z: 5 } { s = r.id; break; } return s; }",
                "dR88\nz=88\n",
            ),
            (
                "whilelet-two-iterations",
                "fn c() -> i64 { let mut n: i64 = 0; let mut s: i64 = 0; while let One { r } = One { r: mk(90 + n) } { s = s + r.id; n = n + 1; if n > 1 { break; } } return s; }",
                "dR90\ndR91\nz=181\n",
            ),
            (
                "whilelet-enum-control",
                "fn c() -> i64 { let mut v: Vec[R] = Vec.new(); v.push(mk(94)); v.push(mk(95)); let mut s: i64 = 0; while let Some(r) = v.pop() { s = s + r.id; } return s; }",
                "dR95\ndR94\nz=189\n",
            ),
            (
                "whilelet-user-enum-control",
                "fn c() -> i64 { let mut s: i64 = 0; while let E.Full(r) = E.Full(mk(96)) { s = r.id; break; } return s; }",
                "dR96\nz=96\n",
            ),
            (
                "iflet-sibling-control",
                "fn c() -> i64 { if let One { r } = One { r: mk(97) } { return r.id; } return 0; }",
                "dR97\nz=97\n",
            ),
            (
                "match-sibling-control",
                "fn c() -> i64 { match One { r: mk(99) } { One { r } => { return r.id; } } }",
                "dR99\nz=99\n",
            ),
        ] {
            let src =
                format!("{H}{cell}\nfn main() {{ let z: i64 = c(); println(f\"z={{z}}\"); }}\n");
            assert_eq!(run_program(&src), Some(want.to_string()), "{label}");
        }
}

/// B-2026-08-31-47 — compiled twin of `tests/interpreter.rs`'s
/// `method_fresh_temp_enum_arg_arm_binds_payload`, same programs and
/// expectations.
///
/// This backend was correct on every row; the interpreter lost the body for
/// the BINDING arm. The twin exists because the property is that the two
/// agree — a fixture on the fixed side alone would keep passing while this
/// one drifted.
#[test]
fn test_e2e_method_fresh_temp_enum_arg_arm_binds_payload() {
    const H: &str = "struct R { id: i64, name: String }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"drop {self.id}\") } }\n\
             enum Box2 { Full(R), Empty }\n\
             struct T { n: i64 }\n\
             fn mk(i: i64) -> R { return R { id: i, name: f\"h{i}\" }; }\n\
             impl T {\n\
             \x20   fn nomatch(ref self, b: Box2) -> i64 { return 1; }\n\
             \x20   fn wild(ref self, b: Box2) -> i64 {\n\
             \x20       match b { Box2.Full(_) => { return 2; } Box2.Empty => { return 0; } } }\n\
             \x20   fn bind(ref self, b: Box2) -> i64 {\n\
             \x20       match b { Box2.Full(r) => { return r.id; } Box2.Empty => { return 0; } } }\n\
             \x20   fn handback(ref self, b: Box2) -> R {\n\
             \x20       match b { Box2.Full(r) => { return r; } Box2.Empty => { return mk(0); } } }\n\
             }\n";
    for (label, body, want) in [
        (
            "nomatch",
            "let n = t.nomatch(Box2.Full(mk(1))); println(f\"n{n}\");",
            "drop 1\nn1\n",
        ),
        (
            "wildcard-arm",
            "let n = t.wild(Box2.Full(mk(2))); println(f\"n{n}\");",
            "drop 2\nn2\n",
        ),
        (
            "binding-arm",
            "let n = t.bind(Box2.Full(mk(3))); println(f\"n{n}\");",
            "drop 3\nn3\n",
        ),
        (
            "handed-back",
            "let r = t.handback(Box2.Full(mk(4))); println(f\"n{r.id}\");",
            "n4\ndrop 4\n",
        ),
    ] {
        let src = format!("{H}fn main() {{\nlet t: T = T {{ n: 1 }};\n{body}\n}}\n");
        assert_eq!(run_program(&src), Some(want.to_string()), "{label}");
    }
}

#[test]
fn test_e2e_file_open_nonexistent_match_arm_io_error_not_found() {
    // Direct match on the IoError variant (vs. F5's
    // ?-propagation through a helper). Pins the variant-tag value
    // codegen emits matches the source-order tag assignment.
    let out = run_program(
        r#"
fn main() with reads(FileSystem) {
    match File.open("/nonexistent_karac_f6_test.txt") {
        Ok(_) => println("unexpected-ok"),
        Err(e) => match e {
            IoError.NotFound => println("not-found"),
            IoError.PermissionDenied => println("permission-denied"),
            _ => println("other"),
        },
    }
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "not-found");
    }
}

// Regression guards documenting that the match-arm tail-binding path
// (originally suspected as follow-on (b)) was never broken: a real
// `Result` scrutinee binds correctly both as a bare tail expression
// and inside a block body. The earlier failure was solely the
// missing read_to_string lowering above (an i64 scrutinee).
#[test]
fn test_ir_match_tail_binding_real_result_ok() {
    let ir = ir_for(
        r#"
fn unwrap_or_zero(r: Result[i64, i64]) -> i64 {
    match r {
        Ok(b) => b,
        Err(_) => 0,
    }
}
"#,
    );
    assert!(function_body(&ir, "unwrap_or_zero").is_some());
}

#[test]
fn test_ir_match_block_binding_real_result_ok() {
    let ir = ir_for(
        r#"
fn unwrap_or_zero(r: Result[i64, i64]) -> i64 {
    match r {
        Ok(b) => { b }
        Err(_) => 0,
    }
}
"#,
    );
    assert!(function_body(&ir, "unwrap_or_zero").is_some());
}

#[test]
fn test_e2e_main_exitcode_canonical_match_ok_unit() {
    // The canonical design.md § Entry Point shape: match a fallible
    // result, `Ok(())` → SUCCESS, `Err(e)` → a custom code. Exercises
    // the `Ok(())` unit-payload pattern (exhaustiveness + match codegen)
    // together with the `ExitCode` arms — both must agree on `i32` width
    // so the match phi merges.
    let cap = run_program_capturing(
        r#"
fn run(ok: bool) -> Result[(), String] {
    if ok { Ok(()) } else { Err("boom") }
}
fn main() -> ExitCode {
    match run(false) {
        Ok(()) => ExitCode.SUCCESS,
        Err(e) => {
            eprintln("Error: {e}");
            ExitCode.from(3)
        }
    }
}
"#,
    );
    if let Some(cap) = cap {
        assert_eq!(cap.status.code(), Some(3), "stderr={:?}", cap.stderr);
    }
}

/// B-2026-08-25-12 — `match f() { Ok(a) => .. }` over a call whose Ok
/// payload is WIDER than `Result`'s 5-word inline area segfaulted. The
/// payload is heap-BOXED (a pointer in w0), but the fresh-temp scrutinee
/// registered the INLINE cleanup, whose drop runs at `&slot.w0` and reads
/// that pointer word as the payload struct's first word. With three
/// `Vec` fields the read ran off the end of the 6-word `Result` alloca and
/// `free`d whatever followed. B-2026-08-06-26 established the class and
/// gated the named-binding path; this is the same gate reaching the other
/// two registration sites.
///
/// Three Vecs is the smallest shape that faults — two stay inside the
/// alloca and mis-free only benign zeros — so the count is load-bearing,
/// not incidental. `std.cli`'s `Args` has exactly three.
#[test]
fn test_e2e_match_ok_of_boxed_wide_result_payload_compiles() {
    let out = run_program(
        r#"
struct Err1 { message: String }
struct Out { v1: Vec[String], v2: Vec[String], v3: Vec[String], tag: i64 }
struct Src { name: String }

impl Src {
    fn go(ref self) -> Result[Out, Err1] {
        return Ok(Out { v1: [], v2: [], v3: [], tag: 7 });
    }
}

fn main() {
    let s = Src { name: "d" };
    match s.go() {
        Ok(a) => { println(a.tag); }
        Err(e) => { println(e.message); }
    }
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "7");
    }
}

/// The same shape carrying REAL heap in the boxed payload, so the arm has
/// something to own and the suppressed inline drop cannot be mistaken for
/// "nothing needed freeing". Pairs with the memory_sanitizer twin, which
/// is what proves the box is still freed exactly once.
#[test]
fn test_e2e_match_ok_of_boxed_wide_result_payload_with_heap_reads_back() {
    let out = run_program(
        r#"
struct Err1 { message: String }
struct Out { v1: Vec[String], v2: Vec[String], v3: Vec[String], tag: i64 }

fn go() -> Result[Out, Err1] {
    let mut a: Vec[String] = Vec.new();
    a.push("alpha");
    let mut b: Vec[String] = Vec.new();
    b.push("beta");
    return Ok(Out { v1: a, v2: b, v3: [], tag: 1 });
}

fn main() {
    match go() {
        Ok(a) => {
            println(a.v1.len());
            match a.v2.get(0) { Some(s) => { println(s); } None => { } }
        }
        Err(e) => { println(e.message); }
    }
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(
            out.trim(),
            "1
beta"
        );
    }
}

mod enum_payload_literal_patterns_b_2026_08_20_11 {
    //! B-2026-08-20-11 — a payload sub-pattern that tests a VALUE rather
    //! than a tag was dropped entirely by codegen, so the arm matched on
    //! the OUTER TAG ALONE: `match Some(5) { Some(7) => .., _ => .. }`
    //! took the `Some(7)` arm. Silent, and build-only — the interpreter
    //! compares properly — so it was invisible to anyone developing with
    //! `karac run`.
    //!
    //! `and_in_nested_variant_conditions` keyed exclusively off
    //! `variant_pattern_enum_and_tag`, both in its early-return gate and
    //! via `else { continue }` in its loop. A literal / range / tuple /
    //! struct / at-binding sub-pattern answers `None` there, so no payload
    //! was ever reconstructed and no test emitted. The fix adds
    //! `pattern_tests_payload_value` as a second admission key and hands
    //! the rebuilt payload back to `compile_pattern_condition`.
    //!
    //! EVERY ASSERT BELOW IS A MISS ARM. Measured on a pre-fix build, all
    //! of the hits already passed and all of the misses were wrong — a
    //! hit-only test is vacuous for this bug, which is exactly why it
    //! survived. Strict `assert_eq!(.., Some(..))` (not the tolerant
    //! `if let Some(out)`) so a missing runtime archive fails loudly
    //! instead of green-skipping the whole class again.
    use super::run_program;

    #[test]
    fn e2e_option_integer_literal_payload_does_not_match_every_some() {
        // The filed repro, both directions.
        assert_eq!(
            run_program(
                "fn cls(p: Option[i64]) -> String {\n\
                         match p { Some(7) => \"seven\", Some(0) => \"zero\", _ => \"other\" }\n\
                     }\n\
                     fn main() {\n\
                         println(cls(Some(5i64)));\n\
                         println(cls(Some(7i64)));\n\
                         println(cls(Some(0i64)));\n\
                         println(cls(None));\n\
                     }\n"
            )
            .as_deref(),
            Some("other\nseven\nzero\nother\n"),
            "Some(<int literal>) must compare the payload, not just the tag"
        );
    }

    #[test]
    fn e2e_payload_literal_across_every_scalar_kind() {
        // char / bool / float / String each reconstruct differently:
        // float needs a bitcast off the i64 payload word and String needs
        // the 3-word vec shape, neither of which has a
        // `pattern_binding_types` entry (a literal binds nothing).
        assert_eq!(
                run_program(
                    "fn ch(p: Option[char]) -> String { match p { Some('a') => \"A\", _ => \"z\" } }\n\
                     fn bo(p: Option[bool]) -> String { match p { Some(true) => \"T\", _ => \"F\" } }\n\
                     fn fl(p: Option[f64]) -> String { match p { Some(1.5) => \"f\", _ => \"o\" } }\n\
                     fn st(p: Option[String]) -> String { match p { Some(\"ok\") => \"OK\", _ => \"no\" } }\n\
                     fn main() {\n\
                         println(ch(Some('a'))); println(ch(Some('b')));\n\
                         println(bo(Some(true))); println(bo(Some(false)));\n\
                         println(fl(Some(1.5f64))); println(fl(Some(2.5f64)));\n\
                         println(st(Some(\"ok\"))); println(st(Some(\"nope\")));\n\
                     }\n"
                )
                .as_deref(),
                Some("A\nz\nT\nF\nf\no\nOK\nno\n"),
                "char / bool / float / String payload literals must all discriminate"
            );
    }

    #[test]
    fn e2e_payload_range_and_or_patterns_discriminate() {
        assert_eq!(
                run_program(
                    "fn rng(p: Option[i64]) -> String { match p { Some(1..=5) => \"low\", _ => \"hi\" } }\n\
                     fn orp(p: Option[i64]) -> String { match p { Some(1 | 2) => \"12\", _ => \"o\" } }\n\
                     fn main() {\n\
                         println(rng(Some(3i64))); println(rng(Some(9i64)));\n\
                         println(orp(Some(2i64))); println(orp(Some(3i64)));\n\
                     }\n"
                )
                .as_deref(),
                Some("low\nhi\n12\no\n"),
                "range and or- payload sub-patterns route through the same gate as literals"
            );
    }

    #[test]
    fn e2e_payload_literal_in_user_enum_tuple_struct_and_at_binding() {
        // Compound payloads: a partially-literal tuple (`Pair(1, y)`), a
        // struct field literal, and an at-binding. Pre-fix every one of
        // these miss arms fell into the first literal arm.
        assert_eq!(
                run_program(
                    "enum Msg { Ping(i64), Named(String), Pair(i64, i64), Nothing }\n\
                     struct P { n: i64, m: i64 }\n\
                     enum Holder { H(P) }\n\
                     fn m1(v: Msg) -> String {\n\
                         match v {\n\
                             Msg.Ping(0) => \"ping0\",\n\
                             Msg.Ping(n) => \"pingN\",\n\
                             Msg.Named(\"hi\") => \"hi\",\n\
                             Msg.Named(s) => \"namedN\",\n\
                             Msg.Pair(1, 2) => \"p12\",\n\
                             Msg.Pair(1, y) => \"p1y\",\n\
                             Msg.Pair(x, y) => \"pXY\",\n\
                             Msg.Nothing => \"none\",\n\
                         }\n\
                     }\n\
                     fn m2(v: Holder) -> String {\n\
                         match v { Holder.H(P { n: 1, m: _ }) => \"n1\", Holder.H(p) => \"nX\" }\n\
                     }\n\
                     fn at(v: Option[i64]) -> String { match v { Some(x @ 5) => \"at5\", _ => \"o\" } }\n\
                     fn main() {\n\
                         println(m1(Msg.Ping(0))); println(m1(Msg.Ping(9)));\n\
                         println(m1(Msg.Named(\"hi\"))); println(m1(Msg.Named(\"yo\")));\n\
                         println(m1(Msg.Pair(1,2))); println(m1(Msg.Pair(1,3))); println(m1(Msg.Pair(4,5)));\n\
                         println(m1(Msg.Nothing));\n\
                         println(m2(Holder.H(P { n: 1, m: 2 }))); println(m2(Holder.H(P { n: 7, m: 2 })));\n\
                         println(at(Some(5i64))); println(at(Some(6i64)));\n\
                     }\n"
                )
                .as_deref(),
                Some("ping0\npingN\nhi\nnamedN\np12\np1y\npXY\nnone\nn1\nnX\nat5\no\n"),
                "tuple / struct / at-binding payload literals must each emit their own test"
            );
    }

    #[test]
    fn e2e_payload_literal_narrow_ints_result_and_nested_option() {
        // Narrow ints exercise the trunc path off the i64 payload word;
        // Result and Option-in-Option confirm the fix is not Option-i64
        // specific (the shape the row was filed against).
        assert_eq!(
                run_program(
                    "fn narrow(v: Option[i32]) -> String { match v { Some(300) => \"a\", _ => \"o\" } }\n\
                     fn byte(v: Option[u8]) -> String { match v { Some(200) => \"b\", _ => \"o\" } }\n\
                     fn nested(v: Option[Option[i64]]) -> String {\n\
                         match v { Some(Some(5)) => \"s5\", Some(Some(n)) => \"sn\", Some(None) => \"sN\", None => \"n\" }\n\
                     }\n\
                     fn resu(v: Result[i64, String]) -> String {\n\
                         match v { Ok(1) => \"ok1\", Ok(n) => \"okN\", Err(\"bad\") => \"eb\", Err(e) => \"eN\" }\n\
                     }\n\
                     fn main() {\n\
                         println(narrow(Some(300i32))); println(narrow(Some(7i32)));\n\
                         println(byte(Some(200u8))); println(byte(Some(7u8)));\n\
                         println(nested(Some(Some(5i64)))); println(nested(Some(Some(6i64))));\n\
                         println(nested(Some(None))); println(nested(None));\n\
                         println(resu(Ok(1i64))); println(resu(Ok(2i64)));\n\
                         println(resu(Err(\"bad\"))); println(resu(Err(\"z\")));\n\
                     }\n"
                )
                .as_deref(),
                Some("a\no\nb\no\ns5\nsn\nsN\nn\nok1\nokN\neb\neN\n"),
                "narrow-int, Result and nested-Option payload literals all discriminate"
            );
    }

    #[test]
    fn e2e_binding_and_wildcard_payloads_still_always_match() {
        // The other direction: `pattern_tests_payload_value` must answer
        // `false` for leaves that only NAME the payload, or the new gate
        // would reconstruct (and test) where nothing should be tested.
        assert_eq!(
                run_program(
                    "fn b(p: Option[i64]) -> String { match p { Some(x) => f\"got {x}\", None => \"none\" } }\n\
                     fn w(p: Option[i64]) -> String { match p { Some(_) => \"some\", None => \"none\" } }\n\
                     fn main() {\n\
                         println(b(Some(5i64))); println(b(None));\n\
                         println(w(Some(5i64))); println(w(None));\n\
                     }\n"
                )
                .as_deref(),
                Some("got 5\nnone\nsome\nnone\n"),
                "binding / wildcard payloads must keep matching unconditionally"
            );
    }
}

mod nested_enum_payload_patterns_b_2026_07_15_5 {
    //! B-2026-07-15-5 — nested enum-variant PATTERNS over heap-BOXED enum
    //! payloads. An inner `Option[i64]` value is 4 words; Option's seeded
    //! payload area is 3 and the enum-in-enum carve-out sizes a user enum's
    //! enum-typed payload area to 1 — both pack sides heap-box the inner
    //! value (box pointer in payload word 0). The match side never deboxed:
    //! `pattern_payload_word_count` / `pattern_payload_llvm_type` defaulted
    //! a `TupleVariant` (and unit-variant `Binding`) sub-pattern to
    //! 1 word / i64, so `reconstruct_payload_value`'s debox predicate
    //! (`want > field_words.len()`) never fired. The nested tag check then
    //! compared the BOX POINTER against the tag — `Option.Some(Option.Some(x))`
    //! silently took the wrong arm (interp: `inner 42`, codegen: `other`),
    //! `Wrap.W(Option.Some(x))` bound garbage (`got 0`), and multi-arm
    //! variants segfaulted. The fix adds enum-width arms for variant-shaped
    //! sub-patterns to both helpers and gates the nested checks in
    //! `and_in_nested_variant_conditions` behind the outer tag comparison
    //! (branch + phi) so the debox load can't NULL-deref a non-matching
    //! variant's zero payload words.
    use super::run_program;

    #[test]
    fn test_e2e_option_in_option_nested_pattern_all_shapes() {
        if let Some(out) = run_program(
            "fn classify(v: Option[Option[i64]]) -> String {\n\
                     match v {\n\
                         Option.Some(Option.Some(x)) => f\"inner {x}\",\n\
                         Option.Some(Option.None) => \"inner none\",\n\
                         Option.None => \"outer none\",\n\
                     }\n\
                 }\n\
                 fn main() {\n\
                     println(classify(Option.Some(Option.Some(42))));\n\
                     println(classify(Option.Some(Option.None)));\n\
                     println(classify(Option.None));\n\
                     // wildcard fallthrough — the exact wrong-arm repro\n\
                     let nested: Option[Option[i64]] = Option.Some(Option.Some(7));\n\
                     match nested {\n\
                         Option.Some(Option.Some(x)) => println(f\"got {x}\"),\n\
                         _ => println(\"other\"),\n\
                     }\n\
                 }",
        ) {
            assert_eq!(out.trim(), "inner 42\ninner none\nouter none\ngot 7");
        }
    }

    #[test]
    fn test_e2e_user_enum_wrapping_option_nested_pattern_bind() {
        // The garbage-bind leg: `Wrap`'s enum-typed payload area is the
        // 1-word carve-out fallback, so the inner Option is always boxed.
        if let Some(out) = run_program(
            "enum Wrap {\n\
                     W(Option[i64]),\n\
                     Empty,\n\
                 }\n\
                 fn describe(w: Wrap) -> String {\n\
                     match w {\n\
                         Wrap.W(Option.Some(x)) => f\"some {x}\",\n\
                         Wrap.W(Option.None) => \"w none\",\n\
                         Wrap.Empty => \"empty\",\n\
                     }\n\
                 }\n\
                 fn main() {\n\
                     println(describe(Wrap.W(Option.Some(5))));\n\
                     println(describe(Wrap.W(Option.None)));\n\
                     println(describe(Wrap.Empty));\n\
                 }",
        ) {
            assert_eq!(out.trim(), "some 5\nw none\nempty");
        }
    }

    #[test]
    fn test_e2e_triple_nested_option_and_moved_string_payload() {
        // Triple nesting exercises the recursive lazy gate (each level's
        // debox load must be reached only after every enclosing tag
        // matched); the String leg moves a heap payload out of the box.
        if let Some(out) = run_program(
                "fn main() {\n\
                     let deep: Option[Option[Option[i64]]] = Option.Some(Option.Some(Option.Some(99)));\n\
                     match deep {\n\
                         Option.Some(Option.Some(Option.Some(x))) => println(f\"deep {x}\"),\n\
                         Option.Some(Option.Some(Option.None)) => println(\"l3 none\"),\n\
                         Option.Some(Option.None) => println(\"l2 none\"),\n\
                         Option.None => println(\"l1 none\"),\n\
                     }\n\
                     let mid: Option[Option[Option[i64]]] = Option.Some(Option.Some(Option.None));\n\
                     match mid {\n\
                         Option.Some(Option.Some(Option.Some(x))) => println(f\"deep {x}\"),\n\
                         Option.Some(Option.Some(Option.None)) => println(\"l3 none\"),\n\
                         Option.Some(Option.None) => println(\"l2 none\"),\n\
                         Option.None => println(\"l1 none\"),\n\
                     }\n\
                     let s: Option[Option[String]] = Option.Some(Option.Some(\"moved\"));\n\
                     match s {\n\
                         Option.Some(Option.Some(v)) => {\n\
                             let joined: String = v + \"!\";\n\
                             println(joined);\n\
                         },\n\
                         _ => println(\"other\"),\n\
                     }\n\
                 }",
            ) {
                assert_eq!(out.trim(), "deep 99\nl3 none\nmoved!");
            }
    }
}

/// B-2026-08-30-19 — A READ-ONLY `match` ARM OVER AN `Option`/`Result`
/// WHOSE STRUCT PAYLOAD CARRIES HEAP TOOK THE PAYLOAD, so a later read of
/// the source was a USE-AFTER-FREE.
///
/// B-2026-08-08-25 made such an arm a BORROW so the source keeps its
/// payload, but its classification admitted only a DIRECT `{ptr,len,cap}`
/// payload — `pattern_binds_direct_inline_heap_payload` required the bound
/// type to be `String` or `Vec`. A struct that merely CONTAINS one was not
/// admitted, so the arm still took it and the second read ran off a freed
/// block: `hello` then garbage on the JIT and both AOT legs, while
/// `--interp` was correct — the run-vs-build signature. Silent, not a
/// crash: the program printed and exited 0.
///
/// The five shapes here are the measured boundary. The first four were
/// BROKEN and each failed differently, which is why all four are pinned
/// rather than one standing in for the rest:
///
///   `Option[S { s: String }]`   valgrind "invalid read of size 2", garbage
///   `Option[S { v: Vec[i64] }]` invalid read — but ONLY through `v[i]`;
///                               `v.len()` reads a length field that
///                               survives the free and looks correct
///   `Option[Outer{Inner{..}}]`  the same failure one hop further down, so
///                               a one-hop fix would still miss it
///   `Result[S { s: String }]`   valgrind-CLEAN and prints empty — the
///                               payload is moved out and zeroed rather
///                               than freed-then-read, so a fix verified
///                               only against the `Option` leg would look
///                               done while this stayed broken
///
/// The last two are CONTROLS, and both were already correct:
///
///   `Option[S { n: i64 }]`      the fix admits a struct only when it
///                               transitively owns heap, so a change that
///                               admitted every struct would still pass the
///                               four above while moving this one off the
///                               path it takes correctly today
///   `Option[shared struct]`     a `shared struct` payload is an RC box, a
///                               different ownership regime — its field read
///                               is deep-cloned (B-2026-08-13-6) because the
///                               box may have other handles. Admitting it as
///                               a borrow drops an owner the RC path still
///                               expects, which is not hypothetical: the
///                               first version of this fix did exactly that
///                               and turned
///                               `asan_shared_struct_heap_field_read_cloned_not_aliased`
///                               into an ASAN memory error while every
///                               value-struct shape stayed correct.
///
/// ASAN twin: `tests/memory_sanitizer.rs::asan_optres_struct_payload_read_only_arm_is_a_borrow`.
#[test]
fn e2e_optres_struct_payload_read_only_arm_does_not_take_the_payload() {
    if let Some(out) = run_program(
        r#"
struct Sstr { s: String }
struct Svec { v: Vec[i64] }
struct Sint { n: i64 }
struct Inner { s: String }
struct Outer { i: Inner }
shared struct Shr { w: String }
fn main() {
    let a: Option[Sstr] = Some(Sstr { s: f"alpha" });
    match a { Some(p) => { println(p.s); } None => {} }
    match a { Some(q) => println(q.s), None => println("no-a") }

    let b: Option[Svec] = Some(Svec { v: [11, 22, 33] });
    match b { Some(p) => { println(p.v[0]); } None => {} }
    match b { Some(q) => println(q.v[2]), None => println("no-b") }

    let c: Option[Outer] = Some(Outer { i: Inner { s: f"nested" } });
    match c { Some(p) => { println(p.i.s); } None => {} }
    match c { Some(q) => println(q.i.s), None => println("no-c") }

    let d: Result[Sstr, i64] = Ok(Sstr { s: f"okstr" });
    match d { Ok(p) => { println(p.s); } Err(_) => {} }
    match d { Ok(q) => println(q.s), Err(_) => println("no-d") }

    let e: Option[Sint] = Some(Sint { n: 42 });
    match e { Some(p) => { println(p.n); } None => {} }
    match e { Some(q) => println(q.n), None => println("no-e") }

    let g: Option[Shr] = Some(Shr { w: f"shared" });
    match g { Some(p) => { println(p.w); } None => {} }
    match g { Some(q) => println(q.w), None => println("no-g") }
}
"#,
    ) {
        assert_eq!(
            out,
            "alpha\nalpha\n11\n33\nnested\nnested\nokstr\nokstr\n42\n42\nshared\nshared\n"
        );
    }
}

/// B-2026-08-30-47, PARTIAL — a read-only `match` arm over an
/// `Option`/`Result` whose payload is a HEAP-OWNING NON-STRUCT.
///
/// B-2026-08-30-19 taught `pattern_binds_direct_inline_heap_payload` about
/// a user struct that transitively owns heap. Its ENTRY POINT stayed
/// struct-only, though, while the field walker one level down was already
/// conservative — so `Result[Map[i64, String]]` matched read-only twice
/// SEGFAULTED, and that is the row asserted here.
///
/// The three `Option[Map]` / `[Set]` / `[SortedMap]` rows come from the
/// second half of the same row. An `Option`/`Result` payload is owned
/// through one of four parallel channels, each with its own registry and
/// scope-exit action, and the read-only classifier knew about two of them
/// — so those three took the payload and the source read back as `None`.
/// Widening the membership predicate is only half of that: each channel's
/// disarm must also honour the retains flag, or the classifier decides
/// "borrow" and the source is disarmed anyway.
///
/// WHAT IS STILL BROKEN is a DIFFERENT defect and is deliberately not
/// here: a read-only arm that DESTRUCTURES the payload rather than binding
/// it whole (`Some(S { s })`, `Some((s, n))`, or an inner `match` over the
/// bound payload) still takes the heap, on EVERY representation — struct
/// payloads read back garbage, tuple and enum payloads abort with a double
/// free. The classifier requires a whole-payload `Binding` by construction,
/// so no widening of it reaches that shape. See B-2026-08-30-52.
///
/// The rest of the rows are CONTROLS that were already correct and must
/// stay correct — this change makes the classifier admit MORE, so the way
/// it can go wrong is by admitting something whose owner it then drops.
/// `Option[shared struct]` is the sharpest of them (an RC box, whose field
/// read is deep-cloned) and a payload-free enum is the second: a blanket
/// "an enum owns heap" rule would move it off the path it takes correctly.
#[test]
fn e2e_optres_nonstruct_heap_payload_read_only_arm_does_not_take_the_payload() {
    let cases: &[(&str, &str, &str)] = &[
            (
                "Result[Map] — segfaulted before this change",
                "fn main() {\n\
                     let mut m0: Map[i64, String] = Map.new();\n\
                     m0.insert(1, f\"rm\");\n\
                     let a: Result[Map[i64, String], i64] = Ok(m0);\n\
                     match a { Ok(p) => { println(p.len()); } Err(_) => {} }\n\
                     match a { Ok(q) => println(q.len()), Err(_) => println(\"err\") }\n\
                 }\n",
                "1\n1\n",
            ),
            (
                "Option[Map] — read back as None before B-2026-08-30-47",
                "fn main() {\n\
                     let mut m0: Map[i64, String] = Map.new();\n\
                     m0.insert(1, f\"mv\");\n\
                     let a: Option[Map[i64, String]] = Some(m0);\n\
                     match a { Some(p) => { println(p.len()); } None => {} }\n\
                     match a { Some(q) => println(q.len()), None => println(\"none\") }\n\
                 }\n",
                "1\n1\n",
            ),
            (
                "Option[Set] — same channel as Option[Map]",
                "fn main() {\n\
                     let mut s0: Set[String] = Set.new();\n\
                     s0.insert(f\"sv\");\n\
                     let a: Option[Set[String]] = Some(s0);\n\
                     match a { Some(p) => { println(p.len()); } None => {} }\n\
                     match a { Some(q) => println(q.len()), None => println(\"none\") }\n\
                 }\n",
                "1\n1\n",
            ),
            (
                "Option[SortedMap] — same channel again",
                "fn main() {\n\
                     let mut m0: SortedMap[i64, String] = SortedMap.new();\n\
                     m0.insert(1, f\"sv\");\n\
                     let a: Option[SortedMap[i64, String]] = Some(m0);\n\
                     match a { Some(p) => { println(p.len()); } None => {} }\n\
                     match a { Some(q) => println(q.len()), None => println(\"none\") }\n\
                 }\n",
                "1\n1\n",
            ),
            // The `if let` spelling, and the ONLY row that exercises the
            // retains guard on the `Map` channel's disarm: the `match` path
            // reaches that suppressor only with owned bindings, so every
            // `match` row above passes with the guard removed and this one
            // does not.
            (
                "Option[Map] via if let — needs the disarm guard",
                "fn main() {\n\
                     let mut m0: Map[i64, String] = Map.new();\n\
                     m0.insert(1, f\"mv\");\n\
                     let a: Option[Map[i64, String]] = Some(m0);\n\
                     if let Some(p) = a { println(p.len()); }\n\
                     if let Some(q) = a { println(q.len()); } else { println(\"none\"); }\n\
                 }\n",
                "1\n1\n",
            ),
            (
                "control: Option[Vec[String]]",
                "fn main() {\n\
                     let a: Option[Vec[String]] = Some([f\"x\", f\"y\"]);\n\
                     match a { Some(p) => { println(p.len()); } None => {} }\n\
                     match a { Some(q) => println(q[1]), None => println(\"none\") }\n\
                 }\n",
                "2\ny\n",
            ),
            (
                "control: Option[enum with no heap] must stay OUT",
                "enum F { A { n: i64 }, B }\n\
                 fn main() {\n\
                     let a: Option[F] = Some(F.A { n: 7 });\n\
                     match a { Some(p) => { match p { A { n } => println(n), B => println(\"b\") } } None => {} }\n\
                     match a { Some(q) => { match q { A { n } => println(n), B => println(\"b\") } } None => {} }\n\
                 }\n",
                "7\n7\n",
            ),
            (
                "control: Option[Option[i64]] must stay OUT",
                "fn main() {\n\
                     let a: Option[Option[i64]] = Some(Some(5));\n\
                     match a { Some(p) => { match p { Some(s) => println(s), None => println(\"in\") } } None => {} }\n\
                     match a { Some(q) => { match q { Some(s) => println(s), None => println(\"in\") } } None => {} }\n\
                 }\n",
                "5\n5\n",
            ),
            (
                "control: Option[shared struct] — an RC box, must stay OUT",
                "shared struct Shr { w: String }\n\
                 fn main() {\n\
                     let a: Option[Shr] = Some(Shr { w: f\"sh\" });\n\
                     match a { Some(p) => { println(p.w); } None => {} }\n\
                     match a { Some(q) => println(q.w), None => println(\"none\") }\n\
                 }\n",
                "sh\nsh\n",
            ),
            (
                "control: Option[scalar] — no owner to drop",
                "fn main() {\n\
                     let a: Option[i64] = Some(9);\n\
                     match a { Some(p) => { println(p); } None => {} }\n\
                     match a { Some(q) => println(q), None => println(\"none\") }\n\
                     let b: Option[u8] = Some(9u8);\n\
                     match b { Some(p) => { println(p); } None => {} }\n\
                     match b { Some(q) => println(q), None => println(\"none\") }\n\
                 }\n",
                "9\n9\n9\n9\n",
            ),
            (
                "control: Option[struct with heap] — the shape B-2026-08-30-19 fixed",
                "struct Sstr { s: String }\n\
                 fn main() {\n\
                     let a: Option[Sstr] = Some(Sstr { s: f\"ctrl\" });\n\
                     match a { Some(p) => { println(p.s); } None => {} }\n\
                     match a { Some(q) => println(q.s), None => println(\"none\") }\n\
                 }\n",
                "ctrl\nctrl\n",
            ),
        ];
    for (label, src, want) in cases {
        let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
        assert!(
            interp_errs.is_empty(),
            "{label}: interpreter errored: {interp_errs:?}"
        );
        assert_eq!(
            interp_out.join(""),
            *want,
            "{label}: the interpreter is the reference for this text"
        );
        if let Some(aot) = run_program(src) {
            assert_eq!(
                aot, *want,
                "{label}: the compiled backend must keep the source's payload \
                     across a read-only arm"
            );
        }
    }
}

/// B-2026-08-30-52, PARTIAL — a read-only `match` / `if let` arm that
/// DESTRUCTURES the `Option`/`Result` payload rather than binding it whole.
///
/// `pattern_binds_direct_inline_heap_payload` requires every
/// payload-consuming sub-pattern to be a plain `Binding`, so a
/// destructuring pattern failed the read-only classifier BY CONSTRUCTION,
/// the arm was never a borrow, and the transfer path ran: the destructured
/// leaf registered as an owner and freed at arm exit while the source's own
/// drop stayed armed. `Option[S { s: String }]` read back garbage;
/// `Option[(String, i64)]` aborted with a double free.
///
/// TWO INDEPENDENT HALVES, and each is needed for a different row below.
/// `pattern_destructures_heap_payload` admits the pattern, judged from the
/// scrutinee's instantiation rather than the leaves — a shorthand field
/// (`S { s }`) carries no sub-`Pattern`, so there is no span to look up.
/// And the payload channel: a payload wider than the payload words is
/// heap-BOXED, which is the channel most of these ride, so
/// `scrutinee_is_boxed_optres_local` admits it — but ONLY together with a
/// destructuring pattern. Admitting the boxed channel wholesale broke five
/// tests, every one a boxed payload bound WHOLE and then moved on or
/// field-moved-out, three of them under ASAN.
///
/// STILL BROKEN, deliberately not asserted here, both filed on the row:
/// an arm that uses TWO bound leaves in one expression
/// (`println(v[0] + n)` where the pattern is `S { v, n }`) — the consume
/// classifier counts the scalar leaf's use as an escape, so the arm is not
/// read-only; and an inner `match` over a whole-bound payload
/// (`Some(p) => match p { A { s } => … }`), where the outer binding's
/// borrow-ness does not propagate into the nested pattern.
///
/// ASAN twin: `asan_optres_destructuring_read_only_arm_is_a_borrow` — the
/// output assertion here only fails a use-after-free when the freed block
/// happens to have been reused.
#[test]
fn e2e_optres_destructuring_read_only_arm_does_not_take_the_payload() {
    let cases: &[(&str, &str, &str)] = &[
        (
            "single-field struct destructure",
            "struct S { s: String }\n\
                 fn main() {\n\
                     let a: Option[S] = Some(S { s: f\"sd\" });\n\
                     match a { Some(S { s }) => { println(s); } None => {} }\n\
                     match a { Some(S { s }) => println(s), None => println(\"none\") }\n\
                 }\n",
            "sd\nsd\n",
        ),
        (
            "multi-field struct destructure — the BOXED channel",
            "struct S { s: String, t: String }\n\
                 fn main() {\n\
                     let a: Option[S] = Some(S { s: f\"aa\", t: f\"bb\" });\n\
                     match a { Some(S { s, t }) => { println(s); } None => {} }\n\
                     match a { Some(S { s, t }) => println(t), None => println(\"none\") }\n\
                 }\n",
            "aa\nbb\n",
        ),
        (
            "`..` rest pattern",
            "struct S { s: String, n: i64 }\n\
                 fn main() {\n\
                     let a: Option[S] = Some(S { s: f\"sn\", n: 5 });\n\
                     match a { Some(S { s, .. }) => { println(s); } None => {} }\n\
                     match a { Some(S { s, .. }) => println(s), None => println(\"none\") }\n\
                 }\n",
            "sn\nsn\n",
        ),
        (
            "tuple payload destructure — double-freed before",
            "fn main() {\n\
                     let a: Option[(String, i64)] = Some((f\"td\", 1));\n\
                     match a { Some((s, n)) => { println(s); } None => {} }\n\
                     match a { Some((s, n)) => println(s), None => println(\"none\") }\n\
                 }\n",
            "td\ntd\n",
        ),
        (
            "nested struct destructure, one hop down",
            "struct Inner { s: String }\n\
                 struct Outer { i: Inner }\n\
                 fn main() {\n\
                     let a: Option[Outer] = Some(Outer { i: Inner { s: f\"deep\" } });\n\
                     match a { Some(Outer { i }) => { println(i.s); } None => {} }\n\
                     match a { Some(Outer { i }) => println(i.s), None => println(\"none\") }\n\
                 }\n",
            "deep\ndeep\n",
        ),
        (
            "Result Ok-destructure",
            "struct S { s: String }\n\
                 fn main() {\n\
                     let a: Result[S, i64] = Ok(S { s: f\"res\" });\n\
                     match a { Ok(S { s }) => { println(s); } Err(_) => {} }\n\
                     match a { Ok(S { s }) => println(s), Err(_) => println(\"err\") }\n\
                 }\n",
            "res\nres\n",
        ),
        (
            "`if let` destructure — its own classifier",
            "struct S { s: String }\n\
                 fn main() {\n\
                     let a: Option[S] = Some(S { s: f\"il\" });\n\
                     if let Some(S { s }) = a { println(s); }\n\
                     if let Some(S { s }) = a { println(s); } else { println(\"none\"); }\n\
                 }\n",
            "il\nil\n",
        ),
        (
            "Vec-typed leaf",
            "struct S { v: Vec[i64] }\n\
                 fn main() {\n\
                     let a: Option[S] = Some(S { v: [11, 22] });\n\
                     match a { Some(S { v }) => { println(v[0]); } None => {} }\n\
                     match a { Some(S { v }) => println(v[1]), None => println(\"none\") }\n\
                 }\n",
            "11\n22\n",
        ),
        // B-2026-08-30-52 sub-mechanism (a): TWO BOUND LEAVES IN ONE
        // EXPRESSION. `n + 1` is a `Call` by the time the escape walker
        // runs (`Path(["i64", "add"])`), so a bare operand looked MOVED and
        // the arm was not read-only — `println(v[0] + n)` read back garbage
        // while `println(v[0] + 1)` on the identical pattern was correct.
        // The walker now reuses `consume_class`'s
        // `is_lowered_primitive_operator`, the predicate B-2026-08-05-3
        // added to the sibling classifier for this exact desugar.
        (
            "two bound leaves in one expression",
            "struct S { v: Vec[i64], n: i64 }\n\
                 fn main() {\n\
                     let a: Option[S] = Some(S { v: [11, 22], n: 5 });\n\
                     match a { Some(S { v, n }) => { println(v[0] + n); } None => {} }\n\
                     match a { Some(S { v, n }) => println(v[1] + n), None => println(\"none\") }\n\
                 }\n",
            "16\n27\n",
        ),
        (
            "control: one bound leaf and a literal — correct before and after",
            "struct S { v: Vec[i64], n: i64 }\n\
                 fn main() {\n\
                     let a: Option[S] = Some(S { v: [11, 22], n: 5 });\n\
                     match a { Some(S { v, n }) => { println(v[0] + 1); } None => {} }\n\
                     match a { Some(S { v, n }) => println(v[1] + 1), None => println(\"none\") }\n\
                 }\n",
            "12\n23\n",
        ),
        // CONTROL: a payload that owns nothing stays OFF the borrow path —
        // `field_te_owns_heap` says so — because it already takes its path
        // correctly and moving it would be a change for no reason.
        (
            "control: heap-free struct payload",
            "struct S { n: i64 }\n\
                 fn main() {\n\
                     let a: Option[S] = Some(S { n: 3 });\n\
                     match a { Some(S { n }) => { println(n); } None => {} }\n\
                     match a { Some(S { n }) => println(n), None => println(\"none\") }\n\
                 }\n",
            "3\n3\n",
        ),
        // CONTROL: the WHOLE-payload binding this row does not touch. It is
        // the sibling classifier's, and admitting it here is what broke
        // `e2e_optres_tuple_payload_is_owned_exactly_once` twice.
        (
            "control: whole-payload binding still works",
            "struct S { s: String }\n\
                 fn main() {\n\
                     let a: Option[S] = Some(S { s: f\"ctrl\" });\n\
                     match a { Some(p) => { println(p.s); } None => {} }\n\
                     match a { Some(q) => println(q.s), None => println(\"none\") }\n\
                 }\n",
            "ctrl\nctrl\n",
        ),
    ];
    for (label, src, want) in cases {
        let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
        assert!(
            interp_errs.is_empty(),
            "{label}: interpreter errored: {interp_errs:?}"
        );
        assert_eq!(
            interp_out.join(""),
            *want,
            "{label}: the interpreter is the reference for this text"
        );
        if let Some(aot) = run_program(src) {
            assert_eq!(
                aot, *want,
                "{label}: the compiled backend must keep the source's payload \
                     across a read-only DESTRUCTURING arm"
            );
        }
    }
}

/// B-2026-08-30-52 sub-mechanism (b) — AN INNER `match` OVER A PAYLOAD THE
/// OUTER ARM BOUND WHOLE TOOK ITS HEAP.
///
/// `match a { Some(p) => match p { E.A { s } => println(s) } }` double freed
/// on `Option[E]` and printed an EMPTY second line on the `Result[E, i64]`
/// twin, while `--interp` was correct on both. Two independent causes, and
/// each hides the other:
///
///   * `borrow_binding_escapes`'s `Match` arm called a bare-`name`
///     scrutinee a move OUTRIGHT ("conservative move" in its own comment),
///     so the outer classifier declined before the inner arms were looked
///     at. It now asks `no_arm_payload_escapes` about those arms instead —
///     recursively, so the reasoning composes to any depth.
///   * Nothing propagated the outer binding's borrow-ness INTO the inner
///     `match`. `borrowed_agg_payload_struct_vars` already holds exactly
///     the names that alias someone else's storage; the inner scrutinee now
///     consults it (under the same read-only gate).
///
/// The `Option` / `Result` split in the pre-fix symptom is the payload
/// WIDTH, not the container: `E` is 4 words, which overflows `Option`'s
/// 3-word inline area and fits `Result`'s 5, so the two halves of one
/// program took different channels. That is why the boxed channel is
/// admitted here too — but only for a binding used SOLELY as a nested
/// `match` scrutinee, never a bare use (moved on) or a projection
/// (field-moved-out), the two shapes the row measured as unsafe.
#[test]
fn e2e_optres_nested_match_over_a_whole_bound_payload_does_not_take_it() {
    let cases: &[(&str, &str, &str)] = &[
            (
                "Option[E] — the BOXED channel; double freed before",
                "enum E { A { s: String }, B }\n\
                 fn main() {\n\
                     let a: Option[E] = Some(E.A { s: f\"inner\" });\n\
                     match a { Some(p) => { match p { E.A { s } => { println(s); } E.B => {} } } None => {} }\n\
                     match a { Some(p) => { match p { E.A { s } => { println(s); } E.B => {} } } None => {} }\n\
                 }\n",
                "inner\ninner\n",
            ),
            (
                "Result[E, i64] — the INLINE channel; printed empty before",
                "enum E { A { s: String }, B }\n\
                 fn main() {\n\
                     let a: Result[E, i64] = Ok(E.A { s: f\"inner\" });\n\
                     match a { Ok(p) => { match p { E.A { s } => { println(s); } E.B => {} } } Err(_) => {} }\n\
                     match a { Ok(p) => { match p { E.A { s } => { println(s); } E.B => {} } } Err(_) => {} }\n\
                 }\n",
                "inner\ninner\n",
            ),
            (
                "`if let` spelling — its own classifier and its own registration",
                "enum E { A { s: String }, B }\n\
                 fn main() {\n\
                     let a: Option[E] = Some(E.A { s: f\"il\" });\n\
                     if let Some(p) = a { match p { E.A { s } => { println(s); } E.B => {} } }\n\
                     if let Some(p) = a { match p { E.A { s } => { println(s); } E.B => {} } } else { println(\"none\"); }\n\
                 }\n",
                "il\nil\n",
            ),
            (
                "`if let` over the inline channel",
                "enum E { A { s: String }, B }\n\
                 fn main() {\n\
                     let a: Result[E, i64] = Ok(E.A { s: f\"ilr\" });\n\
                     if let Ok(p) = a { match p { E.A { s } => { println(s); } E.B => {} } }\n\
                     if let Ok(p) = a { match p { E.A { s } => { println(s); } E.B => {} } } else { println(\"err\"); }\n\
                 }\n",
                "ilr\nilr\n",
            ),
            (
                "tuple-variant payload, not a struct variant",
                "enum E { A(String), B }\n\
                 fn main() {\n\
                     let a: Option[E] = Some(E.A(f\"tv\"));\n\
                     match a { Some(p) => { match p { E.A(s) => { println(s); } E.B => {} } } None => {} }\n\
                     match a { Some(p) => { match p { E.A(s) => { println(s); } E.B => {} } } None => {} }\n\
                 }\n",
                "tv\ntv\n",
            ),
            (
                "the inner arm reads a FIELD of the leaf, not the leaf",
                "struct R { id: i64, tag: String }\n\
                 enum E { A { r: R }, B }\n\
                 fn main() {\n\
                     let a: Option[E] = Some(E.A { r: R { id: 7, tag: f\"t\" } });\n\
                     match a { Some(p) => { match p { E.A { r } => { println(r.id); } E.B => {} } } None => {} }\n\
                     match a { Some(p) => { match p { E.A { r } => { println(r.tag); } E.B => {} } } None => {} }\n\
                 }\n",
                "7\nt\n",
            ),
            // CONTROLS. Each of these must keep the path it takes today: the
            // widening is gated on "every mention of the binding is a nested
            // `match` scrutinee", and these are the three ways that fails.
            (
                "control: the inner arm MOVES the leaf — transfer path, one free",
                "enum E { A { s: String }, B }\n\
                 fn main() {\n\
                     let a: Option[E] = Some(E.A { s: f\"moved\" });\n\
                     match a { Some(p) => { match p { E.A { s } => { let owned: String = s; println(owned); } E.B => {} } } None => {} }\n\
                 }\n",
                "moved\n",
            ),
            (
                "control: the arm moves the payload WHOLE before matching it",
                "enum E { A { s: String }, B }\n\
                 fn main() {\n\
                     let a: Option[E] = Some(E.A { s: f\"whole\" });\n\
                     match a { Some(p) => { let q: E = p; match q { E.A { s } => println(s), E.B => {} } } None => {} }\n\
                 }\n",
                "whole\n",
            ),
            (
                "control: a PROJECTION out of a boxed payload stays off this path",
                "struct S { s: String, t: String }\n\
                 fn main() {\n\
                     let a: Option[S] = Some(S { s: f\"aa\", t: f\"bb\" });\n\
                     match a { Some(p) => { let x: String = p.s; println(x); } None => {} }\n\
                 }\n",
                "aa\n",
            ),
            (
                "control: a user `Drop` body on the payload still fires exactly once",
                "struct R { id: i64 }\n\
                 impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\"); } }\n\
                 enum E { A { r: R }, B }\n\
                 fn main() {\n\
                     let a: Option[E] = Some(E.A { r: R { id: 8 } });\n\
                     match a { Some(p) => { match p { E.A { r } => { println(r.id); } E.B => {} } } None => {} }\n\
                     println(\"end\");\n\
                 }\n",
                "8\ndR8\nend\n",
            ),
        ];
    for (label, src, want) in cases {
        let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
        assert!(
            interp_errs.is_empty(),
            "{label}: interpreter errored: {interp_errs:?}"
        );
        assert_eq!(
            interp_out.join(""),
            *want,
            "{label}: the interpreter is the reference for this text"
        );
        if let Some(aot) = run_program(src) {
            assert_eq!(
                aot, *want,
                "{label}: a nested `match` over a whole-bound payload must not \
                     take the source's heap"
            );
        }
    }
}

/// B-2026-08-31-35 — AN AGGREGATE LITERAL AT A VALUE-POSITION TAIL DID NOT
/// RETRACT THE LOCAL IT CONSUMED, so the body ran twice: once for the
/// consumer that took the value, once at the source's own death.
///
/// THE ROW'S DIAGNOSIS WAS WRONG AND THE CORRECTION IS WHAT LOCATES THE
/// FIX. It read the trigger as the DISCARD path, on the strength of a
/// control showing the `let`-BOUND spelling correct. That control is the
/// UNCONDITIONAL one — `let w = S { r: t, k: 1 };` — and it is the only
/// bound spelling that works. Put any value-position wrapper between the
/// `let` and the literal and the bound spelling doubles identically:
///
///     let w = if n == 0 { S { r: t, k: 1 } } else { … };   dR7 dR7
///     let w = match n { 0 => { S { r: t, k: 1 } } … };     dR7 dR7
///     let w = { S { r: t, k: 1 } };                        dR7 dR7
///
/// A BARE BLOCK doubles with no branch anywhere in it, which is what says
/// the population is "an aggregate literal at a value-position tail" rather
/// than anything about branches or about discards. The unconditional `let`
/// escapes only because `stmts.rs` retracts its sources STATICALLY, which a
/// tail behind a wrapper cannot do — one arm runs, and a static retraction
/// disarms on all paths or none.
///
/// THE MECHANISM ALREADY EXISTED and reached one shape too few. A bare
/// IDENTIFIER tail (`if c { t } else { u }`) has been correct since
/// B-2026-08-28-51 via a per-path `i1` drop flag cleared in the arm's own
/// basic block. The same move one aggregate deeper was invisible to it,
/// because both backends' hooks matched `ExprKind::Identifier` and nothing
/// else. Widening them to `collect_aggregate_literal_sources` — the walker
/// the static retraction already uses — makes the identifier case a special
/// case of the general one rather than a branch beside it.
///
/// PATH-SENSITIVITY IS THE POINT and the mixed case pins it: with two arms
/// consuming two different locals, the one whose arm did NOT run must keep
/// its own body. A static retraction over both arms would lose it.
#[test]
fn e2e_an_arm_literal_consuming_a_local_runs_one_body() {
    const PRELUDE: &str = "struct R { id: i64 }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\"); } }\n\
             struct S { r: R, k: i64 }\n\
             struct S2 { r: R, s: R, k: i64 }\n";
    let cases: &[(&str, &str, &str)] = &[
            (
                "bound `if` arm literal consumes a local",
                "fn go(n: i64) -> i64 { let t = R { id: 7 };\n\
                 let w = if n == 0 { S { r: t, k: 1 } } else { S { r: R { id: 9 }, k: 2 } };\n\
                 return w.k + 6; }\n\
                 fn take() -> i64 { return go(0); }",
                "dR7\nv=7\n",
            ),
            (
                "bound `match` arm literal consumes a local",
                "fn go(n: i64) -> i64 { let t = R { id: 7 };\n\
                 let w = match n { 0 => { S { r: t, k: 1 } } _ => { S { r: R { id: 9 }, k: 2 } } };\n\
                 return w.k + 6; }\n\
                 fn take() -> i64 { return go(0); }",
                "dR7\nv=7\n",
            ),
            (
                "bare BLOCK wrapper, no branch at all",
                "fn go() -> i64 { let t = R { id: 7 };\n\
                 let w = { S { r: t, k: 1 } };\n\
                 return w.k + 6; }\n\
                 fn take() -> i64 { return go(); }",
                "dR7\nv=7\n",
            ),
            (
                "TWO sources in one literal behind a wrapper",
                "fn go() -> i64 { let t = R { id: 7 }; let u = R { id: 8 };\n\
                 let w = { S2 { r: t, s: u, k: 1 } };\n\
                 return w.k + 6; }\n\
                 fn take() -> i64 { return go(); }",
                "dR8\ndR7\nv=7\n",
            ),
            (
                "one source and one MINTED sibling",
                "fn go() -> i64 { let t = R { id: 7 };\n\
                 let w = { S2 { r: t, s: R { id: 8 }, k: 1 } };\n\
                 return w.k + 6; }\n\
                 fn take() -> i64 { return go(); }",
                "dR8\ndR7\nv=7\n",
            ),
            // PATH-SENSITIVITY. Two arms consume two different locals; only the
            // taken arm's source may be disarmed. `u`'s body must still run, at
            // its own death, or a static retraction has been used where a
            // per-path one was required.
            (
                "mixed arms: the NON-taken arm's source keeps its body",
                "fn go(n: i64) -> i64 { let t = R { id: 7 }; let u = R { id: 8 };\n\
                 let w = if n == 0 { S { r: t, k: 1 } } else { S { r: u, k: 2 } };\n\
                 return w.k + 6; }\n\
                 fn take() -> i64 { return go(0); }",
                "dR8\ndR7\nv=7\n",
            ),
            (
                "ELSE taken: the unconsumed source dies on its own",
                "fn go(n: i64) -> i64 { let t = R { id: 7 };\n\
                 let w = if n == 0 { S { r: t, k: 1 } } else { S { r: R { id: 9 }, k: 5 } };\n\
                 return w.k + 2; }\n\
                 fn take() -> i64 { return go(3); }",
                "dR7\ndR9\nv=7\n",
            ),
            // ── B-2026-08-31-35's own DISCARDED repro, the half its first
            // pass left open. The bound spellings above run one body because
            // the consumer registers a drop; a DISCARDED branch registers one
            // too whenever the statement site qualifies to own the merged
            // value, and the taken arm's consumed local must stand down for it
            // exactly the same way. All four surfaces agree.
            (
                "DISCARDED `if` arm literal consumes a local",
                "fn go(n: i64) -> i64 { let t = R { id: 7 };\n\
                 let _ = if n == 0 { S { r: t, k: 1 } } else { S { r: R { id: 9 }, k: 2 } };\n\
                 return 7; }\n\
                 fn take() -> i64 { return go(0); }",
                "dR7\nv=7\n",
            ),
            (
                "DISCARDED `match` arm literal consumes a local",
                "fn go(n: i64) -> i64 { let t = R { id: 7 };\n\
                 let _ = match n { 0 => { S { r: t, k: 1 } } _ => { S { r: R { id: 9 }, k: 2 } } };\n\
                 return 7; }\n\
                 fn take() -> i64 { return go(0); }",
                "dR7\nv=7\n",
            ),
            (
                "DISCARDED bare-statement spelling of the same branch",
                "fn go(n: i64) -> i64 { let t = R { id: 7 };\n\
                 if n == 0 { S { r: t, k: 1 } } else { S { r: R { id: 9 }, k: 2 } };\n\
                 return 7; }\n\
                 fn take() -> i64 { return go(0); }",
                "dR7\nv=7\n",
            ),
            (
                "DISCARDED, mixed arms: the NON-taken arm's source keeps its body",
                "fn go(n: i64) -> i64 { let t = R { id: 7 }; let u = R { id: 8 };\n\
                 let _ = if n == 0 { S { r: t, k: 1 } } else { S { r: u, k: 2 } };\n\
                 return 7; }\n\
                 fn take() -> i64 { return go(0); }",
                "dR7\ndR8\nv=7\n",
            ),
            // B-2026-09-01-7 — the NO-`else` spelling, which was the one cell
            // of this family where the backends DISAGREED: the interpreter ran
            // two bodies where both compiled backends ran one, so an A/B gate
            // would have caught it while its two-tail siblings (doubling
            // everywhere) slipped past as an agreed gap. Closed by the same
            // change, because the interpreter's bare-statement `If` arm reaches
            // the taken-arm disarm exactly as the two-tail spelling does.
            // Measured at `e49a85f^`: interp `dR7 dR7`, AOT `dR7`.
            (
                "DISCARDED no-`else` `if`, arm literal consumes a local",
                "fn go(n: i64) -> i64 { let t = R { id: 7 };\n\
                 if n == 0 { S { r: t, k: 1 } };\n\
                 return 7; }\n\
                 fn take() -> i64 { return go(0); }",
                "dR7\nv=7\n",
            ),
            (
                "DISCARDED no-`else` `if`, arm NOT taken: the local dies on its own",
                "fn go(n: i64) -> i64 { let t = R { id: 7 };\n\
                 if n == 0 { S { r: t, k: 1 } };\n\
                 return 7; }\n\
                 fn take() -> i64 { return go(3); }",
                "dR7\nv=7\n",
            ),
            (
                "DISCARDED, ELSE taken: the unconsumed source dies on its own",
                "fn go(n: i64) -> i64 { let t = R { id: 7 };\n\
                 let _ = if n == 0 { S { r: t, k: 1 } } else { S { r: R { id: 9 }, k: 2 } };\n\
                 return 7; }\n\
                 fn take() -> i64 { return go(3); }",
                "dR9\ndR7\nv=7\n",
            ),
            // CONTROLS — already correct before this row, and together the
            // reason the wrapper rather than the discard is the trigger.
            (
                "control: a DISCARDED branch the statement site does NOT own",
                "fn go(n: i64) -> i64 { let t = R { id: 7 };\n\
                 let _ = if n == 0 { t } else { R { id: 9 } };\n\
                 return 7; }\n\
                 fn take() -> i64 { return go(0); }",
                "dR7\nv=7\n",
            ),
            (
                "control: a DISCARDED all-mint branch stays statement-owned",
                "fn go(n: i64) -> i64 {\n\
                 let _ = if n == 0 { S { r: R { id: 7 }, k: 1 } } else { S { r: R { id: 9 }, k: 2 } };\n\
                 return 7; }\n\
                 fn take() -> i64 { return go(0); }",
                "dR7\nv=7\n",
            ),
            (
                "control: the UNCONDITIONAL `let` spelling",
                "fn go() -> i64 { let t = R { id: 7 };\n\
                 let w = S { r: t, k: 1 };\n\
                 return w.k + 6; }\n\
                 fn take() -> i64 { return go(); }",
                "dR7\nv=7\n",
            ),
            (
                "control: a bare IDENTIFIER arm tail",
                "fn go(n: i64) -> i64 { let t = R { id: 7 }; let u = R { id: 8 };\n\
                 let w = if n == 0 { t } else { u };\n\
                 return w.id; }\n\
                 fn take() -> i64 { return go(0); }",
                "dR8\ndR7\nv=7\n",
            ),
            (
                "control: the arm MINTS its field",
                "fn go(n: i64) -> i64 {\n\
                 let w = if n == 0 { S { r: R { id: 7 }, k: 1 } } else { S { r: R { id: 9 }, k: 2 } };\n\
                 return w.k + 6; }\n\
                 fn take() -> i64 { return go(0); }",
                "dR7\nv=7\n",
            ),
        ];
    for (label, decls, want) in cases {
        let src = format!("{PRELUDE}{decls}\nfn main() {{ println(f\"v={{take()}}\"); }}\n");
        let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(&src);
        assert!(
            interp_errs.is_empty(),
            "{label}: interpreter errored: {interp_errs:?}"
        );
        assert_eq!(interp_out.join(""), *want, "{label}: interpreter");
        if let Some(aot) = run_program(&src) {
            assert_eq!(aot, *want, "{label}: one value, one body");
        }
    }
}

/// B-2026-09-01-11 (shapes 2 and 3) — A DISCARDED BRANCH WITH ONE ARM
/// NAMING A LIVE LOCAL lost the value the OTHER arm minted, in the
/// interpreter only.
///
/// `let _ = if c { E.A(mk(8)) } else { e };` with `c` true printed the
/// enum's own body and stopped (`dE`), against `dE dR8` on both compiled
/// backends; the bare-statement spelling printed nothing at all for the
/// discarded value.
///
/// Both interpreter gates asked a STATIC all-arms question —
/// `discard_producer_runs_payload_walk` for the payload walk,
/// `hands_out_live_binding` / `owns` for ownership itself — and the hazard
/// they were written for is real: the walk is value-driven, so a live
/// local handed out of one arm would have its payload walked while its own
/// binding still owns it. But that hazard belongs to the arm that RUNS.
/// The interpreter has recorded which arm tail produced the value since
/// B-2026-08-29-31, so both gates now ask the taken tail — which is how
/// each compiled backend decides it, one arm's basic block at a time.
///
/// The `guard:` cells are the reason this is not simply a wider gate: an
/// arm's own PATTERN BINDING is a different population from an enclosing
/// local (the first has left scope, the second has not), a no-`else` `if`
/// records no arm tail when the condition is false, and the all-ctor
/// branch must keep the count it already had. Nothing here may double.
///
/// The run where the TAKEN arm is itself the live local and that local
/// carries a payload was left out when this landed (compiled ran the
/// payload's body, the interpreter did not, and the `if` and `match`
/// spellings disagreed in opposite directions); B-2026-09-01-39 settled it
/// and pins it in
/// `e2e_live_local_handed_out_of_a_discarded_branch_runs_payload_body_once`.
#[test]
fn e2e_a_discarded_branch_whose_sibling_arm_names_a_live_local() {
    const PRELUDE: &str = "struct R { id: i64 }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\"); } }\n\
             enum E { A(R), B }\n\
             impl Drop for E { fn drop(mut ref self) { println(\"dE\"); } }\n\
             fn mk(n: i64) -> R { return R { id: n }; }\n\
             fn mke(n: i64) -> E { return E.A(mk(n)); }\n";
    // (label, the body of `go`, expected)
    let cases: &[(&str, &str, &str)] = &[
        (
            "wildcard `let`, the fresh arm taken — the row's shape 2",
            "let e = E.B; let c = true;\n\
                 let _ = if c { E.A(mk(8)) } else { e };",
            "dE\ndR8\ndE\nmid\nv=7\n",
        ),
        (
            "the same with the LIVE-LOCAL arm taken",
            "let e = E.B; let c = false;\n\
                 let _ = if c { E.A(mk(8)) } else { e };",
            "dE\nmid\nv=7\n",
        ),
        (
            "bare statement, the fresh arm taken — shape 3",
            "let e = E.B; let c = true;\n\
                 if c { E.A(mk(8)) } else { e };",
            "dE\ndR8\ndE\nmid\nv=7\n",
        ),
        (
            "bare statement, the live-local arm taken",
            "let e = E.B; let c = false;\n\
                 if c { E.A(mk(8)) } else { e };",
            "dE\nmid\nv=7\n",
        ),
        (
            "the sibling local CARRIES a payload, wildcard `let`",
            "let e = E.A(mk(5)); let c = true;\n\
                 let _ = if c { E.A(mk(8)) } else { e };",
            "dE\ndR8\ndE\ndR5\nmid\nv=7\n",
        ),
        (
            "the sibling local carries a payload, bare statement",
            "let e = E.A(mk(5)); let c = true;\n\
                 if c { E.A(mk(8)) } else { e };",
            "dE\ndR8\ndE\ndR5\nmid\nv=7\n",
        ),
        (
            "nested `else if`, the live local deepest and NOT taken",
            "let e = E.B; let c = true;\n\
                 let _ = if c { E.A(mk(8)) } else { if c { E.B } else { e } };",
            "dE\ndR8\ndE\nmid\nv=7\n",
        ),
        // ── guards: populations the taken-tail question must not move ──
        (
            "guard: a no-`else` `if` whose arm ran",
            "let c = true; let _ = if c { E.A(mk(8)) };",
            "dE\ndR8\nmid\nv=7\n",
        ),
        (
            "guard: a no-`else` `if` whose arm did NOT run",
            "let c = false; let _ = if c { E.A(mk(8)) };",
            "mid\nv=7\n",
        ),
        (
            "guard: an arm's own PATTERN BINDING, wildcard `let`",
            "let o = E.A(mk(3));\n\
                 let _ = match o { E.A(r) => E.A(r), _ => E.B };",
            "dE\ndR3\ndE\nmid\nv=7\n",
        ),
        (
            "guard: an arm's own pattern binding, bare statement",
            "let o = E.A(mk(3));\n\
                 match o { E.A(r) => E.A(r), _ => E.B };",
            "dE\ndR3\ndE\nmid\nv=7\n",
        ),
        (
            // `e` is never mentioned in the branch, so its NLL last use is
            // its own `let` and it dies BEFORE the discard — the inverse
            // order of the two cells above, where the branch mentions it.
            "guard: ALL arms construct — the count it already had",
            "let e = E.A(mk(5)); let c = true;\n\
                 let _ = if c { E.A(mk(8)) } else { E.A(mk(9)) };",
            "dE\ndR5\ndE\ndR8\nmid\nv=7\n",
        ),
        (
            // B-2026-09-01-39 — this cell pinned the AGREED-WRONG `dE`
            // (the payload body lost on all four surfaces) while that row
            // was open; it now reads the bound-local oracle.
            "guard: the local discarded DIRECTLY, no branch",
            "let e = E.A(mk(5));\n\
                 let _ = e;",
            "dE\ndR5\nmid\nv=7\n",
        ),
        (
            // The boundary the taken-tail question is NARROWED at: a CALL
            // arm is not a per-run hazard, it is a statement about what
            // `let _ = mke(9);` does — the own body alone, on every
            // backend. Re-asking it per run would walk `R{8}` here and
            // nowhere else: a fresh divergence, not a fix. Measured — this
            // cell printed `dE dR8` against compiled's `dE` on the first,
            // unnarrowed attempt.
            // B-2026-09-02-13 — this WAS `dE` alone. The guard is
            // still live (the taken-tail question must not reach a call
            // arm), but what a call arm ANSWERS changed: a discarded
            // call producing an own-`Drop` enum now runs the payload
            // body on every surface, so the row reads `dE dR8`.
            "guard: a CALL arm, which the question must NOT reach",
            "let c = true;\n\
                 let _ = if c { E.A(mk(8)) } else { mke(9) };",
            "dE\ndR8\nmid\nv=7\n",
        ),
    ];
    for (label, body, want) in cases {
        let src = format!(
            "{PRELUDE}#[allow(partial_move_of_drop_enum)]\nfn go() -> i64 {{ {body}\n \
                 println(\"mid\"); return 7; }}\n\
                 fn main() {{ println(f\"v={{go()}}\"); }}\n"
        );
        let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(&src);
        assert!(
            interp_errs.is_empty(),
            "{label}: interpreter errored: {interp_errs:?}"
        );
        assert_eq!(interp_out.join(""), *want, "{label}: interpreter");
        if let Some(aot) = run_program(&src) {
            assert_eq!(aot, *want, "{label}: compiled");
        }
    }
}

/// B-2026-09-01-11 (shape 1) — A DISCARDED BRANCH WHOSE **FIRST** ARM IS
/// AN ENUM CONSTRUCTION registered NOTHING on either compiled backend.
///
/// `let _ = if c { E.A(mk(8)) } else { mke(9) };` ran no body at all on
/// jit/aot, against `dE` under `--interp` — and against `let _ = mke(9);`,
/// which runs `dE` on all three.
///
/// The defect is one of arm ORDER, not of ctor arms, and the mirror-image
/// spelling is the proof: `if c { mke(8) } else { E.A(mk(9)) }` ran `dE`
/// correctly all along. `try_track_discarded_user_drop_temp` resolves a
/// branch's type from its FIRST arm through `discard_branch_tail_type_name`,
/// which understood a struct literal and a fn call but not a variant
/// construction — so a call in first position named the type and a ctor in
/// first position yielded `None` and the whole battery stayed silent.
///
/// The fix is two arms on that resolver (`E.A(x)`, whose enum is the path's
/// first segment, and the `E.B` unit spelling) plus an all-ctor exclusion
/// on the two branch arms that call it: a branch whose arms ALL construct
/// inline is owned by the representative-tail redirect, which runs the
/// payload walk too (`dE dR8`), so naming it here as well would register a
/// second owner and double the own body.
///
/// The mixed cases run the own body ALONE, with no payload walk — which is
/// exactly what the direct `let _ = mke(9);` spelling does on every
/// backend, and what `--interp` does for the branch. Both orders and both
/// statement spellings are pinned here because the two that already worked
/// are what would break if the exclusion were dropped.
#[test]
fn e2e_a_discarded_branch_with_a_non_ctor_arm_runs_its_own_body() {
    const PRELUDE: &str = "struct R { id: i64 }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\"); } }\n\
             enum E { A(R), B }\n\
             impl Drop for E { fn drop(mut ref self) { println(\"dE\"); } }\n\
             fn mk(n: i64) -> R { return R { id: n }; }\n\
             fn mke(n: i64) -> E { return E.A(mk(n)); }\n";
    // (label, the discarded statement, the `go` argument, expected)
    let cases: &[(&str, &str, &str, &str)] = &[
        (
            "ctor FIRST, call second, wildcard `let` — the row's shape",
            "let _ = if c { E.A(mk(8)) } else { mke(9) };",
            "true",
            "dE\ndR8\nv=7\n",
        ),
        (
            "the same with the OTHER arm taken",
            "let _ = if c { E.A(mk(8)) } else { mke(9) };",
            "false",
            "dE\ndR9\nv=7\n",
        ),
        (
            "ctor FIRST, bare-statement spelling",
            "if c { E.A(mk(8)) } else { mke(9) };",
            "true",
            "dE\ndR8\nv=7\n",
        ),
        (
            "ctor FIRST, `match` spelling",
            "let _ = match c { true => E.A(mk(8)), _ => mke(9) };",
            "true",
            "dE\ndR8\nv=7\n",
        ),
        (
            "a UNIT variant first, call second",
            "let _ = if c { E.B } else { mke(9) };",
            "true",
            "dE\nv=7\n",
        ),
        (
            "nested `else if`, the call arm deepest",
            "let _ = if c { E.A(mk(8)) } else { if c { E.B } else { mke(9) } };",
            "true",
            "dE\ndR8\nv=7\n",
        ),
        // ── the MIRROR order, which named its type off the call in first
        //    position and was already correct. Pinned because it is what
        //    the all-ctor exclusion and the first-arm rule must not
        //    double: an "any arm is a ctor" reading of this fix printed
        //    `dE dE` on the `match` row below.
        (
            "call FIRST, ctor second",
            "let _ = if c { mke(8) } else { E.A(mk(9)) };",
            "false",
            "dE\ndR9\nv=7\n",
        ),
        (
            "call FIRST, ctor second, `match` spelling",
            "let _ = match c { true => mke(8), _ => E.A(mk(9)) };",
            "false",
            "dE\ndR9\nv=7\n",
        ),
        (
            "call FIRST, UNIT variant second",
            "let _ = if c { mke(8) } else { E.B };",
            "true",
            "dE\ndR8\nv=7\n",
        ),
        // ── the three shapes that already had an owner
        (
            "guard: ALL arms are calls — the first-arm rule already named it",
            "let _ = if c { mke(8) } else { mke(9) };",
            "true",
            "dE\ndR8\nv=7\n",
        ),
        (
            "guard: a bare CALL tail — the direct spelling",
            "let _ = mke(8);",
            "true",
            "dE\ndR8\nv=7\n",
        ),
        (
            "guard: ALL arms are ctors — owned by the redirect, payload too",
            "let _ = if c { E.A(mk(8)) } else { E.A(mk(9)) };",
            "true",
            "dE\ndR8\nv=7\n",
        ),
        (
            "guard: ALL ctor arms, bare-statement spelling",
            "if c { E.A(mk(8)) } else { E.A(mk(9)) };",
            "false",
            "dE\ndR9\nv=7\n",
        ),
    ];
    for (label, stmt, arg, want) in cases {
        let src = format!(
            "{PRELUDE}fn go(c: bool) -> i64 {{ {stmt}\n return 7; }}\n\
                 fn main() {{ println(f\"v={{go({arg})}}\"); }}\n"
        );
        let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(&src);
        assert!(
            interp_errs.is_empty(),
            "{label}: interpreter errored: {interp_errs:?}"
        );
        assert_eq!(interp_out.join(""), *want, "{label}: interpreter");
        if let Some(aot) = run_program(&src) {
            assert_eq!(aot, *want, "{label}: the own body, exactly once");
        }
    }
}

/// B-2026-08-31-26 / B-2026-08-31-27 — A DESTRUCTURING `match` ARM MUST RUN
/// EACH `Drop` BODY EXACTLY ONCE, AND THE TWO BACKENDS WERE WRONG IN
/// OPPOSITE DIRECTIONS.
///
/// -26: both COMPILED backends ran a moved-out field's body TWICE for a
/// bare struct scrutinee (`match h { P { r, .. } => … }`).
/// `suppress_destructured_struct_pattern_cleanup` (#16) already cap-zeroed
/// the moved field's MEMORY in the source, so nothing was freed twice —
/// but the source's `__karac_dropbodies_*` walk still visited the field and
/// ran the body a second time on the husk the zeroing had just left. The
/// bodies half is now `disarm_arm_destructured_struct_field_bodies`.
///
/// -27: the INTERPRETER ran it ZERO times whenever the pattern reached
/// through a container. Two causes:
///   * `arm_moved_user_drop_payload_bindings` collected a payload binding
///     only when the sub-pattern was a bare name, so `Some(P { r, .. })`
///     registered no Drop slot at all;
///   * `is_drop_binding`'s STRUCT arm asked whether the bound type declares
///     a `Drop` of its OWN, while the enum arm beside it asked the
///     TRANSITIVE question — so a payload struct with a Drop-bearing FIELD
///     was silently skipped.
///
/// NEITHER IS A MEMORY ERROR, and that is why both survived a tree with
/// this much ASAN coverage: -26 frees exactly once (the second body just
/// reads a cleared object) and -27 frees nothing extra. Only the observable
/// body COUNT was wrong, so this fixture asserts the exact output text on
/// both backends rather than merely that the program survives.
///
/// The interpreter is NOT the oracle here — it was the wrong one for ten of
/// these rows. Every expected value is what `karac build` printed, which is
/// also what the five controls already agreed on before either fix.
#[test]
fn e2e_destructuring_arm_runs_each_drop_body_exactly_once() {
    const PRELUDE: &str = "struct R { s: String }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.s}\"); } }\n\
             struct P { r: R, n: i64 }\n\
             enum E { A { r: R }, B }\n\
             enum T { A(R), B }\n\
             enum W { C(P), D }\n";
    let cases: &[(&str, &str, &str)] = &[
            ("bare struct, `..` rest [B-2026-08-31-26]", "let h: P = P { r: R { s: \"a\" }, n: 4 };\n match h { P { r, .. } => println(r.s) }", "a\ndRa\nend\n"),
            ("bare struct, every field named [B-2026-08-31-26]", "let h: P = P { r: R { s: \"a\" }, n: 4 };\n match h { P { r, n } => println(r.s) }", "a\ndRa\nend\n"),
            ("bare struct, RENAMED binding [B-2026-08-31-26]", "let h: P = P { r: R { s: \"a\" }, n: 4 };\n match h { P { r: q, .. } => println(q.s) }", "a\ndRa\nend\n"),
            ("bare struct, BLOCK arm body [B-2026-08-31-26]", "let h: P = P { r: R { s: \"a\" }, n: 4 };\n match h { P { r, n } => { if n > 0 { println(r.s) } else { println(\"z\") } } }", "a\ndRa\nend\n"),
            ("`Option` + struct payload destructure [B-2026-08-31-27]", "let o: Option[P] = Some(P { r: R { s: \"a\" }, n: 4 });\n match o { Some(P { r, .. }) => println(r.s), None => {} }", "a\ndRa\nend\n"),
            ("`Option` + enum STRUCT-variant [B-2026-08-31-27]", "let o: Option[E] = Some(E.A { r: R { s: \"a\" } });\n match o { Some(E.A { r }) => println(r.s), Some(E.B) => {}, None => {} }", "a\ndRa\nend\n"),
            ("`Option` + enum TUPLE-variant [B-2026-08-31-27]", "let o: Option[T] = Some(T.A(R { s: \"a\" }));\n match o { Some(T.A(r)) => println(r.s), Some(T.B) => {}, None => {} }", "a\ndRa\nend\n"),
            ("`Result` + struct payload [B-2026-08-31-27]", "let o: Result[P, i64] = Ok(P { r: R { s: \"a\" }, n: 4 });\n match o { Ok(P { r, .. }) => println(r.s), Err(_) => {} }", "a\ndRa\nend\n"),
            ("USER enum + struct payload [B-2026-08-31-27]", "let w: W = W.C(P { r: R { s: \"a\" }, n: 4 });\n match w { W.C(P { r, .. }) => println(r.s), W.D => {} }", "a\ndRa\nend\n"),
            ("whole-bound payload, field-only `Drop` [B-2026-08-31-27]", "let o: Option[P] = Some(P { r: R { s: \"a\" }, n: 4 });\n match o { Some(p) => println(p.n), None => {} }", "4\ndRa\nend\n"),
            ("control: no binding at all", "let h: P = P { r: R { s: \"a\" }, n: 4 };\n match h { P { .. } => println(\"w\") }", "w\ndRa\nend\n"),
            ("control: binds only the NON-Drop field", "let h: P = P { r: R { s: \"a\" }, n: 4 };\n match h { P { n, .. } => println(n) }", "4\ndRa\nend\n"),
            ("control: bare enum scrutinee, always correct", "let e: E = E.A { r: R { s: \"a\" } };\n match e { E.A { r } => println(r.s), E.B => {} }", "a\ndRa\nend\n"),
            ("control: NESTED match over a whole-bound payload", "let o: Option[E] = Some(E.A { r: R { s: \"a\" } });\n match o { Some(p) => { match p { E.A { r } => println(r.s), E.B => {} } }, None => {} }", "a\ndRa\nend\n"),
            ("control: payload IS the `Drop` type", "let o: Option[R] = Some(R { s: \"a\" });\n match o { Some(r) => println(r.s), None => {} }", "a\ndRa\nend\n"),
        ];
    for (label, body, want) in cases {
        let src = format!("{PRELUDE}fn main() {{\n    {body}\n    println(\"end\");\n}}\n");
        let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(&src);
        assert!(
            interp_errs.is_empty(),
            "{label}: interpreter errored: {interp_errs:?}"
        );
        assert_eq!(
            interp_out.join(""),
            *want,
            "{label}: the interpreter must run each body exactly once"
        );
        if let Some(aot) = run_program(&src) {
            assert_eq!(aot, *want, "{label}: run and build must agree");
        }
    }
}

/// B-2026-08-31-38 — THE `if let` / `while let` LEGS RETRACTED THE
/// SOURCE'S PAYLOAD-BODIES WALK FOR A BINDING THAT DOES NOT OWN IT, SO A
/// DESTRUCTURED `Option`-WRAPPED STRUCT PAYLOAD RAN NO `Drop` BODY AT ALL.
///
///     let o: Option[H] = Some(H { r: R { s: pad(1) }, n: 4 });
///     if let Some(H { r, .. }) = o { println(r.s.len()) }
///
///     --interp    47 dR47 47 end     <- correct
///     karac run   47 47 end          <- the body NEVER ran
///     karac build 47 47 end
///
/// `suppress_optres_payload_bodies_for_match` retracts the SOURCE's
/// `__karac_dropelems_opt_*` action on the premise, stated in its own doc,
/// that "the arm's binding owns the resource from then on". The three
/// `let`-family legs called it unconditionally, including when the
/// binding is a BORROW and owns nothing — leaving the body with no owner
/// on either side.
///
/// WHY THE `match` SPELLING WAS ALWAYS CORRECT, which is the whole
/// diagnosis: its suppression battery sits behind
/// `scrut_ref_ptr.is_none() && !pattern_binding_is_borrow`, and this
/// pattern's bindings ARE borrows, so `match` never reached the call and
/// the source's walk still ran the body. Traced: the retraction fires on
/// the `if let` and `let ... else` legs for this program and never on the
/// `match` leg.
///
/// AND `let ... else` MUST KEEP FIRING, which is why the fix is a gate
/// rather than a deletion. Its binding ESCAPES into the enclosing scope
/// and is owned (`optres_bindings_owned` measured `true` on this exact
/// program, against `false` for `if let`), so there the retraction is
/// right and the escaped binding's own drop runs the body. It is a row
/// here precisely so a later widening cannot quietly turn it into a
/// double body.
///
/// THE ROW UNDER-REPORTED THE SURFACE: it recorded `if let` only, and
/// `while let` was measured broken in the same way (`44 end`, zero
/// bodies, on both compiled backends) and is closed by the same gate.
///
/// NO LEAK EITHER WAY — the row asked for this and left it open.
/// `valgrind --leak-check=full` at `KARAC_OPT_LEVEL=0`: 11 allocs / 11
/// frees pre-fix, 12/12 post (the extra pair is the body's own f-string),
/// 0 errors both times. The memory side was always balanced; only the
/// user-visible body was lost, which is what keeps this a run-vs-build
/// row rather than a leak one.
#[test]
fn e2e_a_let_family_destructure_runs_the_payload_drop_body_once() {
    const PRELUDE: &str = "fn pad(t: i64) -> String {\n\
             let mut s: String = String.new();\n\
             s.push_str(\"payload-padded-out-well-past-thirty-six-bytes-\");\n\
             s.push_str(f\"{t}\");\n\
             return s;\n\
             }\n\
             struct R { s: String }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.s.len()}\"); } }\n\
             struct H { r: R, n: i64 }\n\
             struct N { r: R, n: i64 }\n\
             enum W { C(H), D }\n";
    // One `R` is constructed in every row, so exactly ONE body is due.
    // The fourth column is the COMPILED expectation, which differs from
    // the interpreter's on exactly one row — see the `while let` note.
    let cases: &[(&str, &str, &str, &str)] = &[
        (
            "if let, `..` rest [B-2026-08-31-38]",
            "let o: Option[H] = Some(H { r: R { s: pad(1) }, n: 4 });\n \
                 if let Some(H { r, .. }) = o { println(f\"{r.s.len()}\") }",
            "47\ndR47\nend\n",
            "47\ndR47\nend\n",
        ),
        (
            "if let, every field named [B-2026-08-31-38]",
            "let o: Option[H] = Some(H { r: R { s: pad(1) }, n: 4 });\n \
                 if let Some(H { r, n }) = o { println(f\"{r.s.len()}{n}\") }",
            "474\ndR47\nend\n",
            "474\ndR47\nend\n",
        ),
        (
            "if let, RENAMED binding [B-2026-08-31-38]",
            "let o: Option[H] = Some(H { r: R { s: pad(1) }, n: 4 });\n \
                 if let Some(H { r: q, .. }) = o { println(f\"{q.s.len()}\") }",
            "47\ndR47\nend\n",
            "47\ndR47\nend\n",
        ),
        (
            "if let over `Result` [B-2026-08-31-38]",
            "let o: Result[H, i64] = Ok(H { r: R { s: pad(1) }, n: 4 });\n \
                 if let Ok(H { r, .. }) = o { println(f\"{r.s.len()}\") }",
            "47\ndR47\nend\n",
            "47\ndR47\nend\n",
        ),
        (
            // The COMPILED column is what this fix closes: zero bodies
            // before it, one after. The interpreter ran TWO here — the
            // source's payload walk beside the arm's own binding — which
            // was B-2026-09-02-7, filed rather than folded in and closed
            // since; the two columns now agree at one body, and
            // `e2e_while_let_binding_runs_the_payload_drop_body_once_per_pass`
            // is where that leg is covered in full.
            "while let, one pass [B-2026-08-31-38 / interp: B-2026-09-02-7]",
            "let mut q: Option[H] = Some(H { r: R { s: pad(1) }, n: 4 });\n \
                 while let Some(H { r, .. }) = q {\n \
                 println(f\"{r.s.len()}\");\n \
                 q = None;\n \
                 }",
            "47\ndR47\nend\n",
            "47\ndR47\nend\n",
        ),
        // The leg that must KEEP retracting: its binding escapes and owns
        // the payload, so the body comes from the binding, not the source.
        (
            "control: `let ... else` still runs exactly one body",
            "let Some(H { r, .. }) = Some(H { r: R { s: pad(1) }, n: 4 }) else { return };\n \
                 println(f\"{r.s.len()}\")",
            "47\ndR47\nend\n",
            "47\ndR47\nend\n",
        ),
        // The spelling that was correct before the fix, for the reason the
        // fix generalizes.
        (
            "control: the `match` spelling, correct throughout",
            "let o: Option[H] = Some(H { r: R { s: pad(1) }, n: 4 });\n \
                 match o { Some(H { r, .. }) => println(f\"{r.s.len()}\"), None => {} }",
            "47\ndR47\nend\n",
            "47\ndR47\nend\n",
        ),
        (
            "control: `if let` binding NOTHING",
            "let o: Option[H] = Some(H { r: R { s: pad(1) }, n: 4 });\n \
                 if let Some(_) = o { println(\"hit\") }",
            "hit\ndR47\nend\n",
            "hit\ndR47\nend\n",
        ),
        (
            // A SECOND pre-existing divergence this control turned up,
            // in the other direction and on the other backend: when the
            // pattern binds only the NON-`Drop` field, the INTERPRETER
            // ran no body while both compiled backends correctly ran one.
            // That was B-2026-09-02-8, since closed — the shape-only
            // retraction gate read a PARTIAL destructure as a full
            // consume — and the two columns now agree. The full matrix
            // for that shape is
            // `e2e_an_optres_partial_destructure_keeps_the_payload_body`.
            "control: binding only the NON-`Drop` field [B-2026-09-02-8]",
            "let o: Option[H] = Some(H { r: R { s: pad(1) }, n: 4 });\n \
                 if let Some(H { n, .. }) = o { println(f\"{n}\") }",
            "4\ndR47\nend\n",
            "4\ndR47\nend\n",
        ),
        (
            "control: the `match` twin of the row above [B-2026-09-02-8]",
            "let o: Option[H] = Some(H { r: R { s: pad(1) }, n: 4 });\n \
                 match o { Some(H { n, .. }) => println(f\"{n}\"), None => {} }",
            "4\ndR47\nend\n",
            "4\ndR47\nend\n",
        ),
        (
            "control: payload IS the `Drop` type",
            "let o: Option[R] = Some(R { s: pad(1) });\n \
                 if let Some(r) = o { println(f\"{r.s.len()}\") }",
            "47\ndR47\nend\n",
            "47\ndR47\nend\n",
        ),
        (
            "control: the miss edge runs the body too",
            "let o: Option[H] = Some(H { r: R { s: pad(1) }, n: 4 });\n \
                 if let None = o { println(\"none\") } else { println(\"some\") }",
            "some\ndR47\nend\n",
            "some\ndR47\nend\n",
        ),
        (
            "control: bare struct destructure, no `Option` envelope",
            "let h: H = H { r: R { s: pad(1) }, n: 4 };\n \
                 if let H { r, .. } = h { println(f\"{r.s.len()}\") }",
            "47\ndR47\nend\n",
            "47\ndR47\nend\n",
        ),
        (
            "control: USER enum wrapping the same struct payload",
            "let w: W = W.C(H { r: R { s: pad(1) }, n: 4 });\n \
                 if let W.C(H { r, .. }) = w { println(f\"{r.s.len()}\") }",
            "47\ndR47\nend\n",
            "47\ndR47\nend\n",
        ),
    ];
    for (label, body, want_interp, want_compiled) in cases {
        let src = format!("{PRELUDE}fn main() {{\n    {body}\n    println(\"end\");\n}}\n");
        let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(&src);
        assert!(
            interp_errs.is_empty(),
            "{label}: interpreter errored: {interp_errs:?}"
        );
        assert_eq!(
            interp_out.join(""),
            *want_interp,
            "{label}: the interpreter must run the body exactly once"
        );
        if let Some(aot) = run_program(&src) {
            assert_eq!(
                aot, *want_compiled,
                "{label}: the compiled backends must run the body exactly once"
            );
        }
    }
}

/// B-2026-09-02-7 — A `while let` ARM THAT BINDS ITS PAYLOAD RAN THE
/// PAYLOAD'S `Drop` BODY TWICE PER PASS ON THE INTERPRETER, once on both
/// compiled backends.
///
/// `if let` retracts the source place's payload-body walk when the pattern
/// MOVES a Drop-bearing payload out of it
/// (`disarm_moved_out_enum_payload_one`), so the body comes from the arm's
/// binding and from nothing else. `while let` never made that call: its
/// stash fired beside a still-armed source walk and every pass printed the
/// body twice. ONE `R` IS CONSTRUCTED PER PASS, so one body is due per
/// pass — the compiled column was the correct one throughout.
///
/// THE FIX IS THE PAIR, NOT THE CALL. Adding the disarm alone would break
/// the arms that only READ THROUGH the payload: there the disarm declines
/// (`takes_payload` asks that same question), the source keeps its walk,
/// and an ungated stash would still double. So the `reads_through` gate
/// moved across with it, exactly as `if let` carries both.
///
/// THE FRESH-TEMP CASE IS DELIBERATELY UNTOUCHED, and the `v.pop()` row
/// below is its gate. B-2026-08-28-67 declined to gate this leg because
/// "the scrutinee is re-evaluated per iteration, so there is no stable
/// binding whose walk could be handed the payload back" — true of a fresh
/// temp and only of one. An identifier scrutinee is re-READ per iteration
/// but is the same binding throughout and has exactly that walk;
/// `place_walk_is_retractable` is the line between the two.
///
/// THE TWO-PASS ROW IS ALSO AN AUTO-PAR PROBE, and reading it wrong cost
/// this session a nearly-filed row. `karac build`'s DEFAULT (auto-par on)
/// prints `47 47 end dR47` for it — both bodies deferred to scope exit and
/// one of the two lost — against the correct `47 dR47 47 dR47 end` that
/// `KARAC_AUTO_PAR=0` and this harness both produce. That is not a second
/// defect: it is B-2026-08-31-6, an outlined region swallowing the NLL
/// drop points of the statements it spans, reproduced here on a `while
/// let`. Measured identically against the pre-fix compiler, so it is not
/// this fix's doing either — the diff is interpreter-only. Noted rather
/// than asserted because the harness does not build the auto-par leg.
///
/// THE ROW'S TWO "NOT MEASURED" ITEMS ARE ANSWERED HERE. A `while let`
/// over `Result` behaves exactly as the `Option` one does (the disarm keys
/// on the PLACE, not on the envelope), and a `for` loop over a container
/// of the same payload never had the defect: all three surfaces agree
/// there, deferring both bodies to the loop's end.
#[test]
fn e2e_while_let_binding_runs_the_payload_drop_body_once_per_pass() {
    const PRELUDE: &str = "fn pad(t: i64) -> String {\n\
             let mut s: String = String.new();\n\
             s.push_str(\"payload-padded-out-well-past-thirty-six-bytes-\");\n\
             s.push_str(f\"{t}\");\n\
             return s;\n\
             }\n\
             struct R { s: String }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.s.len()}\"); } }\n\
             struct H { r: R, n: i64 }\n\
             enum W { C(H), D }\n";
    let cases: &[(&str, &str, &str, &str)] = &[
        (
            "destructure binding, one pass",
            "let mut q: Option[H] = Some(H { r: R { s: pad(1) }, n: 4 });\n\
                 while let Some(H { r, .. }) = q {\n\
                 println(f\"{r.s.len()}\");\n\
                 q = None;\n\
                 }",
            "47\ndR47\nend\n",
            "47\ndR47\nend\n",
        ),
        (
            "whole-payload binding, the payload IS the `Drop` type",
            "let mut p: Option[R] = Some(R { s: pad(1) });\n\
                 while let Some(r) = p {\n\
                 println(f\"{r.s.len()}\");\n\
                 p = None;\n\
                 }",
            "47\ndR47\nend\n",
            "47\ndR47\nend\n",
        ),
        (
            // Not the assignment: the same loop with a bare `break` in
            // place of `q = None` doubled identically before the fix, which
            // is what rules the overwrite out as the second body's source.
            "the loop exits by `break`, leaving the source live",
            "let mut q: Option[H] = Some(H { r: R { s: pad(1) }, n: 4 });\n\
                 while let Some(H { r, .. }) = q {\n\
                 println(f\"{r.s.len()}\");\n\
                 break;\n\
                 }",
            "47\ndR47\nend\n",
            "47\ndR47\nend\n",
        ),
        (
            // The arm MOVES the payload on out of itself, so the stash is
            // the only owner and the disarm is load-bearing.
            "the arm moves the payload out (`let m = r`)",
            "let mut w: Option[R] = Some(R { s: pad(1) });\n\
                 while let Some(r) = w {\n\
                 let m = r;\n\
                 println(f\"{m.s.len()}\");\n\
                 w = None;\n\
                 }",
            "47\ndR47\nend\n",
            "47\ndR47\nend\n",
        ),
        (
            // THE GATE on the fresh-temp exclusion. `v.pop()` has no place
            // to retract, so the stash must keep firing; standing it down
            // here would hand the payload to nobody and lose both bodies.
            "control: fresh-temp scrutinee keeps its stash [B-2026-07-30-11]",
            "let mut v: Vec[R] = Vec.new();\n\
                 v.push(R { s: pad(1) });\n\
                 v.push(R { s: pad(2) });\n\
                 while let Some(r) = v.pop() {\n\
                 println(f\"pop{r.s.len()}\");\n\
                 }",
            "pop47\ndR47\npop47\ndR47\nend\n",
            "pop47\ndR47\npop47\ndR47\nend\n",
        ),
        (
            "control: binds nothing, correct throughout",
            "let mut u: Option[R] = Some(R { s: pad(1) });\n\
                 while let Some(_) = u {\n\
                 println(\"hit\");\n\
                 u = None;\n\
                 }",
            "hit\ndR47\nend\n",
            "hit\ndR47\nend\n",
        ),
        (
            // The row left this NOT MEASURED. The disarm keys on the PLACE,
            // not on the envelope, so `Result` moves with `Option`.
            "over `Result` rather than `Option`",
            "let mut e: Result[H, i64] = Ok(H { r: R { s: pad(1) }, n: 4 });\n\
                 while let Ok(H { r, .. }) = e {\n\
                 println(f\"{r.s.len()}\");\n\
                 e = Err(1);\n\
                 }",
            "47\ndR47\nend\n",
            "47\ndR47\nend\n",
        ),
        (
            "over a USER enum wrapping the same struct payload",
            "let mut g: W = W.C(H { r: R { s: pad(1) }, n: 4 });\n\
                 while let W.C(H { r, .. }) = g {\n\
                 println(f\"{r.s.len()}\");\n\
                 g = W.D;\n\
                 }",
            "47\ndR47\nend\n",
            "47\ndR47\nend\n",
        ),
        (
            // TWO values constructed, so TWO bodies are due. The
            // interpreter ran FOUR before the fix and runs two now. The
            // compiled column here is the auto-par-OFF one, which is also
            // what this harness builds; `karac build`'s default leg loses
            // one of the two to B-2026-08-31-6 — see the doc comment.
            "two passes, a fresh value stored each pass",
            "let mut k: i64 = 0;\n\
                 let mut z: Option[R] = Some(R { s: pad(1) });\n\
                 while let Some(r) = z {\n\
                 println(f\"{r.s.len()}\");\n\
                 k = k + 1;\n\
                 if k < 2 { z = Some(R { s: pad(2) }); } else { z = None; }\n\
                 }",
            "47\ndR47\n47\ndR47\nend\n",
            "47\ndR47\n47\ndR47\nend\n",
        ),
        (
            // The row's other NOT MEASURED item: a `for` loop over a
            // container of the same payload never had this shape. All three
            // surfaces agree, deferring both bodies to the loop's end.
            "control: `for` over a container is a different shape",
            "let mut v: Vec[R] = Vec.new();\n\
                 v.push(R { s: pad(1) });\n\
                 v.push(R { s: pad(2) });\n\
                 for r in v {\n\
                 println(f\"it{r.s.len()}\");\n\
                 }",
            "it47\nit47\ndR47\ndR47\nend\n",
            "it47\nit47\ndR47\ndR47\nend\n",
        ),
        (
            // The `while let` spelling of B-2026-09-02-8, which was filed
            // off the `if let` one and closed with it: the two spellings
            // did close together, as this row was written to check.
            // Untouched by the `while let` fix above (nothing binds the
            // `Drop`-bearing field, so neither the disarm nor the stash is
            // in play) and kept as the cross-check that the retraction gate
            // reaches this spelling too.
            "control: binds only the NON-`Drop` field [B-2026-09-02-8]",
            "let mut y: Option[H] = Some(H { r: R { s: pad(1) }, n: 4 });\n\
                 while let Some(H { n, .. }) = y {\n\
                 println(f\"{n}\");\n\
                 y = None;\n\
                 }",
            "4\ndR47\nend\n",
            "4\ndR47\nend\n",
        ),
    ];
    for (label, body, want_interp, want_compiled) in cases {
        let src = format!("{PRELUDE}fn main() {{\n    {body}\n    println(\"end\");\n}}\n");
        let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(&src);
        assert!(
            interp_errs.is_empty(),
            "{label}: interpreter errored: {interp_errs:?}"
        );
        assert_eq!(
            interp_out.join(""),
            *want_interp,
            "{label}: the interpreter must run one payload body per pass"
        );
        if let Some(aot) = run_program(&src) {
            assert_eq!(aot, *want_compiled, "{label}: compiled transcript");
        }
    }
}

/// B-2026-09-02-8 — AN `Option`/`Result` PARTIAL DESTRUCTURE THAT BINDS
/// ONLY A NON-`Drop` FIELD LOST THE PAYLOAD'S `Drop` BODY.
///
/// Retracting a source's payload-bodies walk is a HAND-OFF: the pattern
/// moves the `Drop`-bearing payload out and its new binding runs the body
/// instead. The Option/Result leg of that decision was SHAPE-only on both
/// backends — `pattern_claims_ownership` / `pattern_consumes_field` over
/// the whole sub-pattern — so `Some(H { n, .. })`, binding an `i64` beside
/// an untouched `Drop`-bearing `r: R`, counted as a consume. The walk was
/// retracted and nothing took over: the interpreter's own arm stash is
/// filtered by what the bindings actually own and came out empty, and the
/// `let ... else` binding that escapes owns nothing either. The body ran
/// nowhere.
///
/// THE SHAPE-ONLY TEST SURVIVES FOR A BARE BINDING, deliberately. `Some(r)`
/// names a payload whose declared type is a generic parameter, invisible to
/// both backends, and the original justification does hold there: an
/// `Option[i64]` source registers no walk, so retracting it is a no-op.
/// Only a DESTRUCTURE has field types to consult, and only a destructure
/// can name a position the source's walk was covering for. Both backends
/// therefore consult declared field types for a struct sub-pattern and fall
/// back to the shape answer for everything else.
///
/// THE ROW SPANNED TWO FAILURE MODES AND ONE FIX SETTLES BOTH, which is
/// what it predicted. `if let` and `match` were interpreter-only
/// divergences (compiled ran the body); `let ... else` lost the body on ALL
/// FOUR surfaces — agreed-and-wrong, invisible to the A/B rule — because
/// there the compiled retraction is not gated away by
/// B-2026-08-31-38's `optres_bindings_owned` (the flag is TRUE on that leg).
/// One predicate, applied at both backends' retraction sites, closes them
/// together.
///
/// BODIES ONLY, now measured rather than assumed — the row asked for this
/// explicitly. `valgrind --leak-check=full` at `KARAC_OPT_LEVEL=0` on the
/// `let ... else` program: 13 allocs / 13 frees pre-fix, 14/14 post (the
/// extra pair is the restored body's own f-string), 0 errors both times.
/// The buffer was never stranded; only the user-visible body was lost.
///
/// ALL THREE OF THE ROW'S "NOT MEASURED" SHAPES ARE ROWS HERE: the `Result`
/// envelope, an enum STRUCT-VARIANT payload (`Some(V.A { n, .. })`, which is
/// why the declaration lookup resolves a two-segment path as well as a
/// struct name), and a second non-`Drop` field bound alongside the first.
#[test]
fn e2e_an_optres_partial_destructure_keeps_the_payload_body() {
    const PRELUDE: &str = "fn pad(t: i64) -> String {\n\
             let mut s: String = String.new();\n\
             s.push_str(\"payload-padded-out-well-past-thirty-six-bytes-\");\n\
             s.push_str(f\"{t}\");\n\
             return s;\n\
             }\n\
             struct R { s: String }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.s.len()}\"); } }\n\
             struct H { r: R, n: i64 }\n\
             struct H3 { r: R, n: i64, m: i64 }\n\
             enum V { A { r: R, n: i64 }, B }\n";
    // One `R` is constructed in every row, so exactly ONE body is due.
    let cases: &[(&str, &str, &str)] = &[
        (
            "if let, binds only the non-`Drop` field",
            "let a: Option[H] = Some(H { r: R { s: pad(1) }, n: 4 });\n\
                 if let Some(H { n, .. }) = a { println(f\"A{n}\"); }",
            "A4\ndR47\nend\n",
        ),
        (
            "match, the same",
            "let b: Option[H] = Some(H { r: R { s: pad(1) }, n: 4 });\n\
                 match b { Some(H { n, .. }) => println(f\"B{n}\"), None => {} }",
            "B4\ndR47\nend\n",
        ),
        (
            // The agreed-and-wrong leg: no A/B gate could see this one,
            // because all four surfaces printed the same missing body.
            // The source dies at the destructure (nothing bound out of it
            // survives), so its body fires there — ahead of the println,
            // on every surface.
            "let ... else, which lost the body on ALL FOUR surfaces",
            "let e: Option[H] = Some(H { r: R { s: pad(1) }, n: 4 });\n\
                 let Some(H { n, .. }) = e else { return };\n\
                 println(f\"E{n}\");",
            "dR47\nE4\nend\n",
        ),
        (
            "over `Result` rather than `Option`",
            "let a: Result[H, i64] = Ok(H { r: R { s: pad(1) }, n: 4 });\n\
                 if let Ok(H { n, .. }) = a { println(f\"R{n}\"); }",
            "R4\ndR47\nend\n",
        ),
        (
            // Two path segments, so the declaration lookup has to resolve
            // an enum STRUCT VARIANT and not just a struct name.
            "an enum struct-variant payload in the same position",
            "let a: Option[V] = Some(V.A { r: R { s: pad(1) }, n: 4 });\n\
                 if let Some(V.A { n, .. }) = a { println(f\"V{n}\"); }",
            "V4\ndR47\nend\n",
        ),
        (
            "a SECOND non-`Drop` field bound alongside the first",
            "let a: Option[H3] = Some(H3 { r: R { s: pad(1) }, n: 4, m: 5 });\n\
                 if let Some(H3 { n, m, .. }) = a { println(f\"T{n}{m}\"); }",
            "T45\ndR47\nend\n",
        ),
        (
            // THE GATE on the other side: binding the `Drop` field DOES
            // hand the payload over, so the retraction must still fire or
            // the body runs twice.
            "control: binding the `Drop` field still retracts",
            "let o: Option[H] = Some(H { r: R { s: pad(1) }, n: 4 });\n\
                 if let Some(H { r, .. }) = o { println(f\"F{r.s.len()}\"); }",
            "F47\ndR47\nend\n",
        ),
        (
            "control: the `let ... else` spelling of the row above",
            "let o: Option[H] = Some(H { r: R { s: pad(1) }, n: 4 });\n\
                 let Some(H { r, .. }) = o else { return };\n\
                 println(f\"F{r.s.len()}\");",
            "F47\ndR47\nend\n",
        ),
        (
            // A BARE binding keeps the shape answer — the payload's type is
            // a generic parameter neither backend can read here — so this
            // row is what proves the narrowing did not reach it.
            "control: a bare whole-payload binding",
            "let o: Option[R] = Some(R { s: pad(1) });\n\
                 let Some(r) = o else { return };\n\
                 println(f\"W{r.s.len()}\");",
            "W47\ndR47\nend\n",
        ),
        (
            "control: mixed — the `Drop` field AND a scalar",
            "let a: Option[H3] = Some(H3 { r: R { s: pad(1) }, n: 4, m: 5 });\n\
                 if let Some(H3 { r, n, .. }) = a { println(f\"M{n}{r.s.len()}\"); }",
            "M447\ndR47\nend\n",
        ),
        (
            "control: no `Option` envelope, correct throughout",
            "let c: H = H { r: R { s: pad(1) }, n: 4 };\n\
                 if let H { n, .. } = c { println(f\"C{n}\"); }",
            "C4\ndR47\nend\n",
        ),
        (
            "control: binds nothing, correct throughout",
            "let g: Option[H] = Some(H { r: R { s: pad(1) }, n: 4 });\n\
                 if let Some(H { .. }) = g { println(\"G\"); }",
            "G\ndR47\nend\n",
        ),
    ];
    for (label, body, want) in cases {
        let src = format!("{PRELUDE}fn main() {{\n    {body}\n    println(\"end\");\n}}\n");
        let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(&src);
        assert!(
            interp_errs.is_empty(),
            "{label}: interpreter errored: {interp_errs:?}"
        );
        assert_eq!(
            interp_out.join(""),
            *want,
            "{label}: the interpreter must run the payload body exactly once"
        );
        if let Some(aot) = run_program(&src) {
            assert_eq!(
                aot, *want,
                "{label}: the compiled backends must agree with the interpreter"
            );
        }
    }
}

/// B-2026-09-02-14 — A BOUND `Option`/`Result` LOCAL WHOSE ARM MISSED LOST
/// ITS PAYLOAD'S `Drop` BODY, ON ALL FOUR SURFACES.
///
/// `let r = mkerr(); if let Ok(w) = r { … }` never runs `W`'s body. The
/// same local with no `if let` at all runs it correctly, which is the whole
/// diagnosis: the walk EXISTS, so this was a retraction that should not have
/// applied, not a missing registration.
///
/// THE RETRACTION WAS RIGHT AND FLOW-INSENSITIVE. A pattern that takes the
/// payload hands the body to the arm's binding, so the source's walk must
/// stand down — true on the hit edge, and applied on every path out of the
/// construct, including the one where the pattern did not match and no
/// binding exists. Both backends now decide it PER PATH: codegen clears a
/// per-path flag in the arm's own block (`optres_payload_bodies_flag_for`,
/// read at the place's death), and the interpreter's disarm moved inside the
/// match test. The two are the same decision, so they stay in step.
///
/// AGREED-AND-WRONG, so no A/B gate could see it, and the storage is
/// reclaimed either way (valgrind: 9 allocs / 9 frees, 0 errors on a
/// `String`-carrying payload before and after) — a lost side effect, not a
/// leak. Only an absolute expectation catches this class.
///
/// THE LATER-USE ROW IS THE ONE THAT CONSTRAINS THE FIX. When `r` is used
/// again after the `if let`, the body is due at that LATER site and exactly
/// once — which is what rules out simply emitting the bodies on the miss
/// edge. The flag leaves the walk armed and lets the NLL machinery place it
/// at the place's real last use, so both rows come out right from one rule.
///
/// ALL THREE OF THE ROW'S UNMEASURED SHAPES ARE HERE AND WERE AFFECTED: the
/// `match` spelling with a non-binding arm, a USER enum with payloads in
/// both variants, and a heap-carrying payload.
#[test]
fn e2e_a_bound_optres_local_whose_arm_missed_runs_its_payload_drop_body() {
    const PRELUDE: &str = "struct W { id: i64 }\n\
             impl Drop for W { fn drop(mut ref self) { println(f\"dW{self.id}\"); } }\n\
             struct H { tag: String }\n\
             impl Drop for H { fn drop(mut ref self) { println(f\"dH{self.tag}\"); } }\n\
             enum E { A(W), B(W) }\n\
             struct V { id: i64 }\n\
             impl Drop for V { fn drop(mut ref self) { println(f\"dV{self.id}\"); } }\n\
             enum E2 { A(V), B }\n\
             impl Drop for E2 { fn drop(mut ref self) { println(\"dE2\"); } }\n\
             fn mkerr() -> Result[W, W] { return Err(W { id: 7 }); }\n\
             fn mkok() -> Result[W, W] { return Ok(W { id: 1 }); }\n\
             fn mksome() -> Option[W] { return Some(W { id: 5 }); }\n\
             fn mkerrh() -> Result[H, H] { return Err(H { tag: \"seven\" }); }\n\
             fn mkb() -> E { return E.B(W { id: 3 }); }\n";
    let cases: &[(&str, &str, &str)] = &[
            (
                "the row: an `if let` the bound local's value declines",
                "let r: Result[W, W] = mkerr();\n\
                 if let Ok(w) = r { println(f\"v{w.id}\"); }",
                "dW7\nafter\n",
            ),
            (
                // The row's first unmeasured shape. A `match` retracts through
                // the same helper, so an arm set that binds only `Ok` and
                // wildcards the rest lost the body identically.
                "`match` with a non-binding arm [row: NOT MEASURED]",
                "let r: Result[W, W] = mkerr();\n\
                 match r { Ok(a) => { println(f\"ma{a.id}\"); } _ => { println(\"wild\"); } }",
                "wild\ndW7\nafter\n",
            ),
            (
                // The row's second unmeasured shape. This one was a
                // RUN-VS-BUILD divergence rather than an agreed silence:
                // `--interp` printed nothing where all three compiled surfaces
                // printed the body.
                "a USER enum with payloads in both variants [row: NOT MEASURED]",
                "let e: E = mkb();\n\
                 if let E.A(w) = e { println(f\"v{w.id}\"); }",
                "dW3\nafter\n",
            ),
            (
                // …and its `let ... else` spelling, divergent the same way. A
                // user enum that declares its OWN `Drop` was already correct on
                // all four surfaces before the fix and stays so, so the hole was
                // specific to an enum whose only body is its payload's.
                "the same USER enum through `let ... else`",
                "let e: E = mkb();\n\
                 let E.A(w) = e else { println(\"elsearm\"); println(\"after\"); return };\n\
                 println(f\"v{w.id}\");",
                "elsearm\nafter\ndW3\n",
            ),
            (
                // The row's third unmeasured shape. Bodies-only either way —
                // the buffer was always reclaimed.
                "a heap-carrying payload [row: NOT MEASURED]",
                "let r: Result[H, H] = mkerrh();\n\
                 if let Ok(h) = r { println(f\"v{h.tag}\"); }",
                "dHseven\nafter\n",
            ),
            (
                "the `while let` spelling, whose first pass misses",
                "let r: Result[W, W] = mkerr();\n\
                 while let Ok(w) = r { println(f\"v{w.id}\"); break; }",
                "dW7\nafter\n",
            ),
            (
                // `let ... else` was a RUN-VS-BUILD divergence before this fix:
                // `--interp` printed nothing where the compiled backends
                // printed the body at the divergent exit.
                "the `let ... else` spelling, previously an interp/compiled split",
                "let r: Result[W, W] = mkerr();\n\
                 let Ok(w) = r else { println(\"elsearm\"); println(\"after\"); return };\n\
                 println(f\"v{w.id}\");",
                "elsearm\nafter\ndW7\n",
            ),
            (
                // THE CONSTRAINING ROW. `r`'s last use is the later `match`, so
                // the body is due there and exactly once. A miss-edge emission
                // would print `dW7` before `mid` AND again in the `Err` arm.
                "the local used AGAIN after the `if let`: one body, at the later use",
                "let r: Result[W, W] = mkerr();\n\
                 if let Ok(w) = r { println(f\"v{w.id}\"); }\n\
                 println(\"mid\");\n\
                 match r { Ok(a) => { println(f\"ma{a.id}\"); } Err(e) => { println(f\"me{e.id}\"); } }",
                "mid\nme7\ndW7\nafter\n",
            ),
            (
                // THE HIT-EDGE GATE: the arm's binding owns the payload and
                // runs the body itself, so the place's walk must stand down
                // there or every matching `if let` doubles.
                "control: the same local when the arm DOES match",
                "let r: Result[W, W] = mkok();\n\
                 if let Ok(w) = r { println(f\"v{w.id}\"); }",
                "v1\ndW1\nafter\n",
            ),
            (
                // The shape that proves the local's walk exists at all, which
                // is what makes the row a retraction bug rather than a missing
                // registration.
                "control: the same local with no `if let` at all",
                "let r: Result[W, W] = mkerr();\n\
                 println(\"x\");",
                "dW7\nx\nafter\n",
            ),
            (
                // A pattern that binds NOTHING never claimed the payload, so
                // the retraction never fired and this row was correct before
                // the fix too. It pins that it stays correct.
                "control: a pattern that binds nothing keeps the walk",
                "let r: Result[W, W] = mkerr();\n\
                 if let Ok(_) = r { println(\"hit\"); }",
                "dW7\nafter\n",
            ),
            (
                // Same reason, through the `Option`/`None` spelling: `None`
                // claims no ownership.
                "control: an `Option` local declined by a `None` pattern",
                "let o: Option[W] = mksome();\n\
                 if let None = o { println(\"none\"); } else { println(\"some\"); }",
                "some\ndW5\nafter\n",
            ),
            (
                "control: `match` binding both arms, correct throughout",
                "let r: Result[W, W] = mkerr();\n\
                 match r { Ok(a) => { println(f\"ma{a.id}\"); } Err(e) => { println(f\"me{e.id}\"); } }",
                "me7\ndW7\nafter\n",
            ),
            (
                // THE SPLIT THIS FIX HAD TO KEEP. Codegen's optres retraction
                // is per-path now; its USER-ENUM one is still a compile-time
                // removal, so the interpreter's arm scan stays whole-match
                // there. Making both taken-arm-only diverged exactly here: the
                // read-through SECOND arm is the one taken, its own answer is
                // "nothing moved out", and `--interp` printed `v5 dE dR5`
                // against every compiled backend's `v5 dR5 dE`. Interpreter-side
                // this shape is `readthrough_arm_leaves_the_payload_with_its_enum`'s
                // `mixed-arms-one-materializes` row; the cross-backend twin is
                // here because the divergence was compiled-vs-interpreted.
                "control: a USER enum's mixed arms keep the whole-match retraction",
                "let g: E2 = E2.A(V { id: 5 });\n\
                 match g { E2.A(r) if r.id == 1 => { let m = r; println(f\"m{m.id}\"); }\n\
                           E2.A(r) => { println(f\"v{r.id}\"); } E2.B => {} }",
                "v5\ndV5\ndE2\nafter\n",
            ),
            (
                // The `Option` spelling of the row above: taken-arm-only, and
                // the read-through arm leaves the payload with the source, so
                // exactly one body either way.
                "control: an `Option`'s mixed arms, taken-arm-only",
                "let o: Option[W] = mksome();\n\
                 match o { Some(a) if a.id == 1 => { let m = a; println(f\"m{m.id}\"); }\n\
                           Some(a) => { println(f\"v{a.id}\"); } None => {} }",
                "v5\ndW5\nafter\n",
            ),
        ];
    for (label, body, want) in cases {
        let src = format!("{PRELUDE}#[allow(partial_move_of_drop_enum)]\nfn main() {{\n    {body}\n    println(\"after\");\n}}\n");
        let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(&src);
        assert!(
            interp_errs.is_empty(),
            "{label}: interpreter errored: {interp_errs:?}"
        );
        assert_eq!(
            interp_out.join(""),
            *want,
            "{label}: interpreter transcript"
        );
        if let Some(aot) = run_program(&src) {
            assert_eq!(
                aot, *want,
                "{label}: the compiled backends must agree with the interpreter"
            );
        }
    }
}

/// B-2026-08-31-23 — A CONSUMING ARM OVER A BOXED USER-ENUM PAYLOAD
/// DOUBLE FREED, WHILE THE SAME ARM OVER A BOXED STRUCT PAYLOAD WAS FINE.
///
/// `suppress_boxed_payload_struct_destructure_at` resolved the payload
/// pattern's last path segment against `struct_types` ONLY. `Some(E.A { s })`
/// looks up "A", finds nothing, and returns before anything is disarmed —
/// so the box interior's drop stayed armed while the arm's binding owned
/// the same buffer, and both freed it.
///
/// THE AXIS IS THE PAYLOAD'S TYPE, NOT THE MATCH'S SHAPE. The row that
/// filed this recorded the nested `match` spelling as load-bearing and a
/// single-level destructure as a clean control; re-measuring found the
/// single-level ENUM spelling broken too, and the clean control had been a
/// STRUCT payload. Both spellings are pinned here so the correction cannot
/// be lost.
///
/// The nested spelling needs a second half: there the outer pattern is a
/// bare binding, so no payload pattern reaches the disarmer at all. The
/// binding is a bit copy of the box interior and the inner match's own
/// cap-zeroing disarms the COPY, so `boxed_payload_alias` records what it
/// aliases and the box gets disarmed too.
///
/// EVERY ROW ALLOCATES AGAIN INSIDE THE ARM, and that is what makes this
/// fixture a gate rather than decoration. Written first with short literal
/// payloads, all eight rows PASSED against the reverted fix: the second
/// free lands on a block nothing reclaims, so glibc stays quiet and the
/// text is correct. `pad()` gives the payload a real heap buffer and the
/// in-arm allocation reuses the freed block, which turns the same defect
/// into an abort on both compiled backends.
#[test]
fn e2e_consuming_arm_over_a_boxed_enum_payload_frees_once() {
    // Shared prelude: a heap payload big enough to be a real allocation,
    // and a second allocation the arm makes after taking the first.
    const PAD: &str = "fn pad(tag: i64) -> String {\n\
             let mut s: String = String.new();\n\
             s.push_str(\"payload-padded-out-well-past-thirty-six-bytes-\");\n\
             s.push_str(f\"{tag}\");\n\
             return s;\n\
         }\n";
    let cases: &[(&str, String, &str)] = &[
            (
                "single-level enum-variant payload destructure",
                format!(
                    "enum E {{ A {{ s: String }}, B }}\n{PAD}                     fn main() {{\n                         let a: Option[E] = Some(E.A {{ s: pad(1) }});\n                         match a {{ Some(E.A {{ s }}) => {{ let owned: String = s; let extra: String = pad(2); println(owned.len() + extra.len()); }} Some(E.B) => {{}} None => {{}} }}\n                         println(\"end\");\n                     }}\n"
                ),
                "94\nend\n",
            ),
            (
                "nested match over a whole-bound payload, inner arm MOVES the leaf",
                format!(
                    "enum E {{ A {{ s: String }}, B }}\n{PAD}                     fn main() {{\n                         let a: Option[E] = Some(E.A {{ s: pad(1) }});\n                         match a {{ Some(p) => {{ match p {{ E.A {{ s }} => {{ let owned: String = s; let extra: String = pad(2); println(owned.len() + extra.len()); }} E.B => {{}} }} }} None => {{}} }}\n                         println(\"end\");\n                     }}\n"
                ),
                "94\nend\n",
            ),
            (
                "tuple-variant payload, not a struct variant",
                format!(
                    "enum E {{ A(String), B }}\n{PAD}                     fn main() {{\n                         let a: Option[E] = Some(E.A(pad(1)));\n                         match a {{ Some(p) => {{ match p {{ E.A(s) => {{ let owned: String = s; let extra: String = pad(2); println(owned.len() + extra.len()); }} E.B => {{}} }} }} None => {{}} }}\n                         println(\"end\");\n                     }}\n"
                ),
                "94\nend\n",
            ),
            (
                "the `Result` container, whose payload area is 5 words",
                format!(
                    "enum E {{ A {{ s: String, t: String, u: String }}, B }}\n{PAD}                     fn main() {{\n                         let a: Result[E, i64] = Ok(E.A {{ s: pad(1), t: pad(2), u: pad(3) }});\n                         match a {{ Ok(E.A {{ s, .. }}) => {{ let owned: String = s; let extra: String = pad(4); println(owned.len() + extra.len()); }} Ok(E.B) => {{}} Err(_) => {{}} }}\n                         println(\"end\");\n                     }}\n"
                ),
                "94\nend\n",
            ),
            // CONTROLS — each keeps the path it takes today.
            (
                "control: a boxed STRUCT payload was always correct",
                format!(
                    "struct S {{ s: String, t: String }}\n{PAD}                     fn main() {{\n                         let a: Option[S] = Some(S {{ s: pad(1), t: pad(2) }});\n                         match a {{ Some(S {{ s, t }}) => {{ let owned: String = s; let extra: String = pad(3); println(owned.len() + t.len() + extra.len()); }} None => {{}} }}\n                         println(\"end\");\n                     }}\n"
                ),
                "141\nend\n",
            ),
            (
                "control: an INLINE enum payload is not boxed and must not be touched",
                "enum E { A(i64), B }\n                 fn main() {\n                     let a: Option[E] = Some(E.A(7));\n                     match a { Some(E.A(n)) => println(n), Some(E.B) => {} None => println(\"none\") }\n                 }\n"
                    .to_string(),
                "7\n",
            ),
            (
                "control: a READ-ONLY nested arm stays a borrow (B-2026-08-30-52)",
                format!(
                    "enum E {{ A {{ s: String }}, B }}\n{PAD}                     fn main() {{\n                         let a: Option[E] = Some(E.A {{ s: pad(1) }});\n                         match a {{ Some(p) => {{ match p {{ E.A {{ s }} => println(s.len()), E.B => {{}} }} }} None => {{}} }}\n                         match a {{ Some(p) => {{ match p {{ E.A {{ s }} => println(s.len()), E.B => {{}} }} }} None => {{}} }}\n                     }}\n"
                ),
                "47\n47\n",
            ),
        ];
    for (label, src, want) in cases {
        let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
        assert!(
            interp_errs.is_empty(),
            "{label}: interpreter errored: {interp_errs:?}"
        );
        assert_eq!(
            interp_out.join(""),
            *want,
            "{label}: the interpreter is the reference for this text"
        );
        if let Some(aot) = run_program(src) {
            assert_eq!(
                aot, *want,
                "{label}: a consuming arm and the box must not both free the payload"
            );
        }
    }
}

/// Every place shape the method path must now reach, against the
/// free-function spelling of the same call as the in-program oracle: a
/// nested field, a tuple index, a struct-held tuple index, an array index
/// and a Vec index. Each pair prints the same number or the method path
/// regressed.
#[test]
fn method_mut_ref_place_arg_shapes_match_free_fn() {
    assert_eq!(
        run_program(
            "struct Inner { w: i64 }\n\
                 struct Box { v: i64, inner: Inner }\n\
                 struct Pair { t: (i64, i64) }\n\
                 struct H { acc: i64 }\n\
                 impl H { fn bump(ref self, x: mut ref i64) -> i64 { x = x + 1; x } }\n\
                 fn free_bump(x: mut ref i64) -> i64 { x = x + 1; x }\n\
                 fn main() {\n\
                     let h = H { acc: 0 };\n\
                     let mut g1 = Box { v: 1, inner: Inner { w: 1 } };\n\
                     free_bump(mut g1.inner.w);\n\
                     let mut g2 = Box { v: 1, inner: Inner { w: 1 } };\n\
                     h.bump(mut g2.inner.w);\n\
                     println(g1.inner.w);\n\
                     println(g2.inner.w);\n\
                     let mut t1 = (1, 2);\n\
                     free_bump(mut t1.0);\n\
                     let mut t2 = (1, 2);\n\
                     h.bump(mut t2.0);\n\
                     println(t1.0);\n\
                     println(t2.0);\n\
                     let mut p1 = Pair { t: (1, 2) };\n\
                     free_bump(mut p1.t.1);\n\
                     let mut p2 = Pair { t: (1, 2) };\n\
                     h.bump(mut p2.t.1);\n\
                     println(p1.t.1);\n\
                     println(p2.t.1);\n\
                     let mut a1 = [1, 1];\n\
                     free_bump(mut a1[0]);\n\
                     let mut a2 = [1, 1];\n\
                     h.bump(mut a2[0]);\n\
                     println(a1[0]);\n\
                     println(a2[0]);\n\
                     let mut v1 = Vec.new();\n\
                     v1.push(1);\n\
                     free_bump(mut v1[0]);\n\
                     let mut v2 = Vec.new();\n\
                     v2.push(1);\n\
                     h.bump(mut v2[0]);\n\
                     println(v1[0]);\n\
                     println(v2[0]);\n\
                 }"
        ),
        Some("2\n2\n2\n2\n3\n3\n2\n2\n2\n2\n".to_string())
    );
}

/// The QUALIFIED match spelling of the same thing. It collapsed
/// identically before the seed, which is what ruled out "the bare spelling
/// is the problem" and pointed at the missing layout instead.
#[test]
fn memory_ordering_qualified_patterns_keep_their_variant() {
    assert_eq!(
        run_program(
            "fn main() {\n\
                     let m = MemoryOrdering.SeqCst;\n\
                     match m {\n\
                         MemoryOrdering.Relaxed => println(\"Relaxed\"),\n\
                         MemoryOrdering.Acquire => println(\"Acquire\"),\n\
                         MemoryOrdering.Release => println(\"Release\"),\n\
                         MemoryOrdering.AcqRel => println(\"AcqRel\"),\n\
                         MemoryOrdering.SeqCst => println(\"SeqCst\"),\n\
                     }\n\
                 }"
        ),
        Some("SeqCst\n".to_string())
    );
}

/// B-2026-09-10-19 — a NESTED destructuring arm `Some(Some(r))` lost its
/// leaf's `Drop` body compiled, and nothing was missing: a disarm fired on
/// a premise that stopped holding one nesting level down.
///
/// For a boxed NON-STRUCT payload the CALLEE's param-site arm owns the box,
/// and the walk runs at `cmrun.live` behind a `%optresbodies.<name>` arm
/// flag — entry-block `store i1 true`, and a consuming arm stores `false`
/// in its own block. `a56142bd8` narrowed that disarm to arms whose
/// sub-patterns BIND rather than destructure, on the premise that a
/// destructure's leaves each take something and each get their own body.
/// That is true of a TUPLE destructure and false of an ENVELOPE one:
/// nothing registers a body for a leaf bound out of an inner
/// `Option`/`Result`, so `Some(Some(r))` cleared the flag and no walk ran.
///
/// THE IR IS WHAT SETTLED IT, because the one-level control and the nested
/// row differ in mechanism, not degree. Capturing both (`CAPTURE_TO=..
/// KARAC_JIT_RUNNER=..`) showed `Option[R]` carrying no `%optresbodies`
/// flag AT ALL — its payload is a struct, so the CALLER owns the box and
/// runs `__karac_dropelems_opt_R` after the call — while
/// `Option[Option[R]]` had the flag, a `store i1 false` opening
/// `match.body0`, and a `__karac_dropelems_opt_Option_R` call behind it.
/// A test that only compared the two outputs would have read this as a
/// missing walker and sent the fix to the emitter.
///
/// THE LEAF IS A VIEW, NOT A SECOND OWNER — which is why leaving the place
/// armed is safe rather than a double free. `leaf-moved-into-a-call` is the
/// cell that proves it: the arm hands `r` to `eat(r)`, and the output is
/// exactly one `dR71`, valgrind-clean. If the leaf ever does start owning,
/// that cell aborts rather than going quiet.
///
/// The `flat-tuple-destructure-control` cell is load-bearing in the other
/// direction: it must stay DISARMED. Its leaves do own, so a fix that
/// keyed on the arm being nested at all — rather than on each
/// sub-pattern's own shape — would double-free there.
#[test]
fn e2e_nested_envelope_match_arm_runs_its_leaf_drop_body() {
    const PRE: &str = "struct Rv { id: i64, name: String }\n\
             impl Drop for Rv { fn drop(mut ref self) { println(f\"dRv{self.id}\") } }\n";
    for (label, body, want) in [
            // THE ROW: `Some(Some(r))` over a callee-owned boxed payload.
            (
                "nested-option-arm",
                "fn takeR(x: Option[Option[Rv]]) { match x { Some(Some(r)) => { println(f\"a{r.id}\") } Some(None) => { println(\"sn\") } None => { println(\"n\") } } }\n\
                 fn main() { takeR(Some(Some(Rv { id: 71, name: f\"a\" }))); println(\"done\") }\n",
                "a71\ndRv71\ndone\n",
            ),
            // The `Result` spelling, which the row left unmeasured. `Err` is in
            // the arm set so the disarm's variant-name test sees all three.
            (
                "nested-result-arm",
                "fn takeR(x: Result[Result[Rv, i64], i64]) { match x { Ok(Ok(r)) => { println(f\"a{r.id}\") } Ok(Err(e)) => { println(\"oe\") } Err(e) => { println(\"n\") } } }\n\
                 fn main() { takeR(Result[Result[Rv, i64], i64].Ok(Result[Rv, i64].Ok(Rv { id: 71, name: f\"a\" }))); println(\"done\") }\n",
                "a71\ndRv71\ndone\n",
            ),
            // Three deep — the sub-pattern test is per-level, so the middle
            // `Some(..)` must be admitted on its own shape too.
            (
                "three-deep-arm",
                "fn takeR(x: Option[Option[Option[Rv]]]) { match x { Some(Some(Some(r))) => { println(f\"a{r.id}\") } Some(Some(None)) => { println(\"ssn\") } Some(None) => { println(\"sn\") } None => { println(\"n\") } } }\n\
                 fn main() { takeR(Some(Some(Some(Rv { id: 71, name: f\"a\" })))); println(\"done\") }\n",
                "a71\ndRv71\ndone\n",
            ),
            // The leaf MOVED into another call. Exactly one body — the leaf is
            // a view into the payload the place walk owns, not a second owner.
            (
                "leaf-moved-into-a-call",
                "fn eat(r: Rv) { println(f\"e{r.id}\") }\n\
                 fn takeR(x: Option[Option[Rv]]) { match x { Some(Some(r)) => { eat(r) } Some(None) => { println(\"sn\") } None => { println(\"n\") } } }\n\
                 fn main() { takeR(Some(Some(Rv { id: 71, name: f\"a\" }))); println(\"done\") }\n",
                "e71\ndRv71\ndone\n",
            ),
            // The leaf never read at all, so no use site could have carried the
            // body on its behalf.
            (
                "leaf-never-read",
                "fn takeR(x: Option[Option[Rv]]) { match x { Some(Some(r)) => { println(\"hit\") } Some(None) => { println(\"sn\") } None => { println(\"n\") } } }\n\
                 fn main() { takeR(Some(Some(Rv { id: 71, name: f\"a\" }))); println(\"done\") }\n",
                "hit\ndRv71\ndone\n",
            ),
            // A TUPLE leaf bound whole behind the nested variant: the inner
            // arm is still `Some(..)`, and both elements' bodies must run.
            (
                "nested-arm-tuple-leaf-bound-whole",
                "fn takeR(x: Option[Option[(Rv, Rv)]]) { match x { Some(Some(t)) => { println(\"hit\") } Some(None) => { println(\"sn\") } None => { println(\"n\") } } }\n\
                 fn main() { takeR(Some(Some((Rv { id: 71, name: f\"a\" }, Rv { id: 72, name: f\"b\" })))); println(\"done\") }\n",
                "hit\ndRv71\ndRv72\ndone\n",
            ),
            // A tuple leaf DESTRUCTURED inside the nested variant — the two
            // shapes composed, which is the case a per-level test gets right
            // and a whole-arm test does not.
            (
                "nested-arm-tuple-leaf-destructured",
                "fn takeR(x: Option[Option[(Rv, Rv)]]) { match x { Some(Some((a, b))) => { println(f\"a{a.id}\") } Some(None) => { println(\"sn\") } None => { println(\"n\") } } }\n\
                 fn main() { takeR(Some(Some((Rv { id: 71, name: f\"a\" }, Rv { id: 72, name: f\"b\" })))); println(\"done\") }\n",
                "a71\ndRv71\ndRv72\ndone\n",
            ),
            // CONTROL — one level. No `%optresbodies` flag exists here at all
            // (struct payload, caller-owned box), so this cell is untouched by
            // the disarm either way and shows up if that ownership ever moves.
            (
                "one-level-arm-control",
                "fn takeR(x: Option[Rv]) { match x { Some(r) => { println(f\"a{r.id}\") } None => { println(\"n\") } } }\n\
                 fn main() { takeR(Some(Rv { id: 71, name: f\"a\" })); println(\"done\") }\n",
                "a71\ndRv71\ndone\n",
            ),
            // CONTROL, AND THE ONE THAT MUST STAY DISARMED. A flat tuple
            // destructure's leaves DO own, so admitting this arm would run the
            // place walk beside them and double-free. Correct before and after.
            (
                "flat-tuple-destructure-control",
                "fn takeR(x: Option[(Rv, Rv)]) { match x { Some((a, b)) => { println(f\"a{a.id}\") } None => { println(\"n\") } } }\n\
                 fn main() { takeR(Some((Rv { id: 71, name: f\"a\" }, Rv { id: 72, name: f\"b\" }))); println(\"done\") }\n",
                "a71\ndRv71\ndRv72\ndone\n",
            ),
            // CONTROL — the inner `None` arm taken, so the flag's armed path
            // runs over an empty payload and must print nothing extra.
            (
                "inner-none-arm-control",
                "fn takeR(x: Option[Option[Rv]]) { match x { Some(Some(r)) => { println(f\"a{r.id}\") } Some(None) => { println(\"sn\") } None => { println(\"n\") } } }\n\
                 fn main() { let e: Option[Rv] = None; takeR(Some(e)); println(\"done\") }\n",
                "sn\ndone\n",
            ),
        ] {
            let Some(out) = run_program(&format!("{PRE}{body}")) else {
                return;
            };
            assert_eq!(out, want, "[{label}]");
        }
}

/// B-2026-09-12-25 — a boxed user-enum payload destructured by a TUPLE
/// variant pattern (`match x { Option.Some(K.A(r)) => … }`) gave its leaf
/// two owners: the arm's binding, and the boxed payload's own drop still
/// walking into the field the pattern moved out. `free(): double free
/// detected in tcache 2` on every compiled lane, `--interp` clean.
///
/// TWO LEGS, LANDED SEPARATELY. `6ea22e3b4`'s
/// `suppress_nested_boxed_payload_cleanup` reaches through the box and
/// applies the inner-enum disarm to the nested sub-pattern; the ceiling in
/// `suppress_destructured_enum_payload_cleanup_at_limited` decides WHICH
/// positions may be disarmed. Both are needed: reaching the right level
/// fixes the double free, and the ceiling keeps that from becoming a leak
/// for a leaf wider than the envelope's payload area.
///
/// THE ROW'S OWN CONTROL WAS WRONG, and correcting it is what fixed the
/// shape of the repair. It recorded the READ-ONLY twin as clean on all
/// four surfaces, which reads as "the trigger is the leaf ESCAPING" and
/// invites an escape-gated disarm. The twin aborts identically at
/// `KARAC_OPT_LEVEL=0` (cell 2) and looked clean only because at the
/// default `-O2` LLVM deletes the doubled malloc/free pair. A bound leaf
/// at or under the area owns its bit-copy whether the arm moves it on or
/// only reads it, so the disarm keys on the BINDING and the WIDTH — cell 5
/// is the wildcard leaf that would leak if it dropped the first, and cells
/// 14–17 are the boundary that would leak if it dropped the second.
///
/// The struct-shaped spelling (cells 3 and 4) was already correct through
/// B-2026-08-31-23's separate path, which is why it is pinned here as a
/// control rather than as part of the defect.
#[test]
fn e2e_boxed_user_enum_tuple_variant_payload_destructure_has_one_owner() {
    for (label, body, want) in [
            // 1 — THE ROW'S SHAPE: a boxed user-enum payload destructured by a
            //      TUPLE variant pattern whose leaf is pushed into a `mut ref`
            //      accumulator. `free(): double free detected in tcache 2` on all
            //      three compiled lanes before the fix; 13 allocs / 14 frees and one
            //      `Invalid free()` of the payload's `String` buffer under valgrind.
            (
                "tuple-variant-leaf-escapes-into-accumulator",
                "struct R25 { s: String }\n\
             enum K5 { A(R25), B }\n\
             impl Drop for K5 { fn drop(mut ref self) { println(\"dK5\") } }\n\
             #[allow(partial_move_of_drop_enum)]\nfn show(x: Option[K5], acc: mut ref Vec[R25]) { match x { Option.Some(K5.A(r)) => { acc.push(r) } Option.Some(K5.B) => {} Option.None => {} } }\n\
             #[allow(partial_move_of_drop_enum)]\nfn main() { let mut acc: Vec[R25] = []; show(Option.Some(K5.A(R25 { s: f\"z\" })), mut acc); println(f\"len:{acc.len()}\"); println(\"end\") }\n",
                "len:1\nend\n",
            ),
            // 2 — the READ-ONLY twin, and the cell that refutes the row's own
            //      control. The row recorded this shape as clean on all four surfaces;
            //      it aborts identically at `KARAC_OPT_LEVEL=0` and only looked clean
            //      at the default `-O2`, where LLVM deletes the doubled malloc/free
            //      pair. A bound leaf owns its bit-copy whether the arm moves it on or
            //      merely reads it, which is why the fix is not gated on escape.
            (
                "tuple-variant-leaf-read-only",
                "struct R25 { s: String }\n\
             enum K5 { A(R25), B }\n\
             impl Drop for K5 { fn drop(mut ref self) { println(\"dK5\") } }\n\
             #[allow(partial_move_of_drop_enum)]\nfn show(x: Option[K5], acc: mut ref Vec[R25]) { match x { Option.Some(K5.A(r)) => { println(f\"a:{r.s}\") } Option.Some(K5.B) => {} Option.None => {} } }\n\
             #[allow(partial_move_of_drop_enum)]\nfn main() { let mut acc: Vec[R25] = []; show(Option.Some(K5.A(R25 { s: f\"z\" })), mut acc); println(f\"len:{acc.len()}\"); println(\"end\") }\n",
                "a:z\ndK5\nlen:0\nend\n",
            ),
            // 3 — CONTROL: the STRUCT-shaped spelling of cell 1, correct before the
            //      fix because B-2026-08-31-23 already admitted an enum-variant STRUCT
            //      pattern here. The pair is what names the axis: pattern SHAPE, not
            //      the payload's type, the population, or what the arm does.
            (
                "struct-variant-leaf-escapes-control",
                "struct R25 { s: String }\n\
             enum Ks5 { A { r: R25 }, B }\n\
             impl Drop for Ks5 { fn drop(mut ref self) { println(\"dKs5\") } }\n\
             #[allow(partial_move_of_drop_enum)]\nfn show(x: Option[Ks5], acc: mut ref Vec[R25]) { match x { Option.Some(Ks5.A { r }) => { acc.push(r) } Option.Some(Ks5.B) => {} Option.None => {} } }\n\
             #[allow(partial_move_of_drop_enum)]\nfn main() { let mut acc: Vec[R25] = []; show(Option.Some(Ks5.A { r: R25 { s: f\"z\" } }), mut acc); println(f\"len:{acc.len()}\"); println(\"end\") }\n",
                "len:1\nend\n",
            ),
            // 4 — CONTROL: the struct-shaped read-only twin. Clean before and after,
            //      and the evidence that disarming on a BINDING rather than on an
            //      escape is what the struct path has always done.
            (
                "struct-variant-leaf-read-only-control",
                "struct R25 { s: String }\n\
             enum Ks5 { A { r: R25 }, B }\n\
             impl Drop for Ks5 { fn drop(mut ref self) { println(\"dKs5\") } }\n\
             #[allow(partial_move_of_drop_enum)]\nfn show(x: Option[Ks5]) { match x { Option.Some(Ks5.A { r }) => { println(f\"a:{r.s}\") } Option.Some(Ks5.B) => {} Option.None => {} } }\n\
             #[allow(partial_move_of_drop_enum)]\nfn main() { show(Option.Some(Ks5.A { r: R25 { s: f\"z\" } })); println(\"end\") }\n",
                "a:z\ndKs5\nend\n",
            ),
            // 5 — CONTROL, and the cell that would catch an over-broad fix: the leaf
            //      is a WILDCARD, so nothing takes it and the box must stay its only
            //      owner. Disarming here would trade the abort for a leak.
            //      `pattern_consumes_field`'s bind-vs-TEST rule is what keeps it.
            (
                "tuple-variant-wildcard-leaf-control",
                "struct R25 { s: String }\n\
             enum K5 { A(R25), B }\n\
             impl Drop for K5 { fn drop(mut ref self) { println(\"dK5\") } }\n\
             #[allow(partial_move_of_drop_enum)]\nfn show(x: Option[K5]) { match x { Option.Some(K5.A(_)) => { println(\"a\") } Option.Some(K5.B) => {} Option.None => {} } }\n\
             #[allow(partial_move_of_drop_enum)]\nfn main() { show(Option.Some(K5.A(R25 { s: f\"z\" }))); println(\"end\") }\n",
                "a\ndK5\nend\n",
            ),
            // 6 — CONTROL: a `ref`-mode parameter. The callee BORROWS, so there is
            //      only ever one owner and the disarm must not reach it.
            (
                "tuple-variant-ref-mode-param-control",
                "struct R25 { s: String }\n\
             enum K5 { A(R25), B }\n\
             impl Drop for K5 { fn drop(mut ref self) { println(\"dK5\") } }\n\
             #[allow(partial_move_of_drop_enum)]\nfn show(x: ref Option[K5]) { match x { Option.Some(K5.A(r)) => { println(f\"a:{r.s}\") } Option.Some(K5.B) => {} Option.None => {} } }\n\
             #[allow(partial_move_of_drop_enum)]\nfn main() { let x: Option[K5] = Option.Some(K5.A(R25 { s: f\"z\" })); show(x); println(\"end\") }\n",
                "a:z\ndK5\nend\n",
            ),
            // 7 — a NAMED LOCAL rather than a by-value param, so the defect is not
            //      specific to `boxed_struct_payload_param_vars`: the let site's own
            //      `boxed_enum_payload_vars` membership reached the same disarm and
            //      bailed on the same line. Aborted before the fix.
            (
                "tuple-variant-named-local-scrutinee",
                "struct R25 { s: String }\n\
             enum K5 { A(R25), B }\n\
             impl Drop for K5 { fn drop(mut ref self) { println(\"dK5\") } }\n\
             #[allow(partial_move_of_drop_enum)]\nfn main() { let mut acc: Vec[R25] = []; let x: Option[K5] = Option.Some(K5.A(R25 { s: f\"z\" })); match x { Option.Some(K5.A(r)) => { acc.push(r) } Option.Some(K5.B) => {} Option.None => {} } println(f\"len:{acc.len()}\"); println(\"end\") }\n",
                "len:1\nend\n",
            ),
            // 8 — the payload enum declares NO `impl Drop`, and it aborts all the
            //      same. The row listed this as unmeasured and asked whether the enum's
            //      own drop fn was the second owner; it is not — the interior walk the
            //      box carries is, and that exists for the heap fields alone.
            (
                "tuple-variant-payload-enum-without-drop",
                "struct R25 { s: String }\n\
             enum K5 { A(R25), B }\n\
             #[allow(partial_move_of_drop_enum)]\nfn show(x: Option[K5], acc: mut ref Vec[R25]) { match x { Option.Some(K5.A(r)) => { acc.push(r) } Option.Some(K5.B) => {} Option.None => {} } }\n\
             #[allow(partial_move_of_drop_enum)]\nfn main() { let mut acc: Vec[R25] = []; show(Option.Some(K5.A(R25 { s: f\"z\" })), mut acc); println(f\"len:{acc.len()}\"); println(\"end\") }\n",
                "len:1\nend\n",
            ),
            // 9 — FOUR `String` leaves in one tuple variant, each pushed. This cell
            //      aborts at the DEFAULT opt level too (four invalid frees), so it is
            //      the one that guards the class on the plain `--features llvm` leg
            //      rather than only under the `-O0` ASAN ratchet.
            (
                "tuple-variant-four-string-leaves",
                "enum Ks25 { A(String, String, String, String), B }\n\
             impl Drop for Ks25 { fn drop(mut ref self) { println(\"dKs25\") } }\n\
             #[allow(partial_move_of_drop_enum)]\nfn show(x: Option[Ks25], acc: mut ref Vec[String]) { match x { Option.Some(Ks25.A(s, t, u, v)) => { acc.push(s); acc.push(t); acc.push(u); acc.push(v) } Option.Some(Ks25.B) => {} Option.None => {} } }\n\
             #[allow(partial_move_of_drop_enum)]\nfn main() { let mut acc: Vec[String] = []; show(Option.Some(Ks25.A(f\"a\", f\"b\", f\"c\", f\"d\")), mut acc); println(f\"len:{acc.len()}\"); println(\"end\") }\n",
                "len:4\nend\n",
            ),
            // 10 — one level DEEPER (`Some(Kw5.A(W5 { r }))`), the nesting the row
            //       listed as unmeasured. It reaches the disarm through the same
            //       tuple-variant gate and then through the enum path's own
            //       bind-vs-test walk, so no extra recursion was needed.
            (
                "tuple-variant-one-level-deeper",
                "struct R25 { s: String }\n\
             struct W5 { r: R25 }\n\
             enum Kw5 { A(W5), B }\n\
             impl Drop for Kw5 { fn drop(mut ref self) { println(\"dKw5\") } }\n\
             #[allow(partial_move_of_drop_enum)]\nfn show(x: Option[Kw5], acc: mut ref Vec[R25]) { match x { Option.Some(Kw5.A(W5 { r })) => { acc.push(r) } Option.Some(Kw5.B) => {} Option.None => {} } }\n\
             #[allow(partial_move_of_drop_enum)]\nfn main() { let mut acc: Vec[R25] = []; show(Option.Some(Kw5.A(W5 { r: R25 { s: f\"z\" } })), mut acc); println(f\"len:{acc.len()}\"); println(\"end\") }\n",
                "len:1\nend\n",
            ),
            // 11 — the accumulator is a plain STRUCT field rather than a `Vec`, the
            //       row's other unmeasured shape. Two invalid frees before the fix.
            (
                "tuple-variant-leaf-into-struct-field",
                "struct R25 { s: String }\n\
             struct Acc5 { held: R25 }\n\
             enum K5 { A(R25), B }\n\
             impl Drop for K5 { fn drop(mut ref self) { println(\"dK5\") } }\n\
             #[allow(partial_move_of_drop_enum)]\nfn show(x: Option[K5], acc: mut ref Acc5) { match x { Option.Some(K5.A(r)) => { acc.held = r; } Option.Some(K5.B) => {} Option.None => {} } }\n\
             #[allow(partial_move_of_drop_enum)]\nfn main() { let mut acc = Acc5 { held: R25 { s: f\"i\" } }; show(Option.Some(K5.A(R25 { s: f\"z\" })), mut acc); println(f\"h:{acc.held.s}\"); println(\"end\") }\n",
                "h:z\nend\n",
            ),
            // 12 — the arm RETURNS the nested leaf instead of pushing it. The row
            //       noted B-2026-09-12-15's `give` cell returns a WHOLE payload and is
            //       clean; the nested-leaf return is not, and aborted here too.
            (
                "tuple-variant-leaf-returned",
                "struct R25 { s: String }\n\
             enum K5 { A(R25), B }\n\
             impl Drop for K5 { fn drop(mut ref self) { println(\"dK5\") } }\n\
             #[allow(partial_move_of_drop_enum)]\nfn give(x: Option[K5]) -> R25 { match x { Option.Some(K5.A(r)) => { r } Option.Some(K5.B) => { R25 { s: f\"b\" } } Option.None => { R25 { s: f\"n\" } } } }\n\
             #[allow(partial_move_of_drop_enum)]\nfn main() { let g = give(Option.Some(K5.A(R25 { s: f\"z\" }))); println(f\"g:{g.s}\"); println(\"end\") }\n",
                "g:z\nend\n",
            ),
            // 13 — CONTROL for the BOXEDNESS axis: the same enum inside a `Result`,
            //       whose five-word payload area holds it INLINE. No box, no second
            //       owner, clean before and after — which is why the `Result` spelling
            //       the row left unmeasured was never part of the defect.
            (
                "result-inline-payload-control",
                "struct R25 { s: String }\n\
             enum K5 { A(R25), B }\n\
             impl Drop for K5 { fn drop(mut ref self) { println(\"dK5\") } }\n\
             #[allow(partial_move_of_drop_enum)]\nfn show(x: Result[K5, i64], acc: mut ref Vec[R25]) { match x { Result.Ok(K5.A(r)) => { acc.push(r) } Result.Ok(K5.B) => {} Result.Err(e) => { println(f\"e:{e}\") } } }\n\
             #[allow(partial_move_of_drop_enum)]\nfn main() { let mut acc: Vec[R25] = []; show(Result.Ok(K5.A(R25 { s: f\"z\" })), mut acc); println(f\"len:{acc.len()}\"); println(\"end\") }\n",
                "len:1\nend\n",
            ),
            // 14 — THE CEILING, from the side that a broader fix breaks. The leaf is
            //       9 words, WIDER than `Option`'s 3-word payload area, so the arm's
            //       binding is a view into the box and owns nothing: the box must keep
            //       freeing it. The first version of this fix disarmed on the BINDING
            //       alone and leaked all three `String`s here (27 B in 3 blocks),
            //       reddening both ASAN ratchets and four existing fixtures.
            (
                "wide-leaf-read-only-must-not-be-disarmed",
                "struct R39 { s: String, t: String, u: String }\n\
             enum Kw9 { A(R39), B }\n\
             fn mkr(i: i64) -> R39 { return R39 { s: f\"ssssssss{i}\", t: f\"tttttttt{i}\", u: f\"uuuuuuuu{i}\" }; }\n\
             fn nested(x: Option[Kw9]) { match x { Option.Some(Kw9.A(r)) => { println(f\"n:{r.s}\") } Option.Some(Kw9.B) => {} Option.None => {} } }\n\
             #[allow(partial_move_of_drop_enum)]\nfn main() { nested(Option.Some(Kw9.A(mkr(1)))); println(\"end\") }\n",
                "n:ssssssss1\nend\n",
            ),
            // 15 — the exact flip point: 4 words, one over the area, and clean before
            //       and after. With cell 1's 3-word leaf this pair localises the ceiling
            //       to the area rather than to any property of the struct's shape.
            (
                "four-word-leaf-read-only-is-the-flip-point",
                "struct R4w { s: String, a: i64 }\n\
             enum K4w { A(R4w), B }\n\
             fn nested(x: Option[K4w]) { match x { Option.Some(K4w.A(r)) => { println(f\"n:{r.s}:{r.a}\") } Option.Some(K4w.B) => {} Option.None => {} } }\n\
             #[allow(partial_move_of_drop_enum)]\nfn main() { nested(Option.Some(K4w.A(R4w { s: f\"ssssssss1\", a: 7 }))); println(\"end\") }\n",
                "n:ssssssss1:7\nend\n",
            ),
            // 16 — why it is the AREA and not the constant 3. A 5-word leaf is over
            //       `Option`'s ceiling and under `Result`'s, and under `Result` it owns
            //       itself: this cell ABORTED before the fix and is clean after, while
            //       cell 15's 4-word leaf under `Option` was never part of the defect.
            (
                "five-word-leaf-under-result-is-disarmed",
                "struct R5w { s: String, a: i64, b: i64 }\n\
             enum K5w { A(R5w), B }\n\
             fn nested(x: Result[K5w, i64]) { match x { Result.Ok(K5w.A(r)) => { println(f\"n:{r.s}:{r.a}\") } Result.Ok(K5w.B) => {} Result.Err(e) => { println(\"er\") } } }\n\
             #[allow(partial_move_of_drop_enum)]\nfn main() { nested(Result.Ok(K5w.A(R5w { s: f\"ssssssss1\", a: 7, b: 8 }))); println(\"end\") }\n",
                "n:ssssssss1:7\nend\n",
            ),
            // 17 — a 3-word matched leaf beside a 12-word sibling variant, so the
            //       ENUM is wide while the POSITION is not. It aborted before the fix,
            //       which is what makes the ceiling a per-position question.
            (
                "narrow-leaf-beside-a-wider-sibling-variant",
                "struct R3n { s: String }\n\
             struct Wid { a: String, b: String, c: String, d: String }\n\
             enum Kmix { A(R3n), B(Wid) }\n\
             #[allow(partial_move_of_drop_enum)]\nfn main() { let x: Option[Kmix] = Option.Some(Kmix.A(R3n { s: f\"ssssssss1\" })); match x { Option.Some(Kmix.A(r)) => { println(f\"n:{r.s}\") } Option.Some(Kmix.B(w)) => { println(f\"b:{w.a}\") } Option.None => {} } println(\"end\") }\n",
                "n:ssssssss1\nend\n",
            ),
        ] {
            let Some(out) = run_program(body) else {
                return;
            };
            assert_eq!(out, want, "[{label}]");
        }
}

/// B-2026-09-09-11 — a nested pattern over a SHARED enum inside
/// `Option`/`Result` matched NOTHING, so the arm fell through to `None` and
/// the program was silently wrong on every compiled backend.
///
/// `reconstruct_payload_value`'s nested-variant arm rebuilt an inline
/// `{tag, words…}` aggregate out of the enclosing payload words. For a
/// shared enum that word is an RC HANDLE, so the handle landed where the
/// tag belongs and the nested test compared a pointer against a small
/// integer — false for every variant. `pattern_payload_llvm_type` had said
/// "Shared enums stay a single RC pointer" since B-2026-07-15-5; this arm
/// simply never asked.
///
/// WHAT THE PROBE MATRIX ESTABLISHED, because three plausible causes were
/// wrong and the cells are the argument:
///
///   `Some(_)` / `is_some()`   correct  — the OPTION tag test was fine
///   `Some(k)` then `match k`  correct  — the shared enum matched fine ALONE
///   the same nested pattern, NON-shared enum   correct
///   `Option.Some(Kn.A(n))`, shared            fell through to `None`
///
/// and a single-`i64` payload (NOT boxed) failed identically, which refuted
/// the filing row's own "probably the boxing of an RC handle" hypothesis:
/// boxing is not involved at any size.
///
/// The JIT failed differently — `fatal runtime error: stack overflow` — and
/// the row flagged that as an unproven inference about a shared root. It
/// was one: it is gone here too, with no separate change.
///
/// Cells 5-7 are the shapes that were ALREADY correct and must stay so,
/// since the fix changes what a nested sub-pattern reconstructs to.
#[test]
fn e2e_nested_shared_enum_pattern_matches_through_its_handle() {
    const SHARED: &str = "shared enum Kn { A(i64), B }\n";
    for (label, src, want) in [
            // 1 — the defect, minimal: a one-word payload, so nothing is boxed.
            (
                "option-nested-shared",
                format!("{SHARED}fn main() {{ let x: Option[Kn] = Option.Some(Kn.A(7)); match x {{ Option.Some(Kn.A(n)) => {{ println(f\"a:{{n}}\"); }}, Option.Some(Kn.B) => {{ println(\"b\"); }}, Option.None => {{ println(\"none\"); }} }} }}\n"),
                "a:7\n",
            ),
            // 2 — the OTHER variant, which must select the second arm rather
            //     than the first: a tag test that is merely non-crashing would
            //     pass cell 1 and fail here.
            (
                "option-nested-shared-second-variant",
                format!("{SHARED}fn main() {{ let x: Option[Kn] = Option.Some(Kn.B); match x {{ Option.Some(Kn.A(n)) => {{ println(f\"a:{{n}}\"); }}, Option.Some(Kn.B) => {{ println(\"b\"); }}, Option.None => {{ println(\"none\"); }} }} }}\n"),
                "b\n",
            ),
            // 3 — and `None` must still reach the `None` arm, which is the arm
            //     the defect wrongly selected for everything.
            (
                "option-none-still-none",
                format!("{SHARED}fn main() {{ let x: Option[Kn] = Option.None; match x {{ Option.Some(Kn.A(n)) => {{ println(f\"a:{{n}}\"); }}, Option.Some(Kn.B) => {{ println(\"b\"); }}, Option.None => {{ println(\"none\"); }} }} }}\n"),
                "none\n",
            ),
            // 4 — the `Result` spelling, listed NOT MEASURED on the row, and a
            //     heap payload so the binding is exercised too.
            (
                "result-nested-shared-heap-payload",
                "struct R2 { s: String, t: String, u: String }\n\
                 shared enum Ks { A(R2), B }\n\
                 fn mkr(i: i64) -> R2 { return R2 { s: f\"ss{i}\", t: f\"tt{i}\", u: f\"uu{i}\" }; }\n\
                 fn show(x: Result[Ks, i64]) { match x { Result.Ok(Ks.A(r)) => { println(f\"a:{r.s}\"); }, Result.Ok(Ks.B) => {}, Result.Err(e) => {} } }\n\
                 fn main() { let mut i = 0; while i < 3 { show(Result.Ok(Ks.A(mkr(i)))); i = i + 1; } }\n"
                    .to_string(),
                "a:ss0\na:ss1\na:ss2\n",
            ),
            // 5 — CONTROL: the bare spelling with no `Option` wrapper. Always
            //     worked, because a top-level scrutinee goes through
            //     `extract_enum_tag`, which is what the nested path now asks.
            (
                "bare-shared-control",
                format!("{SHARED}fn main() {{ let k: Kn = Kn.A(7); match k {{ Kn.A(n) => {{ println(f\"a:{{n}}\"); }}, Kn.B => {{ println(\"b\"); }} }} }}\n"),
                "a:7\n",
            ),
            // 6 — CONTROL: the whole-payload binding, which reaches the shared
            //     enum through a local and was never affected.
            (
                "whole-payload-binding-control",
                format!("{SHARED}fn main() {{ let x: Option[Kn] = Option.Some(Kn.A(7)); match x {{ Option.Some(k) => {{ match k {{ Kn.A(n) => {{ println(f\"a:{{n}}\"); }}, Kn.B => {{ println(\"b\"); }} }} }}, Option.None => {{ println(\"none\"); }} }} }}\n"),
                "a:7\n",
            ),
            // 7 — CONTROL: the identical nested pattern on a NON-shared enum,
            //     the cell that isolated `shared` as the variable.
            (
                "nonshared-nested-control",
                "enum Kn { A(i64), B }\n\
                 fn main() { let x: Option[Kn] = Option.Some(Kn.A(7)); match x { Option.Some(Kn.A(n)) => { println(f\"a:{n}\"); }, Option.Some(Kn.B) => { println(\"b\"); }, Option.None => { println(\"none\"); } } }\n"
                    .to_string(),
                "a:7\n",
            ),
        ] {
            let Some(out) = run_program(&src) else {
                return;
            };
            assert_eq!(out, want, "[{label}]");
        }
}

/// B-2026-09-05-34 — AN `if let (r, k) = t` OVER AN OWNED TUPLE PARAM RAN THE
/// ELEMENT'S `Drop` BODY ON THE WRONG OWNER (AND FREED ITS HEAP TWICE).
///
/// The row's cell — `fn t_iflet(t: (R, i64)) -> i64 { if let (r, k) = t { k }
/// else { 0 } }` — aborted `free(): double free detected in tcache 2` on the
/// JIT while `--interp` and `karac build` printed `dR9 r0`. The JIT executes
/// raw IR and `karac build` runs `default<O2>` first, which folded one of the
/// two frees away: `KARAC_OPT_LEVEL=0 karac build` aborted too. The memory
/// half is pinned by `tests/memory_sanitizer.rs`'s
/// `asan_iflet_bare_tuple_element_binding_is_not_a_second_owner`; THIS pin is
/// the BODY-count half, which `-O2` did not mask on three cells:
///
/// - `p_rebind` / `l_rebind` / `l_two` — the element rebound inside the block
///   ran `dR` twice (`dR8 dR8 r8` for the local spelling, on the interpreter
///   as well: its single-pattern disarm had no tuple arm, where the `match`
///   form's has had one since B-2026-09-02-26).
/// - `p_out` — the element HANDED OUT of the then-block ran `dR3 r3 dR3` on
///   every compiled backend against `r3 dR3`: the caller-side predicate
///   `fn_returns_param_part_paths` aliased a `match` arm's pattern bindings
///   (B-2026-09-02-24) and not an `if let`'s, so the returned element was
///   never seen to escape and the caller ran its body a second time.
///
/// One mechanism throughout: the three single-pattern legs (`if let`,
/// `while let`, `let … else`) never ran the bare-tuple staging the `match`
/// arm loop runs (`stage_bare_tuple_bindings_for_bind`,
/// `record_bare_tuple_elem_sources`, the bodies disarm, the tail hand-out
/// hook), and the two AST predicates that feed both backends had `match`-only
/// arms. Every cell here is identical on interpreter, JIT, `-O0`, `-O2` and
/// `KARAC_AUTO_PAR=0`.
///
/// THE CONTROLS: `m_read` is the `match` spelling (correct before and after);
/// `p_read` / `p_call` / `p_field` / `p_letelse` / `p_while` / `l_read` were
/// memory-wrong but body-correct at `-O2` and must stay at one body each.
///
/// Twin of `tests/interpreter.rs`'s `test_iflet_bare_tuple_elem_runs_one_body`, pinned to the same string.
#[test]
fn e2e_iflet_bare_tuple_elem_runs_one_body() {
    let Some(out) = run_program(
        r#"struct R { id: i64, tag: String, xs: Vec[i64] }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, tag: f"t{i}", xs: [i] } }
fn consume(x: R) -> i64 { return x.id }
struct H { t: (R, i64) }

fn p_read(t: (R, i64)) -> i64 { if let (r, k) = t { k } else { 0 } }
fn p_call(t: (R, i64)) -> i64 { if let (r, k) = t { consume(r) } else { 0 } }
fn p_rebind(t: (R, i64)) -> i64 { if let (r, k) = t { let m = r; m.id } else { 0 } }
fn p_out(t: (R, i64)) -> R { if let (r, k) = t { r } else { mk(0) } }
fn p_field(h: H) -> i64 { if let (r, k) = h.t { k } else { 0 } }
fn p_letelse(t: (R, i64)) -> i64 { let (r, k) = t else { return 0 }; k }
fn p_while(t: (R, i64)) -> i64 { while let (r, k) = t { return k }; 0 }
fn l_read() -> i64 { let t = (mk(21), 0); if let (r, k) = t { k } else { 0 } }
fn l_rebind() -> i64 { let t = (mk(22), 0); if let (r, k) = t { let m = r; m.id } else { 0 } }
fn l_two() -> i64 { let t = (mk(23), mk(24)); if let (a, b) = t { let m = a; m.id } else { 0 } }
fn m_read(t: (R, i64)) -> i64 { match t { (r, k) => { k } } }

fn main() {
    println("p_read"); let a = p_read((mk(1), 0)); println(f"  r{a}");
    println("p_call"); let b = p_call((mk(2), 0)); println(f"  r{b}");
    println("p_rebind"); let c = p_rebind((mk(3), 0)); println(f"  r{c}");
    println("p_out"); let d = p_out((mk(4), 0)); println(f"  r{d.id}");
    println("p_field"); let e = p_field(H { t: (mk(5), 0) }); println(f"  r{e}");
    println("p_letelse"); let f = p_letelse((mk(6), 0)); println(f"  r{f}");
    println("p_while"); let g = p_while((mk(7), 0)); println(f"  r{g}");
    println("l_read"); let h = l_read(); println(f"  r{h}");
    println("l_rebind"); let i = l_rebind(); println(f"  r{i}");
    println("l_two"); let j = l_two(); println(f"  r{j}");
    println("m_read"); let n = m_read((mk(8), 0)); println(f"  r{n}");
    println("end");
}
"#,
    ) else {
        return;
    };
    assert_eq!(
        out,
        r#"p_read
dR1
  r0
p_call
dR2
  r2
p_rebind
dR3
  r3
p_out
  r4
dR4
p_field
dR5
  r0
p_letelse
dR6
  r0
p_while
dR7
  r0
l_read
dR21
  r0
l_rebind
dR22
  r22
l_two
dR23
dR24
  r23
m_read
dR8
  r0
end
"#
    );
}

/// B-2026-09-05-28 / B-2026-09-05-30 — codegen twin of
/// `tests/interpreter.rs`'s
/// `test_match_arm_element_moved_into_a_callee_or_unread_runs_one_body`,
/// same program and string. The compiled backends were the correct
/// reference throughout: the caller's per-element walk over an owned tuple
/// argument never consulted the whole-param payload predicate here, so an
/// element moved into a by-value callee (`consume(r)`) died in that callee
/// and an unread one (`(r, k) => k`) at the arm's end, one body each.
/// Pinned so the two backends stay at the interpreter's now-matching
/// answer; the ASAN twin runs the same program under the sanitizer.
#[test]
fn e2e_match_arm_element_moved_into_a_callee_or_unread_runs_one_body() {
    let Some(out) = run_program(
        r#"struct R { id: i64, tag: String, xs: Vec[i64] }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, tag: f"t{i}", xs: [i] } }
fn consume(x: R) -> i64 { return x.id }
struct H { n: i64 }
impl H {
    fn m_call(ref self, t: (R, i64)) -> i64 { match t { (r, k) => { consume(r) + self.n } } }
    fn m_unread(ref self, t: (R, i64)) -> i64 { match t { (r, k) => { k + self.n } } }
}
fn p_call(t: (R, i64)) -> i64 { match t { (r, k) => { consume(r) } } }
fn p_unread(t: (R, i64)) -> i64 { match t { (r, k) => { k } } }
fn p_ret(t: (R, i64)) -> R { match t { (r, k) => { r } } }
fn p_read(t: (R, i64)) -> i64 { match t { (r, k) => { r.id } } }
fn p_rebind_call(t: (R, i64)) -> i64 { match t { (r, k) => { let g: R = r; consume(g) } } }
fn p_wild(t: (R, i64)) -> i64 { match t { (_, k) => { k } } }
fn p_let_call(t: (R, i64)) -> i64 { let (r, k) = t; consume(r) }
fn t_two(t: (R, R)) -> R { match t { (a, b) => { a } } }
fn t_two_call(t: (R, R)) -> i64 { match t { (a, b) => { consume(a) } } }
fn t_nested(t: ((R, i64), i64)) -> i64 { match t { ((r, j), k) => { k } } }
fn t_cond(t: (R, i64), c: bool) -> i64 { match t { (r, k) => { if c { consume(r) } else { k } } } }
fn t_nested_match(t: (R, i64)) -> i64 { match t { (r, k) => { match k { 0 => { consume(r) }, _ => { k } } } } }
fn t_two_arms(t: (R, i64)) -> i64 { match t { (r, 0) => { consume(r) }, (r, k) => { k } } }
fn main() {
    let h: H = H { n: 100 };
    { let d: i64 = p_call((mk(1), 0)); println(f"r{d}"); println("one") }
    { let d: i64 = p_unread((mk(2), 0)); println(f"r{d}"); println("two") }
    { let a: R = p_ret((mk(3), 0)); println(f"r{a.id}"); println("three") }
    { let d: i64 = p_read((mk(4), 0)); println(f"r{d}"); println("four") }
    { let d: i64 = p_rebind_call((mk(6), 0)); println(f"r{d}"); println("six") }
    { let d: i64 = p_wild((mk(7), 0)); println(f"r{d}"); println("seven") }
    { let d: i64 = p_let_call((mk(9), 0)); println(f"r{d}"); println("nine") }
    { let t: (R, i64) = (mk(10), 0); let d: i64 = p_call(t); println(f"r{d}"); println("ten") }
    { let t: (R, i64) = (mk(11), 0); let d: i64 = p_unread(t); println(f"r{d}"); println("eleven") }
    { let a: R = t_two((mk(12), mk(13))); println(f"r{a.id}"); println("twelve") }
    { let d: i64 = t_two_call((mk(14), mk(15))); println(f"r{d}"); println("fourteen") }
    { let d: i64 = t_nested(((mk(16), 0), 0)); println(f"r{d}"); println("sixteen") }
    { let d: i64 = h.m_call((mk(17), 0)); println(f"r{d}"); println("seventeen") }
    { let d: i64 = h.m_unread((mk(18), 0)); println(f"r{d}"); println("eighteen") }
    { let d: i64 = t_cond((mk(19), 0), true); println(f"r{d}"); println("nineteen") }
    { let d: i64 = t_cond((mk(20), 0), false); println(f"r{d}"); println("twenty") }
    { let d: i64 = t_nested_match((mk(21), 0)); println(f"r{d}"); println("twentyone") }
    { let d: i64 = t_nested_match((mk(22), 5)); println(f"r{d}"); println("twentytwo") }
    { let d: i64 = t_two_arms((mk(23), 0)); println(f"r{d}"); println("twentythree") }
    { let d: i64 = t_two_arms((mk(24), 3)); println(f"r{d}"); println("twentyfour") }
    { let t: (R, R) = (mk(25), mk(26)); let d: i64 = t_two_call(t); println(f"r{d}"); println("twentysix") }
    println("end")
}
"#,
    ) else {
        return;
    };
    assert_eq!(out, "dR1\nr1\none\ndR2\nr0\ntwo\nr3\ndR3\nthree\ndR4\nr4\nfour\ndR6\nr6\nsix\ndR7\nr0\nseven\ndR9\nr9\nnine\ndR10\nr10\nten\ndR11\nr0\neleven\ndR13\nr12\ndR12\ntwelve\ndR14\ndR15\nr14\nfourteen\ndR16\nr0\nsixteen\ndR17\nr117\nseventeen\ndR18\nr100\neighteen\ndR19\nr19\nnineteen\ndR20\nr0\ntwenty\ndR21\nr21\ntwentyone\ndR22\nr5\ntwentytwo\ndR23\nr23\ntwentythree\ndR24\nr3\ntwentyfour\ndR25\ndR26\nr25\ntwentysix\nend\n");
}

/// B-2026-09-05-33 — a match arm over an owned tuple parameter whose
/// element leaves by a route other than a bare/alias return has ONE owner
/// on every compiled backend. Five routes, two channels:
///
/// BODIES. The caller-side skip list for a tuple argument came from
/// `callee_returned_param_parts` alone, which classifies only a returned
/// expression that DENOTES the element and resolves free functions only.
/// It now unions `fn_returns_param_tuple_arm_elems` (the predicate
/// B-2026-09-05-28 built for the interpreter: forwarded through a call
/// that returns it, handed to a callee that stores it, stored under an
/// outliving root, assigned into a returned place) and resolves through
/// `find_function_ast`, so the METHOD path gets a skip list at all — it
/// carried the plain wrapper's empty set and ran `dR13 r13 dR13` for one
/// object (`five`, `twelve`).
///
/// MEMORY. The shared move-suppressor (`suppress_source_vec_cleanup_for_
/// arg_ex`, reached by `v.push(r)`, `out = r` and a by-value argument
/// alike) zeroed the BINDING's caps, and a bare-tuple element binding is a
/// bit-copy of the scrutinee's element — so the tuple's own drop at the
/// merge freed the buffers the moved value carries: `free(): double free
/// detected in tcache 2` on `three` and `four`. It now also zeroes the
/// SOURCE element (`zero_bare_tuple_elem_source_for_moved`, the hook the
/// arm-tail and `return` sites already use since B-2026-09-05-27).
///
/// `one`/`two`/`three`/`four` are the row's four free-function cells,
/// `five` the method-path bare return, `six` a two-`Drop` method tuple
/// whose unread sibling must still fire, `eight` push-one-consume-other,
/// `ten`..`twelve` the named-local argument spellings, `thirteen` two
/// forwarding calls in one block (the shared temp names composing). The
/// interpreter twin carries the same program; the ASAN twin runs it under
/// the sanitizer.
#[test]
fn e2e_match_arm_element_escaping_by_call_store_or_assignment_has_one_owner() {
    let Some(out) = run_program(
        r#"struct R { id: i64, tag: String, xs: Vec[i64] }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, tag: f"t{i}", xs: [i] } }
fn consume(x: R) -> i64 { return x.id }
fn wrap(x: R) -> R { return x }
fn stash(x: R, v: mut ref Vec[R]) { v.push(x) }
struct H { n: i64 }
impl H {
    fn m_ret(ref self, t: (R, i64)) -> R { match t { (r, k) => { r } } }
    fn m_two(ref self, t: (R, R)) -> R { match t { (a, b) => { b } } }
}
fn t_fwd(t: (R, i64)) -> R { match t { (r, k) => { wrap(r) } } }
fn t_stash(t: (R, i64), v: mut ref Vec[R]) -> i64 { match t { (r, k) => { stash(r, v); k } } }
fn t_push(t: (R, i64), v: mut ref Vec[R]) -> i64 { match t { (r, k) => { v.push(r); k } } }
fn t_assign(t: (R, i64)) -> R { let mut out: R = mk(50); match t { (r, k) => { out = r; } } out }
fn t_two_push(t: (R, R), v: mut ref Vec[R]) -> i64 { match t { (a, b) => { v.push(a); consume(b) } } }
fn main() {
    let h: H = H { n: 1 };
    { let a: R = t_fwd((mk(1), 0)); println(f"r{a.id}"); println("one") }
    { let mut v: Vec[R] = []; let d: i64 = t_stash((mk(2), 0), mut v); println(f"r{d} n{v.len()}"); println("two") }
    { let mut v: Vec[R] = []; let d: i64 = t_push((mk(3), 0), mut v); println(f"r{d} n{v.len()}"); println("three") }
    { let a: R = t_assign((mk(4), 0)); println(f"r{a.id}"); println("four") }
    { let a: R = h.m_ret((mk(5), 0)); println(f"r{a.id}"); println("five") }
    { let a: R = h.m_two((mk(6), mk(7))); println(f"r{a.id}"); println("six") }
    { let mut v: Vec[R] = []; let d: i64 = t_two_push((mk(8), mk(9)), mut v); println(f"r{d} n{v.len()}"); println("eight") }
    { let t: (R, i64) = (mk(10), 0); let a: R = t_fwd(t); println(f"r{a.id}"); println("ten") }
    { let t: (R, i64) = (mk(11), 0); let mut v: Vec[R] = []; let d: i64 = t_push(t, mut v); println(f"r{d} n{v.len()}"); println("eleven") }
    { let t: (R, i64) = (mk(12), 0); let a: R = h.m_ret(t); println(f"r{a.id}"); println("twelve") }
    { let a: R = t_fwd((mk(13), 0)); let b: R = t_fwd((mk(14), 0)); println(f"r{a.id}{b.id}"); println("thirteen") }
    println("end")
}
"#,
    ) else {
        return;
    };
    assert_eq!(out, "r1\ndR1\none\nr0 n1\ndR2\ntwo\nr0 n1\ndR3\nthree\ndR50\nr4\ndR4\nfour\nr5\ndR5\nfive\ndR6\nr7\ndR7\nsix\nr9 n1\ndR8\neight\nr10\ndR10\nten\nr0 n1\ndR11\neleven\nr12\ndR12\ntwelve\nr1314\ndR14\ndR13\nthirteen\nend\n");
}

/// B-2026-09-05-35 — a match arm over an owned by-value ENUM parameter
/// whose payload binding is consumed by a by-value callee, read, or never
/// used runs the payload's `Drop` body exactly once, and a sibling arm's
/// hand-back no longer silences it.
///
/// The whole-param `fn_returns_param_payload` stood the caller's
/// payload-bodies walk down on two over-approximations: a binding passed
/// to ANY call counted as leaving (`consume(r)`, whose callee returns
/// `x.id`), and a MIXED-arm callee answered for the whole parameter
/// (`E.B(k) => k` handing back an `i64` silenced the `E.A` arm too). Both
/// backends asked it, so the cells were agreed-and-wrong. The predicate is
/// now program-aware and per VARIANT (`fn_escaping_param_payload_variants`,
/// with the store routes the tuple-arm predicate already had): the
/// fresh-temp registrar masks the escaping variants' fields out of the
/// payload-bodies walker (`emit_enum_payload_user_drop_bodies_fn_skipping`)
/// and lets the tag switch decide at run time, the named-binding gates
/// swap the binding's walker for the masked one in place
/// (`mask_enum_payload_bodies_for_var`), and the method registrar stops
/// folding the payload route into its memory-only `escapes_frame` exit.
///
/// `one`/`two` are the row's cells, `three` the read, `four`/`five` the
/// return and forwarding hand-outs that must still stand down, `six`/
/// `seven` a stash / push (the store routes — `six` regressed to two
/// bodies mid-fix until the payload predicate learned them), `eight` a
/// statement-position consume, `nine`..`thirteen` a two-variant enum with
/// an empty variant and the `if let` / wildcard spellings, `fourteen`/
/// `fifteen` the METHOD path, `sixteen`..`eighteen` named-local arguments
/// (masked walker), `nineteen` the sibling variant taken. The interpreter
/// twin carries the same program; the ASAN twin runs it under the
/// sanitizer.
#[test]
fn e2e_enum_payload_consumed_or_unread_in_the_arm_runs_one_body() {
    let Some(out) = run_program(
        r#"struct R { id: i64, tag: String, xs: Vec[i64] }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, tag: f"t{i}", xs: [i] } }
fn consume(x: R) -> i64 { return x.id }
fn wrap(x: R) -> R { return x }
fn stash(x: R, v: mut ref Vec[R]) { v.push(x) }
enum E { A(R), B(i64) }
enum O { S(R), N }
struct H { n: i64 }
impl H {
    fn m_call(ref self, b: E) -> i64 { match b { E.A(r) => { consume(r) + self.n }, E.B(k) => { k } } }
    fn m_unread(ref self, b: E) -> i64 { match b { E.A(r) => { self.n }, E.B(k) => { k } } }
}
fn e_call(b: E) -> i64 { match b { E.A(r) => { consume(r) }, E.B(k) => { k } } }
fn e_unread(b: E) -> i64 { match b { E.A(r) => { 5 }, E.B(k) => { k } } }
fn e_read(b: E) -> i64 { match b { E.A(r) => { r.id }, E.B(k) => { k } } }
fn e_ret(b: E) -> R { match b { E.A(r) => { r }, E.B(k) => { mk(k) } } }
fn e_fwd(b: E) -> R { match b { E.A(r) => { wrap(r) }, E.B(k) => { mk(k) } } }
fn e_stash(b: E, v: mut ref Vec[R]) -> i64 { match b { E.A(r) => { stash(r, v); 1 }, E.B(k) => { k } } }
fn e_push(b: E, v: mut ref Vec[R]) -> i64 { match b { E.A(r) => { v.push(r); 1 }, E.B(k) => { k } } }
fn e_call_stmt(b: E) -> i64 { match b { E.A(r) => { let d: i64 = consume(r); d + 1 }, E.B(k) => { k } } }
fn o_call(b: O) -> i64 { match b { O.S(r) => { consume(r) }, O.N => { 0 } } }
fn o_unread(b: O) -> i64 { match b { O.S(r) => { 5 }, O.N => { 0 } } }
fn o_unread_single(b: O) -> i64 { if let O.S(r) = b { 5 } else { 0 } }
fn o_call_single(b: O) -> i64 { if let O.S(r) = b { consume(r) } else { 0 } }
fn o_wild(b: O) -> i64 { match b { O.S(_) => { 5 }, O.N => { 0 } } }
fn main() {
    let h: H = H { n: 100 };
    { let d: i64 = e_call(E.A(mk(1))); println(f"r{d}"); println("one") }
    { let d: i64 = e_unread(E.A(mk(2))); println(f"r{d}"); println("two") }
    { let d: i64 = e_read(E.A(mk(3))); println(f"r{d}"); println("three") }
    { let a: R = e_ret(E.A(mk(4))); println(f"r{a.id}"); println("four") }
    { let a: R = e_fwd(E.A(mk(5))); println(f"r{a.id}"); println("five") }
    { let mut v: Vec[R] = []; let d: i64 = e_stash(E.A(mk(6)), mut v); println(f"r{d} n{v.len()}"); println("six") }
    { let mut v: Vec[R] = []; let d: i64 = e_push(E.A(mk(7)), mut v); println(f"r{d} n{v.len()}"); println("seven") }
    { let d: i64 = e_call_stmt(E.A(mk(8))); println(f"r{d}"); println("eight") }
    { let d: i64 = o_call(O.S(mk(9))); println(f"r{d}"); println("nine") }
    { let d: i64 = o_unread(O.S(mk(10))); println(f"r{d}"); println("ten") }
    { let d: i64 = o_unread_single(O.S(mk(11))); println(f"r{d}"); println("eleven") }
    { let d: i64 = o_call_single(O.S(mk(12))); println(f"r{d}"); println("twelve") }
    { let d: i64 = o_wild(O.S(mk(13))); println(f"r{d}"); println("thirteen") }
    { let d: i64 = h.m_call(E.A(mk(14))); println(f"r{d}"); println("fourteen") }
    { let d: i64 = h.m_unread(E.A(mk(15))); println(f"r{d}"); println("fifteen") }
    { let e: E = E.A(mk(16)); let d: i64 = e_call(e); println(f"r{d}"); println("sixteen") }
    { let e: E = E.A(mk(17)); let d: i64 = e_unread(e); println(f"r{d}"); println("seventeen") }
    { let e: E = E.A(mk(18)); let a: R = e_ret(e); println(f"r{a.id}"); println("eighteen") }
    { let d: i64 = e_call(E.B(19)); println(f"r{d}"); println("nineteen") }
    println("end")
}
"#,
    ) else {
        return;
    };
    assert_eq!(out, "dR1\nr1\none\ndR2\nr5\ntwo\ndR3\nr3\nthree\nr4\ndR4\nfour\nr5\ndR5\nfive\nr1 n1\ndR6\nsix\nr1 n1\ndR7\nseven\ndR8\nr9\neight\ndR9\nr9\nnine\ndR10\nr5\nten\ndR11\nr5\neleven\ndR12\nr12\ntwelve\ndR13\nr5\nthirteen\ndR14\nr114\nfourteen\ndR15\nr100\nfifteen\ndR16\nr16\nsixteen\ndR17\nr5\nseventeen\nr18\ndR18\neighteen\nr19\nnineteen\nend\n");
}

/// B-2026-09-06-20 — a `match` (or `if let`) that destructures a mixed
/// wrap's VIEW slot binds a view. The row is the interpreter's (it gave
/// the arm binding a Drop slot beside the caller's walk); this side was
/// right on the direct read but gave `let m = a;` INSIDE the arm a full
/// body, because `a` was never marked a `param_view_locals` member.
/// `pattern_binding_masked_view_names` now carries the slots the
/// scrutinee binding's `enum_ctor_moved_payload_slots` marks, and the
/// arm binding treats them exactly like a payload of an owned-param
/// scrutinee. Interpreter twin:
/// `test_match_over_a_masked_wrap_slot_binds_a_view`.
///
/// Not here, filed separately: two FRESH payloads die in opposite orders
/// on the two backends, and the STRUCT-literal sibling doubles on this
/// side.
#[test]
fn e2e_match_over_a_masked_wrap_slot_binds_a_view() {
    let Some(out) = run_program(
        r#"struct R { id: i64, name: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, name: f"n{i}" }; }
enum W2 { Two(R, R), None2 }
struct S3 { a: R, b: R }
fn m_direct(r: R) -> i64 { let w: W2 = W2.Two(r, mk(2)); match w { W2.Two(a, b) => { return b.id; } W2.None2 => { return 0; } } }
fn m_rebind(r: R) -> i64 { let w: W2 = W2.Two(r, mk(4)); let w2: W2 = w; match w2 { W2.Two(a, b) => { return b.id; } W2.None2 => { return 0; } } }
fn m_direct_a(r: R) -> i64 { let w: W2 = W2.Two(r, mk(6)); match w { W2.Two(a, b) => { return a.id; } W2.None2 => { return 0; } } }
fn m_unread(r: R) -> i64 { let w: W2 = W2.Two(r, mk(8)); match w { W2.Two(a, b) => { return 1; } W2.None2 => { return 0; } } }
fn m_iflet(r: R) -> i64 { let w: W2 = W2.Two(r, mk(10)); if let W2.Two(a, b) = w { return b.id; } return 0; }
fn m_rebind_in_arm(r: R) -> i64 { let w: W2 = W2.Two(r, mk(12)); match w { W2.Two(a, b) => { let m: R = a; return m.id; } W2.None2 => { return 0; } } }
fn m_swap(r: R) -> i64 { let w: W2 = W2.Two(mk(14), r); match w { W2.Two(a, b) => { return a.id; } W2.None2 => { return 0; } } }
fn t_direct(r: R) -> i64 { let t: (R, R) = (r, mk(21)); match t { (a, b) => { return b.id; } } }
fn main() {
    { let v: i64 = m_direct(mk(1)); println(f"v={v}"); println("one") }
    { let v: i64 = m_rebind(mk(3)); println(f"v={v}"); println("two") }
    { let v: i64 = m_direct_a(mk(5)); println(f"v={v}"); println("three") }
    { let v: i64 = m_unread(mk(7)); println(f"v={v}"); println("four") }
    { let v: i64 = m_iflet(mk(9)); println(f"v={v}"); println("five") }
    { let v: i64 = m_rebind_in_arm(mk(11)); println(f"v={v}"); println("six") }
    { let v: i64 = m_swap(mk(13)); println(f"v={v}"); println("seven") }
    { let v: i64 = t_direct(mk(20)); println(f"v={v}"); println("ten") }
    println("end")
}
"#,
    ) else {
        return;
    };
    assert_eq!(out, "dR2\ndR1\nv=2\none\ndR4\ndR3\nv=4\ntwo\ndR6\ndR5\nv=5\nthree\ndR8\ndR7\nv=1\nfour\ndR10\ndR9\nv=10\nfive\ndR12\ndR11\nv=11\nsix\ndR14\ndR13\nv=14\nseven\ndR20\nv=21\nten\nend\n");
}

/// B-2026-09-06-22 — a `match` (or `if let`) that destructures a mixed
/// STRUCT literal's view field (`let s = S3 { a: r, b: mk(2) }; match s
/// { S3 { a, b } => .. }`) binds a view. The literal's mask
/// (`param_view_struct_fields` / `struct_moved_field_bodies`) guarded
/// `s`'s own walk only; the arm bound `a` out of the masked field and
/// gave it a full body beside the caller's walk — `dR2 dR1 dR1` on every
/// compiled surface against the interpreter's `dR2 dR1`. The struct arm
/// of `masked_payload_view_names_for` now feeds
/// `pattern_binding_masked_view_names`, the set B-2026-09-06-20 added
/// for the enum spelling. Interpreter twin:
/// `test_match_over_a_masked_struct_field_binds_a_view`.
///
/// Not here, filed separately: the `let S3 { a, b } = s;` destructure of
/// the same literal doubles on all four surfaces.
#[test]
fn e2e_match_over_a_masked_struct_field_binds_a_view() {
    let Some(out) = run_program(
        r#"struct R { id: i64, name: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, name: f"n{i}" }; }
struct S3 { a: R, b: R }
fn s_direct(r: R) -> i64 { let s: S3 = S3 { a: r, b: mk(2) }; match s { S3 { a, b } => { return b.id; } } }
fn s_direct_a(r: R) -> i64 { let s: S3 = S3 { a: r, b: mk(4) }; match s { S3 { a, b } => { return a.id; } } }
fn s_unread(r: R) -> i64 { let s: S3 = S3 { a: r, b: mk(6) }; match s { S3 { a, b } => { return 1; } } }
fn s_rebind(r: R) -> i64 { let s: S3 = S3 { a: r, b: mk(8) }; let s2: S3 = s; match s2 { S3 { a, b } => { return b.id; } } }
fn s_iflet(r: R) -> i64 { let s: S3 = S3 { a: r, b: mk(10) }; if let S3 { a, b } = s { return b.id; } return 0; }
fn s_rebind_in_arm(r: R) -> i64 { let s: S3 = S3 { a: r, b: mk(12) }; match s { S3 { a, b } => { let m: R = a; return m.id; } } }
fn s_swap(r: R) -> i64 { let s: S3 = S3 { a: mk(14), b: r }; match s { S3 { a, b } => { return a.id; } } }
fn s_fresh(r: R) -> i64 { let s: S3 = S3 { a: mk(16), b: mk(17) }; match s { S3 { a, b } => { return a.id; } } }
fn s_partial(r: R) -> i64 { let s: S3 = S3 { a: r, b: mk(21) }; match s { S3 { b, .. } => { return b.id; } } }
fn main() {
    { let v: i64 = s_direct(mk(1)); println(f"v={v}"); println("one") }
    { let v: i64 = s_direct_a(mk(3)); println(f"v={v}"); println("two") }
    { let v: i64 = s_unread(mk(5)); println(f"v={v}"); println("three") }
    { let v: i64 = s_rebind(mk(7)); println(f"v={v}"); println("four") }
    { let v: i64 = s_iflet(mk(9)); println(f"v={v}"); println("five") }
    { let v: i64 = s_rebind_in_arm(mk(11)); println(f"v={v}"); println("six") }
    { let v: i64 = s_swap(mk(13)); println(f"v={v}"); println("seven") }
    { let v: i64 = s_fresh(mk(15)); println(f"v={v}"); println("eight") }
    { let v: i64 = s_partial(mk(20)); println(f"v={v}"); println("ten") }
    println("end")
}
"#,
    ) else {
        return;
    };
    assert_eq!(out, "dR2\ndR1\nv=2\none\ndR4\ndR3\nv=3\ntwo\ndR6\ndR5\nv=1\nthree\ndR8\ndR7\nv=8\nfour\ndR10\ndR9\nv=10\nfive\ndR12\ndR11\nv=11\nsix\ndR14\ndR13\nv=14\nseven\ndR17\ndR16\ndR15\nv=16\neight\ndR21\ndR20\nv=21\nten\nend\n");
}

/// B-2026-09-06-33 — a struct `let` pattern's fields were extracted from
/// the value by PATTERN POSITION (`build_extract_value(sv, idx)` with
/// `idx` the pattern field's index), which is only right when the
/// pattern names every field in declaration order. A partial pattern
/// `let P3 { z, .. } = p` read field `x` into `z` (`v=1` where `z` is
/// 3), a reordered `let P3 { z, x, y } = p` permuted all three
/// (`v=132` for `321`), on every compiled surface, no `Drop` involved.
/// The index now resolves by NAME through `struct_field_names`, as the
/// cleanup registration beside it always did. Interpreter twin:
/// `test_partial_or_reordered_struct_let_pattern_binds_by_name`.
#[test]
fn e2e_partial_or_reordered_struct_let_pattern_binds_by_name() {
    let Some(out) = run_program(
        r#"struct R { id: i64, name: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, name: f"n{i}" }; }
struct S3 { a: R, b: R }
struct P3 { x: i64, y: i64, z: i64 }
fn p_view_b(r: R) -> i64 { let s: S3 = S3 { a: r, b: mk(5) }; let S3 { b, .. } = s; return b.id; }
fn p_scalar_partial() -> i64 { let p: P3 = P3 { x: 1, y: 2, z: 3 }; let P3 { z, .. } = p; return z; }
fn p_scalar_swapped() -> i64 { let p: P3 = P3 { x: 1, y: 2, z: 3 }; let P3 { z, x, y } = p; return z * 100 + y * 10 + x; }
fn p_scalar_mid() -> i64 { let p: P3 = P3 { x: 1, y: 2, z: 3 }; let P3 { y, .. } = p; return y; }
fn p_scalar_rename() -> i64 { let p: P3 = P3 { x: 1, y: 2, z: 3 }; let P3 { z: w, x: v, .. } = p; return w * 10 + v; }
fn main() {
    { let v: i64 = p_view_b(mk(4)); println(f"v={v}"); println("one") }
    { let v: i64 = p_scalar_partial(); println(f"v={v}"); println("two") }
    { let v: i64 = p_scalar_swapped(); println(f"v={v}"); println("three") }
    { let v: i64 = p_scalar_mid(); println(f"v={v}"); println("four") }
    { let v: i64 = p_scalar_rename(); println(f"v={v}"); println("five") }
    println("end")
}
"#,
    ) else {
        return;
    };
    assert_eq!(
        out,
        "dR5\ndR4\nv=5\none\nv=3\ntwo\nv=321\nthree\nv=2\nfour\nv=31\nfive\nend\n"
    );
}

/// B-2026-09-06-33 — the Drop-bearing half: with the field misread, a
/// partial `let S3 { b, .. } = s` bound field `a`'s copy to `b`, so
/// `a`'s buffer was freed by the leaf AND by the source's own drop
/// (`free(): double free` on the JIT, `dR2 dR2 dR1 v=2` on AOT) and
/// `b`'s payload was never dropped. Codegen-only: the interpreter loses
/// the `..` rest field's body outright here (B-2026-09-06-34), so this
/// string is the compiled backends' own, pinned so the value and the
/// once-each body count hold.
///
/// B-2026-09-06-40 UPDATED THE `two` CELL. This doc used to add "and drops
/// a reordered pattern's leaves in the other order (B-2026-09-06-40)" —
/// i.e. the string deliberately pinned the behaviour that row called a bug.
/// That row has since landed and the compiled backends drain a reordered
/// pattern in PATTERN order, so `two` reads `dR7 dR8 dR6` where it read
/// `dR8 dR7 dR6`. Nothing about THIS row's subject moved: the binding is
/// still by name, each body still runs once, and the value is unchanged.
/// ASAN twin:
/// `asan_partial_struct_let_pattern_with_drop_fields_clean`.
#[test]
fn e2e_partial_struct_let_pattern_with_drop_fields_binds_by_name() {
    let Some(out) = run_program(
        r#"struct R { id: i64, name: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, name: f"n{i}" }; }
struct S3 { a: R, b: R }
fn p_fresh(r: R) -> i64 { let s: S3 = S3 { a: mk(2), b: mk(3) }; let S3 { b, .. } = s; return b.id; }
fn p_swapped(r: R) -> i64 { let s: S3 = S3 { a: mk(7), b: mk(8) }; let S3 { b, a } = s; return b.id * 100 + a.id; }
fn p_rename(r: R) -> i64 { let s: S3 = S3 { a: mk(12), b: mk(13) }; let S3 { b: q, .. } = s; return q.id; }
fn main() {
    { let v: i64 = p_fresh(mk(1)); println(f"v={v}"); println("one") }
    { let v: i64 = p_swapped(mk(6)); println(f"v={v}"); println("two") }
    { let v: i64 = p_rename(mk(11)); println(f"v={v}"); println("three") }
    println("end")
}
"#,
    ) else {
        return;
    };
    assert_eq!(
        out,
        "dR2\ndR3\ndR1\nv=3\none\ndR7\ndR8\ndR6\nv=807\ntwo\ndR12\ndR13\ndR11\nv=13\nthree\nend\n"
    );
}

/// B-2026-09-20-44 — a GENERIC enum's payload runs its user `Drop` body
/// when the arm reads a PRIMITIVE FIELD off its binding.
///
/// `G.X(v) => { return v.id }` over `enum G[T] { X(T), Y }` at `T = R`
/// printed NEITHER body on `karac run`, -O0 and -O2 while `--interp`
/// printed it, and valgrind read `0 errors, all heap blocks freed` — a PURE
/// body loss with the heap correctly released, which is why no memory gate
/// and neither ASAN ratchet could see the class.
///
/// The gate is `arm_reads_only`, computed from the syntactic consumption
/// classifier, which calls EVERY projection off the binding a partial move.
/// So `v.id` read as a take, the bodies mask ran, and the body ran nowhere.
/// `binding_only_borrowed_with`'s own doc predicts exactly that — "over-
/// reporting a take makes the caller stand down for a taker that does not
/// exist and the body runs nowhere at all" — and the `copy_read` knob it
/// exists to offer is the repair. Only the BODIES gate takes it; the MEMORY
/// retraction keeps the uncorrected syntactic verdict, where over-reporting
/// a take leaves the existing owner alone and is the safe direction.
///
/// The rows in order, and which arm each one is. `ret` and `letret` are the
/// defect: both LOST their body before the fix, and `letret` is what says
/// the term is the RETURN rather than the shape of the expression. `wide`
/// is the same defect at the boxing threshold and is the third cell that
/// moved. Everything else is a guard that read identically on both arms:
/// `prnt` and `none` were already correct (merely USING the binding, or not
/// touching it, is not the term), `whole` returns the binding itself and
/// must still read as a take, `narrow` fits the erased slot so it never
/// boxed, the two `ng*` rows are the non-generic twins, `nest` reads
/// `v.a.id` through a non-primitive field and must still read as a take,
/// and `local` is a named-local scrutinee rather than a by-value param.
/// Nine guards against three moving cells is the ratio this area needs: an
/// over-broad repair shows up here as a DOUBLED body, which is the failure
/// mode the mask's own source comment records for widening it.
///
/// ORDERING NOTE, so a future ordering fix knows this pin has to move: a
/// BOXED payload's body prints BEFORE the caller's `println` of the call
/// result and an INLINE one's prints AFTER (`dRw7` / `wide=7` against
/// `narrow=6` / `dRn6`), and `--interp` defers both to the source binding's
/// death point. That divergence is the B-2026-09-15-17 family and is NOT
/// what this fixture is about; it is pinned here only because a whole-stdout
/// assertion gets the body COUNT for free, which is the property that
/// catches a doubling. The memory twin is in `tests/memory_sanitizer.rs`.
#[test]
fn e2e_generic_enum_arm_primitive_field_read_runs_payload_drop_body() {
    let src = r#"
struct R { id: i64, s: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
struct Rn { id: i64 }
impl Drop for Rn { fn drop(mut ref self) { println(f"dRn{self.id}") } }
struct Rw { id: i64, a: String, b: String }
impl Drop for Rw { fn drop(mut ref self) { println(f"dRw{self.id}") } }
struct Wd { a: R, b: String, c: String }
enum G[T] { X(T), Y }
enum Ng { X(R), Y }

fn ret(g: G[R]) -> i64 { match g { G.X(v) => { return v.id }, G.Y => { return 0 } } }
fn letret(g: G[R]) -> i64 { match g { G.X(v) => { let x: i64 = v.id; return x }, G.Y => { return 0 } } }
fn prnt(g: G[R]) -> i64 { match g { G.X(v) => { println(f"use{v.id}"); return 0 }, G.Y => { return 0 } } }
fn none(g: G[R]) -> i64 { match g { G.X(v) => { return 0 }, G.Y => { return 0 } } }
fn whole(g: G[R]) -> R { match g { G.X(v) => { return v }, G.Y => { return R { id: 0, s: f"b2044-zzzzzzzzzzzzzzzzzzzzzzzz" } } } }
fn narrow(g: G[Rn]) -> i64 { match g { G.X(v) => { return v.id }, G.Y => { return 0 } } }
fn wide(g: G[Rw]) -> i64 { match g { G.X(v) => { return v.id }, G.Y => { return 0 } } }
fn ngret(g: Ng) -> i64 { match g { Ng.X(v) => { return v.id }, Ng.Y => { return 0 } } }
fn ngprnt(g: Ng) -> i64 { match g { Ng.X(v) => { println(f"nguse{v.id}"); return 0 }, Ng.Y => { return 0 } } }
fn nest(g: G[Wd]) -> i64 { match g { G.X(v) => { return v.a.id }, G.Y => { return 0 } } }

fn main() {
    { let a1: R = R { id: 1, s: f"b2044-aaaaaaaaaaaaaaaaaaaaaaaa" }; let w1: G[R] = G.X(a1); println(f"ret={ret(w1)}") }
    { let a2: R = R { id: 2, s: f"b2044-aaaaaaaaaaaaaaaaaaaaaaaa" }; let w2: G[R] = G.X(a2); println(f"letret={letret(w2)}") }
    { let a3: R = R { id: 3, s: f"b2044-aaaaaaaaaaaaaaaaaaaaaaaa" }; let w3: G[R] = G.X(a3); println(f"prnt={prnt(w3)}") }
    { let a4: R = R { id: 4, s: f"b2044-aaaaaaaaaaaaaaaaaaaaaaaa" }; let w4: G[R] = G.X(a4); println(f"none={none(w4)}") }
    { let a5: R = R { id: 5, s: f"b2044-aaaaaaaaaaaaaaaaaaaaaaaa" }; let w5: G[R] = G.X(a5); let q: R = whole(w5); println(f"whole={q.id}") }
    { let a6: Rn = Rn { id: 6 }; let w6: G[Rn] = G.X(a6); println(f"narrow={narrow(w6)}") }
    { let a7: Rw = Rw { id: 7, a: f"b2044-aaaaaaaaaaaaaaaaaaaaaaaa", b: f"b2044-bbbbbbbbbbbbbbbbbbbbbbbb" }; let w7: G[Rw] = G.X(a7); println(f"wide={wide(w7)}") }
    { let a8: R = R { id: 8, s: f"b2044-aaaaaaaaaaaaaaaaaaaaaaaa" }; let w8: Ng = Ng.X(a8); println(f"ngret={ngret(w8)}") }
    { let a9: R = R { id: 9, s: f"b2044-aaaaaaaaaaaaaaaaaaaaaaaa" }; let w9: Ng = Ng.X(a9); println(f"ngprnt={ngprnt(w9)}") }
    { let aa: Wd = Wd { a: R { id: 10, s: f"b2044-aaaaaaaaaaaaaaaaaaaaaaaa" }, b: f"b2044-bbbbbbbbbbbbbbbbbbbbbbbb", c: f"b2044-cccccccccccccccccccccccc" }; let wa: G[Wd] = G.X(aa); println(f"nest={nest(wa)}") }
    { let ab: R = R { id: 11, s: f"b2044-aaaaaaaaaaaaaaaaaaaaaaaa" }; let wb: G[R] = G.X(ab); let n: i64 = match wb { G.X(v) => { v.id }, G.Y => { 0 } }; println(f"local={n}") }
    println("end");
}
"#;
    assert_eq!(
            run_program(src).as_deref(),
            Some("dR1\nret=1\ndR2\nletret=2\nuse3\ndR3\nprnt=0\ndR4\nnone=0\nwhole=5\ndR5\nnarrow=6\ndRn6\ndRw7\nwide=7\nngret=8\ndR8\nnguse9\nngprnt=0\ndR9\ndR10\nnest=10\ndR11\nlocal=11\nend\n"),
        );
}

/// B-2026-09-23-28 — a tuple pattern over a BORROWED tuple. `for (a, b) in edges`
/// with `edges: ref Vec[(i64, i64)]` binds each element as `ref (i64, i64)`,
/// and the typechecker refused the pattern outright ("tuple pattern used but
/// type is `ref (i64, i64)`"), though the same loop over an OWNED local Vec
/// was accepted. design.md's own `for (key, value) in map` was refused the same
/// way whenever the map arrived as a `ref Map` parameter. Once accepted, a
/// `ref String` field reached codegen with no surface name and `name.len()`
/// found no dispatcher, and `let (a, b) = ref v[i]` had no codegen lowering.
/// Scalar fields bind by value, aggregates as borrows; the container keeps
/// ownership, so nothing is freed through the pattern.
#[test]
fn e2e_tuple_pattern_destructures_through_a_borrow() {
    let src = r#"
struct Graph {
    edges: Vec[(i64, i64)],
    tags: Vec[(String, String)],
}
impl Graph {
    fn weight(ref self) -> i64 {
        let mut s = 0;
        for (a, b) in self.edges { s += a * b; }
        return s;
    }
    fn tag_len(ref self) -> i64 {
        let mut s = 0;
        for (k, v) in self.tags { s += k.len() * 10 + v.len(); }
        return s;
    }
}
fn total(edges: ref Vec[(i64, i64)]) -> i64 {
    let mut s = 0;
    for (a, b) in edges { s += a * 10 + b; }
    return s;
}
fn nested(v: ref Vec[(i64, (i64, i64))]) -> i64 {
    let mut s = 0;
    for (a, (b, c)) in v { s += a + b * c; }
    return s;
}
fn muts(v: mut ref Vec[(String, i64)]) -> i64 {
    let mut s = 0;
    for (name, k) in v { s += name.len() + k; }
    v.push(("q".to_string(), 1));
    return s;
}
fn copies(v: ref Vec[(String, i64)]) -> Vec[String] {
    let mut out: Vec[String] = Vec.new();
    for (name, _) in v { out.push(name.clone()); }
    return out;
}
fn enumerated(v: ref Vec[(i64, i64)]) -> i64 {
    let mut s = 0;
    for (i, (a, b)) in v.iter().enumerate() { s += i * (a + b); }
    return s;
}
fn map_total(m: ref Map[String, i64]) -> i64 {
    let mut s = 0;
    for (k, v) in m { s += k.len() + v; }
    return s;
}
fn pair(p: ref (String, i64)) -> i64 {
    let (name, k) = p;
    return name.len() + k;
}
fn by_index(ps: ref Vec[(String, i64)]) -> i64 {
    let mut n = 0;
    for i in 0..ps.len() {
        let (name, k) = ref ps[i];
        n += name.len() * 100 + k;
    }
    return n;
}
fn main() {
    let g = Graph { edges: vec![(1, 2), (3, 4)], tags: vec![("ab".to_string(), "c".to_string())] };
    println(f"{g.weight()} {g.tag_len()} {total(g.edges)}");
    println(f"{nested(vec![(1, (2, 3)), (4, (5, 6))])}");
    let mut ps = vec![("ab".to_string(), 5)];
    println(f"{muts(mut ps)} {ps.len()}");
    let c = copies(ps);
    println(f"{c.len()} {c[0]} {c[1]}");
    println(f"{enumerated(vec![(1, 2), (3, 4), (5, 6)])}");
    let mut m: Map[String, i64] = Map.new();
    m.insert("ab".to_string(), 3);
    m.insert("c".to_string(), 4);
    println(f"{map_total(m)} {m.len()}");
    let u = ("abc".to_string(), 2);
    println(f"{pair(u)} {u.0}");
    println(f"{by_index(ps)} {ps[1].0}");
}
"#;
    assert_eq!(
        run_program(src).as_deref(),
        Some("14 21 46\n41\n7 2\n2 ab q\n29\n10 2\n5 abc\n306 q\n"),
    );
}

/// B-2026-09-23-29 — a DESTRUCTURING closure param on a fused iterator
/// terminal, and `enumerate()` as the chain's source. The fused-chain peel
/// accepted only a plain `|x|`, so `label.iter().enumerate().filter(|(i, l)| i
/// == l).count()` — and every other terminal (`sum`, `any`, `all`, `position`,
/// `for_each`, `partition`) behind a `|(a, b)|` or an `enumerate()` — failed
/// with "no handler for method '<terminal>'", while `fold` and `collect` over
/// the same closure lowered.
#[test]
fn e2e_iterator_terminals_take_destructuring_params_and_enumerate() {
    let src = r#"
struct P { x: i64, y: i64 }
fn fixed(label: ref Vec[i64]) -> i64 {
    return label.iter().enumerate().filter(|(i, l)| i == l).count();
}
fn main() {
    let label = vec![0, 0, 2, 1, 4];
    let v = vec![(1, 2), (5, 3), (4, 6)];
    let ps = vec![P { x: 1, y: 2 }, P { x: 3, y: 4 }];
    println(f"{fixed(label)} {label.iter().enumerate().count()}");
    println(f"{label.iter().enumerate().map(|(i, l)| i * l).sum()}");
    println(f"{label.iter().enumerate().filter(|(i, l)| i == l).map(|(i, l)| i + l).sum()}");
    println(f"{label.iter().enumerate().any(|(i, l)| i > 3 and i == l)} {label.iter().enumerate().all(|(_, l)| l >= 0)}");
    println(f"{v.iter().map(|(a, b)| a * b).sum()} {v.iter().filter(|(a, b)| a < b).count()}");
    println(f"{ps.iter().map(|P { x, y }| x * y).sum()} {v.iter().position(|(a, b)| a > b)}");
    println(f"{v.iter().any(|(a, b)| a > b)} {v.iter().find_map(|(a, b)| if a > b { Some(a - b) } else { None })}");
    v.iter().for_each(|(a, b)| println(f"{a}-{b}"));
    let (x, y) = v.iter().partition(|(a, b)| a < b);
    println(f"{x.len()} {y.len()}");
    for (i, l) in label.iter().enumerate().filter(|(i, l)| i != l) {
        println(f"{i}:{l}");
    }
}
"#;
    assert_eq!(
        run_program(src).as_deref(),
        Some("3 5\n23\n12\ntrue true\n41 2\n14 Some(1)\ntrue Some(2)\n1-2\n5-3\n4-6\n2 1\n1:0\n3:1\n"),
    );
}

/// B-2026-09-23-30 — a call DECLARED `-> Slice[T]` as a `for` source (`for w in
/// g.part(1, 3)`), its `.iter()` form, and its `.iter().enumerate()` form, plus
/// the same enumerate over an inline range slice. All four reached the
/// "for-loop over this iterable is not lowered" error, whose advice — bind the
/// view to a local first — was the only spelling that built.
#[test]
fn e2e_for_over_a_call_that_returns_a_slice() {
    let src = r#"
struct G { nbr: Vec[i64], names: Vec[String] }
impl G {
    fn part(ref self, a: i64, b: i64) -> Slice[i64] { return self.nbr[a..b]; }
    fn some_names(ref self) -> Slice[String] { return self.names[1..]; }
}
fn head(v: ref Vec[i64], k: i64) -> Slice[i64] { return v[0..k]; }
fn main() {
    let g = G { nbr: vec![5, 6, 7, 8], names: vec!["a".to_string(), "bb".to_string(), "ccc".to_string()] };
    let v = vec![3, 4, 5, 6];
    let mut s = 0;
    for w in g.part(1, 3) { s += w; }
    for w in head(g.nbr, 2) { s += w * 100; }
    for n in g.some_names() { s += n.len() * 1000; }
    for (i, w) in g.part(0, 4).iter().enumerate() { s += i * w * 10000; }
    println(f"{s}");
    let mut t = 0;
    for (i, x) in v[1..3].iter().enumerate() { t += i * x; }
    for w in g.part(1, 3).iter() { t += w * 10; }
    for (i, n) in g.some_names().iter().enumerate() { t += i * n.len() * 1000; }
    println(f"{t} {g.names[2]}");
}
"#;
    assert_eq!(run_program(src).as_deref(), Some("446113\n3135 ccc\n"));
}
