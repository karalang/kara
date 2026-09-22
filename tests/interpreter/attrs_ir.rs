//! inline/section/linkage attributes and emitted-IR shape -- fixtures for `tests/interpreter.rs`.
//!
//! Split out of `tests/interpreter.rs` on 2026-09-21. The TEST TARGET is
//! unchanged: this file is a module of that target, so
//! `cargo test --features llvm --test interpreter` still runs everything
//! and CI needs no edit. Run this area alone with:
//!
//!     cargo test --features llvm --test interpreter attrs_ir::
//!
//! New fixtures about inline/section/linkage attributes and emitted-IR shape belong in this file.

use super::*;

// ── std.runtime introspection (Debugger Contract slice 5) ─────────────────────
//
// The tree-walk interpreter has its own par-block evaluation path and does
// NOT construct `KaracFrame` / `ACTIVE_FRAMES` state, so all three APIs
// return the empty / false form per design.md's "try-then-degrade" contract.
// Real values flow through codegen in compiled binaries; the interpreter
// returns degraded results that are still well-typed (empty Vec / false
// bool). When/if interpreter parity for active-frame enumeration ships
// (post-v1), these tests upgrade to assert real values.

#[test]
fn test_runtime_has_debug_metadata_returns_false_in_interpreter() {
    let out = run_no_errors(
        "fn main() {
             let dbg = Runtime.has_debug_metadata();
             if dbg {
                 println(1);
             } else {
                 println(0);
             }
         }",
    );
    assert_eq!(out, "0\n");
}

#[test]
fn test_interner_distinct_strings_distinct_symbols() {
    // Distinct strings get distinct handles; `len` counts distinct keys.
    let output = run(r#"fn main() {
             let mut tab: Interner = Interner.new();
             let a = tab.intern("foo");
             let b = tab.intern("bar");
             let c = tab.intern("foo");
             println(a == b);
             println(a == c);
             println(tab.len());
         }"#);
    assert_eq!(output, "false\ntrue\n2\n");
}

#[test]
fn test_interner_symbol_as_map_key() {
    // The headline use case: `Symbol` is a cheap integer-keyed map key.
    // A handle re-interned from an equal string hits the same entry.
    let output = run(r#"fn main() {
             let mut tab: Interner = Interner.new();
             let a = tab.intern("k");
             let b = tab.intern("k");
             let mut counts: Map[Symbol, i64] = Map.new();
             counts.insert(a, 42);
             println(counts.get(b).unwrap_or(0));
         }"#);
    assert_eq!(output, "42\n");
}

#[test]
fn test_interner_symbol_is_copy_pass_by_value_then_reuse() {
    // Regression for the baked-distinct-type Copy registration: `Symbol`
    // derives `Copy`, so passing one by value to `resolve` does NOT move
    // it — the same handle stays usable afterward. Before the
    // `register_baked_stdlib` fix this failed ownership-check with
    // "value moved here, used again".
    let output = run(r#"fn main() {
             let mut tab: Interner = Interner.new();
             let a = tab.intern("x");
             let _s = tab.resolve(a);
             let mut m: Map[Symbol, i64] = Map.new();
             m.insert(a, 7);
             println(m.get(a).unwrap_or(0));
         }"#);
    assert_eq!(output, "7\n");
}

#[test]
fn test_inverse_hyperbolics_agree_with_the_libm_symbols_codegen_calls() {
    // B-2026-08-29-60. Rust's std does NOT implement `asinh` / `acosh` /
    // `atanh` in terms of libm the way it does `cosh` / `log10` / `atan`; it
    // evaluates them as formulas. Codegen lowers them to `asinh` / `asinhf`,
    // so delegating to `f64::asinh` here was a different ALGORITHM, not a
    // different rounding — the gap B-2026-08-29-41 could not close by picking
    // a width, because the width was already right.
    //
    // Unlike that row's last-ULP cases this was never f32-only: measured over
    // a 120-point series, Rust's formula differed from libm on 14/17/3 of 120
    // f64 inputs and 10/12/12 of 120 f32 inputs. The parent row recorded f64
    // as clean, which was luck of the two receivers it sampled — the same
    // trap it warned about, landing on its own claim.
    //
    // The expectation is COMPUTED from libm rather than hardcoded, so this
    // pins the real invariant ("the interpreter agrees with the symbol
    // codegen calls") against whatever libm the host has, instead of freezing
    // one platform's bits into the suite. Every input below is one where the
    // Rust formula and libm disagree at BOTH widths, so all 16 lines fail
    // without the fix.
    extern "C" {
        fn asinh(x: f64) -> f64;
        fn asinhf(x: f32) -> f32;
        fn acosh(x: f64) -> f64;
        fn acoshf(x: f32) -> f32;
        fn atanh(x: f64) -> f64;
        fn atanhf(x: f32) -> f32;
    }
    let cases: &[(&str, f64)] = &[
        ("asinh", 8.8),
        ("asinh", 9.9),
        ("asinh", 17.1),
        ("acosh", 1.1),
        ("acosh", 1.6),
        ("acosh", 3.7),
        ("atanh", 0.03),
        ("atanh", 0.04),
    ];
    let mut src = String::from("fn main() {\n");
    let mut want = String::new();
    for (i, (m, v)) in cases.iter().enumerate() {
        src.push_str(&format!("    let d{i}: f64 = {v};\n"));
        src.push_str(&format!("    println(d{i}.{m}());\n"));
        src.push_str(&format!("    let s{i}: f32 = {v}f32;\n"));
        src.push_str(&format!("    println(s{i}.{m}());\n"));
        let (wide, narrow) = unsafe {
            match *m {
                "asinh" => (asinh(*v), asinhf(*v as f32) as f64),
                "acosh" => (acosh(*v), acoshf(*v as f32) as f64),
                _ => (atanh(*v), atanhf(*v as f32) as f64),
            }
        };
        want.push_str(&format!("{wide}\n{narrow}\n"));
    }
    src.push_str("}\n");
    assert_eq!(run(&src), want);
}

