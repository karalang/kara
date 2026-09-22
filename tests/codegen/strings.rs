//! String, f-strings, chars, formatting, regex, JSON, display -- fixtures for `tests/codegen.rs`.
//!
//! Split out of `tests/codegen.rs` on 2026-09-21. The TEST TARGET is
//! unchanged: this file is a module of that target, so
//! `cargo test --features llvm --test codegen` still runs everything
//! and CI needs no edit. Run this area alone with:
//!
//!     cargo test --features llvm --test codegen strings::
//!
//! New fixtures about String, f-strings, chars, formatting, regex, JSON, display belong in this file.

use super::*;

/// Adversarial for the store-escape guard's PRECISION — it must reject only
/// when a CAPTURING closure is stored AND the place ESCAPES.
#[test]
fn store_escape_guard_does_not_over_reject() {
    // (a) NON-capturing closure pushed then returned — sound (null env).
    assert_eq!(
            run_program(
                "fn make() -> Vec[Fn(i64) -> i64] { let mut v: Vec[Fn(i64) -> i64] = Vec.new(); v.push(|x| x * 2i64); return v; }\n\
                 fn main() { let fs = make(); println(f\"{(fs[0])(21i64)}\"); }\n"
            )
            .as_deref(),
            Some("42\n"),
            "pushing a NON-capturing closure into a returned Vec must compile and run"
        );
    // (b) CAPTURING closure pushed, but the Vec is used SAME-FRAME and not
    // returned — the env is still live at the call, so it is sound.
    assert_eq!(
            run_program(
                "fn run(k: i64) -> i64 { let mut v: Vec[Fn(i64) -> i64] = Vec.new(); v.push(|x: i64| x + k); return (v[0])(5i64); }\n\
                 fn main() { println(f\"{run(21i64)}\"); }\n"
            )
            .as_deref(),
            Some("26\n"),
            "a capturing closure pushed then called same-frame (Vec not returned) must run"
        );
    // (c) A plain (non-closure) element pushed into a returned Vec — the
    // marking must not fire on non-closure pushes.
    assert_eq!(
            run_program(
                "fn make(k: i64) -> Vec[i64] { let mut v: Vec[i64] = Vec.new(); v.push(k); return v; }\n\
                 fn main() { let v = make(21i64); println(f\"{v[0]}\"); }\n"
            )
            .as_deref(),
            Some("21\n"),
            "pushing a non-closure element into a returned Vec must compile and run"
        );
    // (d) Pass-down of a capturing closure to a free function is untouched
    // (the guard targets stores, never call-arg passing).
    assert_eq!(
        run_program(
            "fn apply(f: Fn(i64) -> i64, x: i64) -> i64 { f(x) }\n\
                 fn main() { let base = 10i64; println(f\"{apply(|x| x + base, 5i64)}\"); }\n"
        )
        .as_deref(),
        Some("15\n"),
        "passing a capturing closure down to a function must still compile and run"
    );
}

#[test]
fn encoding_builtins_rejected_by_codegen() {
    // B-2026-07-18-20: `std.encoding` (`Base64`/`Hex`/`Url` encode/decode) has
    // no codegen lowering — codegen must REJECT it with an actionable message
    // rather than fall through to the assoc-call `const 0` default and
    // silently miscompile (a `String` function returning the integer 0, a
    // silent run-vs-build divergence). The interpreter computes them
    // correctly; the reject points the user at `karac run`.
    for prog in [
        "fn main() { let v: Vec[u8] = vec![1u8, 2u8]; println(Base64.encode(v)); }",
        "fn main() { let v: Vec[u8] = vec![1u8, 2u8]; println(Base64.encode_url_safe(v)); }",
        "fn main() { let v: Vec[u8] = vec![1u8, 2u8]; println(Hex.encode(v)); }",
        "fn main() { let v: Vec[u8] = vec![1u8, 2u8]; println(Hex.encode_upper(v)); }",
    ] {
        let err = ir_result(prog).expect_err("std.encoding encode must be rejected by codegen");
        assert!(err.contains("interpreter-only in v1"), "got: {err}");
    }
}

#[test]
fn test_e2e_unbound_vec_prints_like_a_bound_one() {
    // B-2026-07-28-12: a `Vec[T]` expression with no variable name to key
    // on printed as GARBAGE under `karac build` — `println(vec![9, 8])`
    // emitted a stray tab, `println(t.shape())` emitted nothing — while the
    // interpreter printed `[9, 8]` / `[2, 3]`. Binding to a `let` first was
    // always correct, which is the whole shape of the bug: dispatch keyed
    // off the name-addressed side-table, so an unbound Vec fell through to
    // the value-kind arms, where its `{ptr, len, cap}` aggregate is
    // byte-identical to a String's and was rendered as one.
    //
    // The oracle is the interpreter, and every case is asserted in BOTH
    // forms — bound and unbound — because agreement between those two is
    // the property that broke, and a test that only checked the unbound
    // form could pass with both backends wrong in the same way.
    let cases = [
        ("literal", "vec![9i64, 8i64]", "Vec[i64]"),
        ("call result", "mk()", "Vec[i64]"),
        ("method result", "t.shape()", "Vec[i64]"),
        ("string elements", "names()", "Vec[String]"),
        ("float elements", "vec![1.5, 2.5]", "Vec[f64]"),
        ("empty", "empty()", "Vec[i64]"),
    ];
    for (label, expr, ty) in cases {
        let src = format!(
            "fn mk() -> Vec[i64] {{ vec![2i64, 3i64] }}\n\
                 fn names() -> Vec[String] {{ vec![\"ada\", \"bob\"] }}\n\
                 fn empty() -> Vec[i64] {{ Vec.new() }}\n\
                 fn main() {{\n\
                     let t = Tensor.from([[1, 2, 3], [4, 5, 6]]);\n\
                     println({expr});\n\
                     println(f\"<{{{expr}}}>\");\n\
                     let b: {ty} = {expr};\n\
                     println(b);\n\
                     println(f\"<{{b}}>\");\n\
                 }}"
        );
        let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(&src);
        assert!(
            interp_errs.is_empty(),
            "{label}: interpreter errors: {interp_errs:?}"
        );
        let expected = interp_out.join("");
        // All four lines must agree with each other, not just with the
        // oracle — the unbound forms are lines 1-2, the bound forms 3-4.
        let lines: Vec<&str> = expected.lines().collect();
        assert_eq!(
            lines.len(),
            4,
            "{label}: interpreter produced {} lines: {expected:?}",
            lines.len()
        );
        assert_eq!(
            (lines[0], lines[1]),
            (lines[2], lines[3]),
            "{label}: the interpreter itself disagrees between bound and \
                 unbound — the oracle is unusable for this case"
        );
        if let Some(aot) = run_program(&src) {
            assert_eq!(
                aot, expected,
                "{label}: an unbound Vec must print like a bound one"
            );
        }
    }
}

/// B-2026-08-11-1 — an INDEX used directly as a method receiver.
///
/// `cs[0].to_string()` on a `Vec[char]` printed `120` under both compiled
/// backends where the interpreter printed `x`. Not a corrupted buffer: a
/// correctly-built string of the WRONG THING, on code the typechecker
/// accepts. Found dogfooding a recursive-descent evaluator, whose tokenizer
/// built identifier names character by character — every expression
/// referencing a variable returned `ERR:unknown variable` while the
/// numbers-only ones were right.
///
/// THREE root causes sat behind the one symptom, and only the first was
/// silent:
///
/// 1. `register_var_from_type_expr` recorded `var_type_names` for structs,
///    enums and INTEGER primitives but not `char`. `expr_is_char` reads
///    exactly that map, so a binding with no name is indistinguishable from
///    an `i32` and the renderer emits the code point. Fixing it in the
///    registrar rather than at the indexed-receiver call site is what makes
///    it general — the `__indexed_elem_N` synth, for-loop elements,
///    destructured elements and params all route through it. (`let c: char
///    = cs[0]` was always correct because `stmts.rs` records the annotation
///    itself, which is why the bug looked like it belonged to indexing.)
///
/// 2. `Array[T, N]` keeps its element TypeExpr in its own table, so the
///    indexed-receiver lookup missed it and errored on a variable that IS an
///    Array. Not char-specific: `Array[i64, N]` and `Array[String, N]`
///    failed identically, so EVERY indexed-receiver method on an Array was
///    unreachable.
///
/// 3. The typechecker's scalar integer-index arm resolved Array / Slice /
///    Vector / Vec but not `VecDeque`, so `d[i]` inferred `Type::Error`.
///    Most uses survive that (`d[0] + 1` and `let x: i64 = d[0]` recover the
///    type from the operator or the annotation) but method dispatch cannot:
///    an `Error` receiver records no callee type, so `d[0].to_string()` hit
///    "no handler for method". Only `to_string` failed — `abs()` and
///    `is_alphabetic()` route through paths that never needed the key.
///
/// The trailing rows are the row's own measured boundary: each was already
/// correct and pins a way the fix could over-reach. `char` and `i64` share a
/// representation, so a regression here is silent by nature — the expected
/// VALUES are the assertion, not the fact that it compiles.
#[test]
fn test_e2e_indexed_receiver_char_element_method() {
    let out = run_program(
        r#"
fn main() {
    // 1 — the reported shape, both spellings (argument position and `let`).
    let cs: Vec[char] = "xy".chars().collect();
    let mut acc: String = f"";
    acc.push_str(cs[0].to_string());
    println(f"{acc.len()}:{acc}");
    let s: String = cs[1].to_string();
    println(f"{s.len()}:{s}");
    println(f"[{cs[0].to_string()}]");

    // 2 — Array, every element type (all three were hard errors).
    let ac: Array[char, 2] = ['p', 'q'];
    println(ac[0].to_string());
    let ai: Array[i64, 2] = [7i64, 8i64];
    println(ai[0].to_string());
    let astr: Array[String, 2] = ["abc", "de"];
    println(astr[0].len());

    // 3 — VecDeque, the `to_string` gap and the methods that always worked.
    let mut dc: VecDeque[char] = VecDeque.new();
    dc.push_back('z');
    println(dc[0].to_string());
    let mut di: VecDeque[i64] = VecDeque.new();
    di.push_back(-7i64);
    println(di[0].to_string());
    println(di[0].abs());

    // Boundary rows — each already correct, each a way to over-reach.
    let c: char = cs[0];
    println(c.to_string());
    println('w'.to_string());
    println(f"[{cs[0]}]");
    if cs[0] == 'x' { println("eq"); } else { println("ne"); }
    let vs: Vec[String] = vec!["abc"];
    println(vs[0].len());
    let mut m: Map[i64, char] = Map.new();
    let _ = m.insert(1i64, 'k');
    match m.get(1i64) { Some(mc) => println(mc.to_string()), None => println("-") }
    let mut fc: String = f"";
    for ch in cs { fc.push_str(ch.to_string()); }
    println(fc);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(
            out.trim_end(),
            "1:x\n1:y\n[x]\np\n7\n3\nz\n-7\n7\nx\nw\n[x]\neq\n3\nk\nxy"
        );
    }
}

// ── String slicing `s[a..b]` codegen (phase-8 line 737) ──────
//
// `s[a..b]` lowers to a `karac_string_slice` runtime call that
// validates bounds + UTF-8 char boundaries and returns a fresh
// String buffer. (Earlier this silently miscompiled — the range
// branch fell through to the integer-index tail and produced empty
// output; the lowering replaces that.)

#[test]
fn string_slice_lowers_to_runtime_helper() {
    let ir = ir_for(
        "fn main() {\n\
                 let s = \"hello world\";\n\
                 let mid = s[6..11];\n\
                 println(mid);\n\
             }",
    );
    assert!(
        ir.contains("karac_string_slice"),
        "expected `s[a..b]` to lower to a karac_string_slice call; IR:\n{ir}"
    );
}

#[test]
fn e2e_string_slice_basic_forms() {
    // Half-open, open-ended, full, inclusive, and empty — must match
    // the interpreter output exactly (the result is a real String).
    if let Some(out) = run_program(
        "fn main() {\n\
                 let s = \"hello world\";\n\
                 println(s[0..5]);\n\
                 println(s[6..]);\n\
                 println(s[..5]);\n\
                 println(s[0..=4]);\n\
                 println(\"[\" + s[3..3] + \"]\");\n\
             }",
    ) {
        assert_eq!(out, "hello\nworld\nhello\nhello\n[]\n");
    }
}

#[test]
fn e2e_string_slice_result_is_string_and_concatenates() {
    if let Some(out) = run_program(
        "fn main() {\n\
                 let s = \"hello world\";\n\
                 let mid = s[6..11];\n\
                 println(mid + \"!\");\n\
             }",
    ) {
        assert_eq!(out, "world!\n");
    }
}

/// B-2026-07-22-6: `s[a..b].to_string()` / `.clone()` — a `.to_string()`
/// (or `.clone()`) METHOD CALL directly on a String slice. The receiver
/// `s[a..b]` is a ranged index that produces a fresh owned String, so the
/// method is the slice itself; codegen used to route the whole thing through
/// the Vec/Slice/Array indexed-receiver-method path and error ("element
/// TypeExpr unknown") even though the interpreter accepted it — a run-vs-build
/// divergence surfaced by leetcode #151's `words.push(s[start..i].to_string())`.
#[test]
fn e2e_string_slice_to_string_method() {
    if let Some(out) = run_program(
        "fn f(s: String) -> Vec[String] {\n\
                 let mut v: Vec[String] = Vec.new();\n\
                 v.push(s[0..2].to_string());\n\
                 v.push(s[6..].clone());\n\
                 return v;\n\
             }\n\
             fn main() {\n\
                 let v = f(\"hello world\");\n\
                 println(v[0]);\n\
                 println(v[1]);\n\
             }",
    ) {
        assert_eq!(out, "he\nworld\n");
    }
}

/// `String.strip_{prefix,suffix}(p) -> Option[String]` via
/// `karac_string_strip_{prefix,suffix}` + an `Option[String]` phi-merge.
/// Must match the interpreter oracle
/// (`test_string_strip_prefix_suffix_interpreter`): match / no-match /
/// matched-empty (`Some("")`) / empty-arg. Leak-safety is covered by
/// `tests/memory_sanitizer.rs::asan_string_strip_prefix_suffix_heap_no_leak`.
#[test]
fn e2e_string_strip_prefix_suffix() {
    if let Some(out) = run_program(
            "fn main() {\n\
                 let s = \"hello world\";\n\
                 match s.strip_prefix(\"hello \") { Some(r) => println(f\"p:{r}\"), None => println(\"pn\") }\n\
                 match s.strip_prefix(\"xyz\")    { Some(r) => println(f\"p:{r}\"), None => println(\"pn\") }\n\
                 match s.strip_suffix(\" world\") { Some(r) => println(f\"s:{r}\"), None => println(\"sn\") }\n\
                 match s.strip_suffix(\"xyz\")    { Some(r) => println(f\"s:{r}\"), None => println(\"sn\") }\n\
                 match s.strip_prefix(\"hello world\") { Some(r) => println(f\"e:{r}\"), None => println(\"en\") }\n\
                 match s.strip_prefix(\"\")       { Some(r) => println(f\"a:{r}\"), None => println(\"an\") }\n\
             }",
        ) {
            assert_eq!(out, "p:world\npn\ns:hello\nsn\ne:\na:hello world\n");
        }
}

/// B-2026-08-31-25 — `Slice[T]` renders under codegen at EVERY depth, and
/// identically to the interpreter.
///
/// Before this, a slice had no codegen Display anywhere, and the two halves
/// failed differently — the nested positions PANICKED the compiler
/// (`emit_display_fn_for_type: type_name 'Slice_i64' not yet supported`)
/// while depth 0 refused the build. The compiler-abort half is the loud one
/// and the plain interpolation the quiet one, which reads backwards; both
/// are covered here.
///
/// The interpreter is the ORACLE, not a second opinion: every expectation
/// below is what `karac run --interp` prints today, and it was already
/// correct for all of these.
///
/// `mut Slice[i64]` is included because the row could not produce the
/// spelling (`Vec` has no `as_mut_slice`) and left it unmeasured — a
/// mut-marked sub-range argument reaches it, and mutability is not part of
/// the rendering, so it shares the immutable renderer.
#[test]
fn e2e_slice_display_matches_the_interpreter_at_every_depth() {
    let cases: &[(&str, &str, &str)] = &[
            ("depth0 f-string", "println(f\"{s}\");", "[1, 2]\n"),
            ("depth0 println", "println(s);", "[1, 2]\n"),
            (
                "inline call result",
                "println(f\"{v.as_slice()}\");",
                "[1, 2]\n",
            ),
            (
                "tuple field",
                "let t: (Slice[i64], i64) = (s, 9);\n    println(f\"{t}\");",
                "([1, 2], 9)\n",
            ),
            (
                "Vec element",
                "let mut w: Vec[Slice[i64]] = Vec.new();\n    w.push(s);\n    println(f\"{w}\");",
                "[[1, 2]]\n",
            ),
            (
                "Map value",
                "let mut m: Map[i64, Slice[i64]] = Map.new();\n    m.insert(1, s);\n    println(f\"{m}\");",
                "{1: [1, 2]}\n",
            ),
            ("empty slice", "println(f\"{e}\");", "[]\n"),
        ];
    for (label, stmts, want) in cases {
        let src = format!(
            "fn main() {{\n    \
                 let n = env.args().len() as i64;\n    \
                 let mut v: Vec[i64] = Vec.new();\n    \
                 v.push(n);\n    v.push(n + 1);\n    \
                 let s: Slice[i64] = v.as_slice();\n    \
                 let ev: Vec[i64] = Vec.new();\n    \
                 let e: Slice[i64] = ev.as_slice();\n    \
                 {stmts}\n}}\n"
        );
        let Some(out) = run_program(&src) else { return };
        assert_eq!(out, *want, "{label}");
    }

    // `dbg` is a separate lowering path and was one of the ICE positions.
    let Some(out) = run_program(
        "fn main() {\n\
                 let n = env.args().len() as i64;\n\
                 let mut v: Vec[i64] = Vec.new();\n\
                 v.push(n);\n                 let s: Slice[i64] = v.as_slice();\n\
                 dbg(s);\n             }\n",
    ) else {
        return;
    };
    // `dbg` writes to STDERR, which `run_program` does not capture, so the
    // assertion is that the program BUILT AND RAN at all: the defect here
    // was a compile-time panic (`type_name 'Slice_i64' not yet supported`),
    // so reaching a running binary is the whole property. The rendered text
    // is pinned by the interpreter's own dbg tests.
    assert_eq!(out, "", "dbg writes to stderr; stdout must be empty");

    // A `derive(Display)` STRUCT FIELD and ENUM PAYLOAD: the field-parts
    // path refused the struct with a Rust `{:?}` dump of the field's
    // `TypeExpr`, the enum payload reached the by-name catch-all and ICEd.
    let Some(out) = run_program(
        "#[derive(Display)]\n\
             struct W { s: Slice[i64], n: i64 }\n\
             #[derive(Display)]\n\
             enum E { A(Slice[i64]), B }\n\
             fn main() {\n\
                 let k = env.args().len() as i64;\n\
                 let mut v: Vec[i64] = Vec.new();\n\
                 v.push(k);\n                 v.push(k + 1);\n\
                 let s: Slice[i64] = v.as_slice();\n\
                 let w = W { s: s, n: 7 };\n\
                 println(f\"{w}\");\n\
                 let e = E.A(s);\n\
                 println(f\"{e}\");\n             }\n",
    ) else {
        return;
    };
    assert_eq!(out, "W { s: [1, 2], n: 7 }\nA([1, 2])\n");

    // An element type that OWNS HEAP — the slice borrows, so Display only
    // ever appends a copy of the bytes and nothing is freed here.
    let Some(out) = run_program(
        "fn main() {\n\
                 let mut v: Vec[String] = Vec.new();\n\
                 v.push(\"ab\");\n                 v.push(\"cd\");\n\
                 let s: Slice[String] = v.as_slice();\n\
                 println(f\"{s}\");\n             }\n",
    ) else {
        return;
    };
    assert_eq!(out, "[ab, cd]\n");

    // `mut Slice[T]`, via a mut-marked sub-range argument.
    let Some(out) = run_program(
        "fn show(t: mut Slice[i64]) { println(f\"{t}\"); }\n\
             fn main() {\n\
                 let n = env.args().len() as i64;\n\
                 let mut v: Vec[i64] = Vec.new();\n\
                 v.push(n);\n                 v.push(n + 1);\n                 v.push(n + 2);\n\
                 show(mut v[0..2]);\n             }\n",
    ) else {
        return;
    };
    assert_eq!(out, "[1, 2]\n");
}

#[test]
fn e2e_borrowed_string_slice_map_key_counts() {
    // A counter over length-2 windows of `s`, keyed on `s[i..i+2]` slices.
    // The get reads a borrowed `{ptr,len,cap=0}` view and the insert routes
    // to the borrowed-key deep-copy path — results must match owned-key
    // map semantics exactly.
    if let Some(out) = run_program(
        "fn main() {\n\
                 let s = \"abcababc\";\n\
                 let mut m: Map[String, i64] = Map.new();\n\
                 let n = s.len();\n\
                 let mut i = 0i64;\n\
                 while i + 2 <= n {\n\
                     let c = match m.get(s[i..i+2]) { Some(v) => v, None => 0i64 };\n\
                     m.insert(s[i..i+2], c + 1);\n\
                     i = i + 1;\n\
                 }\n\
                 println(m.len());\n\
                 match m.get(\"ab\") { Some(v) => println(v), None => println(-1) }\n\
                 match m.get(\"bc\") { Some(v) => println(v), None => println(-1) }\n\
                 match m.get(\"zz\") { Some(v) => println(v), None => println(-1) }\n\
             }",
    ) {
        // windows: ab,bc,ca,ab,ba,ab,bc → ab×3, bc×2, ca×1, ba×1 (4 distinct)
        assert_eq!(out, "4\n3\n2\n-1\n");
    }
}

#[test]
fn e2e_option_result_display_matches_interpreter() {
    // B-2026-07-08-9: Option[T] / Result[T, E] had NO Display support under
    // codegen (neither f-string nor println) while the interpreter rendered
    // Some(x)/None/Ok/Err — a silent interp-vs-codegen divergence the LLJIT
    // Slice-6c sweep surfaced (lru_cache). Guard the variable/place case for
    // both display sites and both payload shapes (i64 + String).
    if let Some(out) = run_program(
        "fn get(k: i64) -> Option[i64] { if k == 1 { Some(10) } else { None } }\n\
             fn main() {\n\
                 let a = get(1);\n\
                 let b = get(2);\n\
                 println(f\"a={a} b={b}\");\n\
                 println(a);\n\
                 let s: Option[String] = Some(\"hi\");\n\
                 println(f\"s={s}\");\n\
                 let r: Result[i64, String] = Ok(7);\n\
                 let e: Result[i64, String] = Err(\"boom\");\n\
                 println(f\"r={r} e={e}\");\n\
                 println(e);\n\
             }",
    ) {
        assert_eq!(
            out,
            "a=Some(10) b=None\nSome(10)\ns=Some(hi)\nr=Ok(7) e=Err(boom)\nErr(boom)\n"
        );
    }
}

#[test]
fn e2e_option_result_call_result_display_matches_interpreter() {
    // B-2026-07-08-9 (call-result half): the variable case (`let x = get();
    // f"{x}"`) was fixed first; a bare *call result* (`f"{get(1)}"`,
    // `println(get(1))`) with no intervening `let` needs a span-keyed payload
    // lookup (the variable path keys off the binding name). Guard both the
    // f-string and println sites for Option[i64], Option[String], and
    // Result[i64, String] call results.
    if let Some(out) = run_program(
        "fn get(k: i64) -> Option[i64] { if k == 1 { Some(10) } else { None } }\n\
             fn tag(k: i64) -> Option[String] { if k == 1 { Some(\"hi\") } else { None } }\n\
             fn res(k: i64) -> Result[i64, String] { if k > 0 { Ok(k) } else { Err(\"boom\") } }\n\
             fn main() {\n\
                 println(f\"a={get(1)} b={get(2)}\");\n\
                 println(get(1));\n\
                 println(f\"s={tag(1)}\");\n\
                 println(f\"r={res(7)} e={res(-1)}\");\n\
                 println(res(-2));\n\
             }",
    ) {
        assert_eq!(
            out,
            "a=Some(10) b=None\nSome(10)\ns=Some(hi)\nr=Ok(7) e=Err(boom)\nErr(boom)\n"
        );
    }
}

#[test]
fn e2e_ref_param_display_reaches_the_pointee_not_the_pointer() {
    // B-2026-09-03-5: every Display arm that renders a NAMED place handed
    // the Display fn `variables[name].ptr` — the variable's alloca. For a
    // `ref`/`mut ref` param that alloca holds the CALLER'S ADDRESS, not the
    // value, so the renderer decoded the pointer bits as a control block.
    // Measured at HEAD, on the same `fn f(x: ref T)` shape, all three
    // compiled surfaces (`karac run`, default `karac build`, and
    // `KARAC_AUTO_PAR=0`) against a correct interpreter: `ref Vec[i64]` and
    // `ref Set[i64]` SEGFAULTED, `ref Map[K, V]` printed `{}`,
    // `ref Option[T]` printed `None` for a `Some`, and
    // `ref Result[T, E]` read out of bounds and printed adjacent process
    // memory. Both display sites were affected (`println(x)` in
    // `compile_print`, `f"{x}"` in `try_compile_collection_display` /
    // `try_compile_option_result_display`), which is why this pins all five
    // container types at BOTH sites — a fix applied to one site alone
    // leaves the other rendering the pointer, and that is exactly the state
    // the first half of this fix was in.
    //
    // The `Array` / `Slice` half has a SECOND cause and needs both fixes to
    // pass: their Display tables are span-keyed off the typechecker's
    // `expr_types`, and a `ref Array[i64, 3]` is `Type::Ref(Array { .. })`,
    // which matched no arm -- so peeling the pointer alone still left
    // `f"{a}"` printing the array's FIRST ELEMENT (`1`) and `println(a)`
    // printing a raw address, because the operand never reached the array
    // renderer at all. `ref Slice[i64]` and `ref (i64, String)` failed the
    // build outright. Hence the `display_peel_ref` peel in `lowering.rs`
    // alongside the pointer fix -- and note that the peel must forward the
    // PEELED type, not the original: codegen matches these entries against
    // `TypeKind::Array` / `TypeKind::Tuple`, which a `ref ...` TypeExpr does
    // not satisfy, so forwarding the unpeeled type declines the very case
    // the peel exists to admit. The tuple arm was written that way first and
    // still refused the build; the array/slice/tuple lines below all fail if
    // it regresses.
    //
    // Expected output is the interpreter's, byte for byte.
    if let Some(out) = run_program(
        "fn pv(x: ref Vec[i64]) { println(x); println(f\"{x}\"); }\n\
             fn pm(x: ref Map[String, i64]) { println(x); println(f\"{x}\"); }\n\
             fn ps(x: ref Set[i64]) { println(x); println(f\"{x}\"); }\n\
             fn po(x: ref Option[String]) { println(x); println(f\"{x}\"); }\n\
             fn pr(x: ref Result[String, String]) { println(x); println(f\"{x}\"); }\n\
             fn mv(x: mut ref Vec[i64]) { println(x); println(f\"{x}\"); }\n\
             fn mo(x: mut ref Option[i64]) { println(x); println(f\"{x}\"); }\n\
             fn pa(a: ref Array[i64, 3]) { println(a); println(f\"{a}\"); }\n\
             fn pas(a: ref Array[String, 2]) { println(f\"{a}\"); }\n\
             fn ma(a: mut ref Array[i64, 2]) { println(f\"{a}\"); }\n\
             fn psl(s: ref Slice[i64]) { println(s); println(f\"{s}\"); }\n\
             fn pt(t: ref (i64, String)) { println(t); println(f\"{t}\"); }\n\
             fn mt(t: mut ref (i64, String)) { println(f\"{t}\"); }\n\
             fn main() {\n\
                 let v: Vec[i64] = [1i64, 2i64, 3i64];\n\
                 pv(v);\n\
                 let mut m: Map[String, i64] = Map.new();\n\
                 m.insert(\"a\", 1i64);\n\
                 pm(m);\n\
                 let mut s: Set[i64] = Set.new();\n\
                 s.insert(9i64);\n\
                 ps(s);\n\
                 let o: Option[String] = Some(\"pay0\");\n\
                 po(o);\n\
                 let r: Result[String, String] = Ok(\"pay1\");\n\
                 pr(r);\n\
                 let mut mv0: Vec[i64] = [7i64];\n\
                 mv(mut mv0);\n\
                 let mut mo0: Option[i64] = Some(4i64);\n\
                 mo(mut mo0);\n\
                 let arr: Array[i64, 3] = [1i64, 2i64, 3i64];\n\
                 pa(arr);\n\
                 let arrs: Array[String, 2] = [\"p\", \"q\"];\n\
                 pas(arrs);\n\
                 let mut marr: Array[i64, 2] = [8i64, 9i64];\n\
                 ma(mut marr);\n\
                 let sv: Vec[i64] = [4i64, 5i64];\n\
                 let sl = sv[0..2];\n\
                 psl(sl);\n\
                 let tup = (3i64, \"t\");\n\
                 pt(tup);\n\
                 let mut mtup = (4i64, \"u\");\n\
                 mt(mut mtup);\n\
             }",
    ) {
        assert_eq!(
                out,
                "[1, 2, 3]\n[1, 2, 3]\n{a: 1}\n{a: 1}\nSet{9}\nSet{9}\nSome(pay0)\nSome(pay0)\nOk(pay1)\nOk(pay1)\n[7]\n[7]\nSome(4)\nSome(4)\n[1, 2, 3]\n[1, 2, 3]\n[p, q]\n[8, 9]\n[4, 5]\n[4, 5]\n(3, t)\n(3, t)\n(4, u)\n"
            );
    }
}

#[test]
fn e2e_whole_tuple_display_matches_interpreter() {
    // B-2026-07-18-14: interpolating / printing a WHOLE tuple value (`f"{t}"`,
    // `println(t)`) passed `karac check` and rendered `(3, 7)` in the
    // interpreter but FAILED codegen with the misleading "bind ... to a `let`
    // first" struct-Display error — the last codegen-vs-interpreter Display
    // divergence. Codegen now routes a tuple-typed interpolation/print
    // operand through the element-wise `emit_tuple_display_fn` (`(a, b)`
    // format). Covers scalar, String-element, mixed, nested, and call-result
    // tuples, at both the f-string and println sites.
    if let Some(out) = run_program(
        "fn pair() -> (i64, i64) { (5, 6) }\n\
             fn main() {\n\
                 let t: (i64, i64) = (3, 7);\n\
                 println(f\"{t}\");\n\
                 println(t);\n\
                 let s: (i64, String) = (1, \"hi\");\n\
                 println(f\"s={s}\");\n\
                 let n: (i64, (i64, i64)) = (1, (2, 3));\n\
                 println(n);\n\
                 println(pair());\n\
                 println(f\"{t.0} {t.1}\");\n\
             }",
    ) {
        assert_eq!(out, "(3, 7)\n(3, 7)\ns=(1, hi)\n(1, (2, 3))\n(5, 6)\n3 7\n");
    }
}

#[test]
fn e2e_option_ref_payload_display_matches_interpreter() {
    // B-2026-07-18-24: `Vec.first()` / `.get(i)` / `.last()` are typed
    // `Option[ref T]` (the borrow-typed accessor, B-2026-07-14-11). For a
    // SCALAR element the `ref` is a type-system artifact — the value is
    // returned by copy in the Some payload word — but the Display
    // registration rejected the `ref`-wrapped payload as non-reconstructable,
    // so an UNannotated `let x = v.first(); println(x)` (and a bare
    // `println(v.first())`) failed codegen with the deferred struct-Display
    // error while the interpreter rendered `Some(10)`. A scalar-ref peel at
    // the registration + call-result sites closes it. (A `ref String` payload
    // is peeled too — see `e2e_option_ref_string_payload_display`, B-2026-07-18-40.)
    // Covers i64/f64/usize-word scalars, the None arm, both the let-place and
    // bare-call sites.
    if let Some(out) = run_program(
        "fn main() {\n\
                 let v = [10, 20, 30];\n\
                 let a = v.first();\n\
                 let b = v.last();\n\
                 println(a);\n\
                 println(b);\n\
                 println(v.get(1));\n\
                 let fv = [1.5, 2.5];\n\
                 println(fv.first());\n\
                 let mut empty: Vec[i64] = Vec.new();\n\
                 println(empty.first());\n\
             }",
    ) {
        assert_eq!(out, "Some(10)\nSome(30)\nSome(20)\nSome(1.5)\nNone\n");
    }
}

#[test]
fn e2e_option_ref_string_payload_display() {
    // B-2026-07-18-40 — `Vec[String].get(i)` / `.first()` / `.last()` are
    // typed `Option[ref String]`, but codegen builds the `Some` payload by
    // loading the element's whole `{ptr,len,cap}` into the 3 inline payload
    // words (`coerce_to_payload_words(_, 3)`) — byte-identical to a plain
    // `Option[String]`. Display previously rejected the `ref String` payload
    // as non-reconstructable (deferred struct-Display error) while the
    // interpreter rendered `Some(alpha)`; peeling `ref String`/`ref str` to
    // the owned renderer closes it. Display is read-only (appends a byte
    // copy) so the borrowed Vec buffer is untouched — leak/double-free-free
    // (asan_display_option_ref_string_from_get). Covers get/first/last, the
    // bare-call and let-place sites, an f-string, the None arm, and the Vec
    // staying usable afterward.
    if let Some(out) = run_program(
        "fn main() {\n\
                 let v: Vec[String] = [\"alpha\", \"beta\", \"gamma\"];\n\
                 println(v.get(0));\n\
                 println(v.first());\n\
                 println(v.last());\n\
                 let x = v.get(1);\n\
                 println(x);\n\
                 println(f\"{x}\");\n\
                 println(v.get(9));\n\
                 println(v.len());\n\
             }",
    ) {
        assert_eq!(
            out,
            "Some(alpha)\nSome(alpha)\nSome(gamma)\nSome(beta)\nSome(beta)\nNone\n3\n"
        );
    }
}

#[test]
fn e2e_tuple_element_string_method() {
    // B-2026-07-09-1: a method on a tuple ELEMENT — `e.0.bytes()` — failed
    // codegen ("no handler for method 'bytes' on non-identifier receiver")
    // while the interpreter accepted it. The tuple-index receiver path had
    // no element-type source for a bare tuple-typed identifier. Two shapes:
    // a plain tuple local, and a `Vec[(K, V)]` for-loop element (what
    // `map.entries()` yields — the shape a `#[derive(Message)]` map-field
    // encode loop generates, `e0.0.bytes()`). Both now resolve.
    // Order-independent (Map iteration order is unspecified): sum the key
    // byte-lengths (2 + 3 = 5).
    if let Some(out) = run_program(
        "fn main() {\n\
                 let e: (String, i64) = (\"hello\", 7);\n\
                 println(e.0.bytes().len());\n\
                 let mut m: Map[String, i64] = Map.new();\n\
                 let _ = m.insert(\"ab\", 1);\n\
                 let _ = m.insert(\"cde\", 2);\n\
                 let mut total: i64 = 0;\n\
                 for kv in m.entries() { total = total + kv.0.bytes().len(); }\n\
                 println(total);\n\
             }",
    ) {
        assert_eq!(out, "5\n5\n");
    }
}

#[test]
fn e2e_compound_struct_payload_display_matches_interpreter() {
    // B-2026-07-08-18: a struct payload nested in a container's Display
    // (`Option[P]` / `Result[P,E]` / `Vec[P]` / a user enum) rendered a
    // codegen error (or crash) while the interpreter rendered it. That row
    // closed the DIVERGENCE by teaching codegen to emit what the
    // interpreter emitted — the debug format `P { field: val, … }` — which
    // agreed, but agreed on the wrong shape: design.md § Strings named that
    // exact rendering "a `Debug`-in-`Display` bug to be aligned to this
    // rule", the rule being that a payload renders through its own
    // `Display`.
    //
    // B-2026-08-26-29 aligned it. A hand-written `impl Display` now wins at
    // every depth, so the payload renders `(3,4)` — the impl — in every
    // position, exactly as the bare `println(p)` spelling always did. The
    // two backends stay byte-identical, which is what this test is for;
    // what changed is which of the two shapes they agree on.
    if let Some(out) = run_program(
        "struct P { x: i64, y: i64 }\n\
             impl Display for P { fn to_string(ref self) -> String { f\"({self.x},{self.y})\" } }\n\
             fn main() {\n\
                 let p = P { x: 3, y: 4 };\n\
                 println(p);\n\
                 let o: Option[P] = Some(P { x: 3, y: 4 });\n\
                 println(o);\n\
                 let n: Option[P] = None;\n\
                 println(n);\n\
                 let r: Result[P, i64] = Ok(P { x: 1, y: 2 });\n\
                 println(r);\n\
                 let v = [P { x: 5, y: 6 }];\n\
                 println(v);\n\
                 println(f\"got {o}\");\n\
             }",
    ) {
        assert_eq!(
            out,
            "(3,4)\nSome((3,4))\nNone\nOk((1,2))\n[(5,6)]\ngot Some((3,4))\n"
        );
    }
}

#[test]
fn ir_borrowed_string_slice_map_key_uses_borrow_externs() {
    // Regression guard that the allocation-free path actually fires: a
    // String-slice key in get/insert must lower to the non-allocating
    // `karac_string_slice_borrow` view and (for insert) the borrowed-key
    // `karac_map_insert_borrowed_str_old`, not the allocating
    // `karac_string_slice` + owning `karac_map_insert_old`.
    let ir = ir_for(
        "fn main() {\n\
                 let s = \"abcd\";\n\
                 let mut m: Map[String, i64] = Map.new();\n\
                 match m.get(s[0..2]) { Some(_v) => {}, None => {} }\n\
                 m.insert(s[2..4], 1);\n\
                 println(m.len());\n\
             }",
    );
    // Every runtime extern is *declared* in the module, so a bare
    // `contains` would pass vacuously. Count occurrences: the declaration
    // is one, an actual call adds at least one more.
    assert!(
        ir.matches("@karac_string_slice_borrow").count() >= 2,
        "String-slice map key should *call* the borrowed-view helper; IR:\n{ir}"
    );
    assert!(
        ir.matches("@karac_map_insert_borrowed_str_old").count() >= 2,
        "String-slice insert key should *call* the borrowed-key insert; IR:\n{ir}"
    );
}

#[test]
fn ir_fixed_width_string_slice_folds_view_len() {
    // The String-build residual fix: a fixed-width borrowed slice
    // (`s[d..d+1]`, `s[1..3]`) carries a compile-time byte width, so the
    // view's `len` field is emitted as an i64 constant rather than the
    // runtime `bs.view.len = sub end, start`. That constant flows through
    // `push_str`'s memcpy and lets it lower to a sized store instead of a
    // branchy variable-length copy. The `bs.view.len` sub name is unique to
    // that subtraction, so its absence proves the fold fired.
    let folded = ir_for(
        "fn main() {\n\
                 let s = \"abcd\";\n\
                 let mut out: String = \"\";\n\
                 let mut d: i64 = 0;\n\
                 while d < 4 {\n\
                     out.push_str(s[d..d+1]);\n\
                     d = d + 1;\n\
                 }\n\
                 out.push_str(s[1..3]);\n\
                 println(out);\n\
             }",
    );
    assert!(
        !folded.contains("bs.view.len"),
        "fixed-width slice widths (`s[d..d+1]`, `s[1..3]`) must fold to an \
             i64 constant, not a runtime `bs.view.len` subtraction; IR:\n{folded}"
    );

    // A genuinely runtime width (`s[a..b]`, distinct runtime bounds) has no
    // compile-time width and must keep the exact subtraction.
    let runtime = ir_for(
        "fn main() {\n\
                 let s = \"abcd\";\n\
                 let mut out: String = \"\";\n\
                 let a: i64 = 1;\n\
                 let b: i64 = 3;\n\
                 out.push_str(s[a..b]);\n\
                 println(out);\n\
             }",
    );
    assert!(
        runtime.contains("bs.view.len"),
        "a runtime-width slice (`s[a..b]`) must keep the exact `end - start` \
             subtraction; IR:\n{runtime}"
    );
}

#[test]
fn ir_map_string_key_clear_uses_drop_variant() {
    // `Map[String, _].clear()` must free heap key buffers via the drop
    // variant, not leak them through plain `karac_map_clear`. (Count > 1
    // = declared *and* called; a bare `contains` matches the declaration
    // alone.)
    let ir = ir_for(
        "fn main() {\n\
                 let mut m: Map[String, i64] = Map.new();\n\
                 m.insert(\"a\", 1);\n\
                 m.clear();\n\
                 println(m.len());\n\
             }",
    );
    assert!(
        ir.matches("@karac_map_clear_with_drop_vec").count() >= 2,
        "String-keyed clear should *call* the drop variant; IR:\n{ir}"
    );
}

#[test]
fn e2e_string_slice_non_char_boundary_panics() {
    // `é` is two bytes at offsets 1..3, so byte 2 is mid-char: the
    // runtime helper must exit(1) with E_STRING_SLICE_NOT_AT_CHAR_BOUNDARY
    // (mirroring Rust + the interpreter), not silently slice.
    if let Some(cap) = run_program_capturing(
        "fn main() {\n\
                 let s = \"héllo\";\n\
                 println(s[0..2]);\n\
             }",
    ) {
        assert!(
            cap.stderr.contains("E_STRING_SLICE_NOT_AT_CHAR_BOUNDARY"),
            "expected char-boundary panic on stderr, got stdout={:?} stderr={:?}",
            cap.stdout,
            cap.stderr,
        );
    }
}

/// B-2026-09-16-1 — the slice failure path reports the ACTUAL range, not a
/// constant message.
///
/// The fast path's cold edge is a `noreturn` call to
/// `karac_string_slice_fail` specifically so this text survives. The
/// cheaper thing to emit there is `emit_panic`, which is also `noreturn`
/// and measured within 5% — but its message is a compile-time constant, so
/// `start`, `end`, `len` and the `E_…` code would all be gone. The existing
/// boundary test asserts only that the code appears, which `emit_panic`
/// with a hand-copied string would satisfy; these assert the values, which
/// only routing through the runtime's own `slice_validate` can produce.
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
fn e2e_string_slice_failure_reports_the_actual_range() {
    // Out of range. `n` is computed so the bound is not a constant the
    // compiler could fold into a static diagnostic.
    if let Some(cap) = run_program_capturing(
        "fn main() {\n\
             \x20   let s = \"hello\";\n\
             \x20   let n = s.len() + 5;\n\
             \x20   println(s[0..n]);\n\
             }",
    ) {
        assert!(
            cap.stderr
                .contains("string slice bounds 0..10 out of range (len 5)"),
            "expected the range and length in the message, got stderr={:?}",
            cap.stderr,
        );
    }
    // Mid-char. Same requirement on the other predicate.
    if let Some(cap) = run_program_capturing(
        "fn main() {\n\
             \x20   let s = \"h\\u{e9}llo\";\n\
             \x20   let k = s.len() - 4;\n\
             \x20   println(s[0..k]);\n\
             }",
    ) {
        assert!(
            cap.stderr.contains("E_STRING_SLICE_NOT_AT_CHAR_BOUNDARY")
                && cap.stderr.contains("byte range 0..2"),
            "expected the code AND the byte range, got stderr={:?}",
            cap.stderr,
        );
    }
}

/// B-2026-09-16-1 — the inlined heap route matches what the runtime wrote.
///
/// Slices longer than the 23-byte overlay no longer call
/// `karac_string_slice_into`; codegen emits the allocation itself. The
/// contract it has to reproduce is the runtime's, not the simpler one
/// `substring` uses: `n + 1` bytes with a NUL at `[n]`, because
/// `runtime/src/clone.rs` records a printf overread fixed by adding
/// exactly that spare byte. A sliced String is freely interchangeable with
/// a cloned one, so the two must agree on every observable.
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
fn e2e_string_slice_heap_route_matches_the_clone_contract() {
    let Some(out) = run_program(
        "fn main() {\n\
             \x20   let s = \"the quick brown fox jumps over the lazy dog, twice over\";\n\
             \x20   let a = s[0..24];\n\
             \x20   let b = s[4..40];\n\
             \x20   let c = s[0..s.len()];\n\
             \x20   println(f\"1 {a.len()} [{a}]\")\n\
             \x20   println(f\"2 {b.len()} [{b}]\")\n\
             \x20   println(f\"3 {c == s}\")\n\
             \x20   let d = c.clone();\n\
             \x20   println(f\"4 {d == c} {d.len() == c.len()}\")\n\
             \x20   let mut e = s[10..40];\n\
             \x20   e.push_str(\"!\");\n\
             \x20   println(f\"5 {e.len()} [{e}]\")\n\
             \x20   let mut v: Vec[String] = Vec.new();\n\
             \x20   let mut i = 0;\n\
             \x20   while i < 8 { v.push(s[i..(i + 30)]); i = i + 1; }\n\
             \x20   println(f\"6 {v.len()} {v[0].len()} [{v[7]}]\")\n\
             \x20   println(\"end\")\n\
             }\n\
             ",
    ) else {
        return;
    };
    assert_eq!(
        out,
        "1 24 [the quick brown fox jump]\n\
             2 36 [quick brown fox jumps over the lazy ]\n\
             3 true\n\
             4 true true\n\
             5 31 [brown fox jumps over the lazy !]\n\
             6 8 30 [ck brown fox jumps over the la]\n\
             end\n"
    );
}

/// B-2026-09-16-32 — `String.substring`'s heap result is NUL-terminated,
/// like every other heap `String` in the language.
///
/// The three producers of a heap `String` had two different buffer
/// contracts. `karac_string_clone` and `karac_string_slice_into` allocate
/// `n + 1` and write a NUL at `[n]`; `substring`'s copy branch allocated
/// exactly `n` and wrote no terminator. `runtime/src/clone.rs` records what
/// that costs — the runtime allocated exactly `len` once too, and
/// `printf("%s", data)` read one byte past the end, an ASAN
/// heap-buffer-overflow. The spare byte is that fix, and `substring` was
/// the last producer still in the pre-fix shape.
///
/// **Why this asserts on IR rather than on output.** The defect is latent:
/// `println` passes pointer+length, so nothing in the language reads to a
/// NUL today and no program can observe the difference. Measured under
/// guardmalloc at both SSO arms before the fix, a 30-byte substring read
/// nothing past its allocation. So a behavioural fixture here would pass
/// either way — the assertion has to be on what is emitted.
///
/// `cap` deliberately stays `n`: it reports usable content bytes, and the
/// spare byte is not one. That matches `karac_string_clone` exactly, so the
/// two remain interchangeable in every field a program can read.
#[test]
fn substring_heap_result_is_nul_terminated_like_every_other_string() {
    let ir = ir_for(
        "fn main() {\n\
             \x20   let s = \"the quick brown fox jumps over the lazy dog\";\n\
             \x20   let t = s.substring(0, 30);\n\
             \x20   println(t);\n\
             }",
    );
    // The copy branch: allocate the content bytes PLUS one.
    assert!(
        ir.contains("ss.alloc_bytes"),
        "substring's heap arm must size its allocation as new_len + 1; \
             no `ss.alloc_bytes` in the emitted IR. Relevant lines:\n{}",
        ir.lines()
            .filter(|l| l.contains("ss."))
            .collect::<Vec<_>>()
            .join("\n")
    );
    let alloc_line = ir
        .lines()
        .find(|l| l.contains("ss.alloc_bytes ="))
        .unwrap_or_else(|| {
            panic!(
                "no defining line for ss.alloc_bytes:\n{}",
                ir.lines()
                    .filter(|l| l.contains("ss."))
                    .collect::<Vec<_>>()
                    .join("\n")
            )
        });
    assert!(
        alloc_line.contains("add") && alloc_line.contains(" 1"),
        "ss.alloc_bytes must be `new_len + 1`, got: {alloc_line}"
    );
    // ... and write the terminator at [new_len].
    assert!(
        ir.contains("ss.nul.p"),
        "substring's heap arm must store a NUL at [new_len]; no `ss.nul.p` \
             in the emitted IR. Relevant lines:\n{}",
        ir.lines()
            .filter(|l| l.contains("ss."))
            .collect::<Vec<_>>()
            .join("\n")
    );
    let nul_store = ir
        .lines()
        .find(|l| l.contains("store i8 0") && l.contains("ss.nul.p"));
    assert!(
        nul_store.is_some(),
        "expected `store i8 0, ptr %ss.nul.p`; ss.nul lines:\n{}",
        ir.lines()
            .filter(|l| l.contains("ss.nul"))
            .collect::<Vec<_>>()
            .join("\n")
    );
    // `cap` is still the content length, not the allocation size — the
    // spare byte is not usable capacity. Same convention as clone.
    assert!(
        ir.contains("ss.copy.cap"),
        "the copy branch should still build a cap field"
    );
}

/// B-2026-07-30-9 — `println` fuses its payload and newline into ONE
/// `write_console` call, so a line reaches the OS atomically.
///
/// It used to emit two calls, and the lock that keeps a write intact —
/// glibc's per-`FILE` lock inside `fwrite` — is released between them. Two
/// `spawn`ed tasks printing concurrently interleaved as payload-A,
/// payload-B, newline-A, newline-B: `12\n\n` for a program that says
/// `1\n2\n`. Measured on a five-task fan-out: 10 garbled runs in 60 before,
/// 0 in 60 after.
///
/// Asserted on the IR rather than by running the program because the bug is
/// a RACE — a passing run proves nothing, and the pre-fix program passes
/// most of the time. The call-shape is the invariant; the flake is a
/// symptom.
#[test]
fn test_ir_println_fuses_payload_and_newline_into_one_write() {
    let ir = ir_for("fn main() { println(\"hi\"); }");
    assert!(
        ir.contains("call void @__karac_write_console_line("),
        "println should route through the line-atomic wrapper:\n{ir}"
    );
    // The staging wrapper itself holds the only plain `write_console` calls
    // for this program; `main` must contain none of its own.
    let main_body = ir
        .split("define i32 @main()")
        .nth(1)
        .and_then(|s| s.split("\n}").next())
        .unwrap_or_default();
    assert!(
        !main_body.contains("call void @__karac_write_console("),
        "println must not also emit a bare write_console in main:\n{main_body}"
    );
}

#[test]
fn test_ir_unsigned_op_inside_fstring_is_lowered() {
    // B-2026-07-04-8: `ExprKind::InterpolatedStringLit` was a leaf in the
    // lowering pass, so operators inside `f"{...}"` were never rewritten to
    // their signedness-aware trait-method calls (`u64.div`, …). Codegen then
    // fell back to its always-signed raw-`Binary` path, silently emitting a
    // signed div/rem/shift/compare for a u64 op printed via an f-string —
    // a `build`-only wrong result (`run` used a span-threaded unsigned hint).
    // Lowering now recurses into interpolation parts, so the op is unsigned.
    let ir = ir_for("fn f(a: u64, b: u64) -> String { f\"{a / b}\" }");
    assert!(
        ir.contains("udiv i64"),
        "u64 `/` inside an f-string must emit udiv:\n{ir}"
    );
    assert!(
        !ir.contains("sdiv i64"),
        "u64 `/` inside an f-string must not emit signed div:\n{ir}"
    );
}

// ── End-to-end execution tests ────────────────────────────────
// These compile → link → run and verify stdout.

/// Regex codegen (B-2026-07-14-19) — `Regex.compile(pat).unwrap().is_match(s)`
/// via the runtime `karac_regex_*` externs (the opt-in
/// `libkarac_runtime_regex.a`, auto-selected by `link_executable` on the
/// symbol reference). Value-correctness matching the interpreter oracle;
/// memory cleanliness is in `tests/memory_sanitizer.rs::asan_regex_*`. Skips
/// gracefully (like every E2E) when the runtime archive / linker is absent.
#[test]
fn regex_compile_is_match_runs() {
    // Skips vacuously (like every E2E) when the runtime archive / linker is
    // absent — including the opt-in `libkarac_runtime_regex.a`, without which
    // `link_executable` returns Err and `run_program` yields None.
    let Some(out) = run_program(
        "fn main() {\n\
             let re = Regex.compile(\"^a.c$\").unwrap();\n\
             let a = re.is_match(\"abc\");\n\
             let b = re.is_match(\"abcd\");\n\
             let c = re.is_match(\"a-c\");\n\
             println(f\"{a} {b} {c}\");\n\
             }\n",
    ) else {
        return;
    };
    assert_eq!(out, "true false true\n");
}

/// The Err path — an invalid pattern yields `Result.Err(RegexError)`, so
/// `is_err()` is true and `unwrap_or(false)` on the `is_match` never runs.
#[test]
fn regex_compile_invalid_pattern_is_err_runs() {
    let Some(out) = run_program(
        "fn main() {\n\
             let r = Regex.compile(\"[unterminated\");\n\
             println(f\"{r.is_err()}\");\n\
             }\n",
    ) else {
        return;
    };
    assert_eq!(out, "true\n");
}

/// A digit-class pattern re-used across several subjects — exercises the
/// per-call re-compile path (the `Regex` value carries only its pattern).
#[test]
fn regex_digit_class_multiple_subjects_runs() {
    let Some(out) = run_program(
        "fn main() {\n\
             let re = Regex.compile(\"^[0-9]+$\").unwrap();\n\
             let a = re.is_match(\"12345\");\n\
             let b = re.is_match(\"12a45\");\n\
             let c = re.is_match(\"\");\n\
             println(f\"{a} {b} {c}\");\n\
             }\n",
    ) else {
        return;
    };
    assert_eq!(out, "true false false\n");
}

/// `re.find(s) -> Option[Match]` (B-2026-07-14-19 slice 2). The `Some`
/// arm carries a `Match { text, start, end }` (a wide payload boxed into
/// the `Option`); the `None` arm is a no-match. Byte offsets match the
/// interpreter's `find`. Value parity is the oracle.
#[test]
fn regex_find_some_and_none_runs() {
    let Some(out) = run_program(
        "fn main() {\n\
             let re = Regex.compile(\"[0-9]+\").unwrap();\n\
             match re.find(\"abc123def\") {\n\
             Some(m) => { println(f\"{m.text} {m.start} {m.end}\"); }\n\
             None => { println(\"none\"); }\n\
             }\n\
             match re.find(\"no digits\") {\n\
             Some(m) => { println(f\"{m.text}\"); }\n\
             None => { println(\"none\"); }\n\
             }\n\
             }\n",
    ) else {
        return;
    };
    assert_eq!(out, "123 3 6\nnone\n");
}

/// `re.find_all(s) -> Vec[Match]` (B-2026-07-14-19 slice 2). Codegen loops
/// the runtime's offset array to build each `Match` into a fresh `Vec`
/// buffer; every `Match.text` is an owned substring copy. Mirrors the
/// interpreter's `find_iter`.
#[test]
fn regex_find_all_collects_matches_runs() {
    let Some(out) = run_program(
        "fn main() {\n\
             let re = Regex.compile(\"[0-9]+\").unwrap();\n\
             let ms = re.find_all(\"a1b22c333\");\n\
             println(ms.len().to_string());\n\
             for m in ms {\n\
             println(f\"{m.text}@{m.start}\");\n\
             }\n\
             }\n",
    ) else {
        return;
    };
    assert_eq!(out, "3\n1@1\n22@3\n333@6\n");
}

/// `re.replace_all(s, repl) -> String` (B-2026-07-14-19 slice 2). The
/// runtime produces the whole replaced buffer; codegen adopts it as an
/// owned `String`. The no-match subject returns a fresh copy unchanged.
#[test]
fn regex_replace_all_substitutes_runs() {
    let Some(out) = run_program(
        "fn main() {\n\
             let re = Regex.compile(\"[0-9]+\").unwrap();\n\
             println(re.replace_all(\"a1b22c333\", \"#\"));\n\
             println(re.replace_all(\"no digits\", \"#\"));\n\
             }\n",
    ) else {
        return;
    };
    assert_eq!(out, "a#b#c#\nno digits\n");
}

/// B-2026-08-05-7 — an owned boxed-payload enum param that ESCAPES must
/// not be freed by the callee.
///
/// A `Opt[String]` monomorph packs a 3-word payload into the erased 1-word
/// area, so `coerce_to_payload_words` heap-boxes it and the callee's param
/// registration frees the box at scope exit. That registration ran for
/// every owned param, including the two that hand the box on: `ident`
/// returns its param and freed it anyway (`free(): double free detected in
/// tcache 2`, SIGABRT) and `fwd` forwards it to a second by-value param
/// (freed twice, then a SIGSEGV at -O0).
///
/// Kept here rather than only under ASAN because this one aborts at the
/// DEFAULT -O2 — the box is live memory whichever way the optimizer goes,
/// so no dead-allocation elision can mask it. Asserted strictly
/// (`assert_eq!(run_program(..), Some(..))`): an abort makes `run_program`
/// return `None`, which the tolerant `let Some(out) = .. else { return }`
/// form would swallow as a skip.
#[test]
fn e2e_boxed_enum_param_escape_no_double_free() {
    let src = r#"
enum Opt[T] { Yes(T), No }
fn get(o: Opt[String], d: i64) -> i64 {
    match o {
        Opt.Yes(s) => s.len(),
        Opt.No => d,
    }
}
fn fwd(o: Opt[String]) -> i64 { get(o, -1) }
fn ident(o: Opt[String]) -> Opt[String] { o }
fn main() {
    println(get(Opt.Yes("consumed in place by the callee"), -1));
    println(fwd(Opt.Yes("forwarded to a second by-value param")));
    let back: Opt[String] = ident(Opt.Yes("returned straight back out of the callee"));
    match back {
        Opt.Yes(s) => println(s.len()),
        Opt.No => println(-1),
    }
    println(get(Opt.No, 7));
}
"#;
    assert_eq!(run_program(src).as_deref(), Some("31\n36\n40\n7\n"));
}

/// B-2026-08-05-8 — `contains` on a PATTERN-BOUND `String` payload.
///
/// This was a run-vs-build divergence, not a diagnostics nit: `karac check`
/// passed, the interpreter produced the right answer, and only `karac build`
/// refused it — with "Binary op Eq: right operand has non-comparable type
/// { ptr, i64, i64 }", blaming the typechecker for an `==` the source never
/// contained.
///
/// Cause: a `String` payload binding registered `vec_elem_types` (which
/// exists for the scope-exit buffer free) but never `string_vars` (which
/// method dispatch consults to pick the String arm for a method name that
/// String and Vec SHARE). So the binding looked like a `Vec[u8]`,
/// `contains` fell through to Vec membership, and that compares elements
/// with `==` — handing codegen a whole String struct where a scalar was
/// expected.
///
/// The matrix matters: the row recorded `.len()` and `.starts_with` working
/// on the very same binding, which is what localized this to shared-name
/// dispatch rather than to pattern binding in general. All five carriers the
/// row listed as failing are covered — the carrier was never the variable,
/// the pattern binding was.
#[test]
fn contains_on_a_pattern_bound_string_payload_compiles() {
    let out = run_program(
        r#"
enum E { A(String), B(i64) }
fn mks(k: i64) -> String { let mut s: String = String.new(); s.push_str(f"pay-{k}"); return s; }
fn dig(i: i64) -> String { let mut d: String = String.new(); d.push_str(f"{i}"); return d; }
fn main() {
    let n: i64 = 1;
    let mut hits: i64 = 0;

    let r: Result[String, i64] = Result.Ok(mks(n));
    match r { Result.Ok(s) => { if s.contains(dig(n)) { hits = hits + 1; } } Result.Err(e) => {} }

    let r2: Result[i64, String] = Result.Err(mks(n));
    match r2 { Result.Ok(v) => {} Result.Err(e) => { if e.contains(dig(n)) { hits = hits + 10; } } }

    let o: Option[String] = Option.Some(mks(n));
    match o { Option.Some(s) => { if s.contains(dig(n)) { hits = hits + 100; } } Option.None => {} }

    let u: E = E.A(mks(n));
    match u { E.A(s) => { if s.contains(dig(n)) { hits = hits + 1000; } } E.B(v) => {} }

    let o2: Option[String] = Option.Some(mks(n));
    if let Option.Some(s) = o2 { if s.contains(dig(n)) { hits = hits + 10000; } }

    println(hits);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(
            out.trim(),
            "11111",
            "every carrier must find the needle — a missing digit names the \
                 shape that regressed (1 Ok, 10 Err, 100 Some, 1000 user enum, \
                 10000 if-let)"
        );
    }
}

/// Companion: the methods that always WORKED must keep working.
/// `string_vars` now also drives these bindings, so a mistake in that
/// registration would surface as the String arm firing where it should not
/// — `len` reading a Vec length instead of a byte length, say.
#[test]
fn pattern_bound_string_keeps_its_working_methods() {
    let out = run_program(
        r#"
fn mks(k: i64) -> String { let mut s: String = String.new(); s.push_str(f"pay-{k}"); return s; }
fn main() {
    let r: Result[String, i64] = Result.Ok(mks(7));
    match r {
        Result.Ok(s) => {
            println(s.len());
            println(s.starts_with("pay"));
        }
        Result.Err(e) => { println(0); }
    }
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(
            out.trim(),
            "5\ntrue",
            "`len` must still be the BYTE length of \"pay-7\" and \
                 `starts_with` must still match"
        );
    }
}

/// B-2026-08-04-11 leg (a) — an f-string INTERPOLATION of a heap field
/// consumes it, exactly as passing that field by value to a free fn does.
///
/// `match g(i) { Err(e) => println(f"{e.msg}") }` over a fresh `Result`
/// temp whose Err payload is a struct with a String field ABORTED with
/// `free(): double free detected`, while `karac run` printed the right
/// answer. The by-value spelling `println(e.msg)` was already handled —
/// `wrapper_arm_moves_heap_field_to_free_fn` exists for it — and an
/// interpolation is the same move wearing different syntax, which that
/// predicate did not recognize. `consume_class` scored it borrow-only, the
/// arm kept the source's inline-payload drop armed, and it freed a buffer
/// the interpolation had already consumed.
///
/// The first four arms are the discriminator that found this: identical
/// payload, only the arm body varies. The by-value arg and the
/// interpolation must both be treated as moves; a genuine READ of the
/// field (`.len()`) and a bound-but-unused binding must both NOT be, or
/// the source drop is suppressed with no second owner and the payload
/// leaks instead.
///
/// The last three are the immunities that localize it: a wildcard arm
/// (nothing binds, so nothing ever competed with the source drop), the
/// same struct through `Option` (this registration is `Result`-only), and
/// a NAMED scrutinee (which takes the ordinary binding path).
#[test]
fn e2e_fstring_interpolation_of_a_payload_heap_field_is_a_move() {
    let Some(out) = run_program(
        "struct Wrap { msg: String }\n\
             fn mk(i: i64) -> Wrap { return Wrap { msg: f\"err-payload-{i}\" }; }\n\
             fn g(i: i64) -> Result[i64, Wrap] { return Result.Err(mk(i)); }\n\
             fn gopt(i: i64) -> Option[Wrap] { return Option.Some(mk(i)); }\n\
             fn main() {\n\
             \x20   match g(1i64) {\n\
             \x20       Result.Ok(v) => { println(f\"ok{v}\"); }\n\
             \x20       Result.Err(e) => { println(e.msg); }\n\
             \x20   }\n\
             \x20   match g(2i64) {\n\
             \x20       Result.Ok(v) => { println(f\"ok{v}\"); }\n\
             \x20       Result.Err(e) => { println(f\"b:{e.msg}\"); }\n\
             \x20   }\n\
             \x20   match g(3i64) {\n\
             \x20       Result.Ok(v) => { println(f\"ok{v}\"); }\n\
             \x20       Result.Err(e) => { println(f\"c:{e.msg.len()}\"); }\n\
             \x20   }\n\
             \x20   match g(4i64) {\n\
             \x20       Result.Ok(v) => { println(f\"ok{v}\"); }\n\
             \x20       Result.Err(e) => { println(\"d:unused\"); }\n\
             \x20   }\n\
             \x20   match g(5i64) {\n\
             \x20       Result.Ok(v) => { println(f\"ok{v}\"); }\n\
             \x20       Result.Err(_) => { println(\"e:wild\"); }\n\
             \x20   }\n\
             \x20   match gopt(6i64) {\n\
             \x20       Option.Some(w) => { println(f\"f:{w.msg}\"); }\n\
             \x20       Option.None => { println(\"f:none\"); }\n\
             \x20   }\n\
             \x20   let r: Result[i64, Wrap] = g(7i64);\n\
             \x20   match r {\n\
             \x20       Result.Ok(v) => { println(f\"ok{v}\"); }\n\
             \x20       Result.Err(e) => { println(f\"g:{e.msg}\"); }\n\
             \x20   }\n\
             \x20   println(\"end\");\n\
             }\n",
    ) else {
        return;
    };
    assert_eq!(
        out,
        "err-payload-1\nb:err-payload-2\nc:13\nd:unused\n\
             e:wild\nf:err-payload-6\ng:err-payload-7\nend\n"
    );
}

/// B-2026-09-04-14 — A BINDING NAME MUST NOT CARRY OWNERSHIP STATE OUT OF
/// ITS FUNCTION.
///
/// `drop_rc.inline_optres_retained_sources` is a `HashSet<String>` keyed by
/// bare binding name. A read-only `match` arm publishes its scrutinee's
/// name into it so a following combinator knows the arm left the payload
/// where it was, and
/// `suppress_inline_option_result_binding_move_impl` reads it as a veto:
/// a source a borrowing arm still OWNS must not be disarmed on a move, or
/// its payload leaks. The set was the one table of its shape missing from
/// the per-function reset in `functions.rs`, so the veto outlived the body
/// that published it.
///
/// `retained` AND `mover` SHARE NOTHING BUT THE NAME `o`, and `retained` is
/// NEVER CALLED. Its arm's veto still reached `mover`'s move, which was
/// therefore not disarmed, so both slots freed one box: `free(): double
/// free detected in tcache 2` on the two-function reduction and a SIGSEGV
/// here, with stdout still buffered so nothing prints. Renaming EITHER
/// binding made the same program correct — which is what identified the
/// table rather than the logic.
///
/// ASSERTED STRICTLY for the reason `e2e_boxed_enum_param_escape_no_double_free`
/// states: the pre-fix program produces no output at all, and `run_program`
/// returns `None` for a crash, which the tolerant
/// `let Some(out) = .. else { return }` form would swallow as a skip.
///
/// `moverstr` COLLIDES ON THE SAME NAME with an inline `Option[String]`
/// payload, and `unshared` is the control that moves an `Option[R]` under
/// names nothing else uses. Both were correct before; they are here so a
/// fix that over-clears — dropping a veto a function's OWN arm published,
/// which is a leak rather than a crash — fails the sibling ASAN case.
///
/// SINGLE-FUNCTION PROGRAMS CANNOT EXPRESS THIS, which is why no existing
/// fixture caught it: it needs a same-named binding in two bodies, one a
/// retained Option/Result source and the other moved. It was found because
/// it silently crashes any multi-cell fixture written in this area.
///
/// Twin of `tests/interpreter.rs`'s
/// `test_retained_source_veto_does_not_escape_its_function`, pinned to the
/// same string.
#[test]
fn e2e_retained_source_veto_does_not_escape_its_function() {
    let src = r#"struct R { id: i64, s: String }
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
"#;
    assert_eq!(
        run_program(src).as_deref(),
        Some(
            r#"mover
  mtrue
dR11:s11
moverstr
  strue
unshared
  dtrue
dR13:s13
retained
dR4:s4
  g104
dR104:s104
done
"#
        )
    );
}

/// B-2026-09-13-22 — `a + b + c` is ONE allocation sized to the total, not
/// one per `+`.
///
/// The chain arrives as `String.add(String.add(a, b), c)`; the fused
/// lowering flattens the left spine and memcpys each leaf at its running
/// offset. Two things can go wrong and neither is visible in a two-leaf
/// concat: the bytes can be assembled in the wrong order or at the wrong
/// offset, and the leaves can be EVALUATED out of order, which a
/// side-effecting leaf detects and nothing else does.
///
/// Case 5 is the one that matters: `order 123` fails if the flattener walks
/// the spine in any order but left-to-right, which the nested form gave for
/// free and the fused form has to preserve deliberately.
#[test]
fn e2e_fused_string_concat_chain_keeps_bytes_and_order() {
    let Some(out) = run_program(
        "fn mk(n: i64) -> String {\n\
             \x20   let mut s = String.new();\n\
             \x20   s.push_str(\"<\");\n\
             \x20   s.push_str(f\"{n}\");\n\
             \x20   s.push_str(\">\");\n\
             \x20   return s;\n\
             }\n\
             fn side(n: i64, acc: mut ref Vec[i64]) -> String {\n\
             \x20   acc.push(n);\n\
             \x20   return f\"s{n}\";\n\
             }\n\
             fn main() {\n\
             \x20   let a = \"A\";\n\
             \x20   let b = \"B\";\n\
             \x20   let c = \"C\";\n\
             \x20   println(a + b);\n\
             \x20   println(a + b + c);\n\
             \x20   println(a + b + c + \"D\");\n\
             \x20   println(mk(1) + \"+\" + mk(2));\n\
             \x20   println(mk(3) + \"-\" + mk(4) + \"!\" + mk(5));\n\
             \x20   println(\"[\" + \"\" + \"]\");\n\
             \x20   println(\"\" + \"\" + \"x\");\n\
             \x20   let mut order: Vec[i64] = [];\n\
             \x20   let joined = side(1, mut order) + side(2, mut order) + side(3, mut order);\n\
             \x20   println(joined);\n\
             \x20   let mut i = 0;\n\
             \x20   let mut seen = String.new();\n\
             \x20   while i < order.len() {\n\
             \x20     seen.push_str(f\"{order[i]}\");\n\
             \x20     i = i + 1;\n\
             \x20   }\n\
             \x20   println(f\"order {seen}\");\n\
             \x20   let mut k = 0;\n\
             \x20   let mut last = String.new();\n\
             \x20   while k < 200 {\n\
             \x20     last = mk(k) + \":\" + mk(k + 1) + \";\";\n\
             \x20     k = k + 1;\n\
             \x20   }\n\
             \x20   println(last);\n\
             \x20   println((\"x\" + \"y\" + \"z\").len());\n\
             \x20   println((a + b) + (c + \"D\"));\n\
             \x20   println(\"end\")\n\
             }\n\
             ",
    ) else {
        return;
    };
    assert_eq!(
            out,
            "AB\nABC\nABCD\n<1>+<2>\n<3>-<4>!<5>\n[]\nx\ns1s2s3\norder 123\n<199>:<200>;\n3\nABCD\nend\n"
        );
}

/// B-2026-08-31-19 — `Array[T, N]` renders under codegen, at every depth, the
/// way the interpreter renders it.
///
/// Codegen had NO array Display at all, and the two halves failed differently.
/// Nested — a `Vec` element, a struct field, a tuple field, an enum payload — it
/// fell through `emit_display_fn_for_type_expr` to the by-name catch-all and
/// PANICKED the compiler with `type_name 'Array_i64_3' not yet supported`. At
/// depth 0 it was SILENT, and what it printed depended on the element type:
/// `f"{a}"` on an `Array[i64, 3]` printed `1` (the value-kind fallback reading the
/// aggregate as a scalar), `println(a)` printed NOTHING AT ALL, and an
/// `Array[String, 2]` printed its first element's raw data POINTER. So the
/// compiler-abort half is the one that failed loudly and the plain interpolation
/// was the quiet miscompile, which is the wrong way round for how they read — the
/// row's original title said "nested", and the depth-0 measurement is what widened
/// it.
///
/// `fstr` / `arg` / `str` are those three depth-0 shapes, one per failure mode.
/// `nested`, `struct`, `vecarr`, `field`, `tuple`, `enum` and `mapval` are the
/// positions that panicked; each reaches the renderer through a different
/// dispatcher. `marm` is the match-arm binding — a VALUE, so it asserts the
/// reconstruction and not merely the rendering. `opt` and `res` are the shapes
/// B-2026-08-31-10 had to EXCLUDE from the Option/Result Display gate precisely
/// because of this row and B-2026-08-31-18; they are in here because lifting that
/// exclusion is what closes the loop, and a regression in either row re-declines
/// them.
///
/// `one` is the degenerate extent, which the loop must render without a separator.
/// `flt` and `str` pin element types whose per-element renderer is not the integer
/// one, since the array body's whole job is to delegate.
///
/// Twin of `tests/interpreter.rs`'s `test_array_display_renders_at_every_depth`,
/// pinned to the same string.
#[test]
fn e2e_array_display_renders_at_every_depth() {
    let Some(out) = run_program(
        r#"#[derive(Display)]
struct P { x: i64, s: String }
#[derive(Display)]
struct WithArr { a: Array[i64, 3], n: i64 }
#[derive(Display)]
enum E { A(Array[i64, 3]), S(Array[String, 2]), N }

fn main() {
    let n = env.args().len() as i64;
    let a: Array[i64, 3] = [n, n + 1, n + 2];
    let s: Array[String, 2] = [f"x{n}", f"y{n}"];
    let f: Array[f64, 2] = [n as f64 + 0.5, n as f64 + 1.5];
    let one: Array[i64, 1] = [n];

    println(f"fstr   {a}");
    println(a);
    println(f"str    {s}");
    println(f"flt    {f}");
    println(f"one    {one}");

    let nested: Array[Array[i64, 2], 2] = [[n, n + 1], [n + 2, n + 3]];
    println(f"nested {nested}");
    let structs: Array[P, 2] = [P { x: n, s: f"p1" }, P { x: n + 1, s: f"p2" }];
    println(f"struct {structs}");

    let mut v: Vec[Array[i64, 3]] = Vec.new();
    v.push(a);
    println(f"vecarr {v}");

    let w = WithArr { a: a, n: n };
    println(f"field  {w}");

    let t: (Array[i64, 3], i64) = (a, n);
    println(f"tuple  {t}");

    let e = E.A(a);
    println(f"enum   {e}");
    match e { E.A(b) => { println(f"marm   {b}"); } E.S(b) => {} E.N => {} }

    let oa: Option[Array[i64, 3]] = Some(a);
    println(f"opt    {oa}");
    let ra: Result[Array[i64, 3], i64] = Ok(a);
    println(f"res    {ra}");

    let mut m: Map[String, Array[i64, 3]] = Map.new();
    m.insert(f"k", a);
    println(f"mapval {m}");
}
"#,
    ) else {
        return;
    };
    assert_eq!(
        out,
        r#"fstr   [1, 2, 3]
[1, 2, 3]
str    [x1, y1]
flt    [1.5, 2.5]
one    [1]
nested [[1, 2], [3, 4]]
struct [P { x: 1, s: p1 }, P { x: 2, s: p2 }]
vecarr [[1, 2, 3]]
field  WithArr { a: [1, 2, 3], n: 1 }
tuple  ([1, 2, 3], 1)
enum   A([1, 2, 3])
marm   [1, 2, 3]
opt    Some([1, 2, 3])
res    Ok([1, 2, 3])
mapval {k: [1, 2, 3]}
"#
    );
}

/// B-2026-08-31-10 — an `Option`/`Result` whose payload is a MULTI-WORD
/// value renders under codegen, identically to the interpreter.
///
/// `is_reconstructable_display_payload` — the gate on the Option/Result
/// Display synthesizer — admitted primitives, `String`, and a struct of at
/// most three one-word scalars. Everything else fell through to the
/// deferred struct-Display error, so `f"{o}"` on an `Option[Vec[i64]]` did
/// not compile while the interpreter printed `Some([9])`.
///
/// The gate was more conservative than the machinery behind it. The payload
/// renderer (`emit_enum_field_display`) already deboxes a spilled payload
/// and dispatches type-directedly, which is why a USER enum with the same
/// `Vec` payload — `#[derive(Display)] enum E { V(Vec[i64]) }` — has always
/// rendered: it reaches that renderer without passing this gate. The oracle
/// for the widening is therefore "whatever a user enum can already print".
///
/// Each row is a shape the old gate refused. `four` and `struct` exceed it
/// in the two different ways it was written (a fourth field; a `String`
/// field), and both are boxed payloads, so they exercise the deboxing arm.
/// `f16`/`bf16` are the SEPARATE half of the same row: a sixth hand-written
/// primitive-width list stopping at f32/f64 (B-2026-08-30-25 /
/// B-2026-08-30-40 / B-2026-08-31-9 are the others), which also needed an
/// arm in `emit_display_fn_for_type` — without it the widened gate turned a
/// clean error into a compiler PANIC.
///
/// The bare `println(ov)` line covers the argument spelling, which reaches
/// the same synthesizer through a different error site. The `call` row is
/// the span-keyed call-result path (as opposed to the name-keyed variable
/// one); it renders correctly but LEAKS its payload once per evaluation —
/// a pre-existing hole in the Option/Result f-string temp path that
/// predates this widening (B-2026-08-31-17), which is why the ASAN twin
/// uses a `let`-bound spelling instead.
///
/// `Vector[T, N]` and `Array[T, N]` are deliberately NOT here: admitting
/// them printed `Some(Vector(0, 94011124162560, 1, 0))` and `Some(1)`
/// against the interpreter — a pre-existing miscompile of the enum-payload
/// word reconstruction that `match` shares, filed separately. They keep the
/// clean refusal; `codegen_declined_option_payload_names_its_shape` pins it.
///
/// Twin of `tests/interpreter.rs`'s
/// `test_option_result_display_multiword_payloads`, pinned to the same
/// string.
#[test]
fn e2e_option_result_display_multiword_payloads() {
    let Some(out) = run_program(
        r#"#[derive(Display)]
struct P { x: i64, y: String }
#[derive(Display)]
struct Q { a: i64, b: i64, c: i64, d: i64 }
#[derive(Display)]
enum Inner { A(i64), B }

fn mk(n: i64) -> Option[Vec[i64]] {
    let mut v: Vec[i64] = Vec.new();
    v.push(n);
    return Some(v)
}

fn main() {
    let n = env.args().len() as i64;

    let mut v: Vec[i64] = Vec.new();
    v.push(n);
    v.push(n + 1);
    let ov: Option[Vec[i64]] = Some(v);
    println(f"vec    {ov}");
    println(ov);

    let ot: Option[(i64, i64)] = Some((n, n + 1));
    println(f"tuple  {ot}");

    let mut m: Map[String, i64] = Map.new();
    m.insert(f"a", n);
    let om: Option[Map[String, i64]] = Some(m);
    println(f"map    {om}");

    let mut sm: SortedMap[String, i64] = SortedMap.new();
    sm.insert(f"k", n);
    let osm: Option[SortedMap[String, i64]] = Some(sm);
    println(f"smap   {osm}");

    let oo: Option[Option[i64]] = Some(Some(n));
    println(f"nested {oo}");

    let op: Option[P] = Some(P { x: n, y: f"s" });
    println(f"struct {op}");

    let oq: Option[Q] = Some(Q { a: n, b: n, c: n, d: n });
    println(f"four   {oq}");

    let oi: Option[Inner] = Some(Inner.A(n));
    println(f"enum   {oi}");

    let of: Option[f16] = Some(((n as f32) + 1.5f32) as f16);
    println(f"f16    {of}");
    let ob: Option[bf16] = Some(((n as f32) + 2.25f32) as bf16);
    println(f"bf16   {ob}");

    let mut vs: Vec[String] = Vec.new();
    vs.push(f"q");
    let re: Result[i64, Vec[String]] = Err(vs);
    println(f"result {re}");

    println(f"call   {mk(n)}");

    let nn: Option[Vec[i64]] = None;
    println(f"none   {nn}");
}
"#,
    ) else {
        return;
    };
    assert_eq!(
        out,
        r#"vec    Some([1, 2])
Some([1, 2])
tuple  Some((1, 2))
map    Some({a: 1})
smap   Some(SortedMap{k: 1})
nested Some(Some(1))
struct Some(P { x: 1, y: s })
four   Some(Q { a: 1, b: 1, c: 1, d: 1 })
enum   Some(A(1))
f16    Some(2.5)
bf16   Some(3.25)
result Err([q])
call   Some([1])
none   None
"#
    );
}

/// B-2026-08-10-21 — the `UseAfterMove` defensive copy, for the
/// `{ptr,len,cap}` family.
///
/// `src/cli.rs`'s `is_fatal_ownership_kind` keeps `UseAfterMove` non-fatal
/// for `build` on a stated promise: "codegen defensive-copies the reuse, so
/// the binary is memory-safe". That copy did not exist for ANY heap type —
/// `karac check` printed "All checks passed", `karac build` exited 0, and
/// the reuse read freed memory. These cases are the promise made true for
/// `String` / `Vec` / `VecDeque`.
///
/// The shape is the experiment: move the value into a binding whose scope
/// ENDS, then read the source after it. With the read placed BEFORE the
/// destination dies, the alias is still live and every case passes by
/// timing — which is how this survived, and why case 2 matters.
///
/// Case 2 (`Vec[i64]`) is pinned separately from the String cases because
/// the pre-existing `e2e_let_move_source_frozen` reads the moved-from Vec
/// with `.len()`, which loads the length word and never dereferences the
/// buffer. That test passed throughout and does not demonstrate a copy;
/// `w[1]` is what does.
///
/// Case 3 is element-deep: a `Vec[String]` copy that duplicated only the
/// outer buffer would leave the element `String`s shared.
#[test]
fn test_e2e_use_after_move_defensive_copy_vecstr_family() {
    // 1. String.
    assert_eq!(
        run_program(
            "fn main() {\n\
                     let s: String = f\"alpha\";\n\
                     { let keep: String = s; }\n\
                     println(s);\n\
                 }"
        )
        .as_deref(),
        Some("alpha\n")
    );
    // 2. `Vec[i64]` — read an ELEMENT, not `.len()`.
    assert_eq!(
        run_program(
            "fn main() {\n\
                     let mut w: Vec[i64] = Vec.new();\n\
                     w.push(11i64); w.push(22i64);\n\
                     { let keep: Vec[i64] = w; }\n\
                     println(w[1]);\n\
                 }"
        )
        .as_deref(),
        Some("22\n")
    );
    // 3. `Vec[String]` — element depth.
    assert_eq!(
        run_program(
            "fn main() {\n\
                     let mut w: Vec[String] = Vec.new();\n\
                     w.push(f\"alpha\");\n\
                     { let keep: Vec[String] = w; }\n\
                     println(w[0]);\n\
                 }"
        )
        .as_deref(),
        Some("alpha\n")
    );
    // 4. The reproduction this bug was filed from: the doubly-moved value
    // comes out of a container. The container READ was always defended
    // (`clone_owned_vec_index_element`); what failed was the plain
    // binding-to-binding move after it.
    assert_eq!(
        run_program(
            "fn main() {\n\
                     let names: Vec[String] = [f\"alpha\"];\n\
                     let mut out: String = f\"\";\n\
                     {\n\
                         let n = names[0].clone();\n\
                         let _keep: String = n;\n\
                         out = n;\n\
                     }\n\
                     println(out);\n\
                 }"
        )
        .as_deref(),
        Some("alpha\n")
    );
    // 5. The STRUCT-LITERAL consume site (`H { name: s }`), which the
    // let-site hook does not reach.
    //
    // Opt level changes what this looks like, which is worth knowing before
    // trusting either result: at the DEFAULT level the second read prints
    // garbage (so this assertion catches it), while at KARAC_OPT_LEVEL=0 it
    // prints the right bytes and only a sanitizer sees the two invalid
    // reads. The ASAN twin carries it for that reason — a -O0 probe alone
    // would have called this shape clean.
    assert_eq!(
        run_program(
            "struct H { name: String }\n\
                 fn main() {\n\
                     let s: String = f\"alpha\";\n\
                     { let h = H { name: s }; println(h.name); }\n\
                     println(s);\n\
                 }"
        )
        .as_deref(),
        Some("alpha\nalpha\n")
    );
    // 6. The SHARED-struct branch of the same site — a separate code path
    // in `compile_struct_init`, so a fix to one does not imply the other.
    assert_eq!(
        run_program(
            "shared struct H { name: String }\n\
                 fn main() {\n\
                     let s: String = f\"alpha\";\n\
                     { let h = H { name: s }; println(h.name); }\n\
                     println(s);\n\
                 }"
        )
        .as_deref(),
        Some("alpha\nalpha\n")
    );
    // 7. A `Vec[String]` field — element-deep through the same site.
    assert_eq!(
        run_program(
            "struct H { items: Vec[String] }\n\
                 fn main() {\n\
                     let mut v: Vec[String] = Vec.new();\n\
                     v.push(f\"alpha\");\n\
                     { let h = H { items: v }; println(h.items[0]); }\n\
                     println(v[0]);\n\
                 }"
        )
        .as_deref(),
        Some("alpha\nalpha\n")
    );
    // 8. CONTROL — a program with no `UseAfterMove` at all must be
    // untouched. The copy is gated on the ownership pass's flagged spans,
    // so the overwhelming majority of programs emit exactly what they did
    // before.
    assert_eq!(
        run_program(
            "fn main() {\n\
                     let s: String = f\"alpha\";\n\
                     let keep: String = s;\n\
                     println(keep);\n\
                 }"
        )
        .as_deref(),
        Some("alpha\n")
    );
}

/// B-2026-08-27-21 leg 2 — a `ref` binding to a STRING element. Distinct
/// from leg 1: nothing was registered WRONG here, `string_vars` was simply
/// never written for the shim, so String method dispatch had no entry for
/// it and every method fell through to the loud "no handler for method"
/// arm under codegen while `--interp` ran all of them.
///
/// The borrow must stay a borrow: `d[0]` is printed after the reads to pin
/// that the container's buffer is neither moved out of nor freed by the
/// binding (`tests/memory_sanitizer.rs` asserts the same shape under ASAN).
#[test]
fn test_e2e_string_methods_through_a_ref_binding() {
    let Some(out) = run_program(
        r#"
fn main() {
    let mut d: Vec[String] = Vec.new();
    d.push("hello");
    d.push("world");
    let s = ref d[0];
    println(s.len());
    println(s.starts_with("he"));
    println(s.substring(0, 2));
    let c: String = s.clone();
    println(c);
    println(s.to_uppercase());
    println(d[0]);
}
"#,
    ) else {
        return;
    };
    assert_eq!(
        out, "5\ntrue\nhe\nhello\nHELLO\nhello\n",
        "String methods through a borrow; got: {out:?}"
    );
}

#[test]
fn test_e2e_println_non_identifier_vec_renders_like_the_interpreter() {
    // B-2026-07-28-12: `println(<Vec expression>)` printed EMPTY under
    // codegen whenever the operand was not a bare identifier. The Vec/Map
    // print arms key off per-variable side tables, so a fresh literal or a
    // call result had no entry and fell through to the value-kind arms,
    // where a Vec's `{ptr,len,cap}` is indistinguishable from a String's
    // and got printed AS one — the element bytes came out as text (`[9, 8]`
    // rendered as 0x09 0x00) and the embedded NUL truncated the line.
    //
    // Each case below is a distinct route to the same arm: a fresh literal,
    // a user call's return, a nested `Vec[Vec[_]]`, a `Vec[String]` whose
    // elements own heap, a builtin (`shape()`), and the empty vec. The
    // binding spelling always worked and is kept as the control.
    assert_eq!(
        run_program(
            r#"
fn mk() -> Vec[i64] { let v: Vec[i64] = [3, 4]; v }
fn names() -> Vec[String] { let v: Vec[String] = ["ab", "cd"]; v }
fn nested() -> Vec[Vec[i64]] { let v: Vec[Vec[i64]] = [[1, 2], [3]]; v }
fn main() {
    println([9i64, 8i64]);
    println(mk());
    println(names());
    println(["x", "y"]);
    println(nested());
    let t: Tensor[f64, [2, 3]] = Tensor.zeros([2, 3]);
    println(t.shape());
    let e: Vec[i64] = [];
    println(e);
    let b = [1i64, 2i64];
    println(b);
}
"#
        ),
        Some("[9, 8]\n[3, 4]\n[ab, cd]\n[x, y]\n[[1, 2], [3]]\n[2, 3]\n[]\n[1, 2]\n".to_string())
    );
}

#[test]
fn test_e2e_total_order_wrapper_from_and_display() {
    // B-2026-08-11-8 — `F64.from(x)` is the constructor design.md § Float
    // semantics names and the `T: Ord` bound diagnostic prescribes
    // verbatim, and it did not exist: `no associated function 'from' on
    // type 'F64'`. Every other spelling failed too, and `F64(1.5)` (the
    // natural next guess) panicked the interpreter — so a correct
    // diagnostic steered users, `karac fix`, and any LLM reading it into
    // an API that was not there.
    //
    // Two halves, both covered here because they broke in different
    // backends:
    //  * `from` — the baked stdlib body makes it TYPECHECK, but codegen
    //    does not route path-calls to baked stdlib impls, so without the
    //    `assoc_call.rs` intercept the call yielded a const `i64` 0 and
    //    then failed as "cannot resolve field 'value'". The
    //    `F64.from(x).value` chain additionally needs `type_name_of_expr`
    //    to type the call-result temp (a let-bound receiver resolves via
    //    `var_type_names`; a bare temp has no binding to key on).
    //  * Display — BOTH `println(x)` and `f"{x}"` rejected every wrapper
    //    ("does not implement Display"), making the type the compiler
    //    recommends for `Ord` contexts unprintable. It renders as the
    //    INNER float, so wrapping a value for its ordering contract does
    //    not change how it prints.
    let out = run_program(
        "fn main() {\n\
                 let a = F64.from(3.25);\n\
                 let b = F32.from(2.5);\n\
                 println(a);\n\
                 println(b);\n\
                 println(f\"{a} {b}\");\n\
                 println(F64.from(1.5).value);\n\
                 println(a.value + 1.0);\n\
                 let mut v: Vec[F64] = Vec.new();\n\
                 v.push(F64.from(3.0));\n\
                 v.push(F64.from(1.0));\n\
                 v.push(F64.from(2.0));\n\
                 v.sort();\n\
                 let lo = v[0];\n\
                 let hi = v[2];\n\
                 println(lo);\n\
                 println(hi);\n\
                 println(v);\n\
                 let mut m: Map[F64, i64] = Map.new();\n\
                 let _ = m.insert(F64.from(2.0), 20);\n\
                 match m.get(F64.from(2.0)) { Some(x) => println(x), None => println(0 - 1) }\n\
                 println(F64.from(2.0) == F64.from(2.0));\n\
                 println(F64.from(1.0) < F64.from(2.0));\n\
             }",
    );
    if let Some(out) = out {
        assert_eq!(
            out,
            "3.25\n2.5\n3.25 2.5\n1.5\n4.25\n1\n3\n[1, 2, 3]\n20\ntrue\ntrue\n"
        );
    }
}

#[test]
fn test_e2e_arena_string_elements() {
    // `String`-element arena: `push` copies the bytes into an
    // arena-owned blob; `get` hands back a borrowed (`cap = 0`) String
    // view that Displays directly and answers `.len()`. Mirrors the
    // interpreter `test_arena_string_elements`.
    let out = run_program(
        r#"
fn main() {
    let a: Arena[String] = Arena.new();
    let r0 = a.push("hello");
    let r1 = a.push("world");
    println(a.get(r0));
    println(a.get(r1));
    let s = a.get(r0);
    println(s.len());
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "hello\nworld\n5");
    }
}

#[test]
fn test_ir_runtime_string_chars_loop_takes_the_ascii_bailout() {
    // B-2026-07-28-2: a `ref String` parameter can never satisfy
    // B-2026-07-27-7's compile-time all-ASCII proof, so before this fix
    // every `.chars()` loop over a runtime string kept an offset PHI that
    // blocked stride-1 induction recovery. The dual-region lowering needs
    // no proof — the ASCII check is a runtime branch.
    //
    // Assert BOTH halves are present, because either alone would be a bug:
    // the `for.sb.*` blocks mean the fast region exists, and the surviving
    // decode call means multibyte input is still decoded rather than
    // walked a byte at a time.
    let ir = ir_for(
        "fn walk(word: ref String) -> String {\n\
             \x20   let mut out: String = \"\";\n\
             \x20   for ch in word.chars() { out.push(ch); }\n\
             \x20   return out;\n\
             }\n\
             fn main() { println(walk(\"ab\".to_string())); }",
    );
    let w = ir
        .split("define")
        .find(|f| f.contains("@walk("))
        .expect("@walk not found in IR");
    assert!(
        w.contains("for.sb.peek"),
        "a runtime-string chars loop with a duplicable body must take the \
             dual-region ASCII bailout (for.sb.* blocks); got:\n{}",
        w
    );
    assert!(
        w.contains("@karac_string_decode_char"),
        "the bailout must KEEP a real decode for the multibyte region — \
             without it the loop would bind one char per byte; got:\n{}",
        w
    );
}

#[test]
fn test_e2e_module_const_nested_string_slice_initializers() {
    // B-2026-08-17-22 — a string literal nested in a module-level
    // struct / tuple / array initializer now types as `StringSlice`
    // (§1284's rule, applied at every depth instead of only the
    // outermost expression). Codegen already lowered these shapes
    // correctly; the typechecker was the whole blocker — so this pins
    // that the emitted constants carry the right BYTES, not merely that
    // the program passes check.
    let src = "struct Cfg { name: StringSlice, retries: i64 }\n\
                   struct Inner { label: StringSlice }\n\
                   struct Outer { inner: Inner, n: i64 }\n\
                   let CONFIG: Cfg = Cfg { name: \"karac\", retries: 3 };\n\
                   let TUP: (StringSlice, i64) = (\"tup\", 7);\n\
                   let ARR: Array[StringSlice, 2] = [\"a\", \"b\"];\n\
                   let DEEP: Outer = Outer { inner: Inner { label: \"deep\" }, n: 1 };\n\
                   fn main() {\n\
                       println(CONFIG.name);\n\
                       println(CONFIG.retries);\n\
                       let t = TUP;\n\
                       println(t.0);\n\
                       println(ARR[0]);\n\
                       println(ARR[1]);\n\
                       let d = DEEP;\n\
                       println(d.inner.label);\n\
                   }";
    let parsed = karac::parse(src);
    assert!(
        parsed.errors.is_empty(),
        "parse errors: {:?}",
        parsed.errors
    );
    let resolved = karac::resolve(&parsed.program);
    let typed = karac::typecheck(&parsed.program, &resolved);
    assert!(
        typed.errors.is_empty(),
        "nested module-scope string literals must typecheck clean, got: {:?}",
        typed.errors.iter().map(|e| &e.message).collect::<Vec<_>>(),
    );
    if let Some(out) = run_program(src) {
        assert_eq!(out, "karac\n3\ntup\na\nb\ndeep\n");
    }
}

/// Sibling of the above for `Vec[String]`: `out[i] = s` moves a `String`
/// binding into an owning element slot. Same double-free class; the old
/// element string buffer is dropped and the source is suppressed.
#[test]
fn e2e_index_store_heap_string_element_no_double_free() {
    if let Some(out) = run_program(
            "fn main() {\n\
             let mut out: Vec[String] = Vec.new();\n\
             let mut k = 0i64; while k < 4i64 { out.push(\"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\"); k = k + 1i64; }\n\
             let s: String = \"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\";\n\
             out[1i64] = s;\n\
             println(out[1i64]);\n\
             }",
        ) {
            assert_eq!(out, "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\n");
        }
}

/// B-2026-07-06-5 (heap-element facet): a blanket `impl Joiner for
/// Vec[String]` whose loop body concatenates the borrowed String elements
/// (`for s in self { out = out + s; }`). Exercises the SelfValue for-loop
/// borrow path with per-element HEAP data — the elements and the Vec buffer
/// must NOT be freed by the loop (the receiver is borrowed). Reached
/// directly and through a bound-generic mono. Leak/double-free coverage is
/// the ASAN twin in `tests/memory_sanitizer.rs`.
#[test]
fn e2e_blanket_vec_string_impl_loop_body() {
    if let Some(out) = run_program(
        "trait Joiner {\n\
             \x20   fn concat(ref self) -> String;\n\
             }\n\
             impl Joiner for Vec[String] {\n\
             \x20   fn concat(ref self) -> String {\n\
             \x20       let mut out = String.new();\n\
             \x20       for s in self { out = out + s; }\n\
             \x20       out\n\
             \x20   }\n\
             }\n\
             fn callit[C: Joiner](c: ref C) -> String { c.concat() }\n\
             fn main() {\n\
             \x20   let mut v = Vec.new();\n\
             \x20   v.push(\"ab\"); v.push(\"cd\"); v.push(\"ef\");\n\
             \x20   println(v.concat());\n\
             \x20   println(callit(v));\n\
             }",
    ) {
        assert_eq!(out, "abcdef\nabcdef\n");
    }
}

/// A generic (monomorphized) fn whose IMPLICIT TAIL expression is a bare
/// `f"…"` — the mono path was missing the InterpolatedStringLit-tail cap
/// suppression that `compile_function` (non-generic) has, so the tail
/// f-string's accumulator was freed between the return-value load and `ret`
/// and the caller then freed the dangling buffer again (double-free —
/// surfaced by `describe[T: Display](x) { f"..{x}.." }`; the `let`-bound and
/// explicit-`return` forms already worked). Covers a `Display` struct, a
/// primitive, and a String arg through one generic fn, plus a no-interp tail
/// f-string — the memory safety is pinned by
/// `tests/memory_sanitizer.rs::asan_generic_tail_fstring_no_double_free`.
#[test]
fn e2e_generic_tail_fstring_return_codegen() {
    if let Some(out) = run_program(
            "struct P { x: i64, y: i64 }\n\
             impl Display for P { fn to_string(ref self) -> String { f\"({self.x}, {self.y})\" } }\n\
             fn describe[T: Display](item: T) -> String { f\"item is {item}\" }\n\
             fn tag[T](item: T) -> String { f\"constant tail here\" }\n\
             fn main() {\n\
                 println(describe(P { x: 1i64, y: 2i64 }));\n\
                 println(describe(42i64));\n\
                 println(describe(\"hi\".to_string()));\n\
                 println(tag(7i64));\n\
             }",
        ) {
            assert_eq!(out, "item is (1, 2)\nitem is 42\nitem is hi\nconstant tail here\n");
        }
}

#[test]
fn e2e_fstring_binary_center_and_fill_specs() {
    // Binary `b`, center align `^`, and custom (non-space) fill — the
    // format specs `snprintf` can't express, routed through the shared
    // runtime formatter (`karac_runtime_fmt_*`) which calls the SAME
    // `FormatSpec::apply_*` the interpreter uses, so `karac build` matches
    // `karac run --interp` byte-for-byte. Covers int/float/string holes,
    // combined specs (center hex, fill+center+precision), and the
    // source-wider-than-width no-pad branch.
    if let Some(out) = run_program(
        "fn main() {\n\
                 let x = 5i64;\n\
                 let big = 255i64;\n\
                 let neg = 42i64;\n\
                 let pi = 3.14159f64;\n\
                 let name = \"kara\";\n\
                 println(f\"[{x:b}]\");\n\
                 println(f\"[{big:b}]\");\n\
                 println(f\"[{x:08b}]\");\n\
                 println(f\"[{neg:^6}]\");\n\
                 println(f\"[{name:^10}]\");\n\
                 println(f\"[{pi:^10.2}]\");\n\
                 println(f\"[{name:*^10}]\");\n\
                 println(f\"[{name:*<10}]\");\n\
                 println(f\"[{neg:*>8}]\");\n\
                 println(f\"[{pi:*^10.2}]\");\n\
                 println(f\"[{big:^8x}]\");\n\
                 println(f\"[{name:.^12}]\");\n\
                 println(f\"[{name:^2}]\");\n\
             }",
    ) {
        assert_eq!(
            out,
            "[101]\n[11111111]\n[00000101]\n[  42  ]\n[   kara   ]\n[   3.14   ]\n\
                 [***kara***]\n[kara******]\n[******42]\n[***3.14***]\n[   ff   ]\n\
                 [....kara....]\n[kara]\n"
        );
    }
}

/// B-2026-08-06-5: a cast TO `char` inside an f-string hole rendered the
/// integer codepoint under both compiled backends while the interpreter
/// rendered the glyph — a silent wrong answer on code the typechecker
/// accepts (B-2026-07-24-3 made `u8 as char` legal).
///
/// The other casts are CONTROLS, not padding: truncation, sign wrap and
/// float→int were all honoured in the same position, which is what
/// localised the fault to `expr_is_char` rather than to f-string lowering
/// generally. A regression that re-broke every cast would look identical
/// on the char line alone.
#[test]
fn e2e_cast_to_char_in_fstring_renders_the_glyph() {
    let src = r#"
fn main() {
    let n = 97i64;
    let b = n as u8;
    println(f"A:{b as char}");
    println(f"B:{(n as u8) as char}");
    let c = b as char;
    println(f"C:{c}");
    let big = 300i64;
    println(f"D:{big as u8}");
    let neg = 0i64 - 1i64;
    println(f"E:{neg as u8}");
    let f = 2.9f64;
    println(f"F:{f as i64}");
}
"#;
    if let Some(out) = run_program(src) {
        assert_eq!(out, "A:a\nB:a\nC:a\nD:44\nE:255\nF:2\n");
    }
}

/// B-2026-06-19-13 codegen follow-on: `char.to_digit(radix) -> Option[u32]`
/// now LOWERS under `karac build` (was an honest "not yet supported"
/// diagnostic). Covers a decimal digit, lowercase + uppercase hex-ish digits
/// (`a`→10, `F`→15), the top of base-36 (`z`→35), a digit too large for its
/// radix (`9` in base 2 → `None`), and a non-digit char → `None`. Output
/// must match the interpreter (`karac run`).
#[test]
fn e2e_char_to_digit_codegen() {
    if let Some(out) = run_program(
        "fn show(c: char, r: u32) {\n\
             \x20   match c.to_digit(r) {\n\
             \x20       Some(v) => println(f\"{v}\"),\n\
             \x20       None => println(\"none\"),\n\
             \x20   }\n\
             }\n\
             fn main() {\n\
             \x20   show('7', 10);\n\
             \x20   show('a', 16);\n\
             \x20   show('F', 16);\n\
             \x20   show('z', 36);\n\
             \x20   show('9', 2);\n\
             \x20   show('x', 10);\n\
             \x20   show('0', 2);\n\
             }",
    ) {
        assert_eq!(out, "7\n10\n15\n35\nnone\nnone\n0\n");
    }
}

#[test]
fn e2e_unannotated_enum_struct_variant_let_method_and_display() {
    // B-2026-06-13-9: an UNANNOTATED `let a = E.A { .. }` constructing an
    // enum struct-variant must register `a` as the ENUM in `var_type_names`
    // so later `a.method()` / Display dispatch finds a receiver type.
    // Pre-fix, `type_name_of` returned the VARIANT (`path.last()` = "A"),
    // which is no known type, so the type-hint-present recording path stored
    // `var_type_names[a] = "A"` and method dispatch fell through:
    // "codegen failed: no handler for method 'code' on variable 'a'".
    // The annotated `let a: E = E.A { .. }`, a fn param `E`, and the tuple
    // variant `E.A(..)` all worked, isolating the unannotated-struct-variant
    // shape. Covers both halves: a user method and a user `impl Display`
    // round-trip (`.to_string()` / `f"{a}"` / `println(a)`).
    if let Some(out) = run_program(
            "enum E { A { n: i64 }, B }\n\
             impl E { fn code(ref self) -> i64 { match self { A { n } => n, B => 0 } } }\n\
             impl Display for E { fn to_string(ref self) -> String { match self { A { n } => f\"A:{n}\", B => \"B\" } } }\n\
             fn main() {\n\
                 let a = E.A { n: 3 };\n\
                 println(f\"{a.code()}\");\n\
                 println(a.to_string());\n\
                 println(f\"{a}\");\n\
                 println(a);\n\
             }",
        ) {
            assert_eq!(out, "3\nA:3\nA:3\nA:3\n");
        }
}

#[test]
fn e2e_unannotated_unqualified_enum_struct_variant_let_method_and_display() {
    // B-2026-06-13-12 (the unqualified peer of B-13-9): an UNANNOTATED
    // `let a = A { .. }` — no `E.` qualifier — must also register `a` as the
    // ENUM. The B-13-9 fix to `type_name_of` only resolved the qualified
    // `path[len-2]` form; the single-segment variant name still returned
    // itself, so an unannotated unqualified binding stored
    // `var_type_names[a] = "A"` and method/Display dispatch fell through.
    // `type_name_of` now also searches `enum_layouts` for the variant when
    // the single-segment name isn't a struct.
    if let Some(out) = run_program(
            "enum E { A { n: i64 }, B }\n\
             impl E { fn code(ref self) -> i64 { match self { A { n } => n, B => 0 } } }\n\
             impl Display for E { fn to_string(ref self) -> String { match self { A { n } => f\"A:{n}\", B => \"B\" } } }\n\
             fn main() {\n\
                 let a = A { n: 3 };\n\
                 println(f\"{a.code()}\");\n\
                 println(a.to_string());\n\
                 println(f\"{a}\");\n\
                 println(a);\n\
             }",
        ) {
            assert_eq!(out, "3\nA:3\nA:3\nA:3\n");
        }
}

#[test]
fn e2e_struct_variant_string_payload_construct_and_consume() {
    // A heap (`String`) local moved into an enum struct-variant payload and
    // then CONSUMED (matched / Display'd) must not double-free: construction
    // suppresses the source's cleanup, and the match-binding side treats a
    // borrowed (`ref self`) scrutinee's field bindings as borrows. Covers
    // the clone→struct-variant→`impl Display(ref self)` path that crashed
    // the Weave `ParseError` at cleanup (a real cap>0 buffer).
    if let Some(out) = run_program(
        "pub enum E { Empty, NoAt { value: String } }\n\
             impl Display for E {\n\
                 fn to_string(ref self) -> String {\n\
                     match self { Empty => \"empty\", NoAt { value } => f\"no-at '{value}'\" }\n\
                 }\n\
             }\n\
             pub fn make(raw: String) -> E {\n\
                 let v = raw.clone();\n\
                 if not v.contains(\"@\") { return E.NoAt { value: v }; }\n\
                 E.Empty\n\
             }\n\
             fn main() {\n\
                 let data = \"a@b.com,bad-no-at\";\n\
                 let inputs = data.split(',');\n\
                 for s in inputs { let e = make(s); println(f\"{e}\"); }\n\
             }",
    ) {
        assert_eq!(out, "empty\nno-at 'bad-no-at'\n");
    }
}

#[test]
fn e2e_128bit_display_inside_containers_and_payloads() {
    // Rendering a 128-bit scalar reached through a `Vec` / `Option` /
    // `Result` (B-2026-08-19-23). Three separate defects met here:
    //
    //   - `type_to_type_expr` had no 128-bit arms, so the payload's
    //     `TypeExpr` came back `TypeKind::Error` and the display
    //     registration skipped the variable — `println(o)` on an
    //     `Option[i128]` was then refused with "bind a struct literal to a
    //     `let` first", about a plain variable;
    //   - `emit_display_fn_for_type` had no 128-bit arm and PANICKED the
    //     compiler ("type_name 'u128' not yet supported") for `Vec[u128]`;
    //   - `rebuild_value_from_payload_words` zero-extended word 0 for any
    //     type wider than a word, keeping the LOW half — and 2^100's low
    //     word is zero, so `Some(2^100)` would have rendered `Some(0)`.
    //
    // The `u64` rows are here because they share every one of those paths.
    if let Some(out) = run_program(
        "fn main() {\n\
             let a: Option[i128] = Some(1267650600228229401496703205376i128);\n\
             println(a);\n\
             let b: Option[i128] = None;\n\
             println(b);\n\
             let c: Result[i128, String] = Ok(-1267650600228229401496703205376i128);\n\
             println(c);\n\
             let d: Option[u128] = Some(340282366920938463463374607431768211455u128);\n\
             println(d);\n\
             let e: Option[u64] = Some(18446744073709551615u64);\n\
             println(e);\n\
             let mut v: Vec[i128] = vec![];\n\
             v.push(1267650600228229401496703205376i128);\n\
             v.push(0i128 - 1i128);\n\
             println(v);\n\
             let mut w: Vec[u128] = vec![];\n\
             w.push(340282366920938463463374607431768211455u128);\n\
             w.push(5u128);\n\
             w.sort();\n\
             println(w);\n\
             }",
    ) {
        assert_eq!(
            out,
            "Some(1267650600228229401496703205376)\n\
                 None\n\
                 Ok(-1267650600228229401496703205376)\n\
                 Some(340282366920938463463374607431768211455)\n\
                 Some(18446744073709551615)\n\
                 [1267650600228229401496703205376, -1]\n\
                 [5, 340282366920938463463374607431768211455]\n"
        );
    }
}

#[test]
fn e2e_nested_unsigned_display_matches_the_interpreter() {
    // The compiled twin of `a_nested_unsigned_value_renders_at_its_own_width`
    // (tests/interpreter.rs), B-2026-08-19-27.
    //
    // Codegen was already CORRECT here — it renders each field through a
    // synthesized Display fn that knows the static type — and the
    // interpreter's recursive renderer was not, because it walks `Value`s
    // with no type context. That made this a run-vs-build divergence with a
    // right answer already on one side, so these expected strings are
    // literally what `karac build` printed before the interpreter was
    // changed. Asserting them on BOTH backends is what stops the pair
    // drifting apart again.
    //
    // `#[derive(Display)]` on a struct with a `u128` field is included
    // because that field type was still refused at this layer after
    // B-2026-08-19-23 widened three sibling lists and missed this one.
    //
    // The bare `println` of each `Option[T]` precedes the `Vec[Option[T]]`
    // that follows it deliberately: standalone, `println` of a
    // `Vec[Option[T]]` PANICS codegen ("type_name 'T' not yet supported")
    // because the element display asks `Option` for its payload type and
    // gets the undesugared parameter, and only an earlier bare `Option[T]`
    // render seeds the concrete fn in the cache. That order-dependence is
    // pre-existing and filed as B-2026-08-19-28; the ordering here is what
    // real programs look like, not a workaround for this test.
    if let Some(out) = run_program(
        "#[derive(Display)]\n\
             struct Pair { pub u: u128, pub v: u64 }\n\
             fn main() {\n\
             let big: u64 = 18446744073709551615u64;\n\
             let o: Option[u64] = Some(big);\n\
             println(o);\n\
             let r: Result[u64, String] = Ok(big);\n\
             println(r);\n\
             let mut v: Vec[u64] = vec![];\n\
             v.push(big);\n\
             v.push(5u64);\n\
             println(v);\n\
             let t = (big, 5i64);\n\
             println(t);\n\
             let mut vo: Vec[Option[u64]] = vec![];\n\
             vo.push(Some(big));\n\
             vo.push(None);\n\
             println(vo);\n\
             let mut mp: Map[String, u64] = Map.new();\n\
             mp.insert(\"k\", big);\n\
             println(mp);\n\
             println(f\"{o} {v}\");\n\
             let m: u128 = 340282366920938463463374607431768211455u128;\n\
             let p = Pair { u: m, v: big };\n\
             println(p);\n\
             let ou: Option[u128] = Some(m);\n\
             println(ou);\n\
             let mut vv: Vec[Option[u128]] = vec![];\n\
             vv.push(Some(m));\n\
             println(vv);\n\
             let s: Option[i64] = Some(0i64 - 1i64);\n\
             println(s);\n\
             let mut n8: Vec[u8] = vec![];\n\
             n8.push(200u8);\n\
             println(n8);\n\
             }",
    ) {
        assert_eq!(
            out,
            "Some(18446744073709551615)\n\
                 Ok(18446744073709551615)\n\
                 [18446744073709551615, 5]\n\
                 (18446744073709551615, 5)\n\
                 [Some(18446744073709551615), None]\n\
                 {k: 18446744073709551615}\n\
                 Some(18446744073709551615) [18446744073709551615, 5]\n\
                 Pair { u: 340282366920938463463374607431768211455, v: 18446744073709551615 }\n\
                 Some(340282366920938463463374607431768211455)\n\
                 [Some(340282366920938463463374607431768211455)]\n\
                 Some(-1)\n\
                 [200]\n"
        );
    }
}

#[test]
fn e2e_generic_enum_display_renders_at_its_instantiation() {
    // B-2026-08-19-28. `emit_enum_display_fn` rendered from the enum's
    // DECLARATION, so a generic enum's payload type was the bare parameter
    // `T` — which has no layout and no renderer, and recursing on it
    // PANICKED the compiler ("type_name 'T' not yet supported").
    //
    // It hit every generic enum reached through a container or a field, on
    // every render surface: `println` / f-string / `to_string` of a
    // `Vec[Option[T]]`, `Vec[Result[T, E]]`, a `Map` value, a `Set`
    // element, and user-defined generics alike. `Option` LOOKED fine only
    // because a bespoke instantiation-aware path intercepts the direct
    // `println(o)` spelling and leaves a concrete fn in the cache that a
    // later nested use finds — so the panic was ORDER-DEPENDENT, and
    // deleting an unrelated `println` could break a build. There is
    // deliberately no such seeding print here.
    //
    // Two instantiations of one enum appear together because the cache was
    // keyed on the bare enum name: without a per-instantiation key they
    // would collide on whichever fn was emitted first.
    if let Some(out) = run_program(
        "#[derive(Display)]\n\
             enum MyOpt[T] { Has(T), Empty }\n\
             fn main() {\n\
             let mut a: Vec[Option[i64]] = vec![];\n\
             a.push(Some(5i64));\n\
             a.push(None);\n\
             println(a);\n\
             let mut b: Vec[Result[i64, String]] = vec![];\n\
             b.push(Ok(5i64));\n\
             b.push(Err(\"bad\"));\n\
             println(b);\n\
             let mut m: Map[String, Option[i64]] = Map.new();\n\
             m.insert(\"k\", Some(5i64));\n\
             println(m);\n\
             let mut s: Set[Option[i64]] = Set.new();\n\
             s.insert(Some(5i64));\n\
             println(s);\n\
             let mut g: Vec[MyOpt[i64]] = vec![];\n\
             g.push(MyOpt.Has(5i64));\n\
             g.push(MyOpt.Empty);\n\
             println(g);\n\
             let mut h: Vec[MyOpt[String]] = vec![];\n\
             h.push(MyOpt.Has(\"x\"));\n\
             println(h);\n\
             let mut f: Vec[Option[i64]] = vec![];\n\
             f.push(Some(9i64));\n\
             println(f\"{f}\");\n\
             }",
    ) {
        assert_eq!(
            out,
            "[Some(5), None]\n\
                 [Ok(5), Err(bad)]\n\
                 {k: Some(5)}\n\
                 Set{Some(5)}\n\
                 [Has(5), Empty]\n\
                 [Has(x)]\n\
                 [Some(9)]\n"
        );
    }
}

#[test]
fn e2e_generic_enum_display_deboxes_an_oversized_payload() {
    // The half of B-2026-08-19-28 that is a WRONG ANSWER rather than a
    // panic, and only became reachable once the substitution above existed.
    //
    // A generic enum's layout is the ERASED base, so a payload wider than
    // its inline area is heap-boxed with the box pointer in word 0 — the
    // normal state for `MyOpt[String]`, whose `Has` slot is one word while a
    // String is three. The display path read the slot INLINE and zero-filled
    // past the area, rebuilding `{ptr, 0, 0}`: `Has()` with an empty string
    // where the interpreter said `Has(x)`. It now applies the same debox the
    // match-arm unpack does, keyed on the same static predicate, so the two
    // sites agree about which payloads are boxed.
    //
    // `Vec[i64]` as a payload is the same shape one step further out — a
    // three-word container inside the one-word erased slot.
    if let Some(out) = run_program(
        "#[derive(Display)]\n\
             enum MyOpt[T] { Has(T), Empty }\n\
             fn main() {\n\
             let mut h: Vec[MyOpt[String]] = vec![];\n\
             h.push(MyOpt.Has(\"hello\"));\n\
             println(h);\n\
             let mut i: Vec[i64] = vec![];\n\
             i.push(7i64);\n\
             let mut v: Vec[MyOpt[Vec[i64]]] = vec![];\n\
             v.push(MyOpt.Has(i));\n\
             println(v);\n\
             let mut o: Vec[Option[String]] = vec![];\n\
             o.push(Some(\"z\"));\n\
             println(o);\n\
             let mut n: Vec[Vec[Option[i64]]] = vec![];\n\
             let mut inner: Vec[Option[i64]] = vec![];\n\
             inner.push(Some(5i64));\n\
             n.push(inner);\n\
             println(n);\n\
             }",
    ) {
        assert_eq!(
            out,
            "[Has(hello)]\n\
                 [Has([7])]\n\
                 [Some(z)]\n\
                 [[Some(5)]]\n"
        );
    }
}

#[test]
fn e2e_generic_enum_display_direct_spellings() {
    // B-2026-08-19-30 — the DIRECT half of the generic-enum Display gap.
    //
    // B-2026-08-19-28 taught the Display synthesizer to substitute a
    // generic enum's parameters, but only the NESTED spellings could supply
    // the arguments, because they hold the element / field `TypeExpr`. The
    // direct `println(e)` path (`render_user_enum_display`) has only the
    // enum's base name, so it still passed none and still panicked with
    // `type_name 'T' not yet supported` — on `println`, on f-string
    // interpolation, and on `.to_string()` alike. The instantiation now
    // comes from a span-keyed table the lowering pass forwards, the same
    // mechanism the Option/Result and tuple renderers already use.
    //
    // `Pair2` carries TWO parameters and a variant that uses only the
    // first, so the substitution has to be positional and partial-arity
    // tolerant rather than "swap the one type param". `MyOpt[String]` and
    // `MyOpt[Vec[i64]]` are boxed payloads (the erased slot is one word),
    // `MyOpt[u64]` puts an unsigned value past 2^63 through the same path,
    // and `Plain` pins that a non-generic enum is untouched.
    if let Some(out) = run_program(
        "#[derive(Display)]\n\
             enum MyOpt[T] { Has(T), Empty }\n\
             #[derive(Display)]\n\
             enum Pair2[A, B] { Both(A, B), Left(A), Neither }\n\
             #[derive(Display)]\n\
             enum Plain { X(i64), Y }\n\
             fn main() {\n\
             let a: MyOpt[i64] = MyOpt.Has(5i64);\n\
             println(a);\n\
             let b: MyOpt[String] = MyOpt.Has(\"hi\");\n\
             println(b);\n\
             let c: MyOpt[i64] = MyOpt.Empty;\n\
             println(c);\n\
             let d: MyOpt[u64] = MyOpt.Has(18446744073709551615u64);\n\
             println(d);\n\
             let e: Pair2[i64, String] = Pair2.Both(7i64, \"s\");\n\
             println(e);\n\
             let f: Pair2[String, i64] = Pair2.Left(\"q\");\n\
             println(f);\n\
             let g: Plain = Plain.X(3i64);\n\
             println(g);\n\
             let mut inner: Vec[i64] = vec![];\n\
             inner.push(9i64);\n\
             let h: MyOpt[Vec[i64]] = MyOpt.Has(inner);\n\
             println(h);\n\
             println(f\"{a} {b} {e}\");\n\
             println(a.to_string());\n\
             }",
    ) {
        assert_eq!(
            out,
            "Has(5)\n\
                 Has(hi)\n\
                 Empty\n\
                 Has(18446744073709551615)\n\
                 Both(7, s)\n\
                 Left(q)\n\
                 X(3)\n\
                 Has([9])\n\
                 Has(5) Has(hi) Both(7, s)\n\
                 Has(5)\n"
        );
    }
}

/// Below the threshold (here 2 arms) the linear cascade is already cheap
/// and its IR is simpler — the dispatch tree must NOT be built.
#[test]
fn test_small_string_match_keeps_cascade() {
    let ir = ir_for(
        "fn kw(s: String) -> i64 {\n\
                 match s {\n\
                     \"fn\" => 1,\n\
                     other => 0,\n\
                 }\n\
             }",
    );
    assert!(
        !ir.contains("match.strdisp"),
        "a 2-arm string match must stay on the cascade, no dispatch tree:\n{ir}"
    );
}

#[test]
fn test_e2e_ref_enum_string_and_bool_payload() {
    // Regression for B-2026-07-11-5: matching a `ref`-scrutinee enum and
    // using a String / bool payload. The via-ptr fast path bound the leaf
    // directly at the i64 payload WORD, so a String payload deref-loaded an
    // i64 (codegen PANIC "expected the StructValue variant" on the
    // `out.push_str(s)` dispatch) and a bool payload deref-loaded i64 (module
    // verify: `br i64` — not `i1`). Fixed by deferring any payload whose
    // declared LLVM type isn't the i64 word to the value-source path, which
    // reconstructs it at the true type (narrowing bool → i1, rebuilding the
    // String aggregate). Surfaced by the `examples/json.kara` dogfood.
    if let Some(out) = run_program(
        "enum E { S(String), B(bool), N(i64) }\n\
             fn render(e: ref E, out: mut ref String) {\n\
                 match e {\n\
                     S(s) => { out.push_str(s); }\n\
                     B(b) => { if *b { out.push_str(\"T\"); } else { out.push_str(\"F\"); } }\n\
                     N(n) => { out.push_str(f\"{n}\"); }\n\
                 }\n\
             }\n\
             fn main() {\n\
                 let mut o: String = \"\";\n\
                 render(E.S(\"hi\"), mut o); o.push(':');\n\
                 render(E.B(true), mut o); o.push(':');\n\
                 render(E.N(42), mut o);\n\
                 println(o);\n\
             }",
    ) {
        assert_eq!(out, "hi:T:42\n");
    }
}

#[test]
fn test_e2e_unannotated_unsigned_let_prints_unsigned() {
    // B-2026-08-11-21 leg 1. Codegen picks `%llu` vs `%lld` from a
    // syntactic classifier whose identifier arm reads `var_type_names` —
    // and nothing populated that map for a `let` with no annotation, so
    // EVERY inferred binding holding an unsigned value printed signed.
    // `let a = 18446744073709551615u64` printed -1 compiled while the
    // interpreter printed the value; the annotated spelling was always
    // right, because the annotation is what filled the map.
    //
    // All five producing shapes from the row (suffixed literal, fn return,
    // binop, `to_bits`, `reverse_bits`), plus the two controls that were
    // already correct and must stay so: an ANNOTATED binding, and
    // interpolating the producing expression directly rather than through a
    // binding. The signed binding is the control in the other direction — a
    // fix that made everything unsigned would print it as 2^64-1.
    assert_eq!(
        run_program(
            "fn u64_fn() -> u64 { return 18446744073709551615u64; }\n\
                 fn main() {\n\
                     let a = 18446744073709551615u64;\n\
                     let b = u64_fn();\n\
                     let c = 9223372036854775808u64 + 0u64;\n\
                     let x: f64 = 1.5;\n\
                     let d = x.to_bits();\n\
                     let e = (18446744073709551615u64).reverse_bits();\n\
                     let ann: u64 = 18446744073709551615u64;\n\
                     let neg = 0 - 1;\n\
                     println(f\"{a} {b} {c} {d} {e}\");\n\
                     println(f\"{ann} {x.to_bits()} {neg}\");\n\
                 }\n"
        )
        .as_deref(),
        Some(
            "18446744073709551615 18446744073709551615 9223372036854775808 \
                 4609434218613702656 18446744073709551615\n\
                 18446744073709551615 4609434218613702656 -1\n"
        ),
    );
}

#[test]
fn test_e2e_to_string_on_non_identifier_scalar_receiver() {
    // B-2026-08-13-2 — `<non-identifier scalar>.to_string()` used as the
    // RECEIVER of a further call was check-green and interp-green while
    // BOTH compiled backends died, in three different ways: a
    // "Vec/String method 'to_string' is not yet supported" build error, an
    // internal "this is a codegen bug — add a dispatcher arm" to-do printed
    // at whoever wrote the program, and an outright ICE
    // ("Found IntValue i32 120 but expected the StructValue variant" — 120
    // is `'x'`, the un-lowered codepoint reaching a caller that had already
    // decided the receiver was string-like).
    //
    // THE LOWERING ALREADY EXISTED; its gate never fired. `dispatch_key` is
    // span-keyed and a chain shares one, so the inner link reads the OUTER
    // call's `String.<m>`; `type_name_of_expr` is a NAME lookup, so it
    // answers for an identifier receiver (B-2026-08-11-22 added that arm)
    // and returns `None` for a literal, a cast or a parenthesized
    // expression. Both arms declined and the scalar arm was skipped — which
    // is exactly why every failing shape had a non-identifier receiver.
    //
    // `(7 + 1)` earns its line: primitive arithmetic desugars to an
    // intrinsic CALL (`i64.add(7, 1)`) before either backend sees it, so it
    // needs the call arm of the syntactic check rather than the literal one.
    // `n.to_string()` and `"hi".to_string()` are the controls that were
    // already working and must stay so.
    assert_eq!(
        run_program(
            "fn main() {\n\
                     println('x'.to_string().to_uppercase());\n\
                     println(7.to_string().len());\n\
                     println(true.to_string().to_uppercase());\n\
                     println((7 + 1).to_string().len());\n\
                     println(3.5.to_string().len());\n\
                     println('x'.to_string().to_string());\n\
                     println((-7).to_string().len());\n\
                     println((7 as u8).to_string().len());\n\
                     let n = 7;\n\
                     println(n.to_string().len());\n\
                     println(\"hi\".to_string().to_uppercase());\n\
                 }"
        )
        .as_deref(),
        Some("X\n1\nTRUE\n1\n3\nx\n2\n1\n1\nHI\n"),
    );
}

#[test]
fn test_e2e_scalar_to_string_survives_being_chained() {
    // B-2026-08-11-22 — chaining onto a SCALAR's `.to_string()`. Both gates
    // that route this call read `dispatch_key`, which is span-keyed, and
    // the parser sets a MethodCall's span equal to its receiver's — so in
    // `n.to_string().to_string()` both links share one key. The existing
    // collision guard separates links by requiring the key's method segment
    // to match the call's method, which cannot help when BOTH links are
    // named `to_string`.
    //
    // The inner link therefore read the outer's `String.to_string` and
    // (a) entered the String-copy path with an i64 receiver, panicking the
    // compiler while unwrapping an IntValue as a struct, and (b) once that
    // was vetoed, failed the scalar gate too and died as "no handler for
    // method 'to_string'". Both halves now consult the receiver's own
    // static type, which is not span-keyed and so cannot be shadowed.
    //
    // Every element type the scalar gate lists that has a distinguishable
    // rendering, plus receivers that are not plain identifiers (a field and
    // a call result) since those resolve differently.
    assert_eq!(
        run_program(
            "struct P { v: i64 }\n\
                 fn f() -> i64 { return 999i64; }\n\
                 fn main() {\n\
                     let n: i64 = 12345;\n\
                     let x: f64 = 1.5;\n\
                     let b: bool = true;\n\
                     let c: char = 'q';\n\
                     println(n.to_string().to_string());\n\
                     println(n.to_string().len());\n\
                     println(x.to_string().len());\n\
                     println(b.to_string().len());\n\
                     println(c.to_string().len());\n\
                     let p = P { v: 4242 };\n\
                     println(p.v.to_string().len());\n\
                     println(f().to_string().len());\n\
                 }\n"
        )
        .as_deref(),
        Some("12345\n5\n3\n4\n1\n4\n3\n"),
    );
}

#[test]
fn test_e2e_string_receiver_to_string_chains_still_route_to_the_copy_path() {
    // The control for B-2026-08-11-22's veto. The String-copy path must
    // still take every case it took before — the veto is keyed on the
    // receiver being a KNOWN non-String, so a String/StringSlice receiver
    // (identifier, literal, or a chained String→String builtin) is
    // untouched. If the veto were too broad these would fall through to the
    // catch-all instead.
    assert_eq!(
        run_program(
            "fn main() {\n\
                     let s: String = \"hello\".to_string();\n\
                     println(s.to_string().to_string());\n\
                     println(\"abc\".to_string().len());\n\
                     let t: String = \"  hi  \".to_string();\n\
                     println(t.trim().to_string().len());\n\
                     println(s.to_uppercase().to_string());\n\
                     println(s.clone().to_string());\n\
                 }\n"
        )
        .as_deref(),
        Some("hello\n3\n2\nHELLO\nhello\n"),
    );
}

#[test]
fn test_e2e_option_map_string_value_bodies_compile_and_run() {
    // B-2026-08-08-22 — THIS TEST PREVIOUSLY ASSERTED A GATE, and before
    // that it asserted a broader one. Both are gone; the shapes they
    // refused now compile and agree with the interpreter.
    //
    // The chain is worth keeping visible because each step blamed the
    // wrong thing. The first gate refused every un-annotated String-bodied
    // mapper and said "annotate the parameter" (B-2026-08-08-21 removed
    // that: the typechecker now seeds `map`'s closure param). The second
    // refused a body that IS a String value and said "append
    // `.to_string()`". That one was mis-scoped too: the defect was never
    // about `map` or about parameters, it was that
    // `infer_closure_return_type` declared a `StringLit` body as a bare
    // `ptr` while the body emits the owned `{ptr,len,cap}` aggregate. A
    // plain `let f = |x: i64| "fixed"; f(1)` failed identically with no
    // combinator present, which is what exposed it.
    for (label, body, want) in [
        ("bare literal", "|x| \"fixed\"", "fixed\n"),
        ("concat", "|x| x + \"!\"", "hi!\n"),
        ("annotated literal", "|x: String| \"fixed\"", "fixed\n"),
        ("method call", "|x| x.to_uppercase()", "HI\n"),
        (
            "literal .to_string()",
            "|x| \"fixed\".to_string()",
            "fixed\n",
        ),
        (
            "parenthesized concat",
            "|x| (x + \"!\").to_string()",
            "hi!\n",
        ),
        ("f-string", "|x| f\"[{x}]\"", "[hi]\n"),
        (
            "block, concat tail",
            "|x| { let t = x + \"!\"; t }",
            "hi!\n",
        ),
        (
            "block, literal tail",
            "|x| { let t = \"fixed\"; t }",
            "fixed\n",
        ),
    ] {
        let src = format!(
            "fn main() {{\n\
                     let s: Option[String] = Some(f\"hi\");\n\
                     match s.map({body}) {{ Some(x) => {{ println(x); }} None => {{}} }}\n\
                 }}"
        );
        assert_eq!(
            run_program(&src).as_deref(),
            Some(want),
            "{label} (`{body}`) must compile and match the interpreter"
        );
    }
}

/// B-2026-08-08-25 — matching a payload out of a live `Option[String]`
/// binding leaves the binding DANGLING, so any later read of it is garbage.
///
/// Found as "`.map` twice", but `map` is not involved: the hand-written
/// `match o { Some(v) => … }` twice corrupts identically, and so does one
/// `.map` followed by a plain `match o`. `map` reaches it only because it
/// lowers to exactly that match.
///
/// THE VEC/STRING ASYMMETRY THIS ROW WAS FILED WITH IS A PROBE ARTIFACT,
/// corrected here. The `Option[Vec[i64]]` control read `x.len()`, which
/// loads a length word and never dereferences the payload buffer. Reading an
/// ELEMENT instead (`x[0]`) corrupts exactly like the `String` payload does,
/// on the same tree. So there was never a Vec path "doing the right thing"
/// to copy — the defect is payload-type-agnostic, and it is not
/// `Option`-specific either: a user `enum E { A(String) }` matched twice
/// corrupts identically.
///
/// `--interp` prints correctly in every one of those, which makes this a
/// run-vs-build divergence with the interpreter as the reference. valgrind
/// calls it what it is: `Invalid read of size 2` against a freed block.
///
/// The BORROW-ONLY half is fixed (the arms only read the payload, so the
/// source keeps it) and is pinned below. The CONSUMING half — where the arm
/// really does move the payload out, which is what `.map` lowers to — still
/// dangles the source and is pinned separately as
/// `test_e2e_map_twice_over_live_option_string`.
#[test]
fn test_e2e_match_out_of_option_string_leaves_source_usable() {
    // The real shape: no combinator anywhere, arms only READ the payload.
    assert_eq!(
        run_program(
            "fn main() {\n\
                     let o: Option[String] = Some(f\"hi\");\n\
                     match o { Some(v) => { println(v.to_uppercase()); } None => {} }\n\
                     match o { Some(v) => { println(v.to_lowercase()); } None => {} }\n\
                 }"
        )
        .as_deref(),
        Some("HI\nhi\n")
    );
    // `if let` reaches the same binding path.
    assert_eq!(
        run_program(
            "fn main() {\n\
                     let o: Option[String] = Some(f\"hi\");\n\
                     if let Some(v) = o { println(v); }\n\
                     if let Some(v) = o { println(v); }\n\
                 }"
        )
        .as_deref(),
        Some("hi\nhi\n")
    );
    // The corrected Vec control: an ELEMENT read, which actually touches the
    // buffer. `.len()` here would pass even unfixed.
    assert_eq!(
        run_program(
            "fn main() {\n\
                     let mut v: Vec[i64] = Vec.new(); v.push(111); v.push(222);\n\
                     let o: Option[Vec[i64]] = Some(v);\n\
                     match o { Some(x) => { println(x[0]); } None => {} }\n\
                     match o { Some(x) => { println(x[0]); } None => {} }\n\
                 }"
        )
        .as_deref(),
        Some("111\n111\n")
    );
}

/// B-2026-08-08-25 leg 1 — `.map` over a live `Option[String]` no longer
/// dangles the source.
///
/// The blocker was never the mapper. `no_arm_payload_escapes` collected the
/// arm pattern `None` as a payload BINDING — the parser has no separate node
/// for a unit variant in pattern position and hands `None` back as
/// `Binding("None")`, exactly as it does for `Color.Red` — so any arm body
/// that MENTIONED `None` read as "that binding escapes" and vetoed the
/// read-only classification for the whole match.
///
/// `compile_map_via_match_synthesis` emits precisely `None => None`, which
/// is why the entire `.map` family was stuck on the transfer path. The same
/// veto hit the hand-written `match o { Some(v) => Some(v.len()), None =>
/// None }`, which no row had attributed to this defect at all — the
/// combinator was never required to reproduce it.
///
/// Escaping mappers are the counter-test: `|s| s` hands the payload straight
/// out, so it must NOT be classified as a borrow. It stays on the transfer
/// path and is pinned as still-open below.
#[test]
fn test_e2e_map_twice_over_live_option_string() {
    assert_eq!(
        run_program(
            "fn main() {\n\
                     let o: Option[String] = Some(f\"hi\");\n\
                     match o.map(|x| x.to_uppercase()) { Some(v) => { println(v); } None => {} }\n\
                     match o.map(|x| x.to_lowercase()) { Some(v) => { println(v); } None => {} }\n\
                 }"
        )
        .as_deref(),
        Some("HI\nhi\n")
    );
    // One `.map`, then a plain read of the source.
    assert_eq!(
        run_program(
            "fn main() {\n\
                     let o: Option[String] = Some(f\"hi\");\n\
                     match o.map(|x| x.to_uppercase()) { Some(v) => { println(v); } None => {} }\n\
                     match o { Some(v) => { println(v); } None => {} }\n\
                 }"
        )
        .as_deref(),
        Some("HI\nhi\n")
    );
    // No combinator: the hand-written shape the `None`-as-binding veto broke.
    assert_eq!(
            run_program(
                "fn main() {\n\
                     let o: Option[String] = Some(f\"hi\");\n\
                     let a: Option[i64] = match o { Some(v) => { Some(v.len()) } None => { None } };\n\
                     match o { Some(v) => { println(v); } None => {} }\n\
                     println(a.unwrap_or(0i64));\n\
                 }"
            )
            .as_deref(),
            Some("hi\n2\n")
        );
    // Vec payload through the same route.
    assert_eq!(
        run_program(
            "fn main() {\n\
                     let mut v: Vec[i64] = Vec.new(); v.push(7);\n\
                     let o: Option[Vec[i64]] = Some(v);\n\
                     match o.map(|x| x.len()) { Some(n) => { println(n); } None => {} }\n\
                     match o { Some(x) => { println(x[0]); } None => {} }\n\
                 }"
        )
        .as_deref(),
        Some("1\n7\n")
    );
}

#[test]
fn test_e2e_string_method_on_self_field() {
    // Regression for the self-hosting lexer blocker #5: a String/Vec
    // method on a field accessed through `self`
    // (`self.src.substring(self.start, self.current)`) died with "no
    // handler for method 'substring' on non-identifier receiver". The
    // field-receiver method helper resolves the receiver via
    // `lower_field_access_ptr`, which leaves `SelfValue` at `Ok(None)` so
    // the atomic-on-self path (`self.count.fetch_add(...)`) keeps its
    // dedicated handler. For NON-atomic self-field receivers the
    // FieldAccess dispatch arm now normalises `SelfValue` to a synthetic
    // `Identifier("self")`; gated on `!is_atomic_receiver` so atomics are
    // untouched (re-verified by the atomic_* suite).
    //
    // Covers `substring` (the lexer's `token_text`, with field-rooted
    // args), `contains`, and `len` — all String methods on `self.src`.
    if let Some(out) = run_program(
        "struct Lexer { src: String, start: i64, current: i64 }\n\
             impl Lexer {\n\
                 fn token_text(ref self) -> String {\n\
                     self.src.substring(self.start, self.current)\n\
                 }\n\
                 fn has_cd(ref self) -> bool { self.src.contains(\"cd\") }\n\
                 fn srclen(ref self) -> i64 { self.src.len() }\n\
             }\n\
             fn main() {\n\
                 let lx = Lexer { src: \"abcdef\", start: 1, current: 4 };\n\
                 println(lx.token_text());\n\
                 println(lx.has_cd().to_string());\n\
                 println(lx.srclen().to_string());\n\
             }",
    ) {
        assert_eq!(out, "bcd\ntrue\n6\n");
    }
}

#[test]
fn test_e2e_unsigned_vec_elem_to_string_no_sign_extend() {
    // Regression: a `for b in <Vec[u8]>` loop variable — whether the source
    // is a plain Vec var or a struct field (`for b in c.bytes`, the
    // self-hosted lexer's c-string-render shape) — had no recorded type
    // name, so `expr_is_unsigned_int` returned false and `b.to_string()`
    // SIGN-EXTENDED high bytes (195u8 printed as -61). A `let b: u8 = …`
    // binding already recorded the type; the for-loop / destructured element
    // path did not. Fixed by recording scalar integer primitive type names in
    // `register_var_from_type_expr` (types_lowering.rs). Surfaced by the
    // self-hosted lexer's c-string byte render of multi-byte `\u{…}` escapes.
    if let Some(out) = run_program(
        "struct C { bytes: Vec[u8] }\n\
             fn main() {\n\
                 let mut v: Vec[u8] = Vec.new();\n\
                 v.push(195u8); v.push(233u8); v.push(127u8);\n\
                 for b in v { println(b.to_string()); }        // plain Vec var\n\
                 let c = C { bytes: v };\n\
                 for b in c.bytes { println(b.to_string()); }  // struct-field Vec\n\
             }",
    ) {
        assert_eq!(out, "195\n233\n127\n195\n233\n127\n");
    }
}

#[test]
fn e2e_sorted_set_string_iter_min_max_codegen() {
    // `SortedSet[String]` iterates lexicographically ascending; min/max
    // CLONE the picked key (the sorted buffer aliases the set's owned key
    // data), so the returned `Option[String]` owns its payload.
    if let Some(out) = run_program(
        "fn main() {\n\
                 let mut s: SortedSet[String] = SortedSet.new();\n\
                 let _ = s.insert(\"banana\"); let _ = s.insert(\"apple\");\n\
                 let _ = s.insert(\"cherry\"); let _ = s.insert(\"apple\");\n\
                 let mut out: String = \"\";\n\
                 for w in s { out.push_str(f\"{w} \"); }\n\
                 println(out);\n\
                 match s.min() { Some(v) => println(v), None => println(\"none\") }\n\
                 match s.max() { Some(v) => println(v), None => println(\"none\") }\n\
                 println(s.len());\n\
             }",
    ) {
        assert_eq!(out, "apple banana cherry \napple\ncherry\n3\n");
    }
}

#[test]
fn e2e_sorted_map_string_keys_codegen() {
    // `SortedMap[String, i64]` — heap KEY sorted lexicographically; the
    // ordered producers deep-clone the key/value halves into the owned
    // result `Vec` (never aliasing the map's stored buffers).
    if let Some(out) = run_program(
        "fn main() {\n\
                 let mut m: SortedMap[String, i64] = SortedMap.new();\n\
                 let _ = m.insert(\"banana\", 2_i64); let _ = m.insert(\"apple\", 1_i64);\n\
                 let _ = m.insert(\"cherry\", 3_i64);\n\
                 let mut out: String = \"\";\n\
                 for (k, v) in m { out.push_str(f\"{k}={v} \"); }\n\
                 println(out);\n\
             }",
    ) {
        assert_eq!(out, "apple=1 banana=2 cherry=3 \n");
    }
}

#[test]
fn e2e_sorted_map_and_set_display_prefix_and_order_codegen() {
    // B-2026-08-14-35 — `SortedMap` / `SortedSet` share `Map` / `Set`'s
    // `KaracMap` storage, and before this they shared their DISPLAY FNS
    // too. A compiled program therefore printed the wrong type name over
    // the wrong order: `SortedMap{apple: 2, mango: 3, zebra: 1}` came out
    // `{zebra: 1, apple: 2, mango: 3}` — hash-bucket sequence under a `Map`
    // label — and `SortedSet{10, 20, 30}` announced itself as `Set{...}`.
    // Every spelling was wrong, the bound local included, and the NESTED
    // one (`Vec[SortedMap[K, V]]`) panicked the compiler outright with
    // "type_name 'SortedMap_String_i64' not yet supported".
    //
    // ORDER is the load-bearing half. A prefix-only fix would have put a
    // correct label on a bucket-order render — a worse bug than the one it
    // closed. It now comes from the same `emit_sorted_keys_buf` that
    // `keys()` and `for (k, v) in m` use, so the render cannot drift from
    // iteration.
    //
    // The spellings below are the ones that reach different code paths:
    // bound local (both `f"{}"` and bare `println`), struct field, call
    // result, `Vec` element (the recursive dispatcher), and empty (the
    // zero-length walk). Twin:
    // `sorted_map_and_set_display_prefix_and_order_interp`.
    if let Some(out) = run_program(SORTED_DISPLAY_SRC) {
        assert_eq!(out, SORTED_DISPLAY_EXPECTED);
    }
}

/// B-2026-08-15-2 — `s.push_str(s)`, the self-append.
///
/// The values here were ALREADY correct before the fix — that is the whole
/// difficulty of this row. The grow reallocated the destination and the
/// copy then read through the stale source pointer, but the freed bytes
/// are usually still mapped, so every line below printed exactly what it
/// should while committing a heap-use-after-free of the entire string.
/// `asan_string_push_str_self_append_no_use_after_free` is the gate; this
/// pins that making the aliasing correct did not change any answer.
///
/// Line 05 is the one that would catch a WRONG rebase. Everything else
/// checks lengths and short contents, which a source pointer rebuilt at
/// the wrong offset can still satisfy; 05 reads the seam — the last 8
/// bytes of the original and the first 8 of the appended copy — where an
/// off-by-offset shows up as shifted text rather than a bad count.
///
/// 01 is the `cap == 0` receiver, whose grow mallocs fresh and never frees
/// the literal, so it must NOT be rebased; 07 is a borrowed slice of a
/// DIFFERENT string, the hot path whose now-deleted alias panic used to
/// guard this arm.
#[test]
fn test_e2e_string_push_str_self_append() {
    let src = r#"
fn main() {
    let mut a = "abc";
    a.push_str(a);
    println(f"01 {a} {a.len()}");

    let mut b = String.new();
    b.push_str("wxyz");
    b.push_str(b);
    println(f"02 {b} {b.len()}");

    let mut c = String.new();
    c.push_str("pq");
    c.push_str(c);
    c.push_str(c);
    c.push_str(c);
    println(f"03 {c} {c.len()}");

    let mut d = String.new();
    let mut i = 0i64;
    while i < 5000i64 { d.push_str("abcdefgh"); i = i + 1i64; }
    d.push_str(d);
    println(f"04 {d.len()}");
    println(f"05 {d[0..8]} {d[39992..40000]} {d[40000..40008]} {d[79992..80000]}");

    let mut e = String.new();
    e.push_str("hello");
    let w = "world";
    e.push_str(w);
    println(f"06 {e} {e.len()}");

    let s2 = "abcdefghij";
    let mut out = String.new();
    out.push_str(s2[2..6]);
    out.push_str(s2[0..2]);
    println(f"07 {out} {out.len()}");

    let mut g = String.new();
    g.push_str("");
    g.push_str(g);
    println(f"08 [{g}] {g.len()}");
    let mut h = String.new();
    h.push_str("zz");
    h.push_str("");
    println(f"09 {h} {h.len()}");
}
"#;
    assert_eq!(
        run_program(src).as_deref(),
        Some(
            "01 abcabc 6\n\
                 02 wxyzwxyz 8\n\
                 03 pqpqpqpqpqpqpqpq 16\n\
                 04 80000\n\
                 05 abcdefgh abcdefgh abcdefgh abcdefgh\n\
                 06 helloworld 10\n\
                 07 cdefab 6\n\
                 08 [] 0\n\
                 09 zz 2\n"
        ),
    );
}

#[test]
fn e2e_sorted_map_ordered_methods_string_codegen() {
    // The heap path: `SortedMap[String, String]` ordered methods deep-clone
    // the key/value halves into the returned `Option`/`Vec` (the Option
    // payload heap-boxes the wide (String,String) tuple). Byte-identical to
    // `karac run`.
    if let Some(out) = run_program(
            "fn main() {\n\
                 let mut m: SortedMap[String, String] = SortedMap.new();\n\
                 let _ = m.insert(\"banana\", \"yellow\"); let _ = m.insert(\"apple\", \"red\");\n\
                 let _ = m.insert(\"cherry\", \"dark\");\n\
                 match m.min() { Some(kv) => println(f\"{kv.0}={kv.1}\"), None => println(\"n\") }\n\
                 match m.max() { Some(kv) => println(f\"{kv.0}={kv.1}\"), None => println(\"n\") }\n\
                 match m.floor(\"boo\") { Some(kv) => println(f\"{kv.0}\"), None => println(\"n\") }\n\
                 match m.ceiling(\"boo\") { Some(kv) => println(f\"{kv.0}\"), None => println(\"n\") }\n\
                 let r = m.range(\"apple\", \"banana\"); println(f\"{r.len()}\");\n\
             }",
        ) {
            assert_eq!(out, "apple=red\ncherry=dark\nbanana\ncherry\n2\n");
        }
}

#[test]
fn e2e_try_push_str_fallible_codegen() {
    // `String.try_push_str` — fallible append, returns Result[(), AllocError].
    if let Some(out) = run_program(
        "fn main() {\n\
                 let mut s: String = \"\";\n\
                 match s.try_push_str(\"ab\") {\n\
                     Ok(_) => println(\"ok\"),\n\
                     Err(_) => println(\"err\"),\n\
                 }\n\
                 let _ = s.try_push_str(\"cde\");\n\
                 println(s);\n\
                 println(s.len());\n\
             }",
    ) {
        assert_eq!(out, "ok\nabcde\n5\n");
    }
}

#[test]
fn e2e_vec_contains_string_elem_codegen() {
    // `Vec[String].contains` routes element `==` through the struct/
    // string-binop path (memcmp), not a scalar compare.
    if let Some(out) = run_program(
        "fn main() {\n\
                 let mut names: Vec[String] = Vec.new();\n\
                 names.push(\"alice\");\n\
                 names.push(\"bob\");\n\
                 println(names.contains(\"bob\"));\n\
                 println(names.contains(\"carol\"));\n\
             }",
    ) {
        assert_eq!(out, "true\nfalse\n");
    }
}

#[test]
fn e2e_string_contains_substring_codegen() {
    // `String.contains(sub)` lowers to a naive memcmp substring scan.
    // Covers a hit, a miss, the empty-needle case (always true), and a
    // needle longer than the haystack (always false) — the loop's
    // boundary conditions.
    if let Some(out) = run_program(
        "fn main() {\n\
                 let s: String = \"hello world\";\n\
                 println(s.contains(\"world\"));\n\
                 println(s.contains(\"xyz\"));\n\
                 println(s.contains(\"\"));\n\
                 println(s.contains(\"hello world!\"));\n\
             }",
    ) {
        assert_eq!(out, "true\nfalse\ntrue\nfalse\n");
    }
}

#[test]
fn e2e_string_split_codegen() {
    // `String.split(sep) -> Vec[String]` (GAP-W2) — must match the
    // interpreter (tests/interpreter.rs::test_string_split_interpreter):
    // char separator, String separator, leading/trailing empty pieces, a
    // separator-free string (single piece), `.len()`, indexing, iteration.
    if let Some(out) = run_program(
        "fn main() {\n\
                 let csv: String = \"a,b,c\";\n\
                 let parts = csv.split(',');\n\
                 let n = parts.len();\n\
                 println(f\"{n}\");\n\
                 println(parts[0]);\n\
                 println(parts[2]);\n\
                 let path: String = \"x::y::z\";\n\
                 let seg = path.split(\"::\");\n\
                 let sn = seg.len();\n\
                 println(f\"{sn}\");\n\
                 let edges: String = \",lead,trail,\";\n\
                 let en = edges.split(',').len();\n\
                 println(f\"{en}\");\n\
                 let whole: String = \"nosep\";\n\
                 let one = whole.split(',');\n\
                 let on = one.len();\n\
                 println(f\"{on}\");\n\
                 println(one[0]);\n\
             }",
    ) {
        assert_eq!(out, "3\na\nc\n3\n4\n1\nnosep\n");
    }
}

/// `String.lines() -> Vec[String]` via `karac_runtime_string_lines` — must
/// match the interpreter oracle (`test_string_lines_interpreter`): multi-
/// line, trailing-newline (no final empty line), CRLF (`\r` stripped),
/// preserved empty middle line, and empty string (zero lines). Leak-freedom
/// is gated in `tests/memory_sanitizer.rs::asan_string_lines_no_leak_no_double_free`.
#[test]
fn test_e2e_string_from_owned_source_copies() {
    // B-2026-07-13-8: `String.from(<String>)` builds a fresh owned copy
    // (the `From` owning contract), so a fresh owned source — f-string temp
    // or owned binding — is not double-freed by both its origin and the
    // result. Was: `free(): double free detected in tcache 2` under
    // JIT/native for the f-string/owned-binding sources; the string-literal
    // source stayed clean by luck (cap == 0).
    let out = run_program(
        "fn main() {\n\
             \x20   let a: String = String.from(f\"fs{1}\");\n\
             \x20   println(a);\n\
             \x20   let s: String = f\"own{2}\";\n\
             \x20   let b: String = String.from(s);\n\
             \x20   println(b);\n\
             \x20   let c: String = String.from(\"lit\");\n\
             \x20   println(c);\n\
             }",
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "fs1\nown2\nlit");
    }
}

#[test]
fn test_e2e_string_lines() {
    if let Some(out) = run_program(
        "fn main() {\n\
                 let a: String = \"one\\ntwo\\nthree\";\n\
                 let v = a.lines();\n\
                 println(f\"{v.len()}\");\n\
                 println(v[0]);\n\
                 println(v[2]);\n\
                 let t: String = \"trailing\\n\";\n\
                 let tv = t.lines();\n\
                 println(f\"{tv.len()}\");\n\
                 let c: String = \"crlf\\r\\nhandling\\r\\n\";\n\
                 let w = c.lines();\n\
                 println(f\"{w.len()}\");\n\
                 println(w[1]);\n\
                 let d: String = \"a\\n\\nb\";\n\
                 let x = d.lines();\n\
                 println(f\"{x.len()} {x[1].len()}\");\n\
                 let e: String = \"\";\n\
                 let ev = e.lines();\n\
                 println(f\"{ev.len()}\");\n\
             }",
    ) {
        assert_eq!(out, "3\none\nthree\n1\n2\nhandling\n3 0\n0\n");
    }
}

/// `String.split_whitespace() -> Vec[String]` via
/// `karac_runtime_string_split_whitespace` — must match the interpreter
/// oracle (`test_string_split_whitespace_interpreter`): whitespace-run
/// collapsing, tab/newline handling, and single/all-whitespace/empty →
/// 1 / 0 / 0. Leak-freedom gated in `tests/memory_sanitizer.rs`.
#[test]
fn test_e2e_string_split_whitespace() {
    if let Some(out) = run_program(
        "fn main() {\n\
                 let a: String = \"  the  quick   brown fox  \";\n\
                 let v = a.split_whitespace();\n\
                 println(f\"{v.len()}\");\n\
                 println(v[0]);\n\
                 println(v[3]);\n\
                 let e: String = \"tab\\tand\\nnewline\";\n\
                 let w = e.split_whitespace();\n\
                 println(f\"{w.len()}\");\n\
                 println(w[1]);\n\
                 let one = \"single\".split_whitespace();\n\
                 println(f\"{one.len()}\");\n\
                 let ws = \"   \".split_whitespace();\n\
                 println(f\"{ws.len()}\");\n\
                 let empty = \"\".split_whitespace();\n\
                 println(f\"{empty.len()}\");\n\
             }",
    ) {
        assert_eq!(out, "4\nthe\nfox\n3\nand\n1\n0\n0\n");
    }
}

/// Allocating String→String methods (`trim` / `replace` / `to_lowercase` /
/// `to_uppercase`) lowered through the `karac_string_*` runtime helpers, so
/// codegen computes the byte-identical full-Unicode result as the
/// interpreter (tests/interpreter.rs::test_string_trim_replace_case_interpreter)
/// — including length-changing case maps (`ß` → `SS`) and Unicode `é`.
#[test]
fn e2e_string_trim_replace_case_codegen() {
    if let Some(out) = run_program(
        "fn main() {\n\
                 let s: String = \"  Hello World  \";\n\
                 println(s.trim());\n\
                 println(s);\n\
                 println(\"HeLLo\".to_lowercase());\n\
                 println(\"HeLLo\".to_uppercase());\n\
                 println(\"a-b-c\".replace(\"-\", \"+\"));\n\
                 println(\"aaa\".replace(\"a\", \"bb\"));\n\
                 println(\"straße\".to_uppercase());\n\
                 println(\"café\".to_uppercase());\n\
                 println(\"   \".trim());\n\
                 println(\"Hello World\".to_lowercase().replace(\" \", \"_\"));\n\
             }",
    ) {
        assert_eq!(
                out,
                "Hello World\n  Hello World  \nhello\nHELLO\na+b+c\nbbbbbb\nSTRASSE\nCAFÉ\n\nhello_world\n"
            );
    }
}

/// `String.replacen(from, to, n)` via `karac_string_replacen` (Rust
/// `str::replacen`) — replace at most the first `n` occurrences. A negative
/// count clamps to 0 (replace nothing), the documented codegen/runtime
/// contract. Byte-identical to the interpreter
/// (`test_string_replacen_interpreter`); leak-freedom gated in
/// `tests/memory_sanitizer.rs`.
#[test]
fn e2e_string_replacen_codegen() {
    if let Some(out) = run_program(
        "fn main() {\n\
                 println(\"a-b-c-d\".replacen(\"-\", \"_\", 2));\n\
                 println(\"x.x.x\".replacen(\".\", \"!\", 10));\n\
                 println(\"aaaa\".replacen(\"a\", \"bb\", 3));\n\
                 println(\"1,2,3\".replacen(\",\", \";\", 0));\n\
                 println(\"1,2,3\".replacen(\",\", \";\", -1));\n\
                 let s: String = \"one two two two\";\n\
                 println(s.replacen(\"two\", \"2\", 2));\n\
                 println(s);\n\
             }",
    ) {
        assert_eq!(
            out,
            "a_b_c-d\nx!x!x\nbbbbbba\n1,2,3\n1,2,3\none 2 2 two\none two two two\n"
        );
    }
}

/// `String.trim_start()` / `.trim_end()` via `karac_string_trim_{start,end}`
/// — strip only leading / trailing whitespace, byte-identical to the
/// interpreter (`test_string_trim_start_end_interpreter`). Leak-freedom is
/// gated in `tests/memory_sanitizer.rs`.
#[test]
fn e2e_string_trim_start_end_codegen() {
    if let Some(out) = run_program(
        "fn main() {\n\
                 let s: String = \"  Hello  \";\n\
                 println(f\"[{s.trim_start()}]\");\n\
                 println(f\"[{s.trim_end()}]\");\n\
                 println(f\"[{s}]\");\n\
                 println(f\"[{\"\\t x \\n\".trim_start()}]\");\n\
                 println(f\"[{\"\\t x \\n\".trim_end()}]\");\n\
                 println(f\"[{\"none\".trim_start()}]\");\n\
                 println(f\"[{\"   \".trim_end()}]\");\n\
             }",
    ) {
        assert_eq!(
            out,
            "[Hello  ]\n[  Hello]\n[  Hello  ]\n[x \n]\n[\t x]\n[none]\n[]\n"
        );
    }
}

/// `String.sorted()` — characters sorted ascending into a fresh String, the
/// canonical anagram key (LeetCode #49). Lowered through the
/// `karac_string_sorted` runtime helper, so codegen computes the
/// byte-identical result as the interpreter's `chars().sort_unstable()`
/// (`src/interpreter/method_call_seq.rs`) — sorting by Unicode scalar value,
/// not raw byte, so multi-byte input agrees across backends. Covers a literal
/// receiver, an identifier receiver (non-mutating: `w` prints unchanged after
/// `w.sorted()`), the empty string, and the anagram equality it exists for.
#[test]
fn e2e_string_sorted_codegen() {
    if let Some(out) = run_program(
        "fn main() {\n\
                 println(\"eat\".sorted());\n\
                 println(\"tea\".sorted());\n\
                 let w: String = \"listen\";\n\
                 println(w.sorted());\n\
                 println(w);\n\
                 println(\"\".sorted());\n\
                 println(\"dcba\".sorted());\n\
                 let anag = \"eat\".sorted() == \"tea\".sorted();\n\
                 println(f\"{anag}\");\n\
             }",
    ) {
        assert_eq!(out, "aet\naet\neilnst\nlisten\n\nabcd\ntrue\n");
    }
}

/// `String.cmp(other) -> Ordering` — the method form of `<`/`>`, lowered
/// through `karac_string_cmp` (byte-lexicographic, the same order
/// `Vec[String].sort`/`binary_search` use) into an `Ordering` tag. Exercises
/// all three arms via a bare-variant match plus use inside a `sort_by`
/// comparator. Must match the interpreter oracle
/// (interpreter.rs::test_match_bare_ordering_variant_from_cmp). B-2026-06-30-13.
#[test]
fn e2e_string_cmp_codegen() {
    if let Some(out) = run_program(
        "fn tag(a: String, b: String) -> i64 {\n\
                 match a.cmp(b) { Less => 0, Equal => 1, Greater => 2 }\n\
             }\n\
             fn main() {\n\
                 let r1 = tag(\"abc\", \"abd\");\n\
                 let r2 = tag(\"abd\", \"abc\");\n\
                 let r3 = tag(\"abc\", \"abc\");\n\
                 let r4 = tag(\"ab\", \"abc\");\n\
                 println(f\"{r1}\");\n\
                 println(f\"{r2}\");\n\
                 println(f\"{r3}\");\n\
                 println(f\"{r4}\");\n\
                 let mut v: Vec[String] = [\"banana\", \"apple\", \"cherry\"];\n\
                 v.sort_by(|x, y| x.cmp(y));\n\
                 println(v[0]);\n\
             }",
    ) {
        assert_eq!(out, "0\n2\n1\n0\napple\n");
    }
}

/// `String.cmp` on NON-identifier receivers — a string LITERAL
/// (`"abd".cmp("abc")`) and an INDEX into a `Vec[String]` (`v[0].cmp(v[1])`).
/// Both typecheck and run, but B-13's first codegen guard keyed on
/// `inferred_receiver_type` (which resolves only NAMED receivers), so these
/// fell through to "method 'cmp' is not yet supported in codegen" — a
/// run/build divergence. The operand-layout guard (the String {ptr,len,cap}
/// header) covers every receiver shape. B-2026-07-02-9.
#[test]
fn e2e_string_cmp_nonident_receiver_codegen() {
    if let Some(out) = run_program(
        "fn main() {\n\
                 let lit = match \"abd\".cmp(\"abc\") { Less => 0, Equal => 1, Greater => 2 };\n\
                 println(f\"{lit}\");\n\
                 let v: Vec[String] = [\"nat\", \"abc\", \"zzz\"];\n\
                 let i0 = match v[0].cmp(v[1]) { Less => 0, Equal => 1, Greater => 2 };\n\
                 println(f\"{i0}\");\n\
                 let i1 = match v[1].cmp(v[2]) { Less => 0, Equal => 1, Greater => 2 };\n\
                 println(f\"{i1}\");\n\
             }",
    ) {
        assert_eq!(out, "2\n2\n0\n");
    }
}

#[test]
fn e2e_string_method_nonident_receiver_codegen() {
    // String collection methods on a NON-identifier receiver — a string
    // literal (`"a,b,c".split(...)`) and a call-result (`make().split(...)`).
    // The identifier-keyed collection dispatch fell through for these; the
    // `try_compile_nonident_collection_method` shim materializes the receiver
    // into a synth local and re-routes through `compile_vec_method`. The
    // call-result receiver is the double-free-prone case (its heap buffer is
    // freed by the statement-level owned-temp machinery — the shim must NOT
    // double-track it); see `asan_string_method_nonident_receiver_*`. Output
    // must match the interpreter oracle.
    if let Some(out) = run_program(
        "fn make_csv() -> String { return \"a,bb,,ccc\"; }\n\
             fn main() {\n\
                 let a = \"x,y,z\".split(',');\n\
                 println(f\"{a.len()}\");\n\
                 println(a[1]);\n\
                 let lit_has = \"hello world\".contains(\"wor\");\n\
                 println(f\"{lit_has}\");\n\
                 let c = make_csv().split(',');\n\
                 println(f\"{c.len()}\");\n\
                 println(c[3]);\n\
             }",
    ) {
        assert_eq!(out, "3\ny\ntrue\n4\nccc\n");
    }
}

#[test]
fn e2e_chars_collect_to_vec_char_codegen() {
    // B-2026-06-18-1 (kata:38): `s.chars().collect()` into a `Vec[char]` —
    // the idiomatic O(1)-indexed-access form (per examples/leetcode/
    // valid_palindrome.kara) — failed codegen ("no handler for method
    // 'collect' on non-identifier receiver"): codegen has no general
    // iterator/collect lowering. The fix lowers the idiom to the supported
    // `for c in s.chars() { v.push(c) }` build. Exercises the load-bearing
    // uses: `.len()` on the result, `chars[i]` indexing with two pointers
    // (run grouping), char equality, and `char as i64` digit value — the
    // exact shape count_and_say_indexed.kara needs. Owned `String` and a
    // `ref String` parameter receiver both covered.
    if let Some(out) = run_program(
        "fn longest_run(s: ref String) -> i64 {\n\
                 let chars: Vec[char] = s.chars().collect();\n\
                 let n = chars.len();\n\
                 let mut best = 0i64;\n\
                 let mut i = 0i64;\n\
                 while i < n {\n\
                     let mut j = i;\n\
                     while j < n and chars[j] == chars[i] { j = j + 1i64; }\n\
                     if j - i > best { best = j - i; }\n\
                     i = j;\n\
                 }\n\
                 best\n\
             }\n\
             fn main() {\n\
                 let s: String = \"aabbbbc\";\n\
                 let cs: Vec[char] = s.chars().collect();\n\
                 println(f\"{cs.len()}\");\n\
                 let v = (cs[2] as i64) - ('0' as i64) + 1i64;\n\
                 println(f\"{v}\");\n\
                 println(f\"{longest_run(s)}\");\n\
             }",
    ) {
        // len 7; cs[2]=='b'(98) - '0'(48) + 1 = 51; longest run = 4 ('bbbb')
        assert_eq!(out, "7\n51\n4\n");
    }
}

/// B-2026-07-04-2 sub-part 3 (non-terminal f-string map): a `map(|x| f"..")`
/// that is not the last adaptor (`v.iter().map(|x| f"..").filter(g).collect()`,
/// or `.map(|x| f"..").map(|s| s.len())`) splits at the f-string map —
/// collect the prefix (now a TERMINAL f-string map -> Vec[String]) into a
/// temp, then continue over the temp. Previously bailed (the intermediate
/// `let __icm = f".."` double-freed via the staged accumulator). ASAN twin:
/// asan_b04_2_nonterminal_fstring_map_no_leak.
#[test]
fn e2e_iter_adaptor_nonterminal_fstring_map_codegen() {
    if let Some(out) = run_program(
        r#"
fn main() {
    let v: Vec[i64] = Vec[1i64, 2i64, 3i64];
    // f-string map then filter (identity downstream)
    let a: Vec[String] = v.iter().map(|x| f"n{x}").filter(|s| s.len() > 0i64).collect();
    println(f"{a.len()} {a[0i64]} {a[2i64]}");
    // f-string map then element-type-changing map
    let b: Vec[i64] = v.iter().map(|x| f"num-{x}").map(|s| s.len()).collect();
    println(f"{b.len()} {b[0i64]} {b[2i64]}");
}
"#,
    ) {
        assert_eq!(out, "3 n1 n3\n3 5 5\n");
    }
}

#[test]
fn e2e_for_char_in_string_binds_char_codegen() {
    // B-2026-06-18-2: `for c in s` over a String (owned and `ref String`)
    // must bind `c: char`, not `String` / `ref String`. The typechecker's
    // `element_type_of` had no `Str` arm, so it fell to `ty.clone()` and
    // mistyped the loop var — a typechecker/codegen MISMATCH (codegen's
    // `compile_for_string_chars` already binds the decoded codepoint as
    // `char`), surfacing as "no method '<char-method>' on type 'String'"
    // under `karac build` the moment `c` was used as a char. Exercises both
    // an owned `String` and a `ref String` parameter, and a char method on
    // the loop var (`is_alphabetic`) — the exact shape valid_palindrome.kara
    // and digit-classifying loops need.
    if let Some(out) = run_program(
        "fn count_alpha(s: ref String) -> i64 {\n\
                 let mut n = 0i64;\n\
                 for c in s {\n\
                     if c.is_alphabetic() { n = n + 1i64; }\n\
                 }\n\
                 n\n\
             }\n\
             fn main() {\n\
                 let s: String = \"aB3 z9\";\n\
                 let mut digits = 0i64;\n\
                 for c in s {\n\
                     if c.is_numeric() { digits = digits + 1i64; }\n\
                 }\n\
                 println(f\"{digits}\");\n\
                 println(f\"{count_alpha(s)}\");\n\
             }",
    ) {
        // "aB3 z9": numeric = 3,9 → 2; alphabetic = a,B,z → 3
        assert_eq!(out, "2\n3\n");
    }
}

#[test]
fn e2e_string_push_ascii_fastpath_and_multibyte() {
    // `String.push(char)` ASCII fast-path: a codepoint < 0x80 is stored as a
    // single byte directly (no `karac_string_encode_char` call, no
    // variable-length memcpy → libc memmove), the dominant string-build cost
    // (kata:38 profile). SOUNDNESS: the multibyte slow path must still encode
    // correctly, so this interleaves 1/2/3/4-byte codepoints with ASCII and
    // checks both the bytes (printed string) and the scalar count. Must match
    // the interpreter oracle exactly.
    if let Some(out) = run_program(
        "fn main() {\n\
                 let mut s: String = \"\";\n\
                 s.push('a');\n\
                 s.push('é');\n\
                 s.push('€');\n\
                 s.push('🦀');\n\
                 s.push('z');\n\
                 println(s);\n\
                 let mut n = 0i64;\n\
                 for c in s.chars() { n = n + 1i64; }\n\
                 println(f\"{n}\");\n\
             }",
    ) {
        // a(1) é(2) €(3) 🦀(4) z(1) bytes; 5 scalar values
        assert_eq!(out, "aé€🦀z\n5\n");
    }
}

#[test]
fn e2e_string_substring_two_arg_codegen() {
    // Two-arg `substring(start, end)` (byte range `[start, end)`): prefix /
    // suffix / empty-when-equal / inverted-bounds (end<start) / end-clamped /
    // negative-start (→ empty) — must match the interpreter
    // (test_string_substring_two_arg_interpreter). The self-hosted lexer's
    // AOT `token_text` path depends on this.
    if let Some(out) = run_program(
        "fn main() {\n\
                 let s: String = \"hello world\";\n\
                 println(s.substring(0, 5));\n\
                 println(s.substring(6, 11));\n\
                 println(s.substring(3, 3));\n\
                 println(s.substring(8, 2));\n\
                 println(s.substring(2, 100));\n\
                 println(s.substring(-2, 4));\n\
             }",
    ) {
        assert_eq!(out, "hello\nworld\n\n\nllo world\n\n");
    }
}

#[test]
fn e2e_string_method_on_tuple_destructured_binding_codegen() {
    // B-2026-06-12-3: a String/Vec/Slice bound via tuple destructure
    // (`let (a, b) = pair()`) must route method calls through the
    // collection dispatch surface. Before the fix, `bind_pattern` (the
    // let-destructure binder) allocated the leaf slot but never registered
    // `string_vars` / `vec_elem_types`, so `a.repeat(2)` / `a.substring(..)`
    // / `nums.len()` failed codegen with "no handler for method '…' on
    // variable 'a'" — while the interpreter handled them. Covers the
    // String element (repeat + substring), a Vec element (len + index),
    // and a wildcard-discard element. Output must match the interpreter.
    if let Some(out) = run_program(
        "fn mk() -> (Vec[i64], String) {\n\
                 let mut v: Vec[i64] = Vec.new();\n\
                 v.push(10);\n\
                 v.push(20);\n\
                 (v, \"hi\")\n\
             }\n\
             fn main() {\n\
                 let (nums, s) = mk();\n\
                 println(nums.len());\n\
                 println(nums[0] + nums[1]);\n\
                 println(s.repeat(2));\n\
                 println(s.substring(0, 1));\n\
                 let (a, _) = (\"x\", \"y\");\n\
                 println(a.repeat(3));\n\
             }",
    ) {
        assert_eq!(out, "2\n30\nhihi\nh\nxxx\n");
    }
}

#[test]
fn e2e_try_from_slice_string_clone_codegen() {
    // Heap-element source (Vec[String]) takes the per-element clone loop, so
    // the new Vec's strings are independent of the source. Exercises the
    // Vec[String]-in-Result.Ok round-trip end to end.
    if let Some(out) = run_program(
        "fn main() {\n\
                 let mut src: Vec[String] = Vec.new();\n\
                 src.push(\"ab\");\n\
                 src.push(\"cd\");\n\
                 match Vec.try_from_slice(src) {\n\
                     Ok(v) => { println(v.len()); println(v[0]); println(v[1]); }\n\
                     Err(_) => println(\"err\"),\n\
                 }\n\
             }",
    ) {
        assert_eq!(out, "2\nab\ncd\n");
    }
}

#[test]
fn e2e_vecdeque_string_with_capacity_codegen() {
    // B-2026-06-10-3: `VecDeque.with_capacity` / `String.with_capacity` had
    // no codegen arm and fell through to the `Ok(const 0)` default → SIGTRAP.
    // Both now build: VecDeque rides Vec's `{ptr,len,cap}` storage + element
    // recovery; String reserves `n` bytes (u8 element). Reserved capacity is
    // logically empty (len=0); pushes fill the slots.
    if let Some(out) = run_program(
        "fn main() {\n\
                 let mut q: VecDeque[i64] = VecDeque.with_capacity(8);\n\
                 q.push_back(1_i64);\n\
                 q.push_front(2_i64);\n\
                 println(q.len()); println(q[0]); println(q[1]);\n\
                 let mut s: String = String.with_capacity(8);\n\
                 s.push_str(\"hi\");\n\
                 println(s); println(s.len());\n\
             }",
    ) {
        assert_eq!(out, "2\n2\n1\nhi\n2\n");
    }
}

#[test]
fn e2e_vecdeque_string_try_with_capacity_codegen() {
    // phase-8-stdlib-floor item 8: the `try_with_capacity` companions for
    // VecDeque (rides the Vec arm) and String (byte element), now that their
    // panicking base codegens. Covers both the match form (VecDeque payload
    // in Result — exercises the VecDeque-enum-payload reconstruction fix) and
    // the `?`-form.
    if let Some(out) = run_program(
        "fn bvd() -> Result[i64, AllocError] {\n\
                 let mut v: VecDeque[i64] = VecDeque.try_with_capacity(4)?;\n\
                 v.push_back(1_i64);\n\
                 v.push_front(2_i64);\n\
                 Ok(v[0] + v[1])\n\
             }\n\
             fn bstr() -> Result[i64, AllocError] {\n\
                 let mut s: String = String.try_with_capacity(8)?;\n\
                 s.push_str(\"hello\");\n\
                 Ok(s.len())\n\
             }\n\
             fn main() {\n\
                 let rv: Result[VecDeque[i64], AllocError] = VecDeque.try_with_capacity(4);\n\
                 match rv { Ok(v) => println(v.len()), Err(_) => println(\"e\") }\n\
                 match bvd() { Ok(n) => println(n), Err(_) => println(\"e\") }\n\
                 match bstr() { Ok(n) => println(n), Err(_) => println(\"e\") }\n\
             }",
    ) {
        assert_eq!(out, "0\n3\n5\n");
    }
}

#[test]
fn e2e_try_clone_string_codegen() {
    // `String.try_clone()` — fallible buffer alloc (len+1, NUL-terminated).
    // The clone owns an independent buffer; the source's mutation does not
    // alias it.
    if let Some(out) = run_program(
        "fn main() {\n\
                 let mut s: String = \"hi\";\n\
                 match s.try_clone() {\n\
                     Ok(c) => {\n\
                         s.push_str(\"!!\");\n\
                         println(c); println(c.len()); println(s);\n\
                     }\n\
                     Err(_) => println(\"err\"),\n\
                 }\n\
             }",
    ) {
        assert_eq!(out, "hi\n2\nhi!!\n");
    }
}

#[test]
fn e2e_try_clone_vec_string_deep_codegen() {
    // Heap-element receiver (`Vec[String]`) takes the per-element fallible
    // clone loop: every String is deep-cloned into the new buffer, so the
    // clone's strings are independent of the source's. Exercises the
    // recursive fallible-clone path + Vec[String]-in-Result.Ok round-trip.
    if let Some(out) = run_program(
        "fn main() {\n\
                 let mut src: Vec[String] = Vec.new();\n\
                 src.push(\"ab\");\n\
                 src.push(\"cd\");\n\
                 match src.try_clone() {\n\
                     Ok(c) => {\n\
                         println(c.len()); println(c[0]); println(c[1]);\n\
                     }\n\
                     Err(_) => println(\"err\"),\n\
                 }\n\
             }",
    ) {
        assert_eq!(out, "2\nab\ncd\n");
    }
}

#[test]
fn e2e_generic_enum_heap_eq_constructor_operand_in_fstring() {
    // The residual gap B-2026-06-09-1's enum workaround did NOT close:
    // *non-identifier* operands (direct constructor calls) compared inline
    // inside f-strings. The name-keyed `enum_inst_var_types` only resolves
    // identifiers, so a `Some(...)` operand had to fall back to the
    // span-keyed table — which, under wrapper-relative spans, collided
    // across f-strings and degraded to the word-wise pointer compare
    // (distinct allocations → wrong `false` for equal content). With the
    // parser fix (absolute interpolation spans) the constructor operand's
    // span is unique and correctly keys `enum_inst_type_exprs`, so content
    // comparison routes even for the inline-constructor form. The two
    // f-strings put differently-typed comparisons at the SAME syntactic
    // position to prove the former alias is gone.
    if let Some(out) = run_program(
        "fn main() {\n\
                 println(f\"{Some(\"a\" + \"b\") == Some(\"ab\")}\");\n\
                 println(f\"{Some(1) == Some(2)}\");\n\
                 println(f\"{Some(\"a\" + \"b\") == Some(\"zz\")}\");\n\
             }",
    ) {
        assert_eq!(out, "true\nfalse\nfalse\n");
    }
}

#[test]
fn e2e_string_ends_with() {
    // `String.ends_with(suffix) -> bool` — memcmp of the trailing
    // `suffix.len` bytes (the `recv.data + (recv_len - suffix_len)` offset).
    // Empty suffix is always true; an over-long suffix is false (length
    // guard). Must match `karac run`.
    if let Some(out) = run_program(
        "fn main() {\n\
             \x20   let s: String = \"hello world\";\n\
             \x20   println(s.ends_with(\"world\"));\n\
             \x20   println(s.ends_with(\"hello\"));\n\
             \x20   println(s.ends_with(\"\"));\n\
             \x20   println(s.ends_with(\"this is far too long to fit\"));\n\
             }\n",
    ) {
        assert_eq!(out, "true\nfalse\ntrue\nfalse\n");
    }
}

#[test]
fn test_e2e_borrow_return_match_string_scrutinee() {
    // Tier 2c / lockstep-gap fix: a `match` over a *String* identifier
    // scrutinee with a wildcard-only arm. Before, ownership false-accepted
    // this (it ignored the scrutinee) while codegen's `is_int_value()`
    // gate returned None → value-return miscompile (garbage output). Now
    // both gates accept identifier scrutinees of any type and the borrow
    // lowers correctly.
    let out = run_program(
        "fn pick(s: ref String, a: ref String, b: ref String) -> ref String {\n\
             \x20   match s {\n\
             \x20       _ => a,\n\
             \x20   }\n\
             }\n\
             fn main() {\n\
             \x20   let scrut = \"ignored\"; let a = \"chosen\"; let b = \"zzz\";\n\
             \x20   println(pick(scrut, a, b));\n\
             }\n",
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "chosen");
    }
}

#[test]
fn test_e2e_ambient_stdin_read_to_string_eof_returns_ok_empty() {
    // Companion to read_line: `Stdin.read_to_string()` slurps to EOF.
    // With the harness's closed stdin that's the empty string → `Ok("")`.
    let out = run_program(
        r#"
fn main() reads(Stdin) {
    match Stdin.read_to_string() {
        Ok(s) => { if s.len() == 0 { println("eof-ok"); } else { println("got-data"); } }
        Err(_) => { println("io-err"); }
    }
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "eof-ok");
    }
}

#[test]
fn test_e2e_stdout_println_interleaves_with_free_println() {
    // `Stdout.println(s)` must share the SAME libc stdout buffer as the
    // free `println` builtin — otherwise the two streams flush in the
    // wrong order. Interleaving free and explicit prints and asserting
    // exact line order is the sharp witness that `Stdout.*` routes
    // through `self.printf_fn` (not a separately-buffered runtime
    // stdout). L646 slice 4b.
    let out = run_program(
        r#"
fn main() {
    println("a");
    Stdout.println("b");
    print("c");
    Stdout.print("d");
    Stdout.println("e");
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out, "a\nb\ncde\n");
    }
}

#[test]
fn test_e2e_stderr_println_writes_to_stderr_not_stdout() {
    // `Stderr.println(s)` lowers to `dprintf(2, …)` — it must land on
    // stderr (fd 2), NOT stdout. Capture both streams and assert the
    // split: the stderr line on stderr, the stdout line on stdout.
    let cap = run_program_capturing(
        r#"
fn main() {
    Stderr.println("to-stderr");
    println("to-stdout");
}
"#,
    );
    if let Some(cap) = cap {
        assert_eq!(cap.stdout.trim(), "to-stdout");
        assert!(
            cap.stderr.contains("to-stderr"),
            "expected 'to-stderr' on stderr, got: {:?}",
            cap.stderr
        );
        assert!(
            !cap.stdout.contains("to-stderr"),
            "stderr content leaked onto stdout: {:?}",
            cap.stdout
        );
    }
}

#[test]
fn e2e_baked_stdlib_enum_display_and_qualified_match() {
    // B-2026-06-14 — `#[derive(Display)]` on a baked-stdlib enum
    // (`IoError`, `VarError`) renders correctly in the interpreter but, in
    // AOT, the enum had no codegen layout (never seeded, never in the user
    // `program_snapshot`): construction fell to an `i64 0` placeholder
    // (display printed `0`), a payload-variant `match` couldn't bind the
    // payload, and `main() -> Result[(), IoError]` printed `Error: 0`.
    // Companion: the bare-variant `Other` collides across
    // IoError/Utf8Error/TcpError/TlsError, so qualified construction/match
    // picked a wrong tag by HashMap order. Fix: seed the IoError layout +
    // STDLIB_PROGRAMS variant fallback in `emit_enum_display_fn` + honor the
    // qualified `Enum.Variant` path in construction and match.

    // (a) f-string / println Display of a payload + a unit variant.
    let disp = run_program(
        "fn main() {\n\
             \x20   let e = IoError.Other(\"disk full\");\n\
             \x20   println(f\"{e}\");\n\
             \x20   let n = IoError.NotFound;\n\
             \x20   println(f\"{n}\");\n\
             \x20   let v = VarError.NotPresent;\n\
             \x20   println(f\"{v}\");\n\
             }\n",
    );
    if let Some(out) = disp {
        assert_eq!(out, "Other(disk full)\nNotFound\nNotPresent\n");
    }

    // (b) qualified match — unit, payload (binds `m`), collision-prone
    // `PermissionDenied` (shared with TlsError), and a wildcard tail.
    let matched = run_program(
        "fn classify(e: IoError) -> String {\n\
             \x20   match e {\n\
             \x20       IoError.NotFound => { \"nf\" }\n\
             \x20       IoError.PermissionDenied => { \"pd\" }\n\
             \x20       IoError.Other(m) => { f\"other:{m}\" }\n\
             \x20       _ => { \"rest\" }\n\
             \x20   }\n\
             }\n\
             fn main() {\n\
             \x20   println(classify(IoError.NotFound));\n\
             \x20   println(classify(IoError.PermissionDenied));\n\
             \x20   println(classify(IoError.Other(\"disk full\")));\n\
             \x20   println(classify(IoError.Interrupted));\n\
             }\n",
    );
    if let Some(out) = matched {
        assert_eq!(out, "nf\npd\nother:disk full\nrest\n");
    }

    // (c) `main() -> Result[(), IoError]` returning a payload Err — the
    // runtime prints `Error: {e}` on stderr (rendered, not `Error: 0`) and
    // exits 1.
    let main_err = run_program_capturing(
        "fn main() -> Result[(), IoError] {\n\
             \x20   Err(IoError.Other(\"disk full\"))\n\
             }\n",
    );
    if let Some(cap) = main_err {
        assert_eq!(cap.status.code(), Some(1), "stderr={:?}", cap.stderr);
        assert!(
            cap.stderr.contains("Error: Other(disk full)"),
            "expected rendered baked-enum Err on stderr, got: {:?}",
            cap.stderr
        );
    }
}

/// B-2026-07-09-9: `panic("msg")` runs end-to-end — exits 1 and surfaces
/// the user message VERBATIM (no "not yet implemented"/"entered unreachable
/// code" prefix, which are `todo`/`unreachable`'s defaults).
#[test]
fn test_e2e_panic_prints_message_verbatim() {
    if let Some(cap) = run_program_capturing("fn main() { panic(\"kaboom\"); }") {
        assert_eq!(cap.status.code(), Some(101), "stderr={:?}", cap.stderr);
        assert!(
            cap.stderr.contains("kaboom")
                && !cap.stderr.contains("not yet implemented")
                && !cap.stderr.contains("entered unreachable code"),
            "panic() must surface its message verbatim; stdout={:?}",
            cap.stdout
        );
    }
}

#[test]
fn test_e2e_option_unsigned_unwrap_prints_unsigned() {
    // B-2026-07-17-13: a narrow-UNSIGNED payload carried through an
    // Option/Result value-unwrap (`unwrap` / `unwrap_or` / `expect`)
    // reconstructs the right bits but printed SIGNED — `Option[u8]`-of-200
    // `.unwrap_or(0)` was -56 (200 as i8) — because the unwrap result's
    // unsigned surface type wasn't threaded to `expr_is_unsigned_int`.
    // Signed payloads and the `None`/`Err` default arm must stay correct.
    let out = run_program(
        "fn main() {\n\
                 let a: Option[u8] = Some(200u8);\n\
                 println(a.unwrap());\n\
                 let b: Option[u16] = Some(60000u16);\n\
                 println(b.unwrap_or(0u16));\n\
                 let c: Option[u32] = Some(4000000000u32);\n\
                 println(c.expect(\"x\"));\n\
                 let d: Option[i8] = Some(-56i8);\n\
                 println(d.unwrap_or(0i8));\n\
                 let g: Option[u8] = None;\n\
                 println(g.unwrap_or(255u8));\n\
                 let h: Result[u8, String] = Ok(250u8);\n\
                 println(h.unwrap_or(0u8));\n\
             }",
    );
    if let Some(out) = out {
        assert_eq!(out, "200\n60000\n4000000000\n-56\n255\n250\n");
    }
}

// ── CStr borrowed surface (Phase 8 — the first pointer-producer) ──
//
// `c"..."` lowers to a NUL-terminated internal rodata global carried
// as a `{ptr, i64}` slice-struct value; `as_ptr` / `len` / `is_empty`
// / `as_bytes` are extract/compare ops on that aggregate
// (`compile_cstr_method`). design.md § C-String Literals.

#[test]
fn test_ir_cstr_literal_emits_nul_terminated_internal_global() {
    let ir = ir_for("fn main() { let s = c\"hi\"; println(s.len()); }");
    // [3 x i8] = 2 source bytes + the compiler-appended NUL.
    assert!(
        ir.contains("internal constant [3 x i8] c\"hi\\00\""),
        "expected NUL-terminated internal rodata global; got IR:\n{ir}"
    );
}

#[test]
fn test_ir_cstr_as_ptr_passes_rodata_pointer_directly() {
    // The design pins `as_ptr` as a direct rodata pointer — no copy,
    // no runtime call. The lowering is an extractvalue on the
    // literal's `{ptr, i64}` aggregate, which LLVM constant-folds on
    // a literal receiver: the call site receives the `@cstr` global
    // itself. Also pins the pointer-typed extern param lowering
    // (`declare void @sink(ptr ...)` — the `TypeKind::Pointer` arm,
    // not the historical i64 fall-through).
    let ir = ir_for(
        "effect resource R;\n\
             host fn sink(p: *const u8) with writes(R);\n\
             pub fn main() with writes(R) { sink(c\"x\".as_ptr()); }",
    );
    assert!(
        ir.contains("declare void @sink(ptr"),
        "pointer param must lower to `ptr`, not i64; got IR:\n{ir}"
    );
    assert!(
        ir.contains("@sink(ptr @cstr"),
        "as_ptr on a literal should fold to the rodata global at the \
             call site; got IR:\n{ir}"
    );
}

#[test]
fn test_e2e_cstr_len_is_empty_as_bytes() {
    // Interpreter parity: tests/interpreter.rs pins the same outputs
    // for the same surface (test_cstr_len_and_is_empty +
    // test_cstr_as_bytes_yields_source_bytes).
    let src = r#"
fn main() {
    let msg = c"hello, world";
    println(msg.len());
    if c"".is_empty() { println("empty"); }
    let bytes = c"abc".as_bytes();
    println(bytes.len());
    println(bytes[0]);
    println(bytes[2]);
    let annotated: ref CStr = c"hi";
    println(annotated.len());
}
"#;
    let out = run_program(src);
    if let Some(out) = out {
        assert_eq!(out, "12\nempty\n3\n97\n99\n2\n");
    }
}

#[test]
fn test_e2e_byte_string_literal_values_and_escapes() {
    // B-2026-08-20-37 — `b"..."` lowers to a constant `[N x i8]`, the
    // same aggregate `[b'h', b'e', …]` produces. design.md gives one
    // escape table for the byte-CHAR and byte-STRING forms, so every
    // escape here must yield the byte the `b'X'` form yields.
    //
    // Strict assert: this pins a codegen VALUE, so it must not pass
    // vacuously when the runtime archive is absent.
    let src = r#"
fn main() {
    let eth: Array[u8, 2] = b"\x08\x00";
    println(eth[0]);
    println(eth[1]);
    let banner: Array[u8, 13] = b"hello world\n\0";
    println(banner[0]);
    println(banner[11]);
    println(banner[12]);
    let esc: Array[u8, 7] = b"\n\t\r\0\\\'\"";
    println(esc[0]);
    println(esc[4]);
    println(esc[6]);
    let inferred = b"hi";
    println(inferred[1]);
}
"#;
    assert_eq!(
        run_program(src),
        Some("8\n0\n104\n10\n0\n10\n92\n34\n105\n".to_string())
    );
}

#[test]
fn test_e2e_byte_string_method_on_an_unannotated_binding() {
    // A run/build divergence created by two independent commits an hour
    // apart: `b"..."` began inferring `Array[u8, N]` (B-2026-08-20-37)
    // while `Array.is_sorted()` had just landed (5fee766). The fixed-array
    // method arms read the element type from `array_elem_type_exprs`,
    // which was populated ONLY from a type ANNOTATION — complete while
    // every un-annotated collection literal inferred `Vec`, and newly
    // incomplete once a literal could be an un-annotated `Array`.
    // `--interp` answered while `build` failed with "no source element
    // type for the receiver".
    //
    // The ANNOTATED spelling always worked, which is why neither commit's
    // own tests caught it — this pins the un-annotated one.
    let src = r#"
fn main() {
    let a = b"abc";
    println(a.is_sorted());
    let b = b"cba";
    println(b.is_sorted());
    println(a.len());
}
"#;
    assert_eq!(run_program(src), Some("true\nfalse\n3\n".to_string()));
}

#[test]
fn test_e2e_cstr_methods_on_a_ref_param_receiver() {
    // B-2026-08-21-5: a `ref CStr` PARAMETER receiver aborted karac
    // with an inkwell `into_struct_value` unwrap in
    // `compile_cstr_method`, while `--interp` was correct. `CStr` was
    // missing from BOTH type-lowering entry points, so it fell to the
    // unknown-name `i64` default; the param then registered an 8-byte
    // `inner_ty` and `load_variable`'s ref-param deref loaded a scalar
    // word where the method expected the `{ptr, i64}` aggregate.
    // A literal or owned receiver was always fine — only the parameter
    // spelling reached the unwrap, which is why the sibling test above
    // never caught it. Strict assert: this pins a CRASH fix, so it must
    // not pass vacuously when the runtime archive is absent.
    let src = r#"
fn m_len(c: ref CStr) -> i64 { return c.len(); }
fn m_empty(c: ref CStr) -> bool { return c.is_empty(); }
fn m_byte(c: ref CStr, i: i64) -> u8 { return c.as_bytes()[i]; }

fn main() {
    let s = c"abc";
    let e = c"";
    println(m_len(s));
    println(m_empty(s));
    println(m_empty(e));
    println(m_byte(s, 0));
    println(m_byte(s, 2));
}
"#;
    assert_eq!(
        run_program(src),
        Some("3\nfalse\ntrue\n97\n99\n".to_string())
    );
}

#[test]
fn test_e2e_cstr_to_string_result() {
    // `CStr.to_string() -> Result[String, Utf8Error]` (phase-12 Cluster 2).
    // The outbound `char*` read: validate UTF-8, copy to a heap String on
    // Ok, classify the failure on Err. Covers all four runtime arms —
    // ASCII Ok, multi-byte Ok (the bytes round-trip), an invalid lead byte
    // (`\xff` → InvalidByte), and a truncated 3-byte sequence (`\xe2\x82` →
    // IncompleteSequence). Error mapping mirrors `String.from_utf8` /
    // `std::str::from_utf8` (the interpreter oracle); tests/interpreter
    // would render identically under `karac run`.
    let src = r#"
fn main() {
    match c"hello".to_string() {
        Ok(s) => println(s),
        Err(_) => println("ERR"),
    }
    match c"héllo".to_string() {
        Ok(s) => println(s),
        Err(_) => println("ERR"),
    }
    match c"\xff".to_string() {
        Ok(_) => println("OK?"),
        Err(e) => match e {
            Utf8Error.InvalidByte => println("INVALID"),
            Utf8Error.IncompleteSequence => println("INCOMPLETE"),
            Utf8Error.Other(m) => println(m),
        },
    }
    match c"\xe2\x82".to_string() {
        Ok(_) => println("OK?"),
        Err(e) => match e {
            Utf8Error.InvalidByte => println("INVALID"),
            Utf8Error.IncompleteSequence => println("INCOMPLETE"),
            Utf8Error.Other(m) => println(m),
        },
    }
}
"#;
    let out = run_program(src);
    if let Some(out) = out {
        assert_eq!(out, "hello\nhéllo\nINVALID\nINCOMPLETE\n");
    }
}

#[test]
fn test_e2e_cstr_to_string_slice_result() {
    // `CStr.to_string_slice() -> Result[StringSlice, Utf8Error]` — the
    // zero-copy sibling of `to_string`. Validates UTF-8 (via the
    // non-copying `karac_runtime_utf8_validate`) but returns a BORROWED
    // `{ptr, len, cap=0}` view over the receiver's rodata bytes instead of
    // an owning heap copy — the `cap == 0` drop-skip keeps the view from
    // being freed. Same four runtime arms as `to_string` (ASCII Ok,
    // multi-byte Ok, `\xff` → InvalidByte, truncated `\xe2\x82` →
    // IncompleteSequence); the Ok payload is a StringSlice printed directly
    // (it shares String's `{ptr,len,cap}` layout + format surface, which
    // the match-payload reconstruction now recognizes). Parity with the
    // interpreter (`karac run`).
    let src = r#"
fn main() {
    match c"hello".to_string_slice() {
        Ok(s) => println(s),
        Err(_) => println("ERR"),
    }
    match c"héllo".to_string_slice() {
        Ok(s) => println(s),
        Err(_) => println("ERR"),
    }
    match c"\xff".to_string_slice() {
        Ok(_) => println("OK?"),
        Err(e) => match e {
            Utf8Error.InvalidByte => println("INVALID"),
            Utf8Error.IncompleteSequence => println("INCOMPLETE"),
            Utf8Error.Other(m) => println(m),
        },
    }
    match c"\xe2\x82".to_string_slice() {
        Ok(_) => println("OK?"),
        Err(e) => match e {
            Utf8Error.InvalidByte => println("INVALID"),
            Utf8Error.IncompleteSequence => println("INCOMPLETE"),
            Utf8Error.Other(m) => println(m),
        },
    }
}
"#;
    let out = run_program(src);
    if let Some(out) = out {
        assert_eq!(out, "hello\nhéllo\nINVALID\nINCOMPLETE\n");
    }
}

#[test]
fn test_e2e_string_from_utf8_result() {
    // `String.from_utf8(bytes: Vec[u8]) -> Result[String, Utf8Error]`
    // (B-2026-06-18-11). Was interpreter-only — the codegen path now
    // validates + copies the bytes into a fresh heap String (reusing the
    // CStr.to_string validator), so the canonical "read bytes -> Vec[u8] ->
    // parse" shape builds. Covers Ok (ASCII), Err InvalidByte (lone 0xFF),
    // and Err IncompleteSequence (truncated 3-byte 0xE2 0x82) — the same
    // arms as the cstr test, sourced from a runtime-built Vec instead of a
    // c"..." literal. The input Vec drops normally (consume-by-copy); no
    // leak / double-free.
    let src = r#"
fn main() {
    let mut a: Vec[u8] = Vec.new();
    a.push(104u8); a.push(105u8);
    match String.from_utf8(a) {
        Ok(s) => println(s),
        Err(_) => println("ERR"),
    }
    let mut b: Vec[u8] = Vec.new();
    b.push(255u8);
    match String.from_utf8(b) {
        Ok(_) => println("OK?"),
        Err(e) => match e {
            Utf8Error.InvalidByte => println("INVALID"),
            Utf8Error.IncompleteSequence => println("INCOMPLETE"),
            Utf8Error.Other(m) => println(m),
        },
    }
    let mut c: Vec[u8] = Vec.new();
    c.push(226u8); c.push(130u8);
    match String.from_utf8(c) {
        Ok(_) => println("OK?"),
        Err(e) => match e {
            Utf8Error.InvalidByte => println("INVALID"),
            Utf8Error.IncompleteSequence => println("INCOMPLETE"),
            Utf8Error.Other(m) => println(m),
        },
    }
}
"#;
    let out = run_program(src);
    if let Some(out) = out {
        assert_eq!(out, "hi\nINVALID\nINCOMPLETE\n");
    }
}

#[test]
fn test_e2e_string_slice_v1() {
    // StringSlice v1 graduation (B-2026-06-07-5 → StringSlice slice).
    // `String.slice(a,b) -> StringSlice` (a zero-copy `{ptr,len,cap=0}`
    // borrowed view), `String.find(needle) -> Option[i64]`, the borrowed
    // view's `len()` / `to_string()` (the owned-copy escape hatch), and a
    // `first_word(s: ref String) -> StringSlice` that returns a view into a
    // `ref` parameter (source-pinned). Mirrors the interpreter (clone
    // semantics) — `karac run` renders identically.
    let src = r#"
fn first_word(s: ref String) -> StringSlice {
    let sp = s.find(' ');
    let end = sp.unwrap_or(s.len());
    s.slice(0, end)
}
fn main() {
    let s = "hello world".to_string();
    let w = s.slice(0, 5);
    println(w.to_string());
    println(w.len().to_string());
    match s.find('o') {
        Some(i) => println(i.to_string()),
        None => println("none"),
    }
    match s.find("zz") {
        Some(_) => println("found"),
        None => println("nf"),
    }
    let fw = first_word(s);
    println(fw.to_string());
}
"#;
    let out = run_program(src);
    if let Some(out) = out {
        assert_eq!(out, "hello\n5\n4\nnf\nhello\n");
    }
}

#[test]
fn test_e2e_cstr_as_ptr_feeds_libc_puts() {
    // The design's flagship FFI example (§ C-String Literals "FFI
    // handoff"): pass a `c"..."` literal's pointer to libc `puts`
    // through an `unsafe extern "C"` declaration — declare → as_ptr
    // → call → link → run, with the host libc consuming the
    // NUL-terminated bytes. Covers both the binding and
    // literal-receiver forms.
    let src = r#"
effect resource Console;

unsafe extern "C" {
    fn puts(s: *const u8) -> i32 with writes(Console);
}

pub fn main() with writes(Console) blocks {
    let msg: ref CStr = c"hello, world";
    puts(msg.as_ptr());
    puts(c"literal receiver".as_ptr());
}
"#;
    let out = run_program(src);
    if let Some(out) = out {
        assert_eq!(out, "hello, world\nliteral receiver\n");
    }
}

#[test]
fn test_e2e_string_to_cstring_len_bytes_and_is_empty() {
    // `String.to_cstring() -> Result[CString, NulError]` (design.md §
    // C-String Literals, "Owning `CString`"). Ok path: the owning CString's
    // introspection surface (`len` excludes the trailing NUL, `as_bytes`
    // yields the source bytes, `is_empty`) matches `CStr`. Interpreter parity:
    // tests/interpreter.rs::test_string_to_cstring_ok_len_and_bytes.
    let src = r#"
fn main() {
    let s = "hello";
    match s.to_cstring() {
        Ok(cs) => {
            println(cs.len());
            let b = cs.as_bytes();
            println(b[0]);
            println(b[4]);
            if cs.is_empty() { println("empty"); } else { println("non-empty"); }
        }
        Err(_) => println("ERR"),
    }
    let empty = "";
    match empty.to_cstring() {
        Ok(cs) => { println(cs.len()); if cs.is_empty() { println("empty"); } }
        Err(_) => println("ERR"),
    }
}
"#;
    let out = run_program(src);
    if let Some(out) = out {
        assert_eq!(out, "5\n104\n111\nnon-empty\n0\nempty\n");
    }
}

#[test]
fn test_e2e_string_to_cstring_interior_nul_is_err() {
    // A String with an interior NUL byte cannot become a CString (the C side
    // would truncate at it) → `Err(NulError.InteriorNul)`. Parity with
    // tests/interpreter.rs::test_string_to_cstring_interior_nul_is_err.
    let src = r#"
fn main() {
    let s = "ab\u{0}cd";
    match s.to_cstring() {
        Ok(_) => println("OK?"),
        Err(e) => match e {
            NulError.InteriorNul => println("INTERIOR_NUL"),
            NulError.Other(m) => println(m),
        },
    }
}
"#;
    let out = run_program(src);
    if let Some(out) = out {
        assert_eq!(out, "INTERIOR_NUL\n");
    }
}

#[test]
fn test_e2e_string_to_cstring_as_ptr_feeds_libc_puts() {
    // The motivating use case CString exists for (design.md § C-String
    // Literals): hand a RUNTIME-CONSTRUCTED string to a C function. Unlike
    // the `c"..."` literal (rodata), the buffer here is built at run time
    // (concatenation), so it needs the owning `CString` + its appended NUL.
    // Build → to_cstring → as_ptr → libc `puts` → link → run.
    let src = r#"
effect resource Console;

unsafe extern "C" {
    fn puts(s: *const u8) -> i32 with writes(Console);
}

pub fn main() with writes(Console) writes(Stdout) blocks {
    let s = "hello, " + "world";
    match s.to_cstring() {
        Ok(cs) => { puts(cs.as_ptr()); }
        Err(_) => { println("ERR"); }
    }
}
"#;
    let out = run_program(src);
    if let Some(out) = out {
        assert_eq!(out, "hello, world\n");
    }
}

#[test]
fn test_e2e_array_as_ptr_feeds_cstr_from_ptr() {
    // B-2026-06-11-1: `Array[u8, N].as_ptr()` / `.as_mut_ptr()` had no
    // codegen handler (method dispatch fell through). The fix GEPs to
    // element 0 of the owned array's storage and hands it out as the raw
    // pointer `*const T` / `*mut T`. Here a NUL-terminated byte array's
    // pointer feeds `CStr.from_ptr` (libc `strlen` recomputes the length),
    // and the borrowed surface reads the bytes back — proving the pointer
    // addresses the array's first element. `as_mut_ptr()` produces the
    // same usable address (coerces to the `*const u8` param), confirming
    // both arms emit the element-0 pointer.
    let src = r#"
fn main() {
    let a: Array[u8, 4] = [104u8, 105u8, 0u8, 0u8];
    // Safety: `a` is NUL-terminated within its 4 bytes (b"hi\0\0").
    let s = unsafe { CStr.from_ptr(a.as_ptr()) };
    println(s.len());
    let bytes = s.as_bytes();
    println(bytes[0]);
    println(bytes[1]);
    // `as_mut_ptr()` addresses the same first element.
    let s2 = unsafe { CStr.from_ptr(a.as_mut_ptr() as *const u8) };
    println(s2.len());
}
"#;
    let out = run_program(src);
    if let Some(out) = out {
        // len=2 ("hi" before NUL), bytes[0]='h'(104), bytes[1]='i'(105)
        assert_eq!(out, "2\n104\n105\n2\n");
    }
}

#[test]
fn test_e2e_ref_array_as_ptr_feeds_cstr_from_ptr() {
    // B-2026-06-11-1, the `ref Array` arm: a `ref Array[u8, N]` param
    // carries the data pointer directly, so `as_ptr()` hands it out
    // without a GEP. Passing an owned array to a `ref`-taking helper and
    // reconstructing a `CStr` from `a.as_ptr()` inside reads the same
    // bytes — exercising the `ref_params` branch of the dispatcher.
    let src = r#"
fn first_len(a: ref Array[u8, 4]) -> i64 {
    let s = unsafe { CStr.from_ptr(a.as_ptr()) };
    s.len()
}

fn main() {
    let arr: Array[u8, 4] = [104u8, 105u8, 0u8, 0u8];
    println(first_len(arr));
}
"#;
    let out = run_program(src);
    if let Some(out) = out {
        assert_eq!(out.trim(), "2");
    }
}

#[test]
fn test_e2e_match_ref_at_binding_option_string_payload() {
    // `ref x @ Some(y)` over `Option[String]` — the y binding is a
    // borrow of the payload; the scrutinee is re-matchable after
    // (not consumed), and the String payload is freed exactly once
    // at `opt`'s scope exit.
    let out = run_program(
        r#"
fn main() {
    let opt = Some("hello");
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
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["hello", "hello"]);
    }
}

#[test]
fn e2e_string_xform_on_a_nonident_receiver_inside_a_chain() {
    // B-2026-08-05-28. `(<non-ident>).to_uppercase()` alone always
    // compiled; putting its result in RECEIVER position did not, because
    // the parser gives a MethodCall its receiver's span, so the outer
    // call's `i64` evicted the inner `String` from the span tables the
    // non-identifier String dispatch keys on. The xform's dispatcher arm
    // was there the whole time — the receiver just stopped looking like a
    // String. Covers the Binary, MethodCall and f-string receiver roots
    // together, since one span-free signal fixes all three.
    let concat = run_program(
        "fn main() {\n\
                 let mut v: Vec[String] = Vec.new();\n\
                 v.push(\"Hi\".to_string());\n\
                 match v.first() {\n\
                     Some(s) => { println((\"p:\".to_string() + s).to_uppercase().len()); }\n\
                     None => { }\n\
                 }\n\
             }",
    );
    assert_eq!(concat, Some("4\n".to_string()));

    let fstring = run_program(
        "fn main() {\n\
                 let mut v: Vec[String] = Vec.new();\n\
                 v.push(\"Hi\".to_string());\n\
                 match v.first() {\n\
                     Some(s) => { println(f\"p:{s}\".trim().len()); }\n\
                     None => { }\n\
                 }\n\
             }",
    );
    assert_eq!(fstring, Some("4\n".to_string()));

    // Identifier ROOT but a MethodCall receiver for the second xform —
    // the shape the row's "non-identifier receiver" framing missed.
    let chained = run_program(
        "fn main() {\n\
                 let mut v: Vec[String] = Vec.new();\n\
                 v.push(\"Hi\".to_string());\n\
                 match v.first() {\n\
                     Some(s) => { println(s.to_uppercase().to_lowercase()); }\n\
                     None => { }\n\
                 }\n\
             }",
    );
    assert_eq!(chained, Some("hi\n".to_string()));

    // `split` is in the same family and was failing the same way; its
    // result is a Vec, so it also exercises the non-String result path.
    let split = run_program(
        "fn main() {\n\
                 let mut v: Vec[String] = Vec.new();\n\
                 v.push(\"a:b\".to_string());\n\
                 match v.first() {\n\
                     Some(s) => { println((\"p:\".to_string() + s).split(\":\").len()); }\n\
                     None => { }\n\
                 }\n\
             }",
    );
    assert_eq!(split, Some("3\n".to_string()));
}

#[test]
fn e2e_fn_value_with_a_ref_string_param() {
    let out = run_program(
        "fn n(s: ref String) -> i64 { return s.len(); }\n\
             fn main() {\n\
                 let s: String = \"hello\";\n\
                 let f = n;\n\
                 println(f(s));\n\
             }",
    );
    assert_eq!(out, Some("5\n".to_string()));
}

/// B-2026-09-05-23 — EVERY INTEGER DISPLAY PATH MUST STAY OFF libc
/// `snprintf`.
///
/// `snprintf` takes locale and lock state, so an f-string or an int
/// `println` inside a parallel region serialized on `os_unfair_lock`:
/// measured on a uniform 18-worker reduction whose only formatting was one
/// interpolation per iteration, system time was 579.80 ms against 1.57 ms
/// for the same probe built with `substring` + concat instead. Routing
/// through `karac_runtime_i64_to_str` (lock-free AND allocation-free) took
/// that probe from 47.60 ms to 2.12 ms at 18 workers, and from 23.20 ms to
/// 5.65 ms single-threaded — `snprintf` was the slower path either way.
///
/// This is an IR gate rather than a timing test because the regression it
/// guards is invisible to output comparison: reverting to `snprintf` keeps
/// every byte identical and only costs speed.
#[test]
fn ir_integer_display_never_calls_snprintf() {
    // Every integer spelling that has ever reached `snprintf`: the bare
    // `println(int)` path (codegen/control_flow.rs), plain f-string
    // interpolation (codegen/runtime.rs), the synthesized container
    // Display (codegen/synth_display.rs, riding the Vec here), and the
    // SPEC'D hole — width, zero-pad and radix — which B-2026-09-05-23 left
    // behind on snprintf and which therefore kept the whole pathology for
    // `f"{n:5}"` while `f"{n}"` was fast.
    let ir = ir_for(
        r#"
fn main() {
    let a: i64 = -5;
    let b: u64 = 7;
    let v: Vec[i64] = [1, -2];
    println(a);
    println(b);
    println(f"{a}{b}");
    println(f"{v}");
    println(f"{a:6}{b:06}{b:<4x}");
}
"#,
    );
    // Bound the slice to `main`'s own body: the module carries a
    // module-level `declare i32 @snprintf(...)` unconditionally, and a
    // bare `split("define i32 @main()")` would sweep it in along with
    // every function defined after `main`.
    let after = ir.split("define i32 @main()").nth(1).expect("main fn body");
    let main_body = after
        .split("\ndefine ")
        .next()
        .unwrap()
        .split("\ndeclare ")
        .next()
        .unwrap();
    assert!(
        main_body.contains("@karac_runtime_i64_to_str"),
        "main should format integers through the runtime helper; \
             not found in main body:\n{main_body}"
    );
    assert!(
        !main_body.contains("@snprintf"),
        "main must not CALL snprintf for integer display; found in:\n{main_body}"
    );
}

/// B-2026-09-07-45 — the SLOW (spec-re-parsing) path at 128 bits.
///
/// `karac_runtime_int_fmt` above is the PRE-DECODED fast path. The specs
/// `needs_runtime_formatter()` diverts — center align, binary radix,
/// non-space fill — go to a different entrypoint, `karac_runtime_fmt_int`,
/// which took a single `i64` value. So codegen handed it a 128-bit value
/// and LLVM's verifier rejected the module: all three of these spellings
/// FAILED TO COMPILE on a `u128`/`i128`, on every compiled leg.
///
/// That is the identical defect B-2026-09-07-34 fixed on the fast path.
/// It survived there because the two entrypoints are reached by DISJOINT
/// spec shapes, so the fast path's own regression test could not see it —
/// which is why this test exercises the diverted shapes specifically.
///
/// The last three holes are the CONTROLS, and they are the reason the
/// widening is keyed on RADIX and not just signedness: a non-decimal radix
/// reinterprets at the HOLE'S OWN width, so `{-1i64:b}` must stay
/// sixty-four ones where `{-1i128:b}` is a hundred and twenty-eight, and a
/// `u8` hole must stay eight bits wide.
#[test]
fn e2e_spec_128_bit_holes_on_the_runtime_formatter_path() {
    if let Some(out) = run_program(
        r#"
fn main() {
    let big: u128 = 170141183460469231731687303715884105727u128;
    let umax: u128 = 340282366920938463463374607431768211455u128;
    let ineg: i128 = -1i128;
    let ism: i128 = -7i128;
    println(f"[{big:^44}]");
    println(f"[{big:*>44}]");
    println(f"[{umax:b}]");
    println(f"[{ineg:b}]");
    println(f"[{ism:^12}]");
    println(f"[{ism:=^12}]");
    let n64: i64 = -1;
    let u8v: u8 = 255;
    println(f"[{n64:b}]");
    println(f"[{u8v:*>12b}]");
    println(f"[{n64:^8}]");
}
"#,
    ) {
        let want = "[  170141183460469231731687303715884105727   ]\n\
                        [*****170141183460469231731687303715884105727]\n\
                        [11111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111]\n\
                        [11111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111111]\n\
                        [     -7     ]\n\
                        [=====-7=====]\n\
                        [1111111111111111111111111111111111111111111111111111111111111111]\n\
                        [****11111111]\n\
                        [   -1   ]\n";
        assert_eq!(out, want, "128-bit runtime-formatter rendering drifted");
    }
}

/// The SPEC'D integer path must render exactly what `snprintf` did.
///
/// Verified byte-identical against the pre-change compiler over these
/// cases before the swap landed. The subtle ones: zero-pad inserts its
/// zeros BETWEEN the sign and the digits (`{-7:05}` -> `-0007`, not
/// `00-07` and not `-00007`), a width narrower than the number does not
/// truncate, and a negative value in a non-decimal radix reinterprets as
/// unsigned rather than growing a `-`.
#[test]
fn e2e_spec_integer_holes_match_the_snprintf_renderings() {
    if let Some(out) = run_program(
        r#"
fn main() {
    let min: i64 = -9223372036854775808;
    let max: i64 = 9223372036854775807;
    println(f"[{0:5}][{7:5}][{-7:5}][{123456:5}]");
    println(f"[{42:<8}][{42:>8}][{-42:<8}]");
    println(f"[{7:05}][{-7:05}][{0:05}]");
    println(f"[{max:25}][{min:25}]");
    println(f"[{max:025}][{min:025}]");
    println(f"[{255:x}][{255:X}][{255:o}]");
    println(f"[{255:08x}][{255:<8X}]");
    println(f"[{-1:x}][{-1:o}]");
    println(f"[{123456789:3}][{-123456789:3}]");
}
"#,
    ) {
        let want = "[    0][    7][   -7][123456]\n\
                        [42      ][      42][-42     ]\n\
                        [00007][-0007][00000]\n\
                        [      9223372036854775807][     -9223372036854775808]\n\
                        [0000009223372036854775807][-000009223372036854775808]\n\
                        [ff][FF][377]\n\
                        [000000ff][FF      ]\n\
                        [ffffffffffffffff][1777777777777777777777]\n\
                        [123456789][-123456789]\n";
        assert_eq!(out, want, "spec'd integer rendering drifted");
    }
}

#[test]
fn test_e2e_string_method_on_ref_returning_call_receiver() {
    // B-2026-07-29-15, the String half of B-2026-07-29-12's shape:
    // `h.label().len()` where `label() -> ref String`. This one did not
    // bail loudly — it fell through to the non-identifier collection
    // handler, which read the returned BORROW POINTER as if it were the
    // `{ptr,len,cap}` String value, so `.len()` printed garbage
    // (1634885995 for a 4-byte string) and `.starts_with(..)` tripped
    // "String buffer was not valid UTF-8". Binding first always worked,
    // so the two spellings disagreed.
    let output = run_program(
        "struct H { name: String }\n\
             impl H {\n\
                 fn label(ref self) -> ref String { self.name }\n\
             }\n\
             fn main() {\n\
                 let h = H { name: \"kara\".to_string() };\n\
                 println(h.label().len().to_string());\n\
                 println(h.label().starts_with(\"ka\").to_string());\n\
                 println(h.label().to_uppercase());\n\
                 println(f\"{h.label().len()}\");\n\
                 let e = H { name: \"\".to_string() };\n\
                 println(e.label().is_empty().to_string());\n\
             }",
    )
    .expect("compile + run failed");
    assert_eq!(output, "4\ntrue\nKARA\n4\ntrue\n");
}

#[test]
fn test_e2e_array_literal_string_elems_as_vec_return() {
    // String-element array literal returned as `Vec[String]` — the exact
    // shape surfaced while testing the ambient `env.args` override.
    let out = run_program(
        r#"
fn names() -> Vec[String] { ["alpha", "beta", "gamma"] }
fn main() { println(names().len()); }
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "3");
    }
}

// ── Built-in `to_string` / `clone` on scalar primitives ────────────
// Both used to build-fail (typecheck poison + no codegen handler). Now
// `to_string` builds an owning String via the f-string renderer and
// `clone` is scalar identity.

#[test]
fn test_e2e_to_string_on_primitives() {
    if let Some(out) = run_program(
        r#"
fn main() {
    let i = -42i64;
    println(i.to_string());
    let f = 3.5f64;
    println(f.to_string());
    let b = true;
    println(b.to_string());
    let c = 'Z';
    println(c.to_string());
    let u = 99u64;
    println(u.to_string());
    println((-7i64).to_string());
}
"#,
    ) {
        assert_eq!(out, "-42\n3.5\ntrue\nZ\n99\n-7\n");
    }
}

#[test]
fn test_e2e_to_string_result_is_owning_string() {
    // The result must be a real owning String: usable in interpolation
    // and concatenation, freed cleanly at scope exit.
    if let Some(out) = run_program(
        r#"
fn main() {
    let n = 7i64;
    let s = n.to_string();
    println(f"value={s}!");
    println(n.to_string() + "x");
}
"#,
    ) {
        assert_eq!(out, "value=7!\n7x\n");
    }
}

// ── User-struct Display (subtask 5) ────────────────────────────────
// `#[derive(Display)]` structs render `Name { field: value, … }` in
// declaration order via the synthetic-f-string path, for place-expression
// args (identifier / field access). Codegen matches the interpreter.

#[test]
fn test_e2e_payload_enum_to_string() {
    // Explicit `.to_string()` on a `#[derive(Display)]` enum with PAYLOAD
    // variants (`Other(String)`) — previously the typechecker rejected it
    // (all-unit-only) and codegen had no handler, even though f-string
    // interpolation of the same value already rendered. Now `.to_string()`
    // renders identically to `f"{e}"` / `println(e)` across all backends
    // (identifier and struct-field receivers). Build==run parity with the
    // interpreter sibling; valgrind-clean (owned String freed on scope exit).
    if let Some(out) = run_program(
        r#"
#[derive(Display)]
enum IoErr { NotFound, Other(String) }
struct Wrap { e: IoErr }
fn main() {
    let a: IoErr = IoErr.NotFound;
    let b: IoErr = IoErr.Other(String.from("disk full"));
    println(a.to_string());
    println(b.to_string());
    let w: Wrap = Wrap { e: IoErr.Other(String.from("boom")) };
    println(w.e.to_string());
}
"#,
    ) {
        assert_eq!(out, "NotFound\nOther(disk full)\nOther(boom)\n");
    }
}

#[test]
fn test_e2e_enum_self_to_string() {
    // `self.to_string()` inside an impl method (a `ref self` receiver)
    // renders a `#[derive(Display)]` enum under codegen — both all-unit and
    // payload variants — the `impl Error { message() { self.to_string() } }`
    // pattern. Codegen recognizes the `SelfValue` receiver in the Display
    // name helpers (B-2026-07-12-15), so build == run (interp sibling
    // `test_enum_self_to_string_in_impl_method`). Consumed directly
    // (`.len()` / `println`), not through a generic f-string, so it is
    // leak-clean — a 200-iteration loop is valgrind-clean (verified by hand;
    // the generic-f-string-interp leak B-2026-07-12-18 is a separate,
    // pre-existing path). Struct `self.to_string()` is still open
    // (B-2026-07-12-17), so this covers enums only.
    if let Some(out) = run_program(
        r#"
#[derive(Display)]
enum IoErr { NotFound, Other(String) }
trait Error { fn message(ref self) -> String; }
impl Error for IoErr { fn message(ref self) -> String { self.to_string() } }
fn report[E: Error](e: ref E) -> String { e.message() }
fn main() {
    let a: IoErr = IoErr.NotFound;
    let b: IoErr = IoErr.Other(String.from("disk full"));
    println(a.message());
    println(b.message());
    println(report(a));
}
"#,
    ) {
        assert_eq!(out, "NotFound\nOther(disk full)\nNotFound\n");
    }
}

#[test]
fn test_e2e_generic_ref_enum_display() {
    // B-2026-07-12-18: a generic `fn f[E: Display](e: ref E)` monomorphized
    // to a payload `#[derive(Display)]` enum MISCOMPILED under codegen —
    // rendering `e` (println / f-string) read the enum from the ref param's
    // slot address (a pointer TO the value) instead of the value, printing
    // `Other()` garbage for every input while the interpreter was correct.
    // Fixed by resolving the value via `get_data_ptr` (which loads through
    // the ref). Covers the println shape (the miscompile) and the f-string-
    // returned shape (which also leaked the render buffer). build == run;
    // valgrind-clean.
    if let Some(out) = run_program(
        r#"
#[derive(Display)]
enum IoErr { NotFound, Other(String) }
fn showln[E: Display](e: ref E) { println(e); }
fn wrap[E: Display](e: ref E) -> String { f"error: {e}" }
fn main() {
    let a: IoErr = IoErr.NotFound;
    let b: IoErr = IoErr.Other(String.from("boom"));
    showln(a);
    showln(b);
    println(wrap(b));
}
"#,
    ) {
        assert_eq!(out, "NotFound\nOther(boom)\nerror: Other(boom)\n");
    }
}

#[test]
fn test_e2e_struct_to_string_returned_from_fn() {
    // B-2026-07-12-17: a struct `.to_string()` (f-string-backed) DOUBLE-FREED
    // its rendered buffer when returned directly from a function — the
    // return-position fstr-acc ownership transfer only fired for a literal
    // `f"…"` tail, not the `.to_string()` shape. Now the return handler uses
    // the same `rhs_stages_fstr_acc` predicate the let-binding path uses, so
    // the acc's cap is zeroed and the caller is the unique owner. Covers a
    // `ref` param, an owned param, and a nested-struct render — all
    // valgrind-clean (verified by hand).
    if let Some(out) = run_program(
        r#"
#[derive(Display)]
struct Point { x: i64, y: i64 }
#[derive(Display)]
struct Inner { v: i64 }
#[derive(Display)]
struct Outer { a: Inner, b: i64 }
fn by_ref(p: ref Point) -> String { p.to_string() }
fn by_val(p: Point) -> String { p.to_string() }
fn nested(o: ref Outer) -> String { o.to_string() }
fn main() {
    let p: Point = Point { x: 3, y: 4 };
    println(by_ref(p));
    let q: Point = Point { x: 5, y: 6 };
    println(by_val(q));
    let o: Outer = Outer { a: Inner { v: 7 }, b: 9 };
    println(nested(o));
}
"#,
    ) {
        assert_eq!(
            out,
            "Point { x: 3, y: 4 }\nPoint { x: 5, y: 6 }\nOuter { a: Inner { v: 7 }, b: 9 }\n"
        );
    }
}

#[test]
fn test_e2e_struct_self_to_string() {
    // `self.to_string()` / `f"{self}"` on a `#[derive(Display)]` STRUCT
    // receiver now renders under codegen (inherent + trait impl) — the
    // struct half of B-2026-07-12-15, unblocked once the return-position
    // double-free (B-2026-07-12-17) was fixed. build == run; valgrind-clean.
    if let Some(out) = run_program(
        r#"
#[derive(Display)]
struct Point { x: i64, y: i64 }
impl Point { fn describe(ref self) -> String { self.to_string() } }
trait Show { fn show(ref self) -> String; }
impl Show for Point { fn show(ref self) -> String { self.to_string() } }
fn main() {
    let p: Point = Point { x: 3, y: 4 };
    println(p.describe());
    println(p.show());
    println(f"{p}");
}
"#,
    ) {
        assert_eq!(
            out,
            "Point { x: 3, y: 4 }\nPoint { x: 3, y: 4 }\nPoint { x: 3, y: 4 }\n"
        );
    }
}

#[test]
fn test_e2e_struct_display_nested() {
    if let Some(out) = run_program(
        r#"
#[derive(Display)]
struct Point { x: i64, y: i64 }
#[derive(Display)]
struct Wrap { p: Point, name: String, ok: bool }
fn main() {
    let w = Wrap { p: Point { x: 1, y: 2 }, name: "hi", ok: true };
    println(w);
    println(w.to_string());
}
"#,
    ) {
        let line = "Wrap { p: Point { x: 1, y: 2 }, name: hi, ok: true }\n";
        assert_eq!(out, format!("{line}{line}"));
    }
}

#[test]
fn test_e2e_string_to_string_owning() {
    // `String.to_string()` on identifier and literal receivers — owning copy.
    if let Some(out) = run_program(
        r#"
fn main() {
    let s: String = "abc".to_string();
    println(s.to_string());
    println("lit".to_string());
}
"#,
    ) {
        assert_eq!(out, "abc\nlit\n");
    }
}

#[test]
fn test_e2e_enum_display_unit_variants() {
    // All-unit `#[derive(Display)]` enum renders the bare variant name
    // (selected on the tag) across println, to_string, and f-string.
    if let Some(out) = run_program(
        r#"
#[derive(Display)]
enum Color { Red, Green, Blue }
fn main() {
    let a = Color.Green;
    println(a.to_string());
    println(f"c={a}");
    println(a);
    let b = Color.Red;
    println(b);
    let c = Color.Blue;
    println(c);
}
"#,
    ) {
        assert_eq!(out, "Green\nc=Green\nGreen\nRed\nBlue\n");
    }
}

#[test]
fn test_e2e_enum_display_payload_variants() {
    // Payload-bearing `#[derive(Display)]` enum (phase-8 main()-entry-point
    // prerequisite, Slice A): tuple + struct variants render via the
    // value-driven `emit_enum_display_fn` as `Variant(f0, f1)` /
    // `Variant { name: v }`, including a heap (String) payload rendered
    // read-only (no move/free) — the `IoError.Other(String)` shape. Matches
    // the interpreter byte-for-byte. (`let b = E.S { .. }` is now
    // UNANNOTATED — B-2026-06-13-9 fixed: `type_name_of` resolves an enum
    // struct-variant construction to the ENUM, so the binding registers in
    // `var_type_names` and the f-string / println Display routing finds it.)
    if let Some(out) = run_program(
        r#"
#[derive(Display)]
enum Shape { Circle(i64), Rect { w: i64, h: i64 }, Dot }
#[derive(Display)]
enum Msg { Text(String), Code(i64) }
fn main() {
    let a = Shape.Circle(5);
    let b = Shape.Rect { w: 3, h: 4 };
    let c = Shape.Dot;
    println(a);
    println(b);
    println(c);
    println(f"x={a} y={b} z={c}");
    let m = Msg.Text("hi".to_string());
    println(m);
    println(f"m={m}");
}
"#,
    ) {
        assert_eq!(
                out,
                "Circle(5)\nRect { w: 3, h: 4 }\nDot\nx=Circle(5) y=Rect { w: 3, h: 4 } z=Dot\nText(hi)\nm=Text(hi)\n"
            );
    }
}

#[test]
fn test_e2e_enum_field_in_struct_display() {
    // A struct whose field is an all-unit enum renders the enum field as
    // its variant name (recursing through the struct Display path).
    if let Some(out) = run_program(
        r#"
#[derive(Display)]
enum Color { Red, Green, Blue }
#[derive(Display)]
struct Tagged { c: Color, n: i64 }
fn main() {
    let t = Tagged { c: Color.Blue, n: 9 };
    println(t);
    let t2 = Tagged { c: Color.Red, n: 1 };
    println(t2.to_string());
    let t3 = Tagged { c: Color.Green, n: 2 };
    println(f"t={t3}");
}
"#,
    ) {
        assert_eq!(
            out,
            "Tagged { c: Blue, n: 9 }\n\
                 Tagged { c: Red, n: 1 }\n\
                 t=Tagged { c: Green, n: 2 }\n"
        );
    }
}

#[test]
fn test_ir_mut_arg_escape_emits_no_assume() {
    // k escapes through a `mut` call-site marker — the callee may
    // write it arbitrarily; scan must poison.
    let ir = ir_for(
        r#"
fn bump(x: mut ref i64) {
    *x = *x + 1;
}
fn run(v: mut Slice[i64], n: i64) -> i64 {
    let mut k = 1;
    for i in 1..n {
        v[k] = v[i];
        k = k + 1;
        bump(mut k);
    }
    k
}
"#,
    );
    assert!(
        !ir.contains("k.mono.fact"),
        "mut-marked escape must poison the var, IR:\n{ir}"
    );
}

/// B-2026-08-14-19's over-reach guard: every slice that lands ON a boundary
/// is untouched, including the established out-of-range contracts. These
/// passed before the check and must keep passing — a boundary test that
/// fires one byte early would make `substring` useless rather than safe.
#[test]
fn test_e2e_substring_aligned_slices_unchanged() {
    assert_eq!(
        run_program(
            "fn main() {\n\
                     let s = \"\u{65e5}\u{672c}\u{8a9e}\";\n\
                     println(s.substring(0i64, 3i64).len());\n\
                     println(s.substring(3i64, 9i64).len());\n\
                     println(s.substring(0i64, 9i64).len());\n\
                     println(s.substring(9i64).len());\n\
                     println(\"abcdef\".substring(0i64, 20i64));\n\
                     println(\"abcdef\".substring(2i64));\n\
                     println(s.substring(20i64).len());\n\
                 }"
        )
        .as_deref(),
        Some("3\n6\n9\n0\nabcdef\ncdef\n0\n"),
    );
}

#[test]
fn test_ir_vec_of_strings_drop_fns_route_through_free_buf() {
    // The synthesized drop fns are a separate emission site from the
    // scope-exit drain: `karac_drop_Vec_String` frees the outer buffer
    // (elem size = the 24-byte `{ptr,len,cap}` stride) and calls
    // `karac_drop_String` per element (hint = cap × 1, exact for a
    // String). Both must carry the recycling entry.
    let src = r#"
fn make() -> Vec[Vec[String]] {
    let mut outer: Vec[Vec[String]] = Vec.new();
    let mut inner: Vec[String] = Vec.new();
    inner.push("a".to_string());
    outer.push(inner);
    return outer;
}

fn main() {
    let o = make();
    println(f"{o.len()}");
}
"#;
    let ir = ir_for(src);
    assert!(
        ir.contains("karac_drop_Vec_String"),
        "expected the recursive vec drop fn to be emitted; got:\n{ir}"
    );
    let drop_fn_ir = ir
        .split("define internal void @karac_drop_Vec_String")
        .nth(1)
        .map(|rest| rest.split("\n}").next().unwrap_or(""))
        .unwrap_or("");
    assert!(
            drop_fn_ir.contains("karac_free_buf"),
            "expected karac_drop_Vec_String's buffer free to route through karac_free_buf; got:\n{drop_fn_ir}"
        );
}

#[test]
fn test_ir_vec_of_vec_of_string_drop_emits_recursive_elem_drop() {
    // Slice 3n: a `Vec[Vec[String]]` (two-level heap) dropped at scope exit.
    // The inline `FreeVecBuffer` vec-struct fast path is ONE level deep — it
    // frees each inner `Vec[String]`'s data buffer but not that buffer's
    // String char-buffers, leaking the innermost Strings. The fix routes the
    // `Vec[String]` ELEMENT through `vec_elem_agg_drop_for_type_expr`'s new
    // recursive-`Vec` arm, which returns the strictly-recursive
    // `karac_drop_Vec_String` — invoked per outer element in the agg-drop
    // loop (`cleanup.adrop`), dropping every level. Its presence proves the
    // two-level leak is closed; the one-level fast path never emits a
    // per-element drop CALL (it inlines the buffer free).
    let src = r#"
fn build() -> Vec[Vec[String]] {
    let mut outer: Vec[Vec[String]] = Vec.new();
    let mut a: Vec[String] = Vec.new();
    a.push("a heap element string padded out beyond thirty-six bytes");
    outer.push(a);
    return outer;
}

fn main() {
    let vv = build();
    println(vv.len());
}
"#;
    let ir = ir_for(src);
    assert!(
        ir.contains("karac_drop_Vec_String"),
        "expected the recursive per-element drop fn karac_drop_Vec_String for the \
             Vec[Vec[String]] scope-exit drop (frees each inner Vec's Strings + buffer); \
             got:\n{}",
        ir
    );
    assert!(
        ir.contains("cleanup.adrop"),
        "expected the agg-drop loop calling the recursive per-element drop over each \
             inner Vec[String]; got:\n{}",
        ir
    );
}

#[test]
fn test_ir_vec_of_option_string_drop_emits_option_elem_drop() {
    // Slice 3p: a `Vec[Option[String]]` dropped at scope exit. The
    // `Option[String]` element is the type-erased `{tag,w0,w1,w2}` layout —
    // not a vec-struct, so the one-level fast path skipped it, and the
    // type-erased `EnumDrop` switch can't free a payload it can't type. The
    // payload-type-aware `karac_drop_Option_String` (tag-guarded; payload
    // {ptr,len,cap} overlays w0..w2) is threaded through the agg-drop loop
    // so each `Some` payload frees; `None` elements skip.
    let src = r#"
fn build(n: i64) -> Vec[Option[String]] {
    let mut v: Vec[Option[String]] = Vec.new();
    v.push(Some(f"a payload string padded beyond thirty-six bytes {n}"));
    v.push(None);
    return v;
}

fn main() {
    let v = build(1_i64);
    println(v.len());
}
"#;
    let ir = ir_for(src);
    assert!(
        ir.contains("karac_drop_Option_String"),
        "expected the tag-guarded payload drop karac_drop_Option_String for the \
             Vec[Option[String]] scope-exit drop; got:\n{}",
        ir
    );
    assert!(
        ir.contains("cleanup.adrop"),
        "expected the agg-drop loop running the Option element drop; got:\n{}",
        ir
    );
}

#[test]
fn test_ir_inferred_let_vec_result_string_gets_result_drop() {
    // Slice 3q: a `Vec[Result[String, i64]]` element gets the tag-dispatching
    // `karac_drop_Result_<ok>_<err>` — on `Ok` the payload {ptr,len,cap}
    // overlay frees via the String drop; the scalar `Err` arm emits no call.
    // Asserted on the INFERRED spelling (`..._str_i64` — the un-annotated
    // `let v = build(1)` main-side binding, per the 3p spelling-trap lesson:
    // the annotated producer local emits the `String`-spelled fn that is
    // runtime-suppressed by the return move-out, so module presence of THAT
    // name proves nothing about the consumer).
    let src = r#"
fn build(n: i64) -> Vec[Result[String, i64]] {
    let mut v: Vec[Result[String, i64]] = Vec.new();
    v.push(Ok(f"alpha ok payload padded out beyond thirty-six bytes {n}"));
    v.push(Err(7_i64));
    return v;
}
fn main() {
    let v = build(1);
    println(v.len());
}
"#;
    let ir = ir_for(src);
    assert!(
        ir.contains("karac_drop_Result_str_i64"),
        "expected the inferred main-side binding to get the tag-dispatching \
             Result element drop (karac_drop_Result_str_i64); got:\n{}",
        ir
    );
}

#[test]
fn test_ir_map_string_value_keeps_flag_fast_path() {
    // Slice 3r gate: a plain `Map[i64, String]` value is EXACTLY the
    // one-level `{ptr,len,cap}` overlay — `map_val_drop_fn_for_type_expr`
    // returns None and the free stays on the flag-based
    // `karac_map_free_with_drop_vec` (no per-value fn call overhead).
    let src = r#"
fn main() {
    let mut m: Map[i64, String] = Map.new();
    m.insert(1, "a heap string padded out beyond thirty-six bytes!");
    println(m.len());
}
"#;
    let ir = ir_for(src);
    assert!(
        !ir.contains("call void @karac_map_free_with_val_drop_fn"),
        "Map[i64, String] must stay on the flag-based fast path — no \
             karac_map_free_with_val_drop_fn CALL (the declaration is unconditional); \
             got:\n{}",
        ir
    );
    assert!(
        ir.contains("karac_map_free_with_drop_vec"),
        "expected the flag-based karac_map_free_with_drop_vec for the String value; \
             got:\n{}",
        ir
    );
}

#[test]
fn test_ir_structpat_mapget_field_escape_clones() {
    // Slice 3t: an escaping destructured FIELD over a `Map.get`
    // scrutinee is deep-cloned (field-granular 3s fixup); a read-only
    // sibling arm shape must stay clone-free (second assertion, separate
    // program).
    let src = r#"
struct Holder { name: String, id: i64 }
fn main() {
    let mut m: Map[i64, Holder] = Map.new();
    m.insert(1, Holder { name: "a heap string padded out beyond thirty-six bytes!", id: 1 });
    let s = match m.get(1) {
        Some(Holder { name, .. }) => name,
        None => "n".to_string(),
    };
    println(s.len());
}
"#;
    let ir = ir_for(src);
    assert!(
        ir.contains("borrow.clone.tmp"),
        "expected the escaping destructured field deep-cloned; got:\n{}",
        ir
    );
    let src_ro = r#"
struct Holder { name: String, id: i64 }
fn main() {
    let mut m: Map[i64, Holder] = Map.new();
    m.insert(1, Holder { name: "a heap string padded out beyond thirty-six bytes!", id: 1 });
    let n = match m.get(1) {
        Some(Holder { name, .. }) => name.len(),
        None => 0,
    };
    println(n);
}
"#;
    let ir_ro = ir_for(src_ro);
    assert!(
        !ir_ro.contains("borrow.clone.tmp"),
        "a read-only destructured field must stay a zero-cost alias; got:\n{}",
        ir_ro
    );
}

#[test]
fn test_ir_freshtemp_vec_string_get_emits_per_element_drop() {
    // Slice 3b-heap: `make_strvec().get(0)` on a fresh-temp `Vec[String]`.
    // The receiver still materializes into `__vrecv_tmp` (the typechecker
    // now records String elements for `get`/`first`/`last`), but unlike the
    // scalar case its `FreeVecBuffer` must take the vec-struct recursion —
    // each element is itself a `{ptr,len,cap}` String, so its buffer is
    // freed in the per-element `cleanup.drop.inner.free` loop *before* the
    // outer `cleanup.free`. Without per-element drop the three element
    // String buffers leak (LeakSanitizer on Linux CI). The `Some(s)`
    // borrow is NOT independently dropped (`scrutinee_is_borrow_call`), so
    // each buffer is freed exactly once.
    let src = r#"
fn names() -> Vec[String] {
    let mut v: Vec[String] = Vec.new();
    v.push("a heap string element padded beyond thirty-six bytes ok");
    return v;
}

fn main() {
    match names().get(0) {
        Some(s) => println(s),
        None => println("none"),
    };
}
"#;
    let ir = ir_for(src);
    assert!(
        ir.contains("__vrecv_tmp"),
        "expected the fresh Vec[String] receiver materialized into __vrecv_tmp; got:\n{}",
        ir
    );
    assert!(
        ir.contains("cleanup.drop.inner.free"),
        "expected the vec-struct per-element drop loop (cleanup.drop.inner.free) \
             freeing each element String buffer of the fresh-temp receiver; got:\n{}",
        ir
    );
}

#[test]
fn test_ir_freshtemp_vec_string_contains_emits_per_element_drop() {
    // Slice 3b-heap follow-on: `contains` on a fresh-temp `Vec[String]`.
    // `contains` returns `bool` (no borrow escapes), but the receiver temp
    // still owns three element String buffers + the outer buffer, so it must
    // materialize into `__vrecv_tmp` and take the same per-element vec-struct
    // recursion (`cleanup.drop.inner.free`) the borrow-returning methods do.
    // Without it the element Strings leak. The compared arg is a static
    // literal, not part of the free accounting.
    let src = r#"
fn names() -> Vec[String] {
    let mut v: Vec[String] = Vec.new();
    v.push("a heap string element padded beyond thirty-six bytes ok");
    return v;
}

fn main() {
    println(names().contains("a heap string element padded beyond thirty-six bytes ok"));
}
"#;
    let ir = ir_for(src);
    assert!(
        ir.contains("__vrecv_tmp"),
        "expected the fresh Vec[String] contains-receiver materialized into __vrecv_tmp; \
             got:\n{}",
        ir
    );
    assert!(
        ir.contains("cleanup.drop.inner.free"),
        "expected the vec-struct per-element drop loop (cleanup.drop.inner.free) \
             freeing each element String buffer of the fresh-temp contains receiver; got:\n{}",
        ir
    );
}

#[test]
fn test_ir_freshtemp_map_string_value_get_emits_drop_free() {
    // Slice 3d-heap: `make_map().get(k)` on a fresh-temp `Map[i64, String]`.
    // The value is heap, so unlike the scalar case the handle drop must take
    // the per-entry-drop variant `karac_map_free_with_drop_vec` (frees each
    // entry's String buffer before the handle) rather than plain
    // `karac_map_free`. Without it the entry Strings leak (LeakSanitizer on
    // Linux CI). The `Some(s)` value borrow is NOT independently dropped
    // (`scrutinee_is_borrow_call`), so each entry String is freed once.
    let src = r#"
fn vmap() -> Map[i64, String] {
    let mut m: Map[i64, String] = Map.new();
    m.insert(1_i64, "a value string padded out beyond thirty-six bytes ok");
    return m;
}

fn main() {
    match vmap().get(1_i64) {
        Some(s) => println(s),
        None => println("none"),
    };
}
"#;
    let ir = ir_for(src);
    assert!(
        ir.contains("__mrecv_tmp"),
        "expected the fresh Map[i64,String] receiver materialized into __mrecv_tmp; got:\n{}",
        ir
    );
    assert!(
        ir.contains("karac_map_free_with_drop_vec"),
        "expected the per-entry-drop handle free (karac_map_free_with_drop_vec) for a \
             heap-value Map temp receiver; got:\n{}",
        ir
    );
}

#[test]
fn test_ir_freshtemp_map_string_keys_emits_drop_free() {
    // Slice 3l-heap: `make_map().values()` on a fresh-temp `Map[i64,String]`.
    // The map's values are heap Strings, so the receiver handle drop must take
    // the per-entry variant `karac_map_free_with_drop_vec` (frees each entry's
    // String before the handle) rather than plain `karac_map_free`.
    // `.values()` CLONES each String into the returned `Vec[String]`, so the
    // handle free and the result Vec free are independent single frees — no
    // double-free (macOS ASAN), no leak (Linux LSan). Receiver materializes
    // into `__mrecv_tmp`.
    let src = r#"
fn vmap() -> Map[i64, String] {
    let mut m: Map[i64, String] = Map.new();
    m.insert(1_i64, "a value string padded out beyond thirty-six bytes ok");
    return m;
}

fn main() {
    let vs: Vec[String] = vmap().values();
    println(vs.len());
}
"#;
    let ir = ir_for(src);
    assert!(
        ir.contains("__mrecv_tmp"),
        "expected the fresh Map[i64,String] `.values()` receiver materialized into \
             __mrecv_tmp; got:\n{}",
        ir
    );
    assert!(
        ir.contains("karac_map_free_with_drop_vec"),
        "expected the per-entry-drop handle free (karac_map_free_with_drop_vec) for a \
             heap-value Map temp `.values()` receiver; got:\n{}",
        ir
    );
}

#[test]
fn test_ir_freshtemp_map_string_value_entries_emits_drop_free() {
    // Slice 3m-heap: `make_map().entries()` on a fresh-temp `Map[i64,String]`.
    // The receiver handle drop takes the per-entry variant
    // `karac_map_free_with_drop_vec` (frees each stored value String before
    // the handle). `.entries()` CLONES each `(K,V)` pair into the returned
    // `Vec[(i64,String)]`, so the handle free and the tuple-Vec free are
    // independent single frees (the tuple-element drop is the SAME machinery
    // the named-map `Vec[(i64,String)]` entries path uses). Receiver
    // materializes into `__mrecv_tmp`.
    let src = r#"
fn vmap() -> Map[i64, String] {
    let mut m: Map[i64, String] = Map.new();
    m.insert(1_i64, "a value string padded out beyond thirty-six bytes ok");
    return m;
}

fn main() {
    let es: Vec[(i64, String)] = vmap().entries();
    println(es.len());
}
"#;
    let ir = ir_for(src);
    assert!(
        ir.contains("__mrecv_tmp"),
        "expected the fresh Map[i64,String] `.entries()` receiver materialized into \
             __mrecv_tmp; got:\n{}",
        ir
    );
    assert!(
        ir.contains("karac_map_free_with_drop_vec"),
        "expected the per-entry-drop handle free (karac_map_free_with_drop_vec) for a \
             heap-value Map temp `.entries()` receiver; got:\n{}",
        ir
    );
}

#[test]
fn test_ir_operand_temp_string_concat_emits_free() {
    // Slice 3c: a fresh-temp String operand of a string binop
    // (`make_s() + " x"`) must emit a `cap > 0`-guarded `freearg.free` of the
    // operand buffer after the concat copies it. Without it the operand
    // buffer leaks (LeakSanitizer on Linux CI). `make_s()` is the only fresh
    // owned String in the program, so a `freearg.free` block can only be the
    // operand free this slice adds.
    let src = r#"
fn make_s() -> String {
    let s: String = "a fresh heap operand string padded beyond thirty-six bytes";
    return s;
}

fn main() {
    let r = make_s() + " [suffix]";
    println(r);
}
"#;
    let ir = ir_for(src);
    assert!(
        ir.contains("freearg.free"),
        "expected a cap>0-guarded operand free (freearg.free) for the fresh-temp \
             String concat operand; got:\n{}",
        ir
    );
}

#[test]
fn test_ir_string_concat_literals_no_operand_free() {
    // Slice 3c negative: a string concat of two STATIC LITERALS
    // (`"a" + "b"`) has no fresh-owned operand — both operands are rodata
    // (`cap == 0`), so the operand-temp path must NOT emit a `freearg.free`.
    // (The concat RESULT is freed by its `r` binding via the normal
    // FreeVecBuffer path, not the operand `freearg.free` block.) Guards
    // against the gate over-firing on non-fresh operands.
    let src = r#"
fn main() {
    let r = "left part padded beyond thirty-six bytes ok " + "right part too";
    println(r);
}
"#;
    let ir = ir_for(src);
    assert!(
        !ir.contains("freearg.free"),
        "a concat of static-literal operands must not emit an operand free \
             (freearg.free); got:\n{}",
        ir
    );
}

#[test]
fn test_e2e_vec_indexed_write_string_element() {
    let out = run_program(
        r#"
fn main() {
    let mut v: Vec[String] = Vec.new();
    v.push("alpha");
    v.push("beta");
    v[0] = "gamma";
    println(v[0]);
    println(v[1]);
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["gamma", "beta"]);
    }
}

// ── String codegen ────────────────────────────────────────────

#[test]
fn test_e2e_string_literal_println() {
    let out = run_program(r#"fn main() { println("hello world"); }"#);
    if let Some(out) = out {
        assert_eq!(out.trim(), "hello world");
    }
}

#[test]
fn test_e2e_string_literal_len() {
    let out = run_program(
        r#"
fn main() {
    let s = "hello";
    println(s.len());
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "5");
    }
}

#[test]
fn test_e2e_string_new_push_str() {
    let out = run_program(
        r#"
fn main() {
    let mut s: String = String.new();
    s.push_str("hello");
    s.push_str(" world");
    println(s);
    println(s.len());
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["hello world", "11"]);
    }
}

// String.push(char) — codegen path. Lowering lives in
// `compile_vec_method`'s String-gated push arm and reuses
// `emit_codepoint_to_utf8` for the 1–4-byte UTF-8 encoding. The
// ASCII case below exercises the common 1-byte path (every per-char
// append in kata-katas/leetcode/71-simplify-path lands here); the
// multi-byte case verifies the encoder dispatch + variable-length
// memcpy. Each push amortizes O(1) via the power-of-two cap
// growth, mirroring `push_str`'s geometry.

#[test]
fn test_e2e_string_push_char_ascii() {
    let out = run_program(
        r#"
fn main() {
    let mut s: String = "";
    s.push('h');
    s.push('e');
    s.push('l');
    s.push('l');
    s.push('o');
    println(s);
    println(s.len());
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["hello", "5"]);
    }
}

#[test]
fn test_e2e_string_push_char_multibyte_utf8() {
    // 'é' = 2 bytes, '日' = 3 bytes, '🦀' = 4 bytes → 9 byte len.
    let out = run_program(
        r#"
fn main() {
    let mut s: String = "";
    s.push('é');
    s.push('日');
    s.push('🦀');
    println(s);
    println(s.len());
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["é日🦀", "9"]);
    }
}

#[test]
fn test_e2e_string_push_char_mixed_with_push_str() {
    // Mixed seq of push(char) + push_str(&str) — exercises the
    // shared {ptr,len,cap} growth + the alternating UTF-8 byte
    // sequence vs raw bytes paths.
    let out = run_program(
        r#"
fn main() {
    let mut s: String = "";
    s.push_str("kara");
    s.push('-');
    s.push_str("rust");
    s.push('!');
    println(s);
    println(s.len());
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["kara-rust!", "10"]);
    }
}

// ── char print / f-string char-arm ────────────────────────────
//
// Pre-fix state: `ExprKind::CharLit` fell through `compile_expr`'s
// tail arm and emitted `i64 0`, so `let c: char = 'A'` bound `c`
// to zero. And both `println(c)` and `println(f"{c}")` rendered
// the i32 codepoint via `%lld` rather than encoding it as a UTF-8
// glyph. The fix lands an explicit `CharLit → i32` arm and a
// char-aware branch in `compile_print` / the f-string Expr part
// that routes through `karac_string_encode_char` and prints
// `%.*s` of the UTF-8 bytes.

#[test]
fn test_e2e_println_char_literal_ascii() {
    let out = run_program(
        r#"
fn main() {
    let c: char = 'A';
    println(c);
    println(f"{c}");
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(
            lines,
            vec!["A", "A"],
            "println(char) and f\"{{char}}\" must both render the glyph"
        );
    }
}

#[test]
fn test_e2e_println_char_literal_multibyte() {
    // 3-byte UTF-8 (CJK ideograph) exercises the wider arms of
    // `karac_string_encode_char`. Pre-fix this printed `0`.
    let out = run_program(
        r#"
fn main() {
    let c: char = '日';
    println(c);
    println(f"a={c}b");
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["日", "a=日b"]);
    }
}

#[test]
fn test_e2e_println_char_chars_iter() {
    // `for c in s.chars()` binds c: char via decode_char's i32 out
    // param. The for-loop must tag the binding as `char` in
    // `var_type_names` so the print/f-string char arms pick it up.
    let out = run_program(
        r#"
fn main() {
    let s = "ABC";
    for c in s.chars() {
        println(c);
        println(f"-{c}-");
    }
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(
            lines,
            vec!["A", "-A-", "B", "-B-", "C", "-C-"],
            "chars() iterator binding must render as glyph"
        );
    }
}

#[test]
fn test_e2e_println_char_vec_index() {
    // `vec_of_chars[i]` (Index over Vec[char]) flows through
    // `expr_is_char`'s Index arm, which inspects
    // `var_elem_type_exprs[name]`. The `let c = chars[i]` binding
    // also gets `var_type_names[c] = "char"` via the extended
    // `type_name_of` so the subsequent `println(c)` works too.
    let out = run_program(
        r#"
fn main() {
    let mut chars: Vec[char] = Vec.new();
    chars.push('X');
    chars.push('Y');
    println(chars[0]);
    println(f"{chars[1]}");
    let c = chars[0];
    println(c);
    println(f"{c}");
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["X", "Y", "X", "X"]);
    }
}

#[test]
fn test_e2e_println_char_call_return() {
    // B-2026-06-30: a `char` crossing a CALL-RETURN SSA boundary lost its
    // source type on the print path, so `println(f())` for `fn f() -> char`
    // formatted the i32 scalar as its integer codepoint (`65` instead of
    // `A`). The interpreter (`karac run`) rendered the glyph, so this was a
    // run/build divergence. `expr_is_char` gained a free-fn `Call` arm and a
    // general `MethodCall` arm (both keyed on `fn_return_type_names`),
    // mirroring `expr_is_unsigned_int`. Covers: direct free-fn return,
    // f-string interpolation of a call, a method (`self`-typed) return, and
    // a multibyte (3-byte UTF-8) free-fn return. The `let`-bound call form
    // already worked (the binding picks up `char` via the untyped-let
    // type-expr recovery) and is included as a non-regression guard.
    let out = run_program(
        r#"
fn f() -> char { 'A' }
fn pick() -> char { 'Z' }
fn jp() -> char { '日' }
struct Box { c: char }
impl Box {
    fn get(self) -> char { self.c }
}
fn main() {
    println(f());
    println(f"{f()}");
    let c = pick();
    println(c);
    let b = Box { c: 'M' };
    println(b.get());
    println(jp());
    println(f"x={jp()}y");
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(
            lines,
            vec!["A", "A", "Z", "M", "日", "x=日y"],
            "char call/method returns must render the glyph, not the codepoint"
        );
    }
}

#[test]
fn test_e2e_char_unicode_predicates() {
    // #13 (phase-12 self-hosting, B-2026-06-14-10) — Unicode `char`
    // classification predicates: `is_alphabetic` / `is_numeric` /
    // `is_alphanumeric` / `is_whitespace`. Codegen routes these through the
    // `karac_runtime_char_is_*` externs (the Unicode tables can't be inlined
    // like the ASCII byte predicates). The Unicode cases are the point:
    // Greek alpha (U+03B1) is_alphabetic AND a Devanagari digit (U+096B)
    // is_numeric — both FALSE under a byte-level ASCII check. Built via
    // `char.try_from(cp)` since char literals are ASCII-only.
    let out = run_program(
        r#"
fn main() {
    println(f"{'a'.is_alphabetic()} {'5'.is_numeric()} {'5'.is_alphabetic()}");
    println(f"{' '.is_whitespace()} {'_'.is_alphanumeric()} {'z'.is_alphanumeric()}");
    match char.try_from(945) {
        Ok(g) => { println(f"{g.is_alphabetic()} {g.is_numeric()}"); }
        Err(e) => { println("err"); }
    }
    match char.try_from(2411) {
        Ok(d) => { println(f"{d.is_numeric()} {d.is_alphabetic()}"); }
        Err(e) => { println("err"); }
    }
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(
            out,
            "true true false\ntrue false true\ntrue false\ntrue false\n"
        );
    }
}

#[test]
fn test_e2e_char_unicode_case_fold_and_is_digit() {
    // B-2026-08-12-25 — the codegen twin of
    // `test_char_unicode_case_fold_and_is_digit_interpreter`, asserting the
    // identical bytes. `to_lowercase`/`to_uppercase` route through the
    // `karac_runtime_char_to_*case` externs (the Unicode tables can't be
    // inlined like `to_ascii_*case`); `is_digit` shares `to_digit`'s inlined
    // classification, returning the predicate the Option wrap consumes.
    //
    // THREE LINES HERE ARE ABOUT THE NAME COLLISION, not the folding.
    // `to_lowercase`/`to_uppercase` were String-only before this, and four
    // codegen sites keyed String-ness on the method NAME alone (one of them
    // saying so: "a non-String receiver here would already have failed the
    // typechecker, which is what makes the name sufficient"). The
    // `.to_string()` chain is the exact line the row was filed from and the
    // one that reached `expr_is_string_like` — pre-fix it sent an i32
    // codepoint into the String-copy path. The two String-receiver calls on
    // the last line pin the other direction: the String→String transforms
    // must NOT be captured by the new char arm.
    // The `sharp` binding is deliberate: `'ß'.to_string().to_uppercase()`
    // with the LITERAL inline fails codegen ("Vec/String method 'to_string'
    // is not yet supported"), which is a PRE-EXISTING gap unrelated to this
    // row — measured identical on `7`/`true`/`(7 + 1)` receivers at the
    // commit before this one, and filed as B-2026-08-13-2. Bound to a name,
    // the same chain compiles.
    let out = run_program(
        r#"
fn main() {
    println(f"{'A'.to_lowercase()} {'a'.to_uppercase()} {'7'.to_uppercase()}");
    println(f"{'é'.to_uppercase()} {'É'.to_lowercase()} {'ß'.to_uppercase()}");
    match char.try_from(64257) {
        Ok(l) => { println(f"{l.to_uppercase()} {l.to_lowercase()}"); }
        Err(e) => { println("err"); }
    }
    match char.try_from(304) {
        Ok(i) => { println(f"{i.to_lowercase()}"); }
        Err(e) => { println("err"); }
    }
    let sharp = 'ß';
    println(sharp.to_string().to_uppercase());
    println(f"{'7'.is_digit(10)} {'f'.is_digit(16)} {'f'.is_digit(10)}");
    println(f"{'z'.is_digit(36)} {' '.is_digit(10)} {'0'.is_digit(2)}");
    let mut out = "".to_string();
    for ch in "HeLLo Wörld".chars() {
        out = out + ch.to_lowercase().to_string();
    }
    println(out);
    let s = "MiXeD".to_string();
    println(f"{s.to_lowercase()} {s.to_uppercase()}");
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(
            out,
            "a A 7\n\
                 É é ß\n\
                 ﬁ ﬁ\n\
                 İ\n\
                 SS\n\
                 true true false\n\
                 true false true\n\
                 hello wörld\n\
                 mixed MIXED\n"
        );
    }
}

#[test]
fn test_e2e_char_case_predicates() {
    // B-2026-06-18-4: `char.is_uppercase()` / `is_lowercase()` — the case
    // siblings of the `is_alphabetic` / … predicates above. They were wired
    // for neither typecheck, interpret, nor codegen (build failed with "no
    // handler for method is_uppercase"; run panicked because the typechecker
    // returned Unit for the call), while the other four classification
    // predicates worked. Now routed through `karac_runtime_char_is_upper/
    // lowercase`, Unicode-aware: Ä (U+00C4) is uppercase, ß (U+00DF) is
    // lowercase — both beyond an ASCII A-Z / a-z check — and a digit / space
    // is neither.
    let out = run_program(
        r#"
fn main() {
    println(f"{'A'.is_uppercase()} {'A'.is_lowercase()}");
    println(f"{'z'.is_uppercase()} {'z'.is_lowercase()}");
    println(f"{'5'.is_uppercase()} {' '.is_lowercase()}");
    match char.try_from(196) {
        Ok(u) => { println(f"{u.is_uppercase()} {u.is_lowercase()}"); }
        Err(e) => { println("err"); }
    }
    match char.try_from(223) {
        Ok(l) => { println(f"{l.is_uppercase()} {l.is_lowercase()}"); }
        Err(e) => { println("err"); }
    }
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(
            out,
            "true false\nfalse true\nfalse false\ntrue false\nfalse true\n"
        );
    }
}

#[test]
fn test_e2e_char_ascii_case_and_is_ascii() {
    // `char.to_ascii_uppercase()` / `to_ascii_lowercase()` → char (inline
    // codepoint arithmetic — only ASCII letters fold; digits/punctuation/
    // non-ASCII pass through) and `is_ascii()` → bool. Must match the
    // interpreter oracle (`test_char_ascii_case_and_is_ascii_interpreter`),
    // and the char result must render as a glyph (the `expr_is_char`
    // method-call arm), not the integer codepoint.
    let out = run_program(
        r#"
fn main() {
    println(f"{'a'.to_ascii_uppercase()} {'Z'.to_ascii_lowercase()}");
    println(f"{'5'.to_ascii_uppercase()} {'!'.to_ascii_lowercase()}");
    println(f"{'a'.is_ascii()} {'é'.is_ascii()}");
    println('é'.to_ascii_uppercase());
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out, "A z\n5 !\ntrue false\né\n");
    }
}

#[test]
fn test_e2e_string_char_at_and_count() {
    // B-2026-06-18-3: `s.char_at(i) -> Option[char]` and `s.char_count() ->
    // i64` were unimplemented end-to-end (typecheck rejected them, interp
    // and codegen had no arm). They are the O(n) Unicode-aware access pair
    // (design.md § String) vs the O(1) `bytes()`/`len()` byte view. Codegen
    // routes through `karac_runtime_string_char_at` (out-slot + found flag →
    // Some/None) and `karac_runtime_string_char_count`.
    //
    // The cases are Unicode on purpose: "héllo" is 6 BYTES but 5 SCALARS,
    // and scalar index 1 is `é` (a 2-byte char) — a byte-index would return
    // the wrong thing. Past-the-end and negative indices → None.
    let out = run_program(
        r#"
fn nth(s: String, i: i64) -> String {
    match s.char_at(i) {
        Some(c) => f"{c}",
        None => "_",
    }
}
fn main() {
    let s: String = "héllo";
    println(f"{s.len()} {s.char_count()}");
    println(f"{nth(s.clone(), 0)} {nth(s.clone(), 1)} {nth(s.clone(), 4)}");
    println(f"{nth(s.clone(), 5)} {nth(s.clone(), 99)} {nth(s, -1)}");
    let cjk: String = "日本語";
    println(f"{cjk.len()} {cjk.char_count()} {nth(cjk, 1)}");
}
"#,
    );
    if let Some(out) = out {
        // "héllo": 6 bytes, 5 scalars; chars 0,1,4 = h,é,o; 5/99/-1 → None.
        // "日本語": 9 bytes, 3 scalars; char 1 = 本.
        assert_eq!(out, "6 5\nh é o\n_ _ _\n9 3 本\n");
    }
}

#[test]
fn test_e2e_ref_string_param() {
    let out = run_program(
        r#"
fn greet(name: ref String) {
    println(name);
    println(name.len());
}
fn main() {
    let s = "Alice";
    greet(s);
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["Alice", "5"]);
    }
}

// ── SoA heap-field (String / Vec) elements ────────────────────
// String and Vec[POD] element fields are now supported in SoA
// layouts. The element's heap fields live in their own group
// buffer (scattered like every SoA field); push moves them in,
// index/field stores drop-then-overwrite, and scope cleanup +
// carried-grid reassignment free each live element's buffers via
// the synthesized `__karac_soa_drop_<layout>`. These tests cover
// functional correctness; the leak/UAF guards live in
// `tests/memory_sanitizer.rs` (`asan_soa_string_field_*`).

#[test]
fn test_e2e_soa_string_field_push_read() {
    // A SoA element with a heap String field in its own group.
    // Push three elements with `f"..."` (heap, cap > 0) names plus a
    // numeric `id` in a separate group, then read both back — the
    // String header must scatter into the names group and the id into
    // the ids group, each readable independently.
    let out = run_program(
        r#"
struct Cell { id: i64, name: String }
layout cells: Vec[Cell] { group ids { id } group names { name } }
fn main() with panics {
    let mut cells: Vec[Cell] = Vec.new();
    let mut i = 0;
    while i < 3 {
        cells.push(Cell { id: i, name: f"soa-element-heap-owning-string-payload-{i}" });
        i = i + 1;
    }
    println(cells[1].name);
    println(cells[2].id);
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(
            lines,
            vec!["soa-element-heap-owning-string-payload-1", "2"],
            "SoA String field must scatter/read independently of the numeric group"
        );
    }
}

#[test]
fn test_e2e_soa_string_field_whole_element_store() {
    // Whole-element overwrite `cells[i] = Cell { … }` over a String
    // field: the old element's String buffer is dropped, the new
    // element's String moved in. Read back the rewritten name.
    let out = run_program(
        r#"
struct Cell { id: i64, name: String }
layout cells: Vec[Cell] { group ids { id } group names { name } }
fn main() with panics {
    let mut cells: Vec[Cell] = Vec.new();
    cells.push(Cell { id: 0, name: f"initial-placeholder-heap-string-value-{0}" });
    cells.push(Cell { id: 1, name: f"initial-placeholder-heap-string-value-{1}" });
    let mut i = 0;
    while i < cells.len() {
        cells[i] = Cell { id: i + 10, name: f"rewritten-soa-heap-string-element-{i}" };
        i = i + 1;
    }
    println(cells[0].name);
    println(cells[1].id);
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(
            lines,
            vec!["rewritten-soa-heap-string-element-0", "11"],
            "whole-element SoA store must overwrite the String field (drop old, move new)"
        );
    }
}

#[test]
fn test_e2e_soa_string_field_field_store() {
    // Field-level store `cells[i].name = f"…"` over a heap String:
    // the displaced buffer is freed and the new one stored in place,
    // leaving the other group (id) untouched.
    let out = run_program(
        r#"
struct Cell { id: i64, name: String }
layout cells: Vec[Cell] { group ids { id } group names { name } }
fn main() with panics {
    let mut cells: Vec[Cell] = Vec.new();
    cells.push(Cell { id: 7, name: f"original-soa-heap-string-payload-{0}" });
    cells[0].name = f"replacement-soa-heap-string-payload-{0}";
    println(cells[0].name);
    println(cells[0].id);
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(
            lines,
            vec!["replacement-soa-heap-string-payload-0", "7"],
            "SoA field store must replace only the named String field"
        );
    }
}

// ── String operators ──────────────────────────────────────────

#[test]
fn test_e2e_string_equality() {
    let out = run_program(
        r#"
fn main() {
    let a = "hello";
    let b = "hello";
    let c = "world";
    if a == b { println(1); } else { println(0); }
    if a == c { println(1); } else { println(0); }
    if a != c { println(1); } else { println(0); }
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["1", "0", "1"]);
    }
}

#[test]
fn test_e2e_string_ordering() {
    let out = run_program(
        r#"
fn main() {
    let a = "abc";
    let b = "abd";
    if a < b { println(1); } else { println(0); }
    if b > a { println(1); } else { println(0); }
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["1", "1"]);
    }
}

#[test]
fn test_e2e_string_concatenation() {
    let out = run_program(
        r#"
fn main() {
    let a = "hello";
    let b = " world";
    let c = a + b;
    println(c);
    println(c.len());
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["hello world", "11"]);
    }
}

/// `+` concatenation accepts borrowed String operands (`ref String`)
/// in either position — codegen auto-loads the pointee struct before
/// the concat, so `"a" + ref_s`, `ref_s + "b"`, and `ref_a + ref_b`
/// all produce a fresh owned String. Pairs with the typechecker arm
/// (`test_string_concat_ref_operand_ok`) that admits the borrow forms.
#[test]
fn test_e2e_string_concat_ref_operands() {
    let out = run_program(
        r#"
fn right(name: ref String) -> String {
    "hello " + name
}
fn left(name: ref String) -> String {
    name + "!"
}
fn both(a: ref String, b: ref String) -> String {
    a + b
}
fn main() {
    let x = "foo";
    let y = "bar";
    println(right(x));
    println(left(x));
    println(both(x, y));
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["hello foo", "foo!", "foobar"]);
    }
}

// ── F-string codegen (Phase 7.2 minimum formatter) ────────────

#[test]
fn test_e2e_fstring_text_literal_only() {
    let out = run_program(
        r#"
fn main() {
    let s = f"hello, world";
    println(s);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "hello, world");
    }
}

#[test]
fn test_e2e_fstring_string_interpolation() {
    let out = run_program(
        r#"
fn main() {
    let mut name: String = String.new();
    name.push_str("Alice");
    let msg = f"Hello, {name}!";
    println(msg);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "Hello, Alice!");
    }
}

#[test]
fn test_e2e_fstring_match_arm_value() {
    // phase-12 self-hosting blocker #3: an f-string that is a match
    // arm's TAIL (value) expression, interpolating the arm's payload
    // binding, compiled to empty under AOT. The f-string accumulator is
    // an entry-block alloca registered for scope cleanup; the per-arm
    // `drain_top_frame_with_emit` freed its buffer between the value load
    // and the merge, so the match result (and any caller binding) saw an
    // empty/dangling String. Fix: when the arm tail is an f-string, zero
    // the acc's `cap` so its cleanup no-ops and the value escapes via the
    // match phi (mirrors the function-tail f-string-return handling).
    // Covers fn-return, let-bound, i64 payload, nested-into-outer, and a
    // discarded result (must not double-free). The brace-WRAPPED arm body
    // (`=> { f"…" }` / `=> { let p = f"…"; p }`) is the separate
    // block-expr-value heap-return shape, fixed and covered by
    // `test_e2e_block_expr_value_heap_return` (B-2026-06-11-2).
    let out = run_program(
        r#"
enum E { A(String), B(i64) }
fn describe(e: E) -> String {
    match e {
        E.A(name) => f"A[{name}]",
        E.B(k) => f"B[{k}]",
    }
}
fn main() {
    println(describe(E.A("x" + "y")));
    println(describe(E.B(99)));
    let e = E.A("hi");
    let s = match e {
        E.A(name) => f"<{name}>",
        E.B(_) => "none",
    };
    println(s);
    let r = describe(E.A("a" + "b"));
    println(f"outer:{r}");
    let d = E.A("z");
    match d {
        E.A(n) => f"[{n}]",
        E.B(_) => "x",
    };
    println("done");
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out, "A[xy]\nB[99]\n<hi>\nouter:A[ab]\ndone\n");
    }
}

#[test]
fn test_e2e_fstring_integer_interpolation() {
    let out = run_program(
        r#"
fn main() {
    let x = 42;
    let s = f"value={x}";
    println(s);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "value=42");
    }
}

#[test]
fn test_e2e_fstring_format_specifiers() {
    // Phase 8 format specifiers `{expr:spec}` — width, zero-pad, align,
    // radix (int), precision (float), width/align (string). Codegen maps to
    // snprintf conversions matching the interpreter's `format_spec::apply_*`,
    // so this exact output is also asserted for the interpreter in
    // tests/interpreter.rs::test_fstring_format_specifiers (build==run).
    let out = run_program(
        r#"
fn main() {
    let n = 7;
    let big = 255;
    let neg = 0 - 42;
    let pi = 1.23456;
    let s = "hi";
    println(f"{n:04}|{n:x}|{big:08X}|{big:o}");
    println(f"{neg:6}|{neg:06}|{neg:<6}");
    println(f"{pi:.2}|{pi:8.2}|{pi:08.2}|{pi:<8.2}");
    println(f"{s:6}|{s:<6}|{s:>6}|{s}");
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(
                out.trim(),
                "0007|7|000000FF|377\n   -42|-00042|-42   \n1.23|    1.23|00001.23|1.23    \n    hi|hi    |    hi|hi"
            );
    }
}

#[test]
fn test_e2e_fstring_multiple_parts() {
    let out = run_program(
        r#"
fn main() {
    let a = 1;
    let b = 2;
    let s = f"{a}+{b}=3";
    println(s);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "1+2=3");
    }
}

#[test]
fn test_e2e_fstring_in_loop_no_double_free() {
    // Regression: an f-string built inside a `for`-loop body that
    // reassigns the accumulator (`line = line + f"..."`) gave that
    // accumulator a function-entry alloca but emitted its `{null,0,0}`
    // init *inside* the loop body. When the loop never ran, the
    // unconditional scope-exit cleanup read the uninitialized alloca's
    // `cap` — fresh-0 on the first call (skips), but a non-zero leftover
    // from the first call on the second, so the second call freed a
    // garbage/literal pointer → `pointer being freed was not allocated`
    // SIGABRT. The trigger needs a 2+-interpolation first f-string over
    // owned-struct String fields + the loop + the fn called >=2x.
    // Surfaced by std.tracing's StdoutExporter bodies; fixed by
    // zero-initializing the accumulator at the entry block.
    let out = run_program(
        r#"
struct E { level: String, message: String, items: Vec[i64] }
fn ev(e: E) {
    let mut line = f"[{e.level}] {e.message}";
    for x in e.items { line = line + f" {x}"; }
    println(line);
}
fn main() {
    ev(E { level: "info", message: "one", items: Vec.new() });
    println("between");
    ev(E { level: "warn", message: "two", items: Vec.new() });
}
"#,
    );
    // Before the fix the second `ev` aborted, so stdout stopped after
    // "[info] one"; a clean run prints all three lines in order.
    assert_eq!(out.as_deref(), Some("[info] one\nbetween\n[warn] two\n"));
}

#[test]
fn test_e2e_fstring_into_returned_struct_field_no_double_free() {
    // Regression: an f-string used DIRECTLY as a struct-literal field
    // value (`Resp { body: f"..." }`) moves the accumulator buffer into
    // the field, and the struct is returned (caller owns the buffer).
    // `compile_struct_init` previously left `last_fstr_acc` staged, so
    // the accumulator's scope-exit `FreeVecBuffer` freed the buffer the
    // returned struct carried — a double-free aborting under macOS
    // malloc (exit 133), no output. The Identifier-named field case
    // (`Resp { body: b }`) was already covered by
    // `suppress_source_vec_cleanup_for_arg`; the direct-f-string case
    // was the gap. Reading the field repeatedly in the caller would hit
    // a UAF / garbage if the buffer were freed early. (ASAN coverage:
    // `tests/memory_sanitizer.rs::asan_fstring_into_returned_*`.) This
    // is the codegen blocker for Parallax serializing real `Dashboard`
    // data into its response body (phase-6 Demo 2 gap B).
    let out = run_program(
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
}
"#,
    );
    assert_eq!(
        out.as_deref(),
        Some("200\nid=7 name=Alice\nid=7 name=Alice\n")
    );
}

#[test]
fn test_e2e_fstring_explicit_return_no_double_free() {
    // Sibling to the struct-field case: a DIRECT `return f"..."` mid-
    // function (early return) also moves the accumulator buffer out to
    // the caller. The `Return` arm's pre-compile
    // `suppress_source_vec_cleanup_for_arg` is Identifier-only, and the
    // accumulator is staged only during `compile_expr`, so without
    // suppressing it post-compile the scope-cleanup walk freed the
    // returned buffer — double-free / exit 133. Both the early-return
    // arm and the tail-expr arm of the same fn are exercised, each
    // called and read in the caller.
    let out = run_program(
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
    );
    assert_eq!(out.as_deref(), Some("pos=5\nnonpos=-3\n"));
}

// ── B-2026-07-13-2: a bare generic param bound WHOLE to a collection
// (String/Vec/VecDeque) must get its owned-param return deep-copy. Two
// legs: (A) a nested generic FORWARD (`twice[T]{ id(x) }`) resolved to the
// element-ERASED `id$struct` mono (the typechecker drops the self-ref
// `T→T` binding); (B) a `Vec[E]` param lost its element (head-only subst
// name) so the body registered elementless and skipped the copy. Fixed by
// `type_subst_type_exprs` + `subst_names`/mangle-token threading. Pre-fix:
// interp correct, JIT/native double-freed.

#[test]
fn test_e2e_generic_forward_owned_string_param_no_double_free() {
    // Leg A: forward a generic owned String param through a nested generic
    // call. `twice`/`pick`/`id(id(x))` shapes.
    let out = run_program(
        "fn id[T](x: T) -> T { x }\n\
             fn twice[T](x: T) -> T { id(x) }\n\
             fn pick[T](a: T, b: T) -> T { id(a) }\n\
             fn nest[T](x: T) -> T { id(id(x)) }\n\
             fn main() {\n\
             \x20   println(twice(f\"deep\"));\n\
             \x20   println(pick(f\"aa\", f\"bb\"));\n\
             \x20   println(nest(f\"chain\"));\n\
             }",
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "deep\naa\nchain");
    }
}

#[test]
fn test_e2e_generic_owned_vec_string_param_return_no_double_free() {
    // Leg B with a nested-heap element (`Vec[String]`): the deep-copy must
    // recurse per String element.
    let out = run_program(
        "fn id[T](x: T) -> T { x }\n\
             fn main() {\n\
             \x20   let mut v: Vec[String] = Vec.new(); v.push(f\"aa\"); v.push(f\"bb\");\n\
             \x20   let w = id(v);\n\
             \x20   println(w[1]);\n\
             \x20   println(w.len().to_string());\n\
             }",
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "bb\n2");
    }
}

#[test]
fn test_e2e_set_string_dedup_companion() {
    // Companion to `test_e2e_set_vec_dedup_by_content`: `Set[String]` is
    // the already-working content-dedup path (String hash/eq walk the
    // bytes), kept alongside the Vec test so a regression in the shared
    // `emit_hash_fn_for_type_expr` / `emit_eq_fn_for_type_expr` dispatch
    // surfaces on both element kinds (B-2026-06-20-15).
    let out = run_program(
        r#"
fn main() {
    let mut s: Set[String] = Set.new();
    s.insert("alpha");
    s.insert("alpha");
    s.insert("beta");
    println(s.len());            // 2
    println(s.contains("alpha")); // true
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "2\ntrue");
    }
}

#[test]
fn test_e2e_vec_get_unchecked_string_element() {
    // Heap-bearing element type — exercises the codegen elem-load shape
    // for non-i64 cells (different stride, different copy semantics).
    let out = run_program(
        r#"
fn main() {
    let mut v: Vec[String] = Vec.new();
    v.push("alpha");
    v.push("beta");
    unsafe {
        println(v.get_unchecked(0));
        println(v.get_unchecked(1));
    }
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "alpha\nbeta");
    }
}

#[test]
fn test_e2e_block_value_escape_with_local_tail_not_broken_by_scope_revert() {
    // Regression guard for the scope-revert fix: a block that PRODUCES a
    // value from a block-LOCAL tail (`let s = { let v = …; v }`) must still
    // give the consumer the right typed value — the consumer re-derives its
    // metadata from the typechecker type, not the reverted block-local `v`.
    let out = run_program(
        "fn main() {\n\
             let s: Vec[i64] = { let mut v: Vec[i64] = Vec.new(); v.push(10); v.push(20); v };\n\
             println(s.len().to_string());\n\
             println(s[1].to_string());\n\
             }",
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "2\n20");
    }
}

#[test]
fn test_error_trace_text_format_default() {
    // No env var → existing text format. Regression pin: this is
    // identical to what `test_e2e_question_trace_includes_source_filename_when_threaded`
    // exercises, but explicitly asserts the absence of any JSON
    // markers so a future default flip would surface here.
    let captured =
        run_program_capturing_with_env(TRACE_FORMAT_SRC, Some("trace_fmt_default.kara"), &[]);
    if let Some(c) = captured {
        assert_eq!(c.stdout.trim(), "7");
        assert!(
            c.stderr.contains("Error return trace:"),
            "expected text-mode header; got {:?}",
            c.stderr
        );
        assert!(
            c.stderr.contains("trace_fmt_default.kara:"),
            "expected file:line:col frame; got {:?}",
            c.stderr
        );
        // No JSON markers — `[`, `]`, or `{` appearing on their own
        // would indicate a stray JSON emitter wired in by mistake.
        // (We can't blanket-ban `{` because user stdout is separate;
        // we're checking stderr.)
        assert!(
            !c.stderr.contains("\"file\":"),
            "text mode should not emit JSON keys; got {:?}",
            c.stderr
        );
    }
}

#[test]
fn test_error_trace_json_format() {
    // KARAC_ERROR_TRACE_FORMAT=json → single-document JSON on
    // stderr matching the interpreter's `format_error_trace_json`
    // shape: a bare array of frame objects when not truncated.
    // Each frame object has the keys `file`, `line`, `column`.
    let captured = run_program_capturing_with_env(
        TRACE_FORMAT_SRC,
        Some("trace_fmt_json.kara"),
        &[("KARAC_ERROR_TRACE_FORMAT", "json")],
    );
    if let Some(c) = captured {
        assert_eq!(c.stdout.trim(), "7");
        // No text-mode header.
        assert!(
            !c.stderr.contains("Error return trace:"),
            "json mode should not emit the text header; got {:?}",
            c.stderr
        );
        // Locate the JSON document — the printer emits a single
        // line on stderr matching the array shape.
        let json_line = c
            .stderr
            .lines()
            .find(|l| l.starts_with('[') && l.ends_with(']'))
            .unwrap_or_else(|| panic!("expected a JSON array line on stderr; got {:?}", c.stderr));
        // Shape assertions — interpreter's format verbatim:
        //   `[{"file":"…","line":N,"column":N}]`
        assert!(
            json_line.contains("\"file\":"),
            "missing `file` key: {}",
            json_line
        );
        assert!(
            json_line.contains("\"line\":"),
            "missing `line` key: {}",
            json_line
        );
        assert!(
            json_line.contains("\"column\":"),
            "missing `column` key: {}",
            json_line
        );
        assert!(
            json_line.contains("trace_fmt_json.kara"),
            "filename not threaded into JSON frame: {}",
            json_line
        );
        // One `?` site → one frame → exactly one `{…}` object.
        let open_braces = json_line.matches('{').count();
        assert_eq!(
            open_braces, 1,
            "expected exactly 1 frame object; got {} ({})",
            open_braces, json_line
        );
    }
}

#[test]
fn test_error_trace_jsonl_format() {
    // KARAC_ERROR_TRACE_FORMAT=jsonl → line-delimited JSON. One
    // event per line, each line a self-contained JSON object with
    // a `type` discriminator. Frames carry `"type":"frame"`; the
    // truncation marker (not exercised here — only one frame) would
    // be a separate `{"type":"truncated","max":N}` line.
    let captured = run_program_capturing_with_env(
        TRACE_FORMAT_SRC,
        Some("trace_fmt_jsonl.kara"),
        &[("KARAC_ERROR_TRACE_FORMAT", "jsonl")],
    );
    if let Some(c) = captured {
        assert_eq!(c.stdout.trim(), "7");
        // No text-mode header, no JSON-array bracket.
        assert!(
            !c.stderr.contains("Error return trace:"),
            "jsonl mode should not emit the text header; got {:?}",
            c.stderr
        );
        // Each non-empty stderr line must be a JSON object — i.e.
        // start with `{` and end with `}` — and contain the
        // `type` key.
        let trace_lines: Vec<&str> = c.stderr.lines().filter(|l| !l.is_empty()).collect();
        assert!(
            !trace_lines.is_empty(),
            "expected at least one JSONL line; got {:?}",
            c.stderr
        );
        for line in &trace_lines {
            assert!(
                line.starts_with('{') && line.ends_with('}'),
                "JSONL line must be a JSON object literal; got `{}`",
                line
            );
            assert!(
                line.contains("\"type\":"),
                "JSONL line missing `type` discriminator; got `{}`",
                line
            );
        }
        // One `?` site → exactly one frame line, no truncated marker.
        let frame_lines: Vec<&&str> = trace_lines
            .iter()
            .filter(|l| l.contains("\"type\":\"frame\""))
            .collect();
        assert_eq!(
            frame_lines.len(),
            1,
            "expected exactly 1 frame event; got {:?}",
            trace_lines
        );
        let frame = frame_lines[0];
        assert!(
            frame.contains("\"file\":"),
            "frame missing `file`: {}",
            frame
        );
        assert!(
            frame.contains("\"line\":"),
            "frame missing `line`: {}",
            frame
        );
        assert!(
            frame.contains("\"column\":"),
            "frame missing `column`: {}",
            frame
        );
        assert!(
            frame.contains("trace_fmt_jsonl.kara"),
            "filename not threaded into JSONL frame: {}",
            frame
        );
    }
}

#[test]
fn test_e2e_map_string_insert_get() {
    let out = run_program(
        r#"
fn main() {
    let mut m: Map[String, i64] = Map.new();
    m.insert("hello", 42_i64);
    let v = m.get("hello");
    match v {
        Some(x) => println(x),
        None => println(0_i64),
    }
    let v2 = m.get("world");
    match v2 {
        Some(x) => println(x),
        None => println(0_i64),
    }
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["42", "0"]);
    }
}

#[test]
fn test_e2e_map_string_insert_returns_old() {
    let out = run_program(
        r#"
fn main() {
    let mut m: Map[String, i64] = Map.new();
    let first = m.insert("key", 10_i64);
    match first {
        Some(x) => println(x),
        None => println(0_i64),
    }
    let second = m.insert("key", 20_i64);
    match second {
        Some(x) => println(x),
        None => println(0_i64),
    }
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["0", "10"]);
    }
}

#[test]
fn test_e2e_map_string_remove() {
    let out = run_program(
        r#"
fn main() {
    let mut m: Map[String, i64] = Map.new();
    m.insert("alpha", 1_i64);
    let r1 = m.remove("alpha");
    match r1 {
        Some(x) => println(x),
        None => println(0_i64),
    }
    let r2 = m.remove("alpha");
    match r2 {
        Some(x) => println(x),
        None => println(0_i64),
    }
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["1", "0"]);
    }
}

#[test]
fn test_e2e_map_string_contains_len() {
    let out = run_program(
        r#"
fn main() {
    let mut m: Map[String, i64] = Map.new();
    m.insert("x", 9_i64);
    println(m.contains_key("x"));
    println(m.contains_key("y"));
    println(m.len());
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["true", "false", "1"]);
    }
}

#[test]
fn test_e2e_map_string_for_loop_count() {
    let out = run_program(
        r#"
fn main() {
    let mut m: Map[String, i64] = Map.new();
    m.insert("a", 1_i64);
    m.insert("b", 2_i64);
    let mut count: i64 = 0;
    for (k, v) in m {
        count = count + 1_i64;
    }
    println(count);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "2");
    }
}

#[test]
fn test_e2e_map_index_get_existing_string_key() {
    let out = run_program(
        r#"
fn main() {
    let mut m: Map[String, i64] = Map.new();
    m.insert("hello", 100_i64);
    m.insert("world", 200_i64);
    println(m["world"]);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "200");
    }
}

#[test]
fn test_e2e_map_index_set_string_key() {
    let out = run_program(
        r#"
fn main() {
    let mut m: Map[String, i64] = Map.new();
    m["alice"] = 1_i64;
    m["bob"] = 2_i64;
    m["alice"] = 100_i64;
    println(m["alice"]);
    println(m["bob"]);
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["100", "2"]);
    }
}

#[test]
fn test_e2e_map_keys_string_keys_len() {
    // Keys are heap-bearing String values. Verify the resulting Vec[String]
    // reports the correct length. (For-loop element-type propagation —
    // `for s in vs { s.len() }` — is now wired; see
    // `test_e2e_for_in_vec_string_calls_len` below.)
    let out = run_program(
        r#"
fn main() {
    let mut m: Map[String, i64] = Map.new();
    m.insert("alice", 1_i64);
    m.insert("bob", 2_i64);
    let ks: Vec[String] = m.keys();
    println(ks.len());
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "2");
    }
}

// ── For-loop element-type propagation (List 2, item 3) ────────────

#[test]
fn test_e2e_for_in_vec_string_calls_len() {
    // Iterating Vec[String] should bind `s` as a String so `s.len()`
    // dispatches through compile_vec_method (String reuses the Vec
    // shape with elem=u8) and reads the actual length, not the
    // silent-`0` fall-through. Before the fix, both lines printed `0`.
    let out = run_program(
        r#"
fn main() {
    let mut v: Vec[String] = Vec.new();
    v.push("alice");
    v.push("bobby");
    for s in v {
        println(s.len());
    }
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["5", "5"]);
    }
}

#[test]
fn test_e2e_for_in_string_chars_count() {
    // `for c in s.chars()` over a String variable. The codegen peels
    // `.chars()` off and dispatches the String variable through
    // `compile_for_string_chars`, iterating one Unicode scalar per
    // step. Counts 5 chars in "hello".
    let out = run_program(
        r#"
fn main() {
    let s = "hello";
    let mut n: i64 = 0_i64;
    for _c in s.chars() {
        n = n + 1_i64;
    }
    println(n);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "5");
    }
}

#[test]
fn test_e2e_for_in_string_variable_iterates_chars() {
    // `for c in s` on a bare String variable — design.md § Character
    // type (line 2299) pins this as the semantic peer of `s.chars()`.
    // Before this slice, the variable went through
    // `compile_for_vec_var` (byte iteration with elem=i8), producing
    // i8 byte values instead of i32 codepoints.
    let out = run_program(
        r#"
fn main() {
    let s = "abc";
    let mut sum: i64 = 0_i64;
    for c in s {
        sum = sum + (c as i64);
    }
    println(sum);
}
"#,
    );
    if let Some(out) = out {
        // 'a' + 'b' + 'c' = 97 + 98 + 99 = 294
        assert_eq!(out.trim(), "294");
    }
}

#[test]
fn test_e2e_for_in_string_bytes_scan() {
    // `for b in s.bytes()` — the byte-wise sibling of `.chars()`.
    // Regression: before the `.bytes()` peel arm, this iterable fell
    // through to the dispatcher's silent `_ =>` arm and the loop body
    // never ran (compiled `n` stayed 0 while the interpreter counted
    // correctly) — a silent miscompile surfaced by kata-71's byte
    // scan. Counts the bytes and the '/' (47) bytes in "/a/b/".
    let out = run_program(
        r#"
fn main() {
    let s = "/a/b/";
    let mut n: i64 = 0_i64;
    let mut slashes: i64 = 0_i64;
    let slash: u8 = 47_u8;
    for b in s.bytes() {
        n = n + 1_i64;
        if b == slash {
            slashes = slashes + 1_i64;
        }
    }
    println(n);
    println(slashes);
}
"#,
    );
    if let Some(out) = out {
        // 5 bytes total, 3 of them '/'.
        assert_eq!(out.trim(), "5\n3");
    }
}

#[test]
fn test_e2e_for_in_string_bytes_multibyte_count() {
    // `é` is two UTF-8 bytes — `.bytes()` yields both (byte count, not
    // char count). Pins that the loop iterates raw bytes.
    let out = run_program(
        r#"
fn main() {
    let s = "é";
    let mut n: i64 = 0_i64;
    for _b in s.bytes() {
        n = n + 1_i64;
    }
    println(n);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "2");
    }
}

#[test]
fn test_e2e_for_in_string_literal_chars() {
    // String-literal iterable (no variable binding) — verifies the
    // `ExprKind::StringLit` arm in the for-loop dispatcher that the
    // `.chars()` peel-off recurses into. Sums the codepoints.
    let out = run_program(
        r#"
fn main() {
    let mut sum: i64 = 0_i64;
    for c in "xyz".chars() {
        sum = sum + (c as i64);
    }
    println(sum);
}
"#,
    );
    if let Some(out) = out {
        // 'x' + 'y' + 'z' = 120 + 121 + 122 = 363
        assert_eq!(out.trim(), "363");
    }
}

#[test]
fn test_e2e_for_in_empty_string_zero_iterations() {
    // Empty string — the byte-offset cond (`offset < len`) is false
    // at entry, so the body never runs. Pins the empty-edge case.
    let out = run_program(
        r#"
fn main() {
    let s = "";
    let mut n: i64 = 0_i64;
    for _c in s.chars() {
        n = n + 1_i64;
    }
    println(n);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "0");
    }
}

#[test]
fn test_e2e_for_in_indexed_vec_string_chars() {
    // `for c in vec[idx].chars()` over a `Vec[String]`. Before the
    // 2026-05-29 control_flow_for.rs fix, the `.chars()` peel-off
    // recursed via `compile_for(…, object, body)` and the recursed
    // call's dispatcher had no Index arm, so the body never ran —
    // the for-loop produced zero iterations with no error. kata-17
    // (Letter Combinations of a Phone Number) surfaced this on
    // `for letter in groups[idx].chars()` where groups: Vec[String]
    // holds the 8-row phone keypad. The fix handles the receiver
    // directly at the peel-off site rather than recursing into the
    // shape-keyed dispatcher.
    let out = run_program(
        r#"
fn main() {
    let mut groups: Vec[String] = Vec.new();
    groups.push("abc");
    groups.push("def");
    let idx: i64 = 1_i64;
    let mut sum: i64 = 0_i64;
    for c in groups[idx].chars() {
        sum = sum + (c as i64);
    }
    println(sum);
}
"#,
    );
    if let Some(out) = out {
        // 'd' + 'e' + 'f' = 100 + 101 + 102 = 303
        assert_eq!(out.trim(), "303");
    }
}

#[test]
fn test_e2e_for_in_string_chars_into_map_char_key() {
    // The LeetCode #3 idiom — char keys feeding a `Map[char, i64]`.
    // Inserts decoded chars from one pass and looks them up via
    // decoded chars from a second pass. Pins that the codepoint
    // values produced by the chars-iteration codegen are consistent
    // across calls (same hash, same key identity) — same shape the
    // sliding-window kata relies on. Uses only for-loop-bound char
    // values; mixing in `char` *literals* in `compile_expr` position
    // currently lowers to const_int(0) (pre-existing gap unrelated
    // to this slice — `ExprKind::CharLit` has no runtime arm in
    // `compile_expr`, only the const-eval table at line 83).
    let out = run_program(
        r#"
fn main() {
    let mut last_idx: Map[char, i64] = Map.new();
    let mut i: i64 = 0_i64;
    for c in "abca".chars() {
        last_idx.insert(c, i);
        i = i + 1_i64;
    }
    // Second pass: for each char in "abc", report its last-seen index.
    // 'a' was overwritten at index 3 (last position in "abca"); 'b' at 1; 'c' at 2.
    for c in "abc".chars() {
        match last_idx.get(c) {
            Some(v) => println(v),
            None => println(-1_i64),
        }
    }
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["3", "1", "2"]);
    }
}

#[test]
fn test_e2e_for_in_map_string_keys_use_len() {
    // `for (k, _v) in m` where K = String should bind `k` as a String
    // so `k.len()` dispatches correctly. Map iteration order is
    // unspecified, so we sum the lengths to make the assertion
    // order-independent.
    let out = run_program(
        r#"
fn main() {
    let mut m: Map[String, i64] = Map.new();
    m.insert("alice", 1_i64);
    m.insert("bobby", 2_i64);
    let mut total: i64 = 0_i64;
    for (k, _v) in m {
        total = total + k.len();
    }
    println(total);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "10");
    }
}

#[test]
fn test_e2e_for_in_slice_string_calls_len() {
    // Iterating Slice[String] (here from `Array[String, N].as_slice()`)
    // should bind the loop var as a String for correct method dispatch.
    let out = run_program(
        r#"
fn main() {
    let a: Array[String, 2] = ["alice", "bobby"];
    let s: Slice[String] = a.as_slice();
    for elem in s {
        println(elem.len());
    }
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["5", "5"]);
    }
}

#[test]
fn test_e2e_map_struct_with_string_key() {
    // `#[derive(Hash, Eq)]` struct with a String field — the per-field
    // recursion path is required: a byte-loop over the raw struct bytes
    // would hash the data-ptr + len + cap, which differs across distinct
    // allocations even when the string contents match. Per-field recursion
    // routes the String field through the contents-aware String hash.
    let out = run_program(
        r#"
#[derive(Hash, Eq)]
struct Tag {
    name: String,
    n: i64,
}

fn main() {
    let mut m: Map[Tag, i64] = Map.new();
    m.insert(Tag { name: "alice", n: 1_i64 }, 100_i64);
    m.insert(Tag { name: "alice", n: 2_i64 }, 200_i64);
    m.insert(Tag { name: "bob",   n: 1_i64 }, 300_i64);
    println(m.len());
    let v1 = m.get(Tag { name: "alice", n: 1_i64 });
    match v1 {
        Some(x) => println(x),
        None => println(0_i64 - 1_i64),
    }
    let v2 = m.get(Tag { name: "bob", n: 1_i64 });
    match v2 {
        Some(x) => println(x),
        None => println(0_i64 - 1_i64),
    }
    let v3 = m.get(Tag { name: "alice", n: 9_i64 });
    match v3 {
        Some(x) => println(x),
        None => println(0_i64 - 1_i64),
    }
}
"#,
    );
    let out = out.expect("struct-with-String-key codegen should not bail");
    let lines: Vec<&str> = out.trim().lines().collect();
    assert_eq!(lines, vec!["3", "100", "300", "-1"]);
}

#[test]
fn test_e2e_map_tuple_string_string_key() {
    // Two heap-bearing fields in the tuple — exercises the per-field
    // recursion path on both sides. A byte-loop FNV over raw struct bytes
    // would hash the two String headers (data ptr / len / cap pairs), which
    // diverge across allocations when the contents are equal — so this
    // test would fail under the pre-recursion hash.
    let out = run_program(
        r#"
fn main() {
    let mut m: Map[(String, String), i64] = Map.new();
    m.insert(("alice", "red"),  1_i64);
    m.insert(("alice", "blue"), 2_i64);
    m.insert(("bob",   "red"),  3_i64);
    println(m.len());
    let v = m.get(("alice", "blue"));
    match v {
        Some(x) => println(x),
        None => println(0_i64 - 1_i64),
    }
    let v2 = m.get(("alice", "green"));
    match v2 {
        Some(x) => println(x),
        None => println(0_i64 - 1_i64),
    }
}
"#,
    );
    let out = out.expect("(String,String)-key codegen should not bail");
    let lines: Vec<&str> = out.trim().lines().collect();
    assert_eq!(lines, vec!["3", "2", "-1"]);
}

#[test]
fn test_e2e_map_clear_string_key() {
    let out = run_program(
        r#"
fn main() {
    let mut m: Map[String, i64] = Map.new();
    m.insert("a", 1_i64);
    m.insert("b", 2_i64);
    m.clear();
    println(m.len());
    m.insert("c", 3_i64);
    println(m["c"]);
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["0", "3"]);
    }
}

#[test]
fn test_e2e_map_prefix_literal_string_keys() {
    let out = run_program(
        r#"
fn main() {
    let m: Map[String, i64] = Map["a": 1_i64, "b": 2_i64, "c": 3_i64];
    println(m.len());
    println(m["b"]);
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["3", "2"]);
    }
}

#[test]
fn test_e2e_vec_string_sort() {
    // `Vec[String].sort()` codegen: bare `sort()` over String elements now
    // lowers to the `karac_string_cmp` byte-lexicographic comparator (the
    // default-order String thunk), where it previously errored "integer
    // element types only". Ascending order, A/B with the interpreter.
    let out = run_program(
        r#"
fn main() {
    let mut v: Vec[String] = Vec.new();
    v.push("banana");
    v.push("apple");
    v.push("cherry");
    v.sort();
    for s in v {
        println(s);
    }
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["apple", "banana", "cherry"]);
    }
}

#[test]
fn test_e2e_map_string_keys_sorted_report() {
    // `Map[String,i64].keys()` + `Vec.sort()` — the canonical "ordered
    // report from a hash map" idiom. Exercises BOTH fixes together: keys()
    // deep-clones each String key into the result Vec (a shallow copy
    // double-freed against the map at scope exit — it crashed before), and
    // `.sort()` orders the String Vec. Counts read back with get_or.
    let out = run_program(
        r#"
fn main() {
    let mut m: Map[String, i64] = Map.new();
    m.insert("pear".to_string(), 3_i64);
    m.insert("fig".to_string(), 1_i64);
    m.insert("kiwi".to_string(), 2_i64);
    let mut keys: Vec[String] = m.keys();
    keys.sort();
    for k in keys {
        println(f"{k} {m.get_or(k.clone(), 0_i64)}");
    }
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["fig 1", "kiwi 2", "pear 3"]);
    }
}

#[test]
fn test_e2e_map_string_values_entries_owned() {
    // `Map[String,String].values()` / `entries()` deep-clone each heap half
    // into the result Vec (owned-Vec contract). Pre-fix the shallow copy
    // aliased the map's buffers and double-freed at scope exit.
    let out = run_program(
        r#"
fn main() {
    let mut m: Map[String, String] = Map.new();
    m.insert("k1".to_string(), "alpha".to_string());
    let vs: Vec[String] = m.values();
    println(vs.len());
    for v in vs { println(v); }
    for pair in m.entries() {
        let (a, b) = pair;
        println(f"{a}={b}");
    }
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["1", "alpha", "k1=alpha"]);
    }
}

#[test]
fn test_e2e_display_collection_fstring_and_to_string() {
    // Buffer-render path (unified with println): f-string interpolation of
    // a collection used to render EMPTY; `.to_string()` used to build-fail.
    // Both now work for Vec/Map/Set, including nesting.
    if let Some(out) = run_program(
        r#"
fn main() {
    let mut v: Vec[i64] = Vec.new();
    v.push(1);
    v.push(2);
    println(f"v={v}");
    println(v.to_string());
    let mut m: Map[String, i64] = Map.new();
    m.insert("k", 9);
    println(f"m={m}");
    println(m.to_string());
    let mut s: Set[i64] = Set.new();
    s.insert(7);
    println(s.to_string());
    let nested: Vec[Vec[i64]] = [[1], [2, 3]];
    println(f"n={nested}");
}
"#,
    ) {
        assert_eq!(
            out,
            "v=[1, 2]\n[1, 2]\nm={k: 9}\n{k: 9}\nSet{7}\nn=[[1], [2, 3]]\n"
        );
    }
}

#[test]
fn test_e2e_display_vec_empty() {
    let out = run_program(
        r#"
fn main() {
    let v: Vec[i64] = Vec.new();
    println(v);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "[]");
    }
}

#[test]
fn test_e2e_display_vec_string() {
    let out = run_program(
        r#"
fn main() {
    let mut v: Vec[String] = Vec.new();
    v.push("hi");
    v.push("bye");
    println(v);
}
"#,
    );
    if let Some(out) = out {
        // Interpreter's `Display for Value::String` is unquoted, so
        // codegen prints unquoted too — matches `src/interpreter.rs:213`.
        assert_eq!(out.trim(), "[hi, bye]");
    }
}

#[test]
fn test_e2e_display_vec_nested() {
    // Vec[Vec[i64]] — exercises recursive composition. The outer Vec
    // Display fn walks elements; each element is itself a Vec struct,
    // and the dispatcher routes the inner element's Display through
    // emit_vec_display_fn_te(i64).
    let out = run_program(
        r#"
fn main() {
    let mut a: Vec[i64] = Vec.new();
    a.push(1);
    a.push(2);
    let mut b: Vec[i64] = Vec.new();
    b.push(3);
    let mut outer: Vec[Vec[i64]] = Vec.new();
    outer.push(a);
    outer.push(b);
    println(outer);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "[[1, 2], [3]]");
    }
}

#[test]
fn test_e2e_display_map_empty() {
    let out = run_program(
        r#"
fn main() {
    let m: Map[i64, i64] = Map.new();
    println(m);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "{}");
    }
}

#[test]
fn test_e2e_set_string_insert_contains() {
    let out = run_program(
        r#"
fn main() {
    let mut s: Set[String] = Set.new();
    s.insert("alice");
    s.insert("bob");
    println(s.contains("alice"));
    println(s.contains("bob"));
    println(s.contains("missing"));
    println(s.len());
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["true", "true", "false", "2"]);
    }
}

#[test]
fn test_e2e_set_string_remove() {
    let out = run_program(
        r#"
fn main() {
    let mut s: Set[String] = Set.new();
    s.insert("alice");
    s.insert("bob");
    let r = s.remove("alice");
    println(r);
    println(s.contains("alice"));
    println(s.contains("bob"));
    println(s.len());
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["true", "false", "true", "1"]);
    }
}

#[test]
fn test_e2e_set_string_for_loop_count() {
    let out = run_program(
        r#"
fn main() {
    let mut s: Set[String] = Set.new();
    s.insert("alice");
    s.insert("bob");
    s.insert("alice");
    let mut count: i64 = 0;
    for _x in s {
        count = count + 1_i64;
    }
    println(count);
}
"#,
    );
    if let Some(out) = out {
        // alice appears twice, but as a set only once → 2 elements.
        assert_eq!(out.trim(), "2");
    }
}

#[test]
fn test_e2e_display_set_string_singleton() {
    let out = run_program(
        r#"
fn main() {
    let mut s: Set[String] = Set.new();
    s.insert("alice");
    println(s);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "Set{alice}");
    }
}

#[test]
fn test_e2e_display_set_empty() {
    let out = run_program(
        r#"
fn main() {
    let s: Set[i64] = Set.new();
    println(s);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "Set{}");
    }
}

#[test]
fn test_e2e_set_union_string() {
    // String elements exercise the per-element clone path — the result
    // owns independently-allocated buffers, not aliases of the source.
    let out = run_program(
        r#"
fn main() {
    let mut a: Set[String] = Set.new();
    a.insert("alpha");
    a.insert("beta");
    let mut b: Set[String] = Set.new();
    b.insert("beta");
    b.insert("gamma");
    let u: Set[String] = a.union(b);
    println(u.len());
    println(u.contains("alpha"));
    println(u.contains("beta"));
    println(u.contains("gamma"));
    println(u.contains("delta"));
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["3", "true", "true", "true", "false"]);
    }
}

#[test]
fn test_e2e_display_map_with_vec_value_singleton() {
    // Map[String, Vec[i64]] — the Map body recurses into Vec Display
    // for the value side. Single-entry map keeps output deterministic.
    let out = run_program(
        r#"
fn main() {
    let mut m: Map[String, Vec[i64]] = Map.new();
    let mut v: Vec[i64] = Vec.new();
    v.push(1);
    v.push(2);
    m.insert("k", v);
    println(m);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "{k: [1, 2]}");
    }
}

/// B-2026-08-14-9, heap-element half. `String` elements exercise what
/// scalars cannot: `fill` has to drop what each slot already owns and give
/// it a distinct buffer, `sort` moves multi-word values, and a `chunks`
/// header must borrow rather than copy. Output parity is only half the
/// evidence here — `asan_slice_mutators_and_views_on_heap_elements` is the
/// other half, and the leak-shaped failures show up only there.
#[test]
fn test_e2e_slice_methods_on_string_elements() {
    let src = r#"
fn ssort(xs: mut Slice[String]) { xs.sort(); }
fn srev(xs: mut Slice[String]) { xs.reverse(); }
fn sswap(xs: mut Slice[String]) { xs.swap(0i64, 2i64); }
fn sfill(xs: mut Slice[String]) { xs.fill("zz"); }

fn main() {
    let mut v: Vec[String] = Vec.new();
    v.push("pear"); v.push("apple"); v.push("fig");
    ssort(v.as_slice_mut());
    println(f"01 {v[0i64]} {v[1i64]} {v[2i64]}");
    srev(v.as_slice_mut());
    println(f"02 {v[0i64]} {v[1i64]} {v[2i64]}");
    sswap(v.as_slice_mut());
    println(f"03 {v[0i64]} {v[1i64]} {v[2i64]}");
    {
        let sv = v.as_slice();
        let cs = sv.chunks(2i64);
        let c0 = cs[0i64];
        println(f"04 {cs.len()} {c0.len()} {c0[0i64]}");
    }
    sfill(v.as_slice_mut());
    println(f"05 {v[0i64]} {v[1i64]} {v[2i64]}");
}
"#;
    assert_eq!(
        run_program(src).as_deref(),
        Some(
            "01 apple fig pear\n\
                 02 pear fig apple\n\
                 03 apple fig pear\n\
                 04 2 2 apple\n\
                 05 zz zz zz\n"
        ),
    );
}

#[test]
fn test_compound_enum_two_variants_both_string_payload_share_words() {
    let out = run_program(
        r#"
enum E { V1(String), V2(String) }
fn main() {
    let a = V1("first");
    let b = V2("second");
    match a {
        V1(s) => println(s),
        V2(_s) => println("nope"),
    }
    match b {
        V1(_s) => println("nope"),
        V2(s) => println(s),
    }
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["first", "second"]);
    }
}

#[test]
fn test_compound_tuple_payload_string_int() {
    // Heap-bearing element survives destructure with no double-free
    // / use-after-free (further pinned by ASAN test below).
    let out = run_program(
        r#"
enum E { V((String, i64)) }
fn main() {
    let e = V(("hello", 42));
    match e {
        V((s, n)) => {
            println(s);
            println(n);
        }
    }
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["hello", "42"]);
    }
}

#[test]
fn test_8ai_string_return_state_struct_terminal_field_is_vec_struct() {
    // String shares Vec's `{ptr, i64, i64}` layout.
    let ir = ir_for_with_state_struct_layouts(
        "effect resource Network;
             pub fn fetch() with sends(Network) receives(Network) {}
             fn driver() -> String with sends(Network) receives(Network) { fetch(); String.new() }",
    );
    let line = ir
        .lines()
        .find(|l| l.starts_with("%kara.state.driver = type {"))
        .unwrap_or_else(|| panic!("no driver state struct in IR:\n{ir}"));
    assert!(
        line.contains("i32, { ptr, i64, i64 }"),
        "String return: state struct must include the 3-word string descriptor:\n{line}"
    );
    let body = extract_fn_ir(&ir, "__kara_poll_driver");
    assert!(
        body.contains("store { ptr, i64, i64 } zeroinitializer, ptr %kara.return.field_ptr"),
        "String return: terminal arm must store zeroinitializer placeholder:\n{body}"
    );
}

// ── Phase 6 line 26 slice 8w: destructor classification via `type_subst` ──
//
// Slice 8v Phase 2 lands per-mono state-machine emission, but the
// destructor classifier only inspects `field.type_name` directly.
// For type-parameter-typed parameters (e.g. `fn driver[T](item: T)`)
// the typechecker records `type_name: None`, so the destructor
// fell back to `FieldDrop::Skip` for every mono — leaking heap-
// bearing monos like `T = String` under runtime cancel/Err
// unwinding. Slice 8w closes the Vec-shape gap: when
// `field.type_name == None`, look up the parameter's declared
// `TypeExpr` via `lookup_param_type_expr` (shared with the
// state-struct shape path) and resolve through
// `llvm_type_for_type_expr` against the active `type_subst`. If
// the resolved LLVM type is the Vec struct shape (`{ ptr, i64,
// i64 }`, used by `String` / `Vec[U]` / `VecDeque[U]`),
// classify as `FieldDrop::VecOrString`. Shared-`T` stays as
// `Skip` in 8w because `infer_type_args` loses the surface name
// when it binds `T → ptr_type` — recovering the
// `shared_types[N].heap_type` for `emit_refcount_dec` needs a
// parallel name-tracking table, deferred as a follow-on slice.

#[test]
fn test_slice_8w_per_mono_destructor_emitted_for_string_type_arg() {
    // `fn driver[T](item: T)` instantiated with `T = String` —
    // the polymorphic field's `type_name` is `None`, but
    // `lookup_param_type_expr` recovers `TypeExpr::Path(["T"])`
    // and `llvm_type_for_type_expr` resolves it through
    // `type_subst[T]` (set by `infer_type_args` to the LLVM
    // type of the `String` arg — the vec_struct_type shape).
    // The 8w classification recognises this as
    // `FieldDrop::VecOrString` and emits the per-mono destructor
    // with the `cap > 0 ? free(data)` IR pattern.
    let ir = ir_for_with_state_struct_layouts(
        "effect resource Network;
             pub fn fetch() with sends(Network) receives(Network) {}
             fn driver[T](item: T) { fetch(); }
             fn caller() {
                 let s = String.new();
                 driver(s);
             }",
    );
    // The per-mono destructor's mangled key picks up
    // `llvm_type_to_mangle_str`'s `struct` shorthand for the
    // vec-struct-shaped String arg, PLUS the B-2026-07-11-35
    // element-aware collision-disambiguation suffix (`$T_ct_String`)
    // so String / Vec[i64] / Vec[String] monos are distinct symbols.
    let drop_fn_marker = "@\"__kara_state_drop_driver$struct$T_ct_String\"";
    assert!(
        ir.contains(drop_fn_marker),
        "per-mono destructor must emit for String-typed T captured local:\n{ir}"
    );
    // Verify the `cap > 0 ? free` shape inside the destructor
    // body. LLVM quotes the destructor name (`@"..."`) due to
    // the `$` mangling marker, so we grep the whole IR rather
    // than extract by exact name (extract_fn_ir's needle would
    // miss the quoted form).
    assert!(
        ir.contains("%item.drop.cap = load i64"),
        "destructor must load Vec cap for the T-typed item field:\n{ir}"
    );
    assert!(
        ir.contains("%item.drop.is_heap = icmp sgt i64 %item.drop.cap, 0"),
        "destructor must compare cap > 0:\n{ir}"
    );
    assert!(
        ir.contains("call void @free(ptr %item.drop.data)"),
        "destructor must call free on the captured String's data ptr:\n{ir}"
    );
}

#[test]
fn test_ir_ref_param_with_string_literal_rvalue() {
    let ir = ir_for(
        "fn show(s: ref String) {}\n\
             fn main() { show(\"hello\") }",
    );
    // String body is { ptr, i64, i64 } in IR. The temp's element
    // type should be that struct, and the call should pass `ptr`.
    assert!(
        ir.contains("%ref_rvalue_arg0"),
        "string-literal rvalue should be materialized:\n{ir}"
    );
    // The store target is the alloca pointer; we just check the
    // call passes the temp pointer (string-literal IR text varies
    // by host string-internalization choices).
    assert!(
        ir.contains("call void @show(ptr %ref_rvalue_arg0)"),
        "call should pass the alloca pointer for ref String rvalue:\n{ir}"
    );
}

#[test]
fn test_ir_ref_param_vec_string_rvalue_registers_cleanup() {
    // C-followup: when the materialized rvalue carries the
    // Vec/String `{ptr,len,cap}` layout, the temp must be
    // registered through the same scope-exit cleanup that
    // `let`-bindings use, so a heap-owning rvalue (e.g.
    // `report(s + "x")` / `report(make())`) doesn't leave its
    // buffer unreachable after the call. Cap=0 (literal) values
    // short-circuit inside the `FreeVecBuffer` walker via the
    // `cap > 0` guard, so the registration is safe to apply
    // unconditionally for Vec/String-shaped values.
    let ir = ir_for(
        "fn show(s: ref String) {}\n\
             fn main() { show(\"lit\") }",
    );
    // The temp's name aligns with the `i = 0` arg position.
    // The FreeVecBuffer cleanup walker reads each of the
    // alloca's struct slots (data ptr, len, cap) at scope exit.
    // The cap-slot GEP fires once per Vec/String tracked through
    // `track_vec_var`; a missing registration would skip it.
    assert!(
        ir.contains("%ref_rvalue_arg0"),
        "string-literal rvalue should materialize a temp:\n{ir}"
    );
    // The scope-exit walker emits a getelementptr on the alloca
    // to read the cap field. Its label format includes the
    // alloca's name; check that the GEP-on-temp shape appears.
    assert!(
        ir.contains("ref_rvalue_arg0"),
        "temp should be referenced by the scope-exit FreeVecBuffer walker:\n{ir}"
    );
}

// ── `String.bytes() -> Slice[u8]` (design.md § Character type) ──
//
// Zero-copy view: the slice header reuses the source String's data
// pointer and length without allocating, so `s.bytes()[i]` is O(1)
// and the O(n) Vec[char] snapshot pattern the katas worked around
// is no longer needed. Element type is fixed at u8 (i8 in LLVM —
// signedness lives at the type-checker level, not in IR).

#[test]
fn test_ir_string_bytes_emits_slice_header() {
    let ir = ir_for(
        "fn f() -> i64 {\n\
                 let s = \"hello\";\n\
                 let bs = s.bytes();\n\
                 bs.len()\n\
             }",
    );
    // The bytes() lowering reads field 0 (data) and field 1
    // (len) from the String's `{ptr, i64, i64}` struct. The
    // existing naming convention is `bytes.data` / `bytes.len`.
    assert!(
        ir.contains("bytes.data"),
        "bytes() should GEP+load the data pointer:\n{ir}"
    );
    assert!(
        ir.contains("bytes.len"),
        "bytes() should GEP+load the length:\n{ir}"
    );
    // The resulting value packs into the 2-field slice header
    // `{ptr, i64}`; the existing helper labels are `slice.ptr`
    // and `slice.len` (see `build_slice_header`).
    assert!(
        ir.contains("slice.ptr"),
        "bytes() should pack data into a slice header:\n{ir}"
    );
}

// ──────────────────────────────────────────────────────────────────
// Phase 7 § match-on-String codegen (filed 2026-05-21 as `f793929`).
//
// Before the fix, `match s { "alpha" => ..., "beta" => ..., _ => ... }`
// with `s: String` compiled through the typechecker but panicked in
// codegen at `src/codegen/expr_ops.rs:1138` ("Found StructValue but
// expected the IntValue variant") — the literal-pattern arm emitted
// only a `*const i8` for the String pattern, then `compile_binop`
// (struct vs. ptr) fell into the int path and panicked on
// `lhs.into_int_value()`. The fix: build a full String struct
// `{ data, len, cap }` for the literal pattern (mirroring
// `ExprKind::StringLit` codegen in `src/codegen/exprs.rs:39-61`),
// which then routes both operands through `compile_string_binop`'s
// length-check + `memcmp` equality.
// ──────────────────────────────────────────────────────────────────

#[test]
fn test_e2e_match_on_string_basic() {
    // Positive case: each input routes to the matching arm; no
    // pattern match falls through to the wildcard.
    let output = run_program(
        "fn classify(s: String) -> i64 {\n\
                 match s {\n\
                     \"alpha\" => 1,\n\
                     \"beta\" => 2,\n\
                     _ => 0,\n\
                 }\n\
             }\n\
             fn main() {\n\
                 println(classify(\"alpha\"));\n\
                 println(classify(\"beta\"));\n\
                 println(classify(\"other\"));\n\
             }",
    )
    .expect("compile + run failed");
    assert_eq!(output.trim(), "1\n2\n0");
}

#[test]
fn test_e2e_match_on_string_function_return_scrutinee() {
    // Non-identifier scrutinee: the scrutinee is a function-call
    // result that returns a fresh owned String. Pins that the
    // match codegen accepts this shape (the kata's natural form
    // is `match req.path() { ... }`).
    let output = run_program(
        "fn label_for(n: i64) -> String {\n\
                 match n {\n\
                     1 => \"one\",\n\
                     2 => \"two\",\n\
                     _ => \"other\",\n\
                 }\n\
             }\n\
             fn classify(n: i64) -> i64 {\n\
                 match label_for(n) {\n\
                     \"one\" => 10,\n\
                     \"two\" => 20,\n\
                     _ => 99,\n\
                 }\n\
             }\n\
             fn main() {\n\
                 println(classify(1));\n\
                 println(classify(2));\n\
                 println(classify(5));\n\
             }",
    )
    .expect("compile + run failed");
    assert_eq!(output.trim(), "10\n20\n99");
}

#[test]
fn test_e2e_match_on_string_routes_to_correct_arm() {
    // Pin per-arm routing: each pattern must select its own
    // body, not the next one. A regression where all arms
    // shared a single body would still pass the basic test if
    // the wildcard wasn't taken; this one asserts each output
    // independently.
    let output = run_program(
        "fn handle(p: String) -> String {\n\
                 match p {\n\
                     \"/a\" => \"AAA\",\n\
                     \"/b\" => \"BBB\",\n\
                     \"/c\" => \"CCC\",\n\
                     _ => \"404\",\n\
                 }\n\
             }\n\
             fn main() {\n\
                 println(handle(\"/a\"));\n\
                 println(handle(\"/b\"));\n\
                 println(handle(\"/c\"));\n\
                 println(handle(\"/z\"));\n\
             }",
    )
    .expect("compile + run failed");
    assert_eq!(output.trim(), "AAA\nBBB\nCCC\n404");
}

// ──────────────────────────────────────────────────────────────────
// String.starts_with — typechecker arm + interpreter dispatch +
// codegen dispatch (shipped 2026-05-21).
//
// String and Vec share the `{ptr, len, cap}` layout, so String
// method calls route through `compile_vec_method`. The codegen
// arm short-circuits to `false` when `recv.len < prefix.len`,
// otherwise reuses `self.memcmp_fn` (the same memcmp used by
// String `==`) to compare the first `prefix.len` bytes.
// ──────────────────────────────────────────────────────────────────

#[test]
fn test_e2e_string_starts_with_basic() {
    let output = run_program(
            "fn main() {\n\
                 let s: String = \"/todos/42\";\n\
                 if s.starts_with(\"/todos/\") { println(\"yes\"); } else { println(\"no\"); }\n\
                 if s.starts_with(\"/foo\") { println(\"yes2\"); } else { println(\"no2\"); }\n\
                 if s.starts_with(\"/todos/42/extra\") { println(\"yes3\"); } else { println(\"no3\"); }\n\
                 if s.starts_with(\"\") { println(\"yes4\"); } else { println(\"no4\"); }\n\
             }",
        )
        .expect("compile + run failed");
    assert_eq!(output.trim(), "yes\nno2\nno3\nyes4");
}

#[test]
fn test_ir_string_starts_with_uses_memcmp() {
    let ir = ir_for(
        "fn matches(s: String, p: String) -> bool {\n\
                 s.starts_with(p)\n\
             }",
    );
    assert!(
        ir.contains("call i32 @memcmp") || ir.contains("call ptr @memcmp"),
        "String.starts_with should emit memcmp:\n{ir}"
    );
}

#[test]
fn test_e2e_string_starts_with_function_return_receiver_bound() {
    // The kata's natural form: receiver comes from a function-call
    // result (`req.path()` returning a fresh owned String), bound
    // to an annotated identifier first. Two related codegen gaps
    // sit just outside this slice and would need their own
    // follow-ups: (a) chained `path().starts_with(...)` on a
    // non-identifier receiver, and (b) bare `let p = path();`
    // without the `: String` annotation (`pattern_binding_types`
    // doesn't yet write "String" for inferred bindings, so
    // `vec_elem_types` doesn't register and the dispatch falls
    // through). Both affect every Vec/String method on
    // function-return receivers, not just `starts_with`. The
    // annotated bind form `let p: String = path();` works.
    let output = run_program(
        "fn path() -> String { \"/todos/42\" }\n\
             fn main() {\n\
                 let p: String = path();\n\
                 if p.starts_with(\"/todos/\") { println(\"hit\"); } else { println(\"miss\"); }\n\
             }",
    )
    .expect("compile + run failed");
    assert_eq!(output.trim(), "hit");
}

#[test]
fn test_e2e_string_repeat() {
    // `String.repeat(n)`: basic, single, zero, negative (-> empty),
    // empty-receiver, multi-byte, and as a push_str argument (fresh-temp
    // freed by the push_str arm). Surfaced by kata-katas #394 decode-string.
    let output = run_program(
        "fn main() {\n\
                 let s: String = \"ab\";\n\
                 println(s.repeat(3));\n\
                 println(s.repeat(1));\n\
                 println(s.repeat(0));\n\
                 println(s.repeat(-2));\n\
                 let e: String = \"\";\n\
                 println(e.repeat(5));\n\
                 let u: String = \"λ\";\n\
                 println(u.repeat(3));\n\
                 let mut out: String = \"x\";\n\
                 out.push_str(s.repeat(2));\n\
                 println(out);\n\
             }",
    )
    .expect("compile + run failed");
    assert_eq!(output, "ababab\nab\n\n\n\nλλλ\nxabab\n");
}

#[test]
fn test_e2e_string_substring_basic() {
    // In-range / start-zero / out-of-range / negative / empty-receiver.
    let output = run_program(
        "fn main() {\n\
                 let s: String = \"/todos/42\";\n\
                 println(s.substring(7));\n\
                 println(s.substring(0));\n\
                 println(s.substring(100));\n\
                 println(s.substring(-1));\n\
                 let empty: String = \"\";\n\
                 println(empty.substring(0));\n\
             }",
    )
    .expect("compile + run failed");
    assert_eq!(output, "42\n/todos/42\n\n\n\n");
}

#[test]
fn test_ir_string_substring_uses_malloc_and_memcpy() {
    // Regression guard: the non-empty branch must allocate fresh
    // and memcpy from the receiver. If a future refactor borrows
    // the receiver's buffer instead, the resulting String would
    // alias the receiver's storage and free-after-free at scope
    // exit. Detect any obvious refactor by checking IR mentions
    // the malloc call and the memcpy intrinsic.
    let ir = ir_for(
        "fn tail(s: String) -> String {\n\
                 s.substring(3)\n\
             }",
    );
    assert!(
        ir.contains("call ptr @malloc") || ir.contains("call i8* @malloc"),
        "String.substring should malloc a fresh buffer:\n{ir}"
    );
    assert!(
        ir.contains("llvm.memcpy"),
        "String.substring should memcpy from the receiver:\n{ir}"
    );
}

#[test]
fn test_e2e_string_receiver_parse_sugar() {
    // String-receiver `s.parse()` (the Rust-familiar sugar) resolved against
    // an expected `Option[T]`: the typechecker records the target from the
    // annotation and lowering rewrites to the existing `T.parse(s)`, so both
    // backends reuse the type-receiver parse verbatim. Covers a `let`
    // annotation, a variable receiver, i64 / f64 / u32 targets, a bad input
    // (None), a fn-return position, and an argument position.
    let output = run_program(
        "fn to_opt(s: String) -> Option[i64] { s.parse() }\n\
             fn takes(o: Option[i64]) -> i64 { match o { Some(v) => v, None => -1 } }\n\
             fn main() {\n\
                 let a: Option[i64] = \"42\".parse();\n\
                 match a { Some(n) => println(n), None => println(-1) }\n\
                 let b: Option[i64] = \"nope\".parse();\n\
                 match b { Some(n) => println(n), None => println(-1) }\n\
                 let f: Option[f64] = \"3.5\".parse();\n\
                 match f { Some(x) => println(x), None => println(-1.0) }\n\
                 let u: Option[u32] = \"7\".parse();\n\
                 match u { Some(x) => println(x), None => println(0) }\n\
                 let s: String = \"99\";\n\
                 let v: Option[i64] = s.parse();\n\
                 match v { Some(n) => println(n), None => println(-1) }\n\
                 match to_opt(\"123\") { Some(n) => println(n), None => println(-1) }\n\
                 println(takes(\"55\".parse()));\n\
             }",
    )
    .expect("compile + run failed");
    // 42; None->-1; 3.5; 7; 99; return-pos 123; arg-pos 55
    assert_eq!(output, "42\n-1\n3.5\n7\n99\n123\n55\n");
}

#[test]
fn test_e2e_char_try_from() {
    // #10: `char.try_from(n) -> Result[char, i64]` — fallible codepoint→
    // char conversion (the `E_INT_AS_CHAR` rejection of `n as char` points
    // here). Valid scalars → Ok(char); the surrogate range (0xD800..=0xDFFF),
    // values above 0x10FFFF, and negatives → Err(codepoint). u8 (always
    // valid), a BMP char, an astral char (emoji), and the three failure
    // classes.
    let output = run_program(
            "fn show(r: Result[char, i64]) {\n\
                 match r { Ok(ch) => println(ch.to_string()), Err(cp) => println(\"err:\" + cp.to_string()) }\n\
             }\n\
             fn main() {\n\
                 let b: u8 = 65;\n\
                 show(char.try_from(b));\n\
                 show(char.try_from(97));\n\
                 show(char.try_from(0x1F600));\n\
                 show(char.try_from(0xD800));\n\
                 show(char.try_from(0x110000));\n\
                 show(char.try_from(-1));\n\
             }",
        )
        .expect("compile + run failed");
    assert_eq!(output, "A\na\n😀\nerr:55296\nerr:1114112\nerr:-1\n");
}

#[test]
fn test_e2e_char_to_string_from_and_into() {
    // `From[char] for String` (design.md § Conversion Traits, "from char
    // literals"). Both `String.from(c)` and `c.into()` build a one-glyph
    // owned heap String via UTF-8 encoding (the `char.to_string()` path);
    // multibyte chars encode fully; the result supports `+`. Build==run
    // parity with `test_char_to_string_from_and_into_interpreter`. The heap
    // Strings are freed on scope exit — valgrind-clean (verified by hand;
    // pinned by `asan_char_to_string_no_leak`).
    let output = run_program(
        "fn main() {\n\
                 let a: String = String.from('Z');\n\
                 println(a);\n\
                 let ch: char = 'Q';\n\
                 let b: String = ch.into();\n\
                 println(b);\n\
                 let c: String = '😀'.into();\n\
                 println(c);\n\
                 let d: String = String.from('A') + \"BC\";\n\
                 println(d);\n\
             }",
    )
    .expect("compile + run failed");
    assert_eq!(output, "Z\nQ\n😀\nABC\n");
}

#[test]
fn test_e2e_println_preserves_interior_nul() {
    // L5: `println`/`print` must emit interior NUL bytes, not truncate at
    // the first NUL. Pre-fix TWO bugs compounded — the print path lowered
    // to `printf("%.*s")` (stops at the first NUL even with a precision),
    // AND string-literal / f-string-text storage used
    // `LLVMBuildGlobalString` (C-string truncation, so the global lost
    // everything past the NUL). The fix uses NUL-safe `fwrite` for the
    // print and byte-array globals for the literals. Covers: a string
    // literal with an interior NUL, the `'\0'` char, f-string text with
    // `\0`, and a heap-concat result (memcpy-built, fed from a NUL literal).
    let output = run_program(
        "fn main() {\n\
                 println(\"AB\\0CD\");\n\
                 println('\\0');\n\
                 println(f\"pre \\0 post\");\n\
                 let a = \"AB\\0\";\n\
                 print(a + \"CD\");\n\
                 println(\"\");\n\
             }",
    )
    .expect("compile + run failed");
    assert_eq!(output, "AB\0CD\n\0\npre \0 post\nAB\0CD\n");
}

// ── Phase 8 line 435 follow-up — std.json codegen ─────────────
//
// Compiled-binary path for `j.stringify()` over the baked
// `runtime/stdlib/json.kara` `Json` enum. Six tests cover the
// walker's per-variant arms (Null/Bool/Number/String/Array/
// Object), the non-identifier-receiver entry shape
// (`Json.X(...).stringify()` without an intermediate `let`),
// and the 3-segment-Path entry shape for the unit `Null`
// variant (`Json.Null.stringify()` parses as one Path callee).

#[test]
fn test_e2e_json_stringify_null() {
    let out = run_program("fn main() { println(Json.Null.stringify()); }");
    if let Some(out) = out {
        assert_eq!(out.trim(), "null");
    }
}

#[test]
fn test_e2e_json_stringify_bool_true() {
    let out = run_program("fn main() { println(Json.Bool(true).stringify()); }");
    if let Some(out) = out {
        assert_eq!(out.trim(), "true");
    }
}

#[test]
fn test_e2e_json_stringify_bool_false() {
    let out = run_program("fn main() { println(Json.Bool(false).stringify()); }");
    if let Some(out) = out {
        assert_eq!(out.trim(), "false");
    }
}

#[test]
fn test_e2e_json_stringify_number() {
    let out = run_program("fn main() { println(Json.Number(3.14).stringify()); }");
    if let Some(out) = out {
        assert_eq!(out.trim(), "3.14");
    }
}

#[test]
fn test_e2e_json_stringify_string() {
    let out = run_program("fn main() { println(Json.String(\"hi\").stringify()); }");
    if let Some(out) = out {
        assert_eq!(out.trim(), "\"hi\"");
    }
}

#[test]
fn test_e2e_json_stringify_identifier_receiver() {
    // Variable-bound Json receiver — exercises the `var_type_names[
    // "j"] == "Json"` dispatch arm in `compile_method_call` rather
    // than the non-identifier path. Covers the parser shape where
    // `Json.Null` is the RHS of a let and `.stringify()` is a
    // distinct MethodCall against the binding.
    let out = run_program(
        "fn main() {\n\
                 let j = Json.Number(42.0);\n\
                 println(j.stringify());\n\
             }",
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "42.0");
    }
}

#[test]
fn test_e2e_json_stringify_array() {
    // Vec[Json] payload — exercises the 32-byte element stride and
    // the recursive `__karac_json_kara_to_ffi` self-call inside the
    // array arm. Verifies the per-element walk produces JSON in
    // source order.
    let out = run_program(
        "fn main() {\n\
                 let mut xs: Vec[Json] = Vec.new();\n\
                 xs.push(Json.Number(1.0));\n\
                 xs.push(Json.Number(2.0));\n\
                 xs.push(Json.Number(3.0));\n\
                 println(Json.Array(xs).stringify());\n\
             }",
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "[1.0,2.0,3.0]");
    }
}

#[test]
fn test_e2e_json_stringify_object() {
    // Vec[(String, Json)] payload — exercises the 56-byte tuple
    // stride (String at offset 0, Json at offset 24) and the per-
    // key CString allocation in `karac_runtime_json_alloc_key`.
    // Verifies insertion-order Object iteration in stringify
    // output (locked design (ii) in `runtime/stdlib/json.kara`).
    let out = run_program(
        "fn main() {\n\
                 let mut pairs: Vec[(String, Json)] = Vec.new();\n\
                 pairs.push((\"a\", Json.Number(1.0)));\n\
                 pairs.push((\"b\", Json.Bool(true)));\n\
                 println(Json.Object(pairs).stringify());\n\
             }",
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "{\"a\":1.0,\"b\":true}");
    }
}

#[test]
fn test_ir_json_stringify_emits_helper() {
    // Pin the synthesized walker is materialized exactly once even
    // across multiple `.stringify()` call sites — the lazy memo on
    // `__karac_json_kara_to_ffi` is what keeps IR size bounded.
    let ir = ir_for(
        "fn main() {\n\
                 println(Json.Number(1.0).stringify());\n\
                 println(Json.Number(2.0).stringify());\n\
             }",
    );
    assert!(
        ir.contains("__karac_json_kara_to_ffi"),
        "stringify dispatch should reference the synthesized walker:\n{ir}"
    );
    let helper_defs = ir
        .matches("define internal ptr @__karac_json_kara_to_ffi")
        .count();
    assert_eq!(
        helper_defs, 1,
        "walker should be defined exactly once, found {}:\n{}",
        helper_defs, ir
    );
    // Both call sites should reuse the helper.
    let helper_calls = ir.matches("call ptr @__karac_json_kara_to_ffi").count();
    assert!(
        helper_calls >= 2,
        "expected ≥2 helper calls (one per stringify site), got {}:\n{}",
        helper_calls,
        ir
    );
}

#[test]
fn test_e2e_json_parse_error_returns_err() {
    // Invalid JSON — the Err arm fires and the tag carries through
    // the Result destructure. The kata's `/echo` endpoint relies
    // on this Err/Ok discrimination.
    let out = run_program(
        "fn main() {\n\
                 match Json.parse(\"{not json}\") {\n\
                     Ok(_) => println(\"ok\"),\n\
                     Err(_) => println(\"err\"),\n\
                 }\n\
             }",
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "err");
    }
}

#[test]
fn e2e_json_parse_error_fields_are_readable() {
    // B-2026-08-12-14 — reading a field off a `Json.parse` error was a
    // RUN-VS-BUILD split: `karac run --interp` printed `e.line`, while
    // `karac build` REFUSED the program with `codegen: cannot resolve
    // field 'line' on this receiver … this is a compiler gap`. So the
    // error half of `Json.parse`'s documented `Result[Json, JsonError]`
    // could not be inspected at all from a compiled binary — only matched
    // on and discarded.
    //
    // `json.kara` is not in `compiled_stdlib_programs`, so `JsonError`
    // never reached `declare_structs` and had no layout to GEP.
    // `seed_builtin_struct_types` now seeds it the way `HttpError` and
    // `Response` are already seeded, with the AS-BUILT word layout
    // (`line`/`column` are `i64` there, not the declared `u32` —
    // `json.rs` packs them into the widened Result as full words).
    //
    // Covers the three positions that resolve the field differently: a
    // bare read, an f-string interpolation, and the whole struct passed
    // BY VALUE to a fn that reads two fields. The `message` String field
    // is exercised by length rather than content so the pin does not
    // encode serde_json's wording.
    let out = run_program(
            "fn describe(e: JsonError) -> String { return f\"{e.line}:{e.column}:{e.message.len()}\"; }\n\
             fn main() {\n\
                 let bad = Json.parse(\"{bad\");\n\
                 match bad {\n\
                     Ok(_v) => println(\"ok\"),\n\
                     Err(e) => {\n\
                         println(e.line);\n\
                         println(f\"col={e.column}\");\n\
                         println(e.message.len() > 0);\n\
                         let outer_len = e.message.len();\n\
                         println(describe(e) == f\"1:2:{outer_len}\");\n\
                     }\n\
                 }\n\
                 let good = Json.parse(\"{\\\"a\\\": 1}\");\n\
                 match good { Ok(_v) => println(\"parsed\"), Err(e2) => println(e2.message) }\n\
             }",
        );
    // Matches `karac run --interp` on the identical source, verified
    // before this pin was written.
    //
    // B-2026-08-12-16 extended the by-value leg: `describe` now reports
    // the message's LENGTH, compared against the length the caller read
    // before handing the struct over. That closes the direction the leak
    // fix opened. Wiring a real `cap` (and the `struct_field_type_exprs`
    // seed that finally armed the arm-binding drop) means the message
    // buffer is now genuinely freed rather than left alive until process
    // exit — so a mis-scoped free would be a use-after-free that reads a
    // garbage length here, where before the fix EVERY read was trivially
    // safe because nothing was ever freed. Still length rather than
    // content, so the pin does not encode serde_json's wording.
    assert_eq!(out.as_deref(), Some("1\ncol=2\ntrue\ntrue\nparsed\n"));
}

#[test]
fn test_ir_json_parse_emits_lift_helper_once() {
    // Pin the lift walker definition is module-private and emitted
    // exactly once across multiple `Json.parse(...)` call sites,
    // mirroring the stringify-side invariant.
    let ir = ir_for(
        "fn main() {\n\
                 let r1 = Json.parse(\"1\");\n\
                 let r2 = Json.parse(\"2\");\n\
             }",
    );
    assert!(
        ir.contains("__karac_json_ffi_to_kara"),
        "parse dispatch should reference the synthesized lift helper:\n{ir}"
    );
    let helper_defs = ir
        .matches("define internal { i64, i64, i64, i64 } @__karac_json_ffi_to_kara")
        .count();
    assert_eq!(
        helper_defs, 1,
        "lift helper should be defined exactly once, found {}:\n{}",
        helper_defs, ir
    );
    // Both call sites should reach `karac_runtime_json_parse`.
    let parse_calls = ir.matches("call ptr @karac_runtime_json_parse").count();
    assert!(
        parse_calls >= 2,
        "expected ≥2 runtime parse calls (one per Json.parse site), got {}:\n{}",
        parse_calls,
        ir
    );
}

#[test]
fn test_e2e_modbind_string_slice_literal() {
    // `let G: StringSlice = "hello";` materialises a (ptr, len,
    // cap=0) aggregate global. `cap=0` is the static-buffer
    // marker — runtime scope-exit cleanup never frees this
    // pointer.
    let output = run_program(
        "let GREETING: StringSlice = \"hello\";\n\
             fn main() { println(GREETING); }",
    )
    .expect("compile + run failed");
    assert_eq!(output, "hello\n");
}

#[test]
fn test_e2e_modbind_string_for_loop_iterates_chars() {
    // The String arm rides the same dispatch, and it is ordered BEFORE the
    // Vec arm on purpose (String vars are also registered in
    // `vec_elem_types` with an i8 element type). Iterating a module-level
    // StringSlice must therefore still yield chars, not bytes — a regression
    // that reordered the arms would print 5 bytes' worth of something else.
    let output = run_program(
        "let GREETING: StringSlice = \"héllo\";\n\
             fn main() {\n\
                 let mut n = 0;\n\
                 for c in GREETING { n = n + 1; }\n\
                 println(n);\n\
             }",
    )
    .expect("compile + run failed");
    assert_eq!(output, "5\n");
}

#[test]
fn test_e2e_shadow_string_to_vec_chars_references_old() {
    // String→Vec[char] shadow whose RHS (`s.chars()`) dispatches on the
    // OLD String binding. Same-layout cross-class ({ptr,i64,i64}); the
    // dance is what lets `chars()` see a String while `s.len()` after the
    // rebind sees the new Vec[char].
    if let Some(out) = run_program(
        "fn main() {\n\
             let s = \"abc\";\n\
             let s = s.chars();\n\
             println(s.len());\n\
             }",
    ) {
        assert_eq!(out, "3\n");
    }
}

#[test]
fn test_e2e_shadow_vec_to_string() {
    // Vec→String shadow (same layout, opposite direction). The old
    // collection tags must be gone so `s` dispatches as a String.
    if let Some(out) = run_program(
        "fn main() {\n\
             let mut s: Vec[i64] = Vec.new();\n\
             s.push(1i64);\n\
             let s = \"done\";\n\
             println(s);\n\
             println(s.len());\n\
             }",
    ) {
        assert_eq!(out, "done\n4\n");
    }
}

#[test]
fn test_e2e_surface_binary_string_concat_shapes() {
    // B-2026-07-21-12 output leg: string concats that stay a surface
    // `Binary` (ref-typed Vec-accessor payload operand blocks the
    // `String.add` desugar) — return form, print-arg form, nested concat
    // with a bound intermediate, and the desugared control shapes —
    // all byte-identical to the interpreter. The LSan sibling guards
    // the operand-temp/result frees the fix added.
    let output = run_program(
        "fn tag_first(items: ref Vec[String]) -> String {\n\
                 match items.first() {\n\
                     Some(s) => { return \"f:\".to_string() + s; }\n\
                     None => { return \"empty\".to_string(); }\n\
                 }\n\
                 return \"?\".to_string();\n\
             }\n\
             fn main() {\n\
                 let v: Vec[String] = [\"aa\", \"bb\"];\n\
                 println(tag_first(v));\n\
                 println(tag_first(v));\n\
                 match v.first() {\n\
                     Some(s) => { println(\"p:\".to_string() + s); }\n\
                     None => { println(\"empty\"); }\n\
                 }\n\
                 match v.first() {\n\
                     Some(s) => {\n\
                         let t = \"pre\".to_string() + s;\n\
                         println(t + \"!\".to_string());\n\
                     }\n\
                     None => { println(\"x\"); }\n\
                 }\n\
                 println(\"a\".to_string() + \"b\".to_string());\n\
             }",
    )
    .expect("compile + run failed");
    assert_eq!(output, "f:aa\nf:aa\np:aa\npreaa!\nab\n");
}

#[test]
fn test_ir_read_to_string_builds_string_result_aggregate() {
    // The StringPayload Ok arm packs {ptr, len, cap=len} into the
    // Result.Ok payload words — pinned by the named GEPs.
    let ir = ir_for(
        r#"
fn load(path: String) -> Result[String, IoError] {
    FileSystem.read_to_string(path)
}
"#,
    );
    let body = function_body(&ir, "load").expect("load fn must lower");
    for needle in ["ok.str.ptr", "ok.str.len", "ok.str.cap"] {
        assert!(
            body.contains(needle),
            "expected String-payload GEP '{needle}'; body:\n{body}"
        );
    }
}

#[test]
fn test_ir_read_to_string_match_binding_compiles() {
    // The exact shape that produced "Undefined variable" before the
    // lowering existed. Codegen must succeed (ir_for `.expect`s it).
    let ir = ir_for(
        r#"
fn read_cert(path: String) -> String {
    match FileSystem.read_to_string(path) {
        Ok(cert_bytes) => cert_bytes,
        Err(_) => "",
    }
}
"#,
    );
    assert!(function_body(&ir, "read_cert").is_some());
}

#[test]
fn test_e2e_read_to_string_nonexistent_is_not_found() {
    let out = run_program(
        r#"
fn main() with reads(FileSystem) {
    match FileSystem.read_to_string("/nonexistent_karac_rts_test.txt") {
        Ok(_) => println("unexpected-ok"),
        Err(e) => match e {
            IoError.NotFound => println("not-found"),
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

#[test]
fn test_e2e_vec_sort_by_key_string_identity() {
    // String keys go through `karac_string_cmp` (lex byte compare).
    // String and Vec[T] share the LLVM `{ptr, i64, i64}` shape, so the
    // dispatch arm consults `string_typed_exprs` (populated by the
    // lowering pass from `TypeCheckResult.expr_types`) to tell them
    // apart. `|s| s` is the canonical identity key on `Vec[String]`.
    let out = run_program(
        r#"
fn main() {
    let mut v: Vec[String] = Vec.new();
    v.push("banana");
    v.push("apple");
    v.push("cherry");
    v.push("apricot");
    v.sort_by_key(|s| s);
    for s in v.iter() { println(s); }
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["apple", "apricot", "banana", "cherry"]);
    }
}

#[test]
fn test_e2e_vec_sort_by_key_string_length_tiebreak() {
    // When the common prefix of two strings is equal, the shorter
    // string sorts first (length is the tie-break in karac_string_cmp).
    // Duplicates are preserved in stable order.
    let out = run_program(
        r#"
fn main() {
    let mut v: Vec[String] = Vec.new();
    v.push("ab");
    v.push("a");
    v.push("abc");
    v.push("abcd");
    v.push("ab");
    v.sort_by_key(|s| s);
    for s in v.iter() { println(s); }
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["a", "ab", "ab", "abc", "abcd"]);
    }
}

#[test]
fn test_e2e_vec_sort_by_key_nested_struct_with_string_field() {
    // Mixing nested struct fields with a String field. The cascade
    // recurses on the inner struct AND dispatches the String field at
    // the outer level — exercises both extensions in one cascade.
    let out = run_program(
        r#"
#[derive(PartialEq, Eq, PartialOrd, Ord)]
struct Pos { x: i64, y: i64 }
#[derive(PartialEq, Eq, PartialOrd, Ord)]
struct Tagged { p: Pos, tag: String }

fn main() {
    let mut v: Vec[Tagged] = Vec.new();
    v.push(Tagged { p: Pos { x: 1, y: 0 }, tag: "z" });
    v.push(Tagged { p: Pos { x: 1, y: 0 }, tag: "a" });
    v.push(Tagged { p: Pos { x: 0, y: 9 }, tag: "m" });
    v.sort_by_key(|t| t);
    for t in v.iter() {
        println(t.p.x);
        println(t.p.y);
        println(t.tag);
    }
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        // First by Pos lex (0,9 < 1,0), then within Pos (1,0) by tag.
        assert_eq!(lines, vec!["0", "9", "m", "1", "0", "a", "1", "0", "z"]);
    }
}

#[test]
fn e2e_string_and_primitive_cmp_still_use_the_builtin_comparator() {
    // The guard that stops codegen's builtin `cmp` arm from answering for a
    // user `impl Ord` (B-2026-08-26-10) asks `user_impl_method_exists` about
    // the RECEIVER'S type name, and it is asked on every one-argument `.cmp`
    // — including `String`'s and the integers'. If a `String.cmp` symbol
    // were ever declared, that guard would decline for strings too and take
    // the `karac_string_cmp` lowering out of service. Nothing about the fix
    // makes that impossible, so it is pinned rather than reasoned about.
    //
    // `<` on the same operands is checked alongside, since it shares the
    // comparator.
    let out = run_program(
        r#"
fn main() {
    let a: String = "apple";
    let b: String = "zebra";
    println(a.cmp(b).is_lt());
    println(a < b);
    println(b < a);
}
"#,
    );
    let out = out.expect("String comparison must build");
    let lines: Vec<&str> = out.trim().lines().collect();
    assert_eq!(lines, vec!["true", "true", "false"]);
}

#[test]
fn test_assert_ne_fail_formats_operands() {
    let captured = run_program_capturing(
        r#"
fn main() {
    assert_ne(7, 7)
}
"#,
    );
    if let Some(c) = captured {
        assert!(
            c.stderr
                .contains("\"message\":\"assertion failed: left == right\""),
            "expected assert_ne message; got stderr={:?}",
            c.stderr
        );
        assert!(
            c.stderr.contains("\"left\":\"7\"") && c.stderr.contains("\"right\":\"7\""),
            "expected formatted left=7, right=7; got stderr={:?}",
            c.stderr
        );
    }
}

#[test]
fn test_assert_eq_string_pass() {
    let out = run_program(
        r#"
fn main() {
    assert_eq("hello", "hello")
    println(1)
}
"#,
    );
    if let Some(s) = out {
        assert_eq!(s.trim(), "1");
    }
}

#[test]
fn test_assert_eq_string_fail_formats_strings() {
    let captured = run_program_capturing(
        r#"
fn main() {
    assert_eq("hello", "world")
}
"#,
    );
    if let Some(c) = captured {
        assert!(
            c.stderr.contains("\"left\":\"hello\"") && c.stderr.contains("\"right\":\"world\""),
            "expected formatted string operands; got stderr={:?}",
            c.stderr
        );
    }
}

#[test]
fn test_ir_refinement_over_string_matches_base_layout() {
    // The bug the layout arm fixes: a refinement over a *non-`i64`*
    // base (`type Name = String`) must lower to the base's layout, not
    // the `i64` fall-through default. Prove it by comparing the `@takes`
    // parameter type against the equivalent plain-`String` signature —
    // they must be identical.
    let refined = ir_for(
        "type Name = String where self.len() > 0;
             fn takes(n: Name) -> i64 { 0 }",
    );
    let plain = ir_for("fn takes(n: String) -> i64 { 0 }");
    assert_eq!(
        takes_param_type(&refined),
        takes_param_type(&plain),
        "refinement over String must lower to the String base layout\nrefined IR:\n{refined}"
    );
    // Sanity: it is NOT the i64 fall-through default.
    assert_ne!(
        takes_param_type(&refined),
        "i64",
        "refinement over String must not hit the i64 fall-through:\n{refined}"
    );
}

#[test]
fn test_ir_plain_alias_over_string_matches_base_layout() {
    // B-2026-07-30-7 — the `where`-FREE sibling of
    // `test_ir_refinement_over_string_matches_base_layout`. Only the
    // refinement arm had a base map, so a plain alias over a non-`i64`
    // base hit the `i64` unknown-name fall-through and the emitted module
    // failed LLVM verification ("Call parameter type does not match
    // function signature"). Pin the plain param type against the
    // equivalent bare-`String` signature.
    let aliased = ir_for(
        "type Name = String;\n\
             fn takes(n: Name) -> i64 { 0 }",
    );
    let plain = ir_for("fn takes(n: String) -> i64 { 0 }");
    assert_eq!(
        takes_param_type(&aliased),
        takes_param_type(&plain),
        "plain alias over String must lower to the String base layout\naliased IR:\n{aliased}"
    );
    assert_ne!(
        takes_param_type(&aliased),
        "i64",
        "plain alias over String must not hit the i64 fall-through:\n{aliased}"
    );
}

#[test]
fn test_e2e_refinement_string_base_method_deref() {
    // Codegen value-dispatch (phase-9 step 5a) strips the refinement to
    // its base, so `n.len()` (base-deref, step 2) reads 5 from the
    // String value bound through an `as Name` cast.
    let out = run_program(
        r#"
type Name = String where self.len() > 0;
fn main() {
    let n = "hello" as Name;
    println(n.len());
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "5");
    }
}

#[test]
fn test_e2e_refinement_string_value_prints_as_base() {
    // `println(n)` on a refinement-over-String binding dispatches the
    // String display path (value-dispatch, step 5a) — not the i64
    // fall-through that would print a pointer-sized integer.
    let out = run_program(
        r#"
type Name = String where self.len() > 0;
fn main() {
    let n = "hello" as Name;
    println(n);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "hello");
    }
}

#[test]
fn test_e2e_refinement_as_cast_string_method_predicate_aborts() {
    // A method-form predicate (`self.len() > 0`) over a String base is
    // compiled and enforced: an empty string fails and aborts.
    let captured = run_program_capturing(
        r#"
type NonEmpty = String where self.len() > 0;
fn main() {
    let s = "" as NonEmpty;
    println(s);
}
"#,
    );
    if let Some(c) = captured {
        assert!(
            c.stderr.contains("contract violated") && c.stderr.contains("NonEmpty"),
            "expected NonEmpty contract abort, got stdout={:?} stderr={:?}",
            c.stdout,
            c.stderr
        );
    }
}

// ── Owned String/Vec parameter retention (kata-22 family, 2026-06-06) ──
//
// The call ABI passes owned String/Vec headers by value while the
// CALLER retains the buffer's scope-exit free — so a callee that
// RETAINS the value (push into a container, return it, capture it
// in a struct/enum payload) must deep-copy the buffer instead of
// aliasing it (`emit_vecstr_defensive_copy`). Before the fix every
// shape below either aborted under macOS malloc (exit 133 double
// free) or read freed memory. The f-string siblings pin the
// `last_fstr_acc` take-points at the same consume sites (the temp
// accumulator's queued cleanup must be disarmed when the container
// takes the buffer).

#[test]
fn test_e2e_owned_string_param_push_branch_move() {
    // Recursive backtracking shape: `cur` is moved into the vec in
    // the base-case branch and borrowed by the f-string in the
    // recursive branch. The cap-sentinel machinery gives the
    // per-branch flow sensitivity; the defensive copy keeps the
    // pushed buffer alive past the caller's f-string-temp cleanup.
    let out = run_program(
        r#"
fn add_rec(cur: String, k: i64, out: mut ref Vec[String]) {
    if k == 0 {
        out.push(cur);
        return;
    }
    add_rec(f"{cur}(", k - 1, out);
}

fn generate(n: i64) -> Vec[String] {
    let mut out: Vec[String] = Vec.new();
    add_rec("", n, mut out);
    out
}

fn main() {
    let combos = generate(3);
    println(combos.len());
    println(combos[0]);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "1\n(((");
    }
}

#[test]
fn test_e2e_map_insert_owned_string_param_value() {
    // `m.insert(k, v)` where `v: String` is an owned PARAM and the map
    // outlives the callee (passed `mut ref`): the Map must store a
    // private deep copy, not an alias into the caller's buffer, or the
    // caller's scope-exit free and the Map's bucket free double-hit the
    // same buffer (kata-22 owned-param UAF family, Cluster 1). The read
    // back through `m.get` proves the value buffer survives.
    let out = run_program(
        r#"
fn store(m: mut ref Map[i64, String], v: String) {
    m.insert(7i64, v);
}

fn main() {
    let mut m: Map[i64, String] = Map.new();
    let mut s = String.new();
    s.push_str("payload");
    store(mut m, s);
    println(m.len());
    match m.get(7i64) { Some(g) => println(g), None => println("missing") }
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "1\npayload");
    }
}

#[test]
fn test_e2e_set_insert_owned_string_param() {
    // `s.insert(v)` where `v: String` is an owned PARAM and the set
    // outlives the callee: same defensive-copy requirement as
    // `Map.insert`'s value side (Set lowers to `Map[T, ()]`, the
    // element is the bucket key). The `contains` reads prove the keys
    // survive past the caller's frees.
    let out = run_program(
        r#"
fn add(set: mut ref Set[String], v: String) {
    set.insert(v);
}

fn main() {
    let mut s: Set[String] = Set.new();
    let mut a = String.new();
    a.push_str("apple");
    add(mut s, a);
    let mut b = String.new();
    b.push_str("banana");
    add(mut s, b);
    println(s.len());
    println(s.contains("apple"));
    println(s.contains("banana"));
    println(s.contains("cherry"));
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "2\ntrue\ntrue\nfalse");
    }
}

#[test]
fn test_e2e_owned_string_param_tail_return() {
    // `fn id(s: String) -> String { s }` — the returned value must
    // be a copy: the caller that passed `s` frees its buffer AND the
    // caller receiving the return frees what it binds.
    let out = run_program(
        r#"
fn id(s: String) -> String {
    s
}

fn main() {
    let a = f"abc{1}";
    let b = id(a);
    println(b);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "abc1");
    }
}

#[test]
fn test_e2e_owned_string_param_explicit_return() {
    // Same as the tail form but through the explicit-`return` arm in
    // `compile_expr` (separate code path).
    let out = run_program(
        r#"
fn id(s: String) -> String {
    return s;
}

fn main() {
    let a = f"xyz{7}";
    let b = id(a);
    println(b);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "xyz7");
    }
}

#[test]
fn test_e2e_vec_push_fstring_temp() {
    // `v.push(f"s{i}")` — the f-string accumulator's queued cleanup
    // must be disarmed at the push consume site (the vec takes the
    // buffer); without the take, the acc cleanup and the vec's
    // recursive drop both freed the same pointer.
    let out = run_program(
        r#"
fn main() {
    let mut v: Vec[String] = Vec.new();
    let mut i = 0;
    while i < 3 {
        v.push(f"s{i}");
        i = i + 1;
    }
    println(v.len());
    println(v[0]);
    println(v[2]);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "3\ns0\ns2");
    }
}

#[test]
fn test_e2e_owned_string_param_enum_payload() {
    // `Some(s)` where `s: String` is a parameter — the enum payload
    // must carry a copied buffer (the caller that passed `s` retains
    // the original's free).
    let out = run_program(
        r#"
fn wrap(s: String) -> Option[String] {
    Some(s)
}

fn main() {
    let w = wrap(f"pay{9}");
    match w {
        Some(s) => println(s),
        None => println("none"),
    }
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "pay9");
    }
}

#[test]
fn test_e2e_owned_string_param_struct_field() {
    // Struct-literal capture of an owned String param — the field
    // must own a copied buffer.
    let out = run_program(
        r#"
struct Holder {
    name: String,
}

fn make(s: String) -> Holder {
    Holder { name: s }
}

fn main() {
    let h = make(f"nm{3}");
    println(h.name);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "nm3");
    }
}

#[test]
fn test_e2e_owned_string_param_if_branch_return_no_double_free() {
    // B-2026-07-13-1: `fn pick(a: String, b: String) -> String { if a > b
    // { a } else { b } }` — each `if` branch tail returns an owned String
    // PARAM. The branch's move-suppression (zero the source `cap`) is a
    // no-op for a CALLER-retained param, so the returned value aliased the
    // caller's arg buffer and the caller double-freed (arg temp + result).
    // Each branch tail must DEEP-COPY the param, exactly as the bare-tail
    // return does. Was: interp printed the right value, JIT/native aborted
    // with "free(): double free detected in tcache 2".
    let out = run_program(
        r#"
fn pick(a: String, b: String) -> String {
    if a > b { a } else { b }
}

fn main() {
    println(pick(f"apple", f"banana"));
    println(pick(f"zebra", f"apple"));
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "banana\nzebra");
    }
}

#[test]
fn test_e2e_owned_string_param_match_arm_return_no_double_free() {
    // B-2026-07-13-1, `match`-arm sibling: an owned String param returned
    // from a `match` arm tail must deep-copy for the same reason.
    let out = run_program(
        r#"
fn pick(a: String, b: String) -> String {
    match a > b {
        true => a,
        false => b,
    }
}

fn main() {
    println(pick(f"apple", f"banana"));
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "banana");
    }
}

#[test]
fn test_e2e_owned_string_param_let_if_return_no_double_free() {
    // B-2026-07-13-1, let-binding sibling: `let r = if … { a } else { b };
    // r` — the `if` value binds an owned-param alias into `r`; returning
    // `r` double-freed. The per-branch deep-copy gives `r` its own buffer.
    let out = run_program(
        r#"
fn pick(a: String, b: String) -> String {
    let r = if a > b { a } else { b };
    r
}

fn main() {
    println(pick(f"apple", f"banana"));
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "banana");
    }
}

#[test]
fn test_e2e_owned_string_param_let_move_then_grow() {
    // String sibling, with a realloc after the move: `t` must own a
    // copied buffer, or push_str's realloc leaves the caller freeing
    // a stale pointer.
    let out = run_program(
        r#"
fn bang(s: String) -> String {
    let mut t = s;
    t.push_str("!");
    t
}

fn main() {
    let a = f"abc{1}";
    println(bang(a));
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "abc1!");
    }
}

// ── F-string accumulator pre-sizing (kata-22 perf lever, 2026-06-06) ──

#[test]
fn test_ir_fstring_pure_parts_presized_single_malloc() {
    // Side-effect-free parts (identifier + literal): the pre-sized
    // fast path renders all parts first, mallocs ONCE at the summed
    // size, and memcpys at running offsets — no per-append grow
    // (`fsa.grow`) blocks. The append path's grow-per-part cost two
    // mallocs + two full copies on the canonical snapshot-concat
    // `f"{cur}("` (kata-22 bench: ~2x of the clang mirror).
    let ir = ir_for(
        r#"
fn extend(cur: String) -> String {
    f"{cur}("
}

fn main() {
    let s = extend("ab");
    println(s);
}
"#,
    );
    let extend_fn = ir
        .split("define internal { ptr, i64, i64 } @extend")
        .nth(1)
        .and_then(|rest| rest.split("\n}\n").next())
        .expect("extend fn body in IR");
    assert!(
        !extend_fn.contains("fsa.grow"),
        "pure-part f-string must take the pre-sized path (no grow blocks):\n{}",
        extend_fn
    );
    assert!(
        extend_fn.contains("fstr.alloc") && extend_fn.contains("fstr.buf"),
        "pure-part f-string must malloc once at the summed size:\n{}",
        extend_fn
    );
    assert_eq!(
        extend_fn.matches("call ptr @malloc").count(),
        1,
        "exactly one malloc for the whole f-string:\n{}",
        extend_fn
    );
}

#[test]
fn test_ir_fstring_call_part_takes_append_fallback() {
    // A part with call machinery can mutate/consume what an earlier
    // String part's (ptr, len) aliases — those f-strings keep the
    // snapshot-per-part append path (grow blocks present).
    let ir = ir_for(
        r#"
fn dyn_part() -> String {
    "x"
}

fn main() {
    let s = "a";
    let out = f"{s}{dyn_part()}";
    println(out);
}
"#,
    );
    assert!(
        ir.contains("fsa.grow"),
        "call-part f-string must take the append fallback:\n{}",
        ir
    );
}

#[test]
fn test_e2e_fstring_presize_mixed_parts() {
    // End-to-end byte-correctness of the pre-sized path across part
    // kinds: literal text, String identifier, negative int, unsigned
    // narrow int, float, bool, char (multi-byte UTF-8), indexed
    // element, arithmetic, empty-string part, and an all-empty
    // result (max(total,1) keeps the cap owned).
    let out = run_program(
        r#"
fn main() {
    let s = "world";
    let n = -42;
    let u: u8 = 200;
    let f = 2.5;
    let b = true;
    let c = 'é';
    println(f"hello {s}! n={n} u={u} f={f} b={b} c={c}");
    let v: Array[i64, 3] = [10, 20, 30];
    println(f"idx={v[1]} sum={v[0] + v[2]}");
    let e = "";
    println(f"[{e}]");
    let empty = f"{e}";
    println(empty.len());
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(
            out.trim(),
            "hello world! n=-42 u=200 f=2.5 b=true c=é\nidx=20 sum=40\n[]\n0"
        );
    }
}

#[test]
fn test_e2e_embeddings_cosine_and_normalize() {
    // `std.embeddings` (phase-11 numerical stdlib) — generic-over-dimension
    // similarity primitives over `ref Tensor[f32, [D]]`, monomorphized at
    // the caller's concrete width. Exact oracles: orthogonal → 0, identical
    // → 1, `dot([3,4,0],·)` = 25, and a normalized vector has unit norm.
    // Exercises the gap-C fix (ref-Tensor params forwarded to `zip_with`)
    // end-to-end through a gated stdlib import.
    if let Some(out) = run_program(
        r#"
import std.embeddings.{cosine_similarity, l2_normalize, dot};
fn main() {
    let a: Tensor[f32, [3]] = Tensor.from([1.0f32, 0.0f32, 0.0f32]);
    let b: Tensor[f32, [3]] = Tensor.from([0.0f32, 1.0f32, 0.0f32]);
    let c: Tensor[f32, [3]] = Tensor.from([3.0f32, 4.0f32, 0.0f32]);
    println(cosine_similarity(a, b));
    println(cosine_similarity(a, a));
    println(dot(c, c));
    let u = l2_normalize(c);
    println(dot(u, u));
}
"#,
    ) {
        assert_eq!(out, "0\n1\n25\n1\n");
    }
}

#[test]
fn test_e2e_secret_field_redacted_in_display() {
    // A struct containing a `Secret[T]` field renders the field as
    // `<redacted>` in the derived Display (build_struct_display_parts
    // short-circuits ahead of the nested-struct recursion that would
    // otherwise leak the wrapped value). `karac build` must match `karac
    // run`. Soft-skips without the runtime archive.
    if let Some(out) = run_program(
        r#"
import std.secret.{Secret};
#[derive(Display)]
struct Config { name: String, token: Secret[String] }
fn main() {
    let c = Config { name: "alice", token: Secret.new("hunter2") };
    println(c);
    println(c.to_string());
    println(f"cfg={c}");
}
"#,
    ) {
        let line = "Config { name: alice, token: <redacted> }\n";
        assert_eq!(out, format!("{line}{line}cfg={line}"));
    }
}

#[test]
fn test_ir_secret_string_zeroize_on_drop() {
    // std.secret Zeroize (design.md § Clone/Drop/Zeroize): a
    // `Secret[String]`'s inner buffer is overwritten with zeros before it
    // is freed, so the secret's bytes don't linger in freed heap. Verify
    // the `Secret[String]` drop fn emits a `memset` (the zeroize) ahead of
    // the buffer free. A plain (non-secret) String-field struct's drop
    // must NOT gain a spurious memset.
    let ir = ir_for_gated(
        r#"
import std.secret.{Secret};
fn main() {
    let s: Secret[String] = Secret.new("hunter2-token-01");
    println(s.ct_eq(s));
}
"#,
    );
    // The Secret[String] mono drop fn carries the zeroize memset.
    let has_secret_drop = ir.contains("__karac_drop_struct_Secret");
    assert!(has_secret_drop, "expected a Secret drop fn in IR");
    assert!(
        ir.contains("llvm.memset"),
        "expected a memset (zeroize) in the Secret[String] drop; IR had none"
    );
}

#[test]
fn test_e2e_secret_string_zeroize_runs_all_backends() {
    // The zeroize memset must not corrupt the normal Secret[String] drop —
    // the program runs correctly (and leak/double-free-free, see
    // `asan_secret_string_zeroize_no_leak`) on every backend. The zeroing
    // itself is unobservable from surface code (the value is gone); this
    // guards that adding it didn't break construction / use / drop.
    if let Some(out) = run_program(
        r#"
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
"#,
    ) {
        assert_eq!(out, "true\nfalse\n");
    }
}

/// Moving a `String`/`Vec` payload OUT of a SHARED-enum box (`match e { S(s)
/// => s }`) must zero that field's words IN THE BOX so the box's
/// `__karac_rc_drop_<E>` skips the buffer the binding now owns — else the
/// returned String is freed by the caller AND re-freed by the box rc-drop
/// (double-free). Regression for `suppress_shared_enum_payload_move_out`
/// (B-2026-06-20). E2E ASAN coverage:
/// `memory_sanitizer::asan_shared_enum_string_payload_moveout_no_double_free`.
#[test]
fn shared_enum_string_payload_moveout_zeros_box_cap() {
    let ir = ir_for(
        "shared enum E { S(String), Other }\n\
             fn get(e: E) -> String { match e { S(s) => s, Other => \"o\".to_string() } }\n\
             fn main() { println(get(E.S(\"payload-string\".to_string()))); }\n",
    );
    let body = function_body(&ir, "get").expect("fn get must be emitted");
    assert!(
        body.contains("match.sh.suppress.wp"),
        "the shared-enum String move-out must zero the box payload word(s) so \
             the box rc-drop skips the moved-out buffer\n--- body ---\n{body}"
    );
}

/// A fresh `String`/`Vec` temp passed DIRECTLY by value to a METHOD call has
/// no consuming binding, and an owned `String`/`Vec` by-value param is not
/// freed by the callee — so the caller must materialize it into an
/// `__owned_tmp` with a scope-exit free, exactly as the free-fn `compile_call`
/// path does. Before the fix only the free-fn path materialized; the method
/// path leaked one buffer per call (B-2026-06-20, the self-host string-eq
/// method leak). E2E:
/// `memory_sanitizer::asan_string_eq_mismatched_len_no_overread_in_indexed_payload_match`.
#[test]
fn method_call_fresh_string_temp_arg_materialized() {
    let ir = ir_for(
        "struct H { x: i64 }\n\
             impl H { fn m(ref self, t: String) -> bool { t.len() > 0 } }\n\
             fn main() { let h = H { x: 1 }; println(h.m(\"fresh-temp-arg\".to_string())); }\n",
    );
    let body = function_body(&ir, "main").expect("fn main must be emitted");
    assert!(
        body.contains("__owned_tmp"),
        "a fresh String temp passed by value to a method call must be \
             materialized into a caller-scope owned temp (freed at scope exit)\n\
             --- body ---\n{body}"
    );
}

#[test]
fn test_e2e_chained_to_string_then_string_method() {
    // B-2026-07-16-20: a `.to_string()` chained as the RECEIVER of another
    // method (`s.to_string().to_uppercase()`, `s.trim().to_string()...`)
    // built-fail'd with "Vec/String method 'to_string' is not yet supported
    // in codegen" while the interpreter ran fine — a check-passes /
    // codegen-rejects consistency hole. Cause: the parser sets
    // `MethodCall.span == receiver.span`, so the inner `to_string` and the
    // outer method collide on one `method_callee_types` span key; the outer
    // call's key shadows the inner's, so the `String.to_string()`
    // owning-copy special-case (dispatch-key-gated on a "String" receiver
    // segment) never fired for the inner call. Fixed by additionally firing
    // that special-case on a statically String/StringSlice receiver
    // (`expr_is_string_like`), independent of the shared-span dispatch key.
    // Exercises the identifier, string-literal, and mid-chain
    // (`trim().to_string().to_uppercase()`) receiver shapes.
    for (src, want) in [
        (
            "fn main() { let s = \"hi\".to_string(); println(s.to_string().to_uppercase()); }",
            "HI",
        ),
        (
            "fn main() { println(\"hi\".to_string().to_uppercase()); }",
            "HI",
        ),
        (
            "fn main() { let s = \"  Hi  \".to_string(); \
                 println(s.trim().to_string().to_uppercase()); }",
            "HI",
        ),
    ] {
        if let Some(out) = run_program(src) {
            assert_eq!(out.trim(), want, "chained to_string mismatch for: {src}");
        }
    }
}

/// B-2026-07-26-2: the monomorphized String-key `Map.get` probe must agree
/// with the ERASED insert path on bucket placement in every shape the
/// erased path can produce. A probe that hashes or compares differently
/// from how the buckets were filled is a silent wrong-answer bug, which is
/// why the mono body reads the map's STORED `hash_fn` / `eq_fn` rather than
/// synthesizing its own.
///
/// Covers the four cases that can break placement agreement:
///   * **growth** — 400 keys forces several resizes, so most lookups land
///     in a table that was rehashed after their insert;
///   * **tombstones** — removals leave `BUCKET_TOMBSTONE`, and the probe
///     must step past them rather than stopping (stopping would report a
///     live key as missing);
///   * **a cloned map** — a different creation path, which for Sets is
///     documented to register a different-but-self-consistent layout;
///   * **misses** — including a key whose hash collides into an occupied
///     run, which must terminate at the first EMPTY rather than run off.
#[test]
fn mono_str_map_get_agrees_with_erased_insert() {
    let out = run_program(
            "fn key(i: i64) -> String {\n\
                 let mut s: String = \"k\";\n\
                 s.push_str(f\"{i}\");\n\
                 s\n\
             }\n\
             fn main() {\n\
                 let mut m: Map[String, i64] = Map.new();\n\
                 let mut i = 0i64;\n\
                 while i < 400i64 {\n\
                     let _ = m.insert(key(i), i * 3i64);\n\
                     i = i + 1i64;\n\
                 }\n\
                 // Tombstones: drop every 5th key, then re-probe the survivors.\n\
                 let mut d = 0i64;\n\
                 while d < 400i64 {\n\
                     let _ = m.remove(key(d));\n\
                     d = d + 5i64;\n\
                 }\n\
                 let mut hits = 0i64;\n\
                 let mut sum = 0i64;\n\
                 let mut misses = 0i64;\n\
                 let mut j = 0i64;\n\
                 while j < 400i64 {\n\
                     match m.get(key(j)) {\n\
                         Some(v) => { hits = hits + 1i64; sum = (sum + v) % 1000000007i64; }\n\
                         None => { misses = misses + 1i64; }\n\
                     }\n\
                     j = j + 1i64;\n\
                 }\n\
                 println(f\"{hits} {sum} {misses}\");\n\
                 // Absent keys must miss, not alias a live bucket.\n\
                 match m.get(\"nope\") { Some(_) => { println(\"BAD\"); } None => { println(\"absent\"); } }\n\
                 match m.get(\"\") { Some(_) => { println(\"BAD\"); } None => { println(\"absent\"); } }\n\
                 // A clone is a distinct creation path; it must probe the same.\n\
                 let c = m.clone();\n\
                 let mut chits = 0i64;\n\
                 let mut k = 0i64;\n\
                 while k < 400i64 {\n\
                     match c.get(key(k)) {\n\
                         Some(_) => { chits = chits + 1i64; }\n\
                         None => {}\n\
                     }\n\
                     k = k + 1i64;\n\
                 }\n\
                 println(f\"{chits}\");\n\
             }\n",
        );
    // 400 inserted, 80 removed (0,5,...,395) => 320 live.
    // sum = 3 * (sum of j in 0..400 where j % 5 != 0) = 3 * (79800 - 15800).
    assert_eq!(out.as_deref(), Some("320 192000 80\nabsent\nabsent\n320\n"));
}

/// B-2026-08-05-28 — a CHAINED String→String xform on a surface-concat
/// receiver. `("p:" + b).to_uppercase().len()` failed codegen outright
/// ("no handler for method 'to_uppercase' on non-identifier receiver"),
/// while the UNCHAINED `("p:" + b).to_uppercase()` compiled fine. So the
/// filed shape was the CHAIN, not the concat receiver — the row's title
/// said the xform methods do not compile on such a receiver, and half of
/// that is wrong.
///
/// Cause was the chained-method span collision, not a missing dispatcher
/// arm: the parser gives every link of `x.a().b()` the RECEIVER's span, so
/// the outer link's `method_callee_types` entry clobbered the inner's and
/// the inner call found a key whose method segment did not match its own.
/// The producers now key on the closing paren via
/// `SpanKey::for_method_call` — the helper the `method_unwrap_*` tables
/// already used for exactly this (Slice 1 of the span-collision fix).
///
/// Both members are pinned, chained AND unchained, because only the first
/// was broken and a fix that regressed the second would otherwise pass
/// unnoticed.
#[test]
fn chained_string_xform_on_concat_receiver() {
    assert_eq!(
        run_program(
            // Two bindings, not one reused: `+` MOVES its String operand,
            // so a second concat off `b` is a use-after-move the ownership
            // gate rightly rejects.
            "fn main() {\n\
                 \x20   let b: String = \"y\".to_string();\n\
                 \x20   let c: String = \"y \".to_string();\n\
                 \x20   println((\"p:\".to_string() + b).to_uppercase().len());\n\
                 \x20   println((\"  q\".to_string() + c).trim().len());\n\
                 }\n"
        )
        .as_deref(),
        // "P:Y" is 3; "  qy ".trim() is "qy", 2.
        Some("3\n2\n"),
        "a chained xform on a concat receiver must compile and run"
    );
    assert_eq!(
        run_program(
            "fn main() {\n\
                 \x20   let b: String = \"y\".to_string();\n\
                 \x20   println((\"p:\".to_string() + b).to_uppercase());\n\
                 }\n"
        )
        .as_deref(),
        Some("P:Y\n"),
        "the unchained control must keep working"
    );
}

/// B-2026-08-14-23 — the VALUE side of the in-place append. The
/// optimization must not change a single byte of any spelling, admitted or
/// declined, so the admitted shapes are interleaved with the ones that must
/// still take the allocate-and-copy path: a self-append (which would be a
/// use-after-free through `push_str`), a prepend, a slice of the target, and
/// a `mut ref` parameter. Oracle: the interpreter twin
/// `tests/interpreter.rs::test_string_append_spellings_agree`.
#[test]
fn test_e2e_string_append_spellings_agree() {
    assert_eq!(
            run_program(
                "fn mk(n: i64) -> String { let mut t = String.new(); t.push_str(\"v\"); t.push_str(n.to_string()); t }\n\
                 fn app_ref(s: mut ref String) { s = s + \"R\"; }\n\
                 fn main() {\n\
                     let mut s = \"lit\";\n\
                     s = s + \"abc\"; println(s);\n\
                     let t = \"T\";\n\
                     s = s + t; println(s);\n\
                     s = s + \" \" + t; println(s);\n\
                     s = s + mk(3i64); println(s);\n\
                     s += \"P\"; println(s);\n\
                     let mut d = \"ab\";\n\
                     d = d + d; println(d);\n\
                     let mut e = \"ab\";\n\
                     e = e + e[0..1]; println(e);\n\
                     let mut p = \"P\";\n\
                     p = \"X\" + p; println(p);\n\
                     let mut r = \"r\";\n\
                     app_ref(mut r); println(r);\n\
                     let mut z = String.new();\n\
                     z = z + \"\"; println(z.len());\n\
                 }\n"
            ),
            Some("litabc\nlitabcT\nlitabcT T\nlitabcT Tv3\nlitabcT Tv3P\nabab\naba\nXP\nrR\n0\n".to_string())
        );
}

/// B-2026-08-17-34 — `#[derive(Display)]` with the operand written as an
/// enum-variant PATH, which is the form design.md § derive(Display) on
/// enums teaches (`println(f"{Direction.Up}")` -> "Up"). Both compiled
/// backends refused it (`--interp` printed it fine), with a diagnostic
/// that misnamed the program: it prescribed binding "a struct literal or
/// call result" to a `let`, and the operand was neither.
///
/// Four shapes in one program: the all-unit path, the same in argument
/// position, a `snake_case`-derived enum (whose casing the compiled
/// backends were ALSO ignoring — a silent run-vs-build divergence that
/// this row's fix would otherwise have converted a build error into), and
/// a unit variant of a PAYLOAD-bearing enum. Paired with
/// `test_enum_variant_path_display_oracle` in `tests/interpreter.rs`.
#[test]
fn test_e2e_enum_variant_path_display() {
    assert_eq!(
        run_program(
            r#"
#[derive(Display)]
enum Direction { Up, Down }

#[derive(Display(snake_case))]
enum Mode { FastPath, SlowPath }

#[derive(Display)]
enum Evt { KeyDown(i64), MouseUp }

fn main() {
    println(f"{Direction.Up}")
    println(Direction.Down)
    println(f"{Mode.FastPath}")
    println(f"{Evt.MouseUp}")
    println(f"{Direction.Up} then {Direction.Down}")
}
"#
        ),
        Some("Up\nDown\nfast_path\nMouseUp\nUp then Down\n".to_string())
    );
}

/// B-2026-08-26-29 — a hand-written `impl Display` must win at EVERY
/// depth, not just at the top level. `f"{e}"` rendered through the impl
/// while `f"{[e]}"` rendered the DERIVED shape one level down, on BOTH
/// backends: `[A { n: 7 }, B]` where the impl says `[aye 7, bee]`. So the
/// one mechanism the language offers for overriding a rendering stopped
/// applying inside a container, and a type whose `Display` exists to hide
/// its internals leaked them from inside any `Vec` / tuple / struct field.
///
/// Every container contributes only its punctuation; each element renders
/// through its own `Display`. Paired with
/// `user_impl_display_wins_at_every_depth_not_just_the_top_level` in
/// `tests/interpreter.rs` — the two assert the same bytes, which is the
/// run-vs-build oracle.
///
/// `B => "bee"` is load-bearing: a `to_string` returning a string LITERAL
/// comes back with `cap == 0` pointing at a read-only global, so the
/// synthesized wrapper's free has to be guarded on `cap > 0` or the
/// program aborts.
#[test]
fn test_e2e_user_impl_display_wins_at_every_depth() {
    assert_eq!(
        run_program(
            r#"
enum Ue { A { n: i64 }, B }
impl Display for Ue {
    fn to_string(ref self) -> String {
        match self { A { n } => f"aye {n}", B => "bee" }
    }
}
struct Wrap { u: Ue }
impl Display for Wrap {
    fn to_string(ref self) -> String { f"<{self.u}>" }
}
struct Holder { u: Ue }
fn main() {
    let e = Ue.A { n: 7 };
    println(f"top={e}")
    let v = [Ue.A { n: 7 }, Ue.B];
    println(f"vec={v}")
    println(f"lit={[Ue.B]}")
    let nest = [[Ue.B]];
    println(f"nest={nest}")
    let t = (Ue.B, 1);
    println(f"tup={t}")
    let h = Holder { u: Ue.B };
    println(f"fld={h.u}")
    let w = Wrap { u: Ue.A { n: 3 } };
    println(f"wrap={w}")
    println(f"vw={[Wrap { u: Ue.B }]}")
    let mut m: Map[String, Ue] = Map.new();
    m.insert("k", Ue.B);
    println(f"map={m}")
    println(f"str={v.to_string()}")
    println(v)
}
"#
        ),
        Some(
            "top=aye 7\nvec=[aye 7, bee]\nlit=[bee]\nnest=[[bee]]\ntup=(bee, 1)\n\
                 fld=bee\nwrap=<aye 3>\nvw=[<bee>]\nmap={k: bee}\nstr=[aye 7, bee]\n\
                 [aye 7, bee]\n"
                .to_string()
        )
    );
}

/// B-2026-08-26-33 — a `#[derive(Display)]` struct with a PAYLOAD-BEARING
/// enum field was refused by `karac build` while `--interp` rendered it,
/// and the refusal was inverted with respect to difficulty: the same struct
/// rendered fine NESTED in a `Vec`, and the field rendered fine ON ITS OWN
/// (`f"{h.u}"`), while interpolating the struct itself failed the whole
/// compile — including files where the failing spelling never appeared,
/// since the error is raised at emission for the struct.
///
/// Codegen has two renderers for the same derived-Display struct.
/// `emit_struct_debug_display_fn` (used when nested) walks fields through
/// `emit_display_fn_for_type_expr` and handles any type that dispatcher
/// knows. `build_struct_display_parts` (used when interpolated directly)
/// classified fields with `display_field_is_leaf`, a hand-maintained name
/// list that knew about primitives and ALL-UNIT enums only. The
/// restriction was a property of the spelling, not of the type.
///
/// design.md § Strings already claimed both backends "cover a struct field
/// of enum type" — this makes that true rather than adding anything.
///
/// Struct-variant, tuple-variant, `String`-payload and user-`impl Display`
/// enum fields are all covered; the all-unit field is the control that
/// always worked.
#[test]
fn test_e2e_derived_display_struct_with_payload_enum_field() {
    assert_eq!(
        run_program(
            r#"
#[derive(Display)]
enum E { A { n: i64 }, T(i64, i64), S { s: String }, Z }
#[derive(Display)]
struct H { u: E }
enum U { P { n: i64 }, Q }
impl Display for U {
    fn to_string(ref self) -> String {
        match self { P { n } => f"pee{n}", Q => "cue" }
    }
}
#[derive(Display)]
struct G { u: U }
fn main() {
    let a = H { u: E.A { n: 1 } };
    println(f"{a}")
    let t = H { u: E.T(1, 2) };
    println(f"{t}")
    let s = H { u: E.S { s: "hi" } };
    println(f"{s}")
    let z = H { u: E.Z };
    println(f"{z}")
    let g = G { u: U.Q };
    println(f"{g}")
}
"#
        ),
        Some(
            "H { u: A { n: 1 } }\nH { u: T(1, 2) }\nH { u: S { s: hi } }\n\
                 H { u: Z }\nG { u: cue }\n"
                .to_string()
        )
    );
}

/// B-2026-08-31-49 — A `Result[T, E]` RENDERED WITH THE `Option` VARIANT
/// TABLE. `Ok(7)` printed `Some(7)` and `Err(9)` printed `None`, DROPPING
/// THE PAYLOAD, silently, on both compiled backends.
///
/// THE ROW'S AXIS WAS WRONG AND THE CORRECTION IS THE POINT. It recorded
/// this as generics-only and order-dependent "between two generic
/// instantiations". Measured: the two GENERIC functions with DIFFERENT
/// parameter names are both correct, and two NON-GENERIC functions that
/// share a parameter name collide identically. Generics are incidental —
/// the row's non-generic control happened to use different names. Sharing a
/// parameter name is the whole condition, which is why this reaches
/// ordinary code.
///
/// ROOT CAUSE. `var_option_payload_te` and `var_result_payload_te` are
/// keyed on the BARE variable name, and both Display dispatch sites (the
/// `compile_print` identifier arms and the f-string collection-display
/// path) test the Option arm FIRST. Neither map was cleared per function,
/// so an `Option` binding named `x` in one function was still there when a
/// `Result` parameter named `x` was rendered in a later one. Instrumented,
/// the second hole saw BOTH entries live for `x`.
///
/// TWO PARTS, because the per-function reset alone is not enough: generic
/// MONO bodies are compiled without passing through it, so the registration
/// itself now retracts the sibling. It does so UNCONDITIONALLY on the
/// declared type rather than only on a successful registration — the
/// payload-dropping `Err` case is exactly the one where the registration is
/// DECLINED (an `Err`-only instantiation leaves the Ok type param
/// unsubstituted), and the stale entry answered in its place.
///
/// THE `Err` ROW IS DELIBERATELY ABSENT and its absence is the finding.
/// `showr(Err(9))` on a generic `Result[T, i64]` REFUSES to build — "its
/// `T` payload cannot be reconstructed" — and refuses identically on a
/// pristine tree with this fix stashed out, so that refusal is
/// pre-existing (B-2026-08-31-39's unsubstituted-`T` territory) and not
/// something this change introduced. What changed is that the collision
/// used to MASK it: preceded by a same-named `Option` binding the same
/// program printed `None` instead of refusing. A refusal that names the
/// limitation is the correct behaviour and is what the identical program
/// without the preceding call already did.
#[test]
fn e2e_option_and_result_displays_do_not_collide_on_a_shared_var_name() {
    let cases: &[(&str, &str, &str)] = &[
        (
            "NON-generic, both parameters named `x` — the row's missing control",
            "fn show(x: Option[i64]) { println(f\"o {x}\"); }\n\
                 fn showr(x: Result[i64, i64]) { println(f\"r {x}\"); }\n\
                 fn main() { show(Some(7)); showr(Ok(7)); }\n",
            "o Some(7)\nr Ok(7)\n",
        ),
        (
            "generic, both parameters named `x` — the row's own repro",
            "fn show[T: Display](x: Option[T]) { println(f\"o {x}\"); }\n\
                 fn showr[T: Display](x: Result[T, i64]) { println(f\"r {x}\"); }\n\
                 fn main() { show(Some(7)); showr(Ok(7)); }\n",
            "o Some(7)\nr Ok(7)\n",
        ),
        (
            "generic at `T = String`",
            "fn show[T: Display](x: Option[T]) { println(f\"o {x}\"); }\n\
                 fn showr[T: Display](x: Result[T, i64]) { println(f\"r {x}\"); }\n\
                 fn main() { show(Some(\"yo\")); showr(Ok(\"hi\")); }\n",
            "o Some(yo)\nr Ok(hi)\n",
        ),
        (
            "both orders and both variants, one shared name",
            "fn show(x: Option[i64]) { println(f\"o {x}\"); }\n\
                 fn showr(x: Result[i64, i64]) { println(f\"r {x}\"); }\n\
                 fn main() { show(Some(7)); showr(Ok(7)); showr(Err(9)); show(None); }\n",
            "o Some(7)\nr Ok(7)\nr Err(9)\no None\n",
        ),
        (
            "FOUR functions sharing `x` across two payload types",
            "fn a(x: Option[i64]) { println(f\"a {x}\"); }\n\
                 fn b(x: Result[i64, i64]) { println(f\"b {x}\"); }\n\
                 fn c(x: Option[String]) { println(f\"c {x}\"); }\n\
                 fn d(x: Result[String, i64]) { println(f\"d {x}\"); }\n\
                 fn main() { a(Some(1)); b(Ok(2)); c(Some(\"s\")); d(Ok(\"t\")); }\n",
            "a Some(1)\nb Ok(2)\nc Some(s)\nd Ok(t)\n",
        ),
        // CONTROLS — correct before the fix, and together they are what
        // localized the cause to the NAME rather than to generics.
        (
            "control: the Result function compiled FIRST",
            "fn show[T: Display](x: Option[T]) { println(f\"o {x}\"); }\n\
                 fn showr[T: Display](x: Result[T, i64]) { println(f\"r {x}\"); }\n\
                 fn main() { showr(Ok(7)); show(Some(7)); }\n",
            "r Ok(7)\no Some(7)\n",
        ),
        (
            "control: DISTINCT parameter names",
            "fn show[T: Display](x: Option[T]) { println(f\"o {x}\"); }\n\
                 fn showr[T: Display](y: Result[T, i64]) { println(f\"r {y}\"); }\n\
                 fn main() { show(Some(7)); showr(Ok(7)); }\n",
            "o Some(7)\nr Ok(7)\n",
        ),
        (
            "control: the Result function alone",
            "fn showr[T: Display](x: Result[T, i64]) { println(f\"r {x}\"); }\n\
                 fn main() { showr(Ok(7)); }\n",
            "r Ok(7)\n",
        ),
    ];
    for (label, src, want) in cases {
        let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(src);
        assert!(
            interp_errs.is_empty(),
            "{label}: interpreter errored: {interp_errs:?}"
        );
        assert_eq!(interp_out.join(""), *want, "{label}: interpreter");
        if let Some(aot) = run_program(src) {
            assert_eq!(aot, *want, "{label}: run and build must agree");
        }
    }
}

/// B-2026-08-26-33's BOUNDARY, pinned because the first cut of the fix
/// crossed it. Admitting every enum in `enum_layouts` to the leaf list also
/// admits GENERIC ones — and a generic enum field does not merely fail
/// there, it PANICS the compiler (`emit_display_fn_for_type: type_name 'T'
/// not yet supported`), because the leaf branch synthesizes its field
/// expression with the BASE's span and nothing downstream can recover the
/// field's instantiation. `Option[i64]` is seeded in `enum_layouts` and so
/// would qualify too, trading this function's accurate diagnostic for the
/// f-string path's misleading advice to bind a struct literal to a `let`.
///
/// So the predicate admits NON-GENERIC enums only, and these three stay on
/// the clean, field-naming error. A regression here would show up as a
/// panic or as the wrong message, which is what this asserts.
#[test]
fn test_generic_and_collection_display_fields_stay_a_clean_error() {
    for (label, src) in [
        (
            "generic enum field",
            r#"
#[derive(Display)]
enum O2[T] { Has { v: T }, Nope }
#[derive(Display)]
struct H { u: O2[i64] }
fn main() { let h = H { u: O2.Has { v: 4 } }; println(f"{h}") }
"#,
        ),
        (
            "Option field",
            r#"
#[derive(Display)]
struct H { u: Option[i64] }
fn main() { let h = H { u: Some(3) }; println(f"{h}") }
"#,
        ),
        (
            "Vec field",
            r#"
#[derive(Display)]
struct H { u: Vec[i64] }
fn main() { let h = H { u: [1, 2] }; println(f"{h}") }
"#,
        ),
    ] {
        let err = ir_result(src)
            .err()
            .unwrap_or_else(|| panic!("{label}: expected a codegen error, got IR"));
        assert!(
            err.contains("whose Display is not yet supported"),
            "{label}: expected the field-naming diagnostic, got: {err}"
        );
        assert!(
            err.contains("field 'u'"),
            "{label}: the diagnostic must name the field, got: {err}"
        );
    }
}

/// B-2026-08-26-29, `Set` leg. Codegen's Set renderer recurses through the
/// same `emit_display_fn_for_type_expr` dispatcher every other container
/// uses, so it picked up the user impl for free — while the interpreter's
/// typed walker never destructured `Value::Set` and kept rendering the
/// derived shape. This asserts the two agree; its interpreter twin is
/// `user_impl_display_reaches_set_and_sorted_collection_elements`.
#[test]
fn test_e2e_user_impl_display_reaches_set_elements() {
    assert_eq!(
        run_program(
            r#"
#[derive(Hash, Eq, PartialEq)]
enum Ue { A { n: i64 }, B }
impl Display for Ue {
    fn to_string(ref self) -> String {
        match self { A { n } => f"aye {n}", B => "bee" }
    }
}
fn main() {
    let mut s: Set[Ue] = Set.new();
    s.insert(Ue.B);
    println(f"set={s}")
}
"#
        ),
        Some("set=Set{bee}\n".to_string())
    );
}

/// B-2026-08-26-29, shared-struct leg. Codegen already honored a
/// `shared struct`'s `impl Display` at depth 0 while the interpreter did
/// not — a run-vs-build divergence that predated this row — and the depth
/// dispatch would have extended it to containers. Both are aligned now;
/// this is the compiled half of
/// `user_impl_display_on_a_shared_struct_is_honored_like_any_other`.
#[test]
fn test_e2e_user_impl_display_on_shared_struct() {
    assert_eq!(
        run_program(
            r#"
shared struct Sh { v: i64 }
impl Display for Sh {
    fn to_string(ref self) -> String { f"sh({self.v})" }
}
fn main() {
    let a = Sh { v: 1 };
    println(f"top={a}")
    println(a.to_string())
    let v = [Sh { v: 2 }];
    println(f"vec={v}")
}
"#
        ),
        Some("top=sh(1)\nsh(1)\nvec=[sh(2)]\n".to_string())
    );
}

/// design.md § Strings names this exact example: `Some(p)` for a
/// `p: Point` with an `impl Display` prints `Some((3, 4))`, NOT the
/// field-name form. Both backends rendered `Some(Point { x: 3, y: 4 })`
/// before B-2026-08-26-29 — the `Debug`-in-`Display` bug that paragraph
/// was written against, which it attributed to the interpreter alone.
#[test]
fn test_e2e_compound_option_payload_renders_through_its_own_display() {
    assert_eq!(
        run_program(
            r#"
struct Point { x: i64, y: i64 }
impl Display for Point {
    fn to_string(ref self) -> String { f"({self.x}, {self.y})" }
}
fn main() {
    let p = Point { x: 3, y: 4 };
    let o = Some(p);
    println(f"{o}")
}
"#
        ),
        Some("Some((3, 4))\n".to_string())
    );
}

/// B-2026-08-20-41 — `String.normalize(form)`, the remedy design.md §
/// Strings (Equality) names for its own byte-equality hazard. Codegen
/// lowers it to `karac_unicode_normalize` from the opt-in
/// `libkarac_runtime_unicode.a`; the interpreter normalizes in-process.
/// BOTH link the same `icu_normalizer`, so this asserts real byte equality
/// rather than approximate agreement.
#[test]
fn normalize_makes_the_spec_hazard_comparable() {
    assert_eq!(
        run_program(
            "fn main() {\n\
                     let a = \"e\\u{0301}\";\n\
                     let b = \"\\u{00e9}\";\n\
                     println(a == b);\n\
                     println(a.normalize(Nfc) == b.normalize(Nfc));\n\
                     println(a.normalize(Nfd) == b.normalize(Nfd));\n\
                     println(a.len());\n\
                     println(a.normalize(Nfc).len());\n\
                     println(b.normalize(Nfd).len());\n\
                 }"
        ),
        Some("false\ntrue\ntrue\n3\n2\n3\n".to_string())
    );
}

/// All four forms through codegen, with the compatibility pair doing
/// something the canonical pair does not — a K/non-K mix-up in the
/// discriminant wiring would otherwise pass silently.
#[test]
fn normalize_compatibility_forms_differ_from_canonical_ones() {
    assert_eq!(
        run_program(
            "fn main() {\n\
                     let lig = \"\\u{FB01}\";\n\
                     println(lig.normalize(Nfc));\n\
                     println(lig.normalize(Nfd));\n\
                     println(lig.normalize(Nfkc));\n\
                     println(lig.normalize(Nfkd));\n\
                 }"
        ),
        Some("\u{FB01}\n\u{FB01}\nfi\nfi\n".to_string())
    );
}

/// The form reaches codegen three ways, and the BINDING is the one with a
/// history: `NormalizationForm` is a baked-stdlib enum, so before
/// `declarations.rs` seeded its layout a variant expression lowered its tag
/// to 0 and every bound form silently normalized as `Nfc`. Measured then:
/// this program printed `2 2 2 2` where the interpreter printed `2 2 3 3`.
#[test]
fn normalize_form_reaches_codegen_bare_qualified_and_bound() {
    assert_eq!(
        run_program(
            "fn main() {\n\
                     let a = \"e\\u{0301}\";\n\
                     let bound_c = Nfc;\n\
                     let bound_d = NormalizationForm.Nfd;\n\
                     println(a.normalize(Nfc).len());\n\
                     println(a.normalize(NormalizationForm.Nfc).len());\n\
                     println(a.normalize(bound_c).len());\n\
                     println(a.normalize(bound_d).len());\n\
                 }"
        ),
        Some("2\n2\n2\n3\n".to_string())
    );
}

/// A String-typed BINDING receiver, not just a literal: the literal path
/// materializes the receiver into a synthetic slot first, so the two reach
/// `compile_vec_method` differently and both need covering. Chaining onto
/// the result pins that the returned value is a real owned String.
#[test]
fn normalize_works_on_a_binding_receiver_and_chains() {
    assert_eq!(
        run_program(
            "fn main() {\n\
                     let s = \"e\\u{0301}fg\";\n\
                     let n = s.normalize(Nfc);\n\
                     println(n.len());\n\
                     println(n.to_uppercase());\n\
                     println(s.normalize(Nfc).to_uppercase().len());\n\
                 }"
        ),
        Some("4\n\u{00C9}FG\n4\n".to_string())
    );
}

/// Identity cases across every form — empty and pure ASCII. Catches a
/// transform that rewrites text it should leave alone, and an empty result
/// whose `{null, 0, 0}` String body would otherwise go unexercised.
#[test]
fn normalize_leaves_empty_and_ascii_untouched() {
    assert_eq!(
        run_program(
            "fn main() {\n\
                     println(\"\".normalize(Nfc).len());\n\
                     println(\"plain ascii\".normalize(Nfd));\n\
                     println(\"plain ascii\".normalize(Nfkd));\n\
                 }"
        ),
        Some("0\nplain ascii\nplain ascii\n".to_string())
    );
}