#[test]
fn test_cbrt_agrees_with_the_libm_symbol_codegen_calls() {
    // B-2026-08-30-4, interpreter half of the oracle pair. `cbrt` was kept
    // OUT of the float-math table because Rust's `f64::cbrt` disagrees with
    // the symbol codegen emits; the shim block in `src/float_math.rs` is what
    // let it in, exactly as it did for the inverse hyperbolics above.
    //
    // `cbrt` is not simply a fourth member of that set, though, and the
    // difference is what this pair exists to hold down. Its two
    // implementations are BOTH reachable from a Rust program:
    // `compiler_builtins` ships a weak `cbrt`/`cbrtf` that shadows the
    // platform libm's wherever a Rust object is in the link, so which one a
    // lane gets depends on how that lane was linked rather than on what it
    // asked for. The interpreter and an AOT binary both land on
    // compiler_builtins'; the JIT resolves through `dlsym`, which cannot see
    // a local archive symbol, and used to land on the platform's — a
    // `run == build` split visible on one lane only. See
    // `tests/lljit_e2e.rs::jit_e2e_cbrt_resolves_the_implementation_the_other_lanes_link`
    // for that half; here the assertion is the ordinary one, that the
    // interpreter calls the symbol rather than reimplementing it.
    //
    // Expectation COMPUTED from the linked symbol, not hardcoded, for the
    // reason the test above states. Inputs after the first differ between the
    // two implementations at BOTH widths (121 of 2000 sampled k/10 values do),
    // so this fixture is also the one that would catch the interpreter being
    // moved onto the other implementation. Character-identical to
    // `tests/codegen.rs::test_e2e_cbrt_agrees_with_the_interpreter`.
    extern "C" {
        fn cbrt(x: f64) -> f64;
        fn cbrtf(x: f32) -> f32;
    }
    let cases: &[f64] = &[27.0, 0.2, 1.6, 4.1, 4.7, 5.3, 7.3, 7.9, -10.6];
    let mut src = String::from("fn main() {\n");
    let mut want = String::new();
    for (i, v) in cases.iter().enumerate() {
        src.push_str(&format!("    let d{i}: f64 = {v:?};\n"));
        src.push_str(&format!("    println(d{i}.cbrt());\n"));
        src.push_str(&format!("    let s{i}: f32 = {v:?}f32;\n"));
        src.push_str(&format!("    println(s{i}.cbrt());\n"));
        let (wide, narrow) = unsafe { (cbrt(*v), cbrtf(*v as f32) as f64) };
        want.push_str(&format!("{wide}\n{narrow}\n"));
    }
    src.push_str("}\n");
    assert_eq!(run(&src), want);
}

/// Interpreter mirror of `codegen.rs`'s
/// `e2e_option_local_returned_hands_its_inline_payload_to_the_caller`.
///
/// The interpreter was never wrong here — B-2026-08-29-56 is a compiled-only
/// double free — so these rows are PARITY PINS, not a reproduction: they assert
/// the strings the compiled backends must now produce, so a future interpreter
/// change cannot drift away from the backend the fix just corrected.
#[test]
fn option_local_returned_hands_its_inline_payload_to_the_caller() {
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
        (
            "tail-local",
            "match c_tail() { Some(s) => println(f\"[{s}]\"), None => println(\"none\"), }\n",
            "[zz]\npost\n",
        ),
        (
            "stmt-return",
            "match c_stmt() { Some(s) => println(f\"[{s}]\"), None => println(\"none\"), }\n",
            "[zz]\npost\n",
        ),
        (
            "bare-tail-control",
            "match c_bare() { Some(s) => println(f\"[{s}]\"), None => println(\"none\"), }\n",
            "[zz]\npost\n",
        ),
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
        ("caller-discards", "c_tail();\n", "post\n"),
        (
            "caller-holds",
            "let r = c_tail(); println(\"held\");\n",
            "held\npost\n",
        ),
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
        assert_eq!(run(&src), want, "{label}");
    }
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
///
/// Interpreter twin of `e2e_inline_struct_result_transfer_disarms_its_source` (tests/codegen.rs) — same program string, same
/// pin.
#[test]
fn test_inline_struct_result_transfer_disarms_its_source() {
    let out = run(r#"struct R { id: i64, tag: String }
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
"#);
    assert_eq!(
        out,
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
    );
}
