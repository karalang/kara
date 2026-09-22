//! integer and float behaviour, SIMD lanes, math intrinsics -- fixtures for `tests/codegen.rs`.
//!
//! Split out of `tests/codegen.rs` on 2026-09-21. The TEST TARGET is
//! unchanged: this file is a module of that target, so
//! `cargo test --features llvm --test codegen` still runs everything
//! and CI needs no edit. Run this area alone with:
//!
//!     cargo test --features llvm --test codegen numerics::
//!
//! New fixtures about integer and float behaviour, SIMD lanes, math intrinsics belong in this file.

use super::*;

/// Printing a whole `Vector[T, N]` must give the SAME text on every backend
/// (B-2026-08-29-52).
///
/// Before the fix there were three answers and no diagnostic: the
/// interpreter rendered the lanes, `karac run` printed the aggregate's
/// ADDRESS (identifiably so — the same number for two vectors of different
/// element types), and `karac build` printed one stray lane. `karac check`
/// said "All checks passed."
///
/// Written as a twin comparison against the interpreter AND against pinned
/// text: the twin alone would pass if both sides drifted together, and the
/// pin alone would not catch a backend that stopped matching the reference.
///
/// The bare `println(v)` spelling is covered alongside `f"{v}"` because it
/// failed DIFFERENTLY and more quietly — the compiled backends dropped the
/// line entirely, printing nothing at all where the interpreter printed the
/// vector. Both spellings now route through one renderer, and this asserts
/// they still do.
#[test]
fn e2e_vector_display_agrees_with_the_interpreter() {
    let cases: &[(&str, &str, &str)] = &[
        (
            "i32 lanes, f-string",
            "let v: Vector[i32, 4] = Vector[i32, 4](1, -2, 3, -4);\n\
                 println(f\"{v}\");",
            "Vector(1, -2, 3, -4)\n",
        ),
        (
            "i32 lanes, bare println",
            "let v: Vector[i32, 4] = Vector[i32, 4](1, -2, 3, -4);\n\
                 println(v);",
            "Vector(1, -2, 3, -4)\n",
        ),
        (
            "u8 lanes above i8 range",
            "let v: Vector[u8, 4] = Vector[u8, 4](200u8, 1u8, 2u8, 3u8);\n\
                 println(f\"{v}\");",
            "Vector(200, 1, 2, 3)\n",
        ),
        (
            "f64 lanes",
            "let g: Vector[f64, 2] = Vector[f64, 2](1.5, -2.25);\n\
                 println(f\"{g}\");",
            "Vector(1.5, -2.25)\n",
        ),
        (
            "f32 lanes",
            "let g: Vector[f32, 4] = Vector[f32, 4](1.5f32, 2.5f32, 3.5f32, 4.5f32);\n\
                 println(f\"{g}\");",
            "Vector(1.5, 2.5, 3.5, 4.5)\n",
        ),
        (
            "f16 lanes",
            "let h: Vector[f16, 2] = Vector[f16, 2](1.5f16, 2.25f16);\n\
                 println(f\"{h}\");",
            "Vector(1.5, 2.25)\n",
        ),
        (
            "bf16 lanes",
            "let b: Vector[bf16, 2] = Vector[bf16, 2](1.5bf16, 2.5bf16);\n\
                 println(f\"{b}\");",
            "Vector(1.5, 2.5)\n",
        ),
        (
            "i64 lanes, non-power-of-two N",
            "let e: Vector[i64, 3] = Vector[i64, 3](7, 8, 9);\n\
                 println(f\"{e}\");",
            "Vector(7, 8, 9)\n",
        ),
        (
            "unbound expression, not a variable",
            "let v: Vector[i32, 4] = Vector[i32, 4](1, 2, 3, 4);\n\
                 println(f\"{v + v}\");",
            "Vector(2, 4, 6, 8)\n",
        ),
        (
            "two vectors and text in one f-string",
            "let v: Vector[i32, 2] = Vector[i32, 2](1, 2);\n\
                 let g: Vector[f64, 2] = Vector[f64, 2](0.5, 1.5);\n\
                 println(f\"a {v} b {g} c\");",
            "a Vector(1, 2) b Vector(0.5, 1.5) c\n",
        ),
        // The four rows below were the one shape this twin could NOT hold
        // while B-2026-08-30-9 was open: an unsigned lane above `i64::MAX`
        // rendered here as the value and on the interpreter as a signed
        // reinterpretation (`Vector(-1, 1)`), so they lived in a separate
        // PINNED test that asserted only the compiled side. That row is
        // fixed -- the interpreter's `render_typed_mode` grew a
        // `Value::Vector` arm, so lanes read back through the element type
        // -- and absorbing them here is what its author's handoff asked
        // for. The pinned test is gone: this twin strictly subsumes it,
        // because it asserts the interpreter AND the compiled backend
        // against the same text instead of the compiled backend alone.
        (
            "u64 lanes above i64::MAX",
            "let u: Vector[u64, 2] = Vector[u64, 2](18446744073709551615u64, 1u64);\n\
                 println(f\"{u}\");",
            "Vector(18446744073709551615, 1)\n",
        ),
        (
            "u64 lanes above i64::MAX, bare println",
            "let u: Vector[u64, 2] = Vector[u64, 2](18446744073709551615u64, 1u64);\n\
                 println(u);",
            "Vector(18446744073709551615, 1)\n",
        ),
        // Exactly 2^63 -- the first value that does not fit the signed
        // carrier. The `u64::MAX` row alone would pass against a fix that
        // special-cased the all-ones pattern.
        (
            "u64 lane exactly at the signed boundary",
            "let u: Vector[u64, 2] = Vector[u64, 2](9223372036854775808u64, 1u64);\n\
                 println(f\"{u}\");",
            "Vector(9223372036854775808, 1)\n",
        ),
        // `u128` is the second width whose top half misses the carrier, and
        // it is here because reading one back at 64 bits keeps only the low
        // half -- the shape B-2026-08-19-23 hit on the scalar path.
        (
            "u128 lanes above i64::MAX",
            "let w: Vector[u128, 2] = \n\
                 Vector[u128, 2](340282366920938463463374607431768211455u128, 1u128);\n\
                 println(f\"{w}\");",
            "Vector(340282366920938463463374607431768211455, 1)\n",
        ),
    ];
    for (label, body, want) in cases {
        let src = format!("fn main() {{\n{body}\n}}\n");
        let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(&src);
        assert!(
            interp_errs.is_empty(),
            "{label}: interpreter errored: {interp_errs:?}"
        );
        assert_eq!(
            interp_out.join(""),
            *want,
            "{label}: the interpreter is the reference for this text and no \
                 longer produces it",
        );
        if let Some(aot) = run_program(&src) {
            assert_eq!(
                aot, *want,
                "{label}: the compiled backend must print the vector's lanes \
                     — it printed the aggregate's address, one stray lane, or \
                     nothing at all before B-2026-08-29-52",
            );
        }
    }
}

/// A `Vector[T, N]` reached through a CONTAINER must render the same text on
/// every backend (B-2026-08-30-39) — the nested half of the depth-0 twin
/// above.
///
/// Before the fix these were not a wrong answer, they were a compiler
/// PANIC with no span: `emit_display_fn_for_type_expr` had no `Vector` arm,
/// so every container layer over a vector fell through to the by-name
/// catch-all and aborted `karac build` with
/// `type_name 'Vector_u64_2' not yet supported`. The interpreter rendered
/// all of them, and depth 0 already worked through the f-string PART
/// renderer — which is what made the boundary so sharp and so easy to hit.
///
/// EVERY ROW IS A DIFFERENT RECURSION PATH into the new arm, not a
/// restatement of one: `Vec` reaches it through `emit_vec_display_fn_te`,
/// the tuple through `emit_tuple_display_fn`, the `Map` through its value
/// emitter. Two-deep rows are here because a fix applied at the outermost
/// container rather than at the shared dispatcher would pass the one-deep
/// rows and fail these.
///
/// The signed row is the control that keeps the lane rule honest — `-1`
/// must still print `-1` one level down, which is what stops "render lanes
/// unsigned" from being implemented unconditionally. The `u128` row is the
/// second width whose top half misses a 64-bit carrier.
#[test]
fn e2e_nested_vector_display_agrees_with_the_interpreter() {
    let cases: &[(&str, &str, &str)] = &[
        (
            "Vec of vector",
            "let u: Vector[u64, 2] = Vector[u64, 2](18446744073709551615u64, 1u64);\n\
                 let vs: Vec[Vector[u64, 2]] = [u];\n\
                 println(f\"{vs}\");",
            "[Vector(18446744073709551615, 1)]\n",
        ),
        (
            "Vec of Vec of vector",
            "let u: Vector[u64, 2] = Vector[u64, 2](18446744073709551615u64, 1u64);\n\
                 let vv: Vec[Vec[Vector[u64, 2]]] = [[u]];\n\
                 println(f\"{vv}\");",
            "[[Vector(18446744073709551615, 1)]]\n",
        ),
        (
            "tuple holding a vector",
            "let u: Vector[u64, 2] = Vector[u64, 2](18446744073709551615u64, 1u64);\n\
                 let t = (u, 7i64);\n\
                 println(f\"{t}\");",
            "(Vector(18446744073709551615, 1), 7)\n",
        ),
        (
            "tuple inside a tuple",
            "let u: Vector[i32, 2] = Vector[i32, 2](1i32, -2i32);\n\
                 let t = ((u, 1i64), 2i64);\n\
                 println(f\"{t}\");",
            "((Vector(1, -2), 1), 2)\n",
        ),
        (
            "Map value of vector type",
            "let iv: Vector[i32, 4] = Vector[i32, 4](1i32, -2i32, 3i32, -4i32);\n\
                 let mut m: Map[i64, Vector[i32, 4]] = Map.new();\n\
                 m.insert(1i64, iv);\n\
                 println(f\"{m}\");",
            "{1: Vector(1, -2, 3, -4)}\n",
        ),
        (
            "Vec of float vector",
            "let fv: Vector[f64, 2] = Vector[f64, 2](1.5f64, -2.25f64);\n\
                 let fs: Vec[Vector[f64, 2]] = [fv];\n\
                 println(f\"{fs}\");",
            "[Vector(1.5, -2.25)]\n",
        ),
        // The control: a genuinely signed lane must still read as signed one
        // level down, exactly as it does at depth 0.
        (
            "Vec of signed vector",
            "let s: Vector[i64, 2] = Vector[i64, 2](-1, 1);\n\
                 let ss: Vec[Vector[i64, 2]] = [s];\n\
                 println(f\"{ss}\");",
            "[Vector(-1, 1)]\n",
        ),
        (
            "Vec of u128 vector",
            "let w: Vector[u128, 2] = \n\
                 Vector[u128, 2](340282366920938463463374607431768211455u128, 1u128);\n\
                 let ws: Vec[Vector[u128, 2]] = [w];\n\
                 println(f\"{ws}\");",
            "[Vector(340282366920938463463374607431768211455, 1)]\n",
        ),
    ];
    for (label, body, want) in cases {
        let src = format!("fn main() {{\n{body}\n}}\n");
        let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(&src);
        assert!(
            interp_errs.is_empty(),
            "{label}: interpreter errored: {interp_errs:?}"
        );
        assert_eq!(
            interp_out.join(""),
            *want,
            "{label}: the interpreter is the reference for this text and no \
                 longer produces it",
        );
        if let Some(aot) = run_program(&src) {
            assert_eq!(
                aot, *want,
                "{label}: the compiled backend disagrees with the \
                     interpreter — it PANICKED the compiler before B-2026-08-30-39",
            );
        }
    }
}

/// `dbg()` of a `Vector[T, N]` must give the same text on every backend
/// (B-2026-08-30-39) — the `Debug`-mode sibling of the two `Display`
/// comparisons above.
///
/// `dbg` panicked at EVERY depth, including depth 0 where `println(v)` and
/// `f"{v}"` were both already correct, because it does not go through the
/// f-string part renderer at all: it recovers the argument's `TypeExpr` and
/// calls the by-pointer dispatcher, which had no `Vector` arm. The depth-0
/// row is therefore not redundant with the `Display` twin — it is the one
/// that shows the two paths reaching the same renderer.
///
/// It also covers the SECOND half of the fix, which the `Display` rows
/// cannot: `type_to_type_expr` had no `Type::Vector` arm, so the TypeExpr
/// `dbg` recovered was `TypeKind::Error` and the panic named
/// `type_name 'unknown'` rather than the real `'Vector_u64_2'`. A fix that
/// added only the display arm would still abort here.
///
/// `dbg` writes to STDERR, which is why this reads `stderr` rather than
/// going through `run_program`.
#[test]
fn e2e_dbg_of_a_vector_agrees_with_the_interpreter() {
    let cases: &[(&str, &str, &str)] = &[
        (
            "dbg at depth 0",
            "let u: Vector[u64, 2] = Vector[u64, 2](18446744073709551615u64, 1u64);\n\
                 dbg(u);",
            "Vector(18446744073709551615, 1)",
        ),
        (
            "dbg of a Vec of vectors",
            "let u: Vector[u64, 2] = Vector[u64, 2](18446744073709551615u64, 1u64);\n\
                 let vs: Vec[Vector[u64, 2]] = [u];\n\
                 dbg(vs);",
            "[Vector(18446744073709551615, 1)]",
        ),
        (
            "dbg of a float vector",
            "let f: Vector[f64, 2] = Vector[f64, 2](1.5f64, -2.25f64);\n\
                 dbg(f);",
            "Vector(1.5, -2.25)",
        ),
        // Signed and narrow controls, same role as in the Display twin.
        (
            "dbg of a signed vector",
            "let s: Vector[i64, 2] = Vector[i64, 2](-1, 1);\n\
                 dbg(s);",
            "Vector(-1, 1)",
        ),
        (
            "dbg of a u8 vector above i8 range",
            "let n: Vector[u8, 4] = Vector[u8, 4](200u8, 1u8, 2u8, 3u8);\n\
                 dbg(n);",
            "Vector(200, 1, 2, 3)",
        ),
    ];
    for (label, body, want) in cases {
        let src = format!("fn main() {{\n{body}\n}}\n");
        let (_out, interp_dbg) =
            karac::run_program_with_dbg(&src, karac::interpreter::DbgOutputMode::Terminal);
        let interp = interp_dbg.join("\n");
        assert!(
            interp.contains(want),
            "{label}: the interpreter must render the vector's lanes — \
                 got {interp:?}, wanted {want:?}",
        );
        if let Some(run) = run_program_capturing(&src) {
            assert!(
                run.stderr.contains(want),
                "{label}: the compiled backend disagrees with the \
                     interpreter — it PANICKED the compiler before \
                     B-2026-08-30-39 — got {:?}, wanted {want:?}",
                run.stderr,
            );
        }
    }
}

/// B-2026-09-19-8: a `u64`-keyed `Map` / `Set` / `SortedMap` could not find
/// a key it had just inserted, at every opt level and on every lane. The
/// per-key hash fn codegen synthesizes for a `u64` key is named
/// `karac_hash_u64`, which was also the name of a BY-VALUE runtime extern;
/// the synthesizer reused the extern, the map called it with a key
/// POINTER, and every probe hashed a stack address. `SortedMap`'s value
/// lookup rides the same `karac_map_get`, so its iteration and `dbg`
/// printed garbage values — B-2026-09-19-4's CI symptom, where only the
/// arm64 JIT's stack layout happened to expose it.
///
/// `i64` and `u32` rows are the controls: their synthesized names never
/// collided. The Fx row covers `karac_hash_u64_fx`, the same collision
/// under the `FxBuildHasher` suffix.
#[test]
fn a_u64_keyed_map_or_set_finds_the_keys_it_inserted() {
    let cases: &[(&str, &str, &str)] = &[
        (
            "Map[u64, u64]",
            "let mut m: Map[u64, u64] = Map.new();\n\
                 m.insert(5u64, 7u64);\n\
                 m.insert(18446744073709551615u64, 9u64);\n\
                 println(f\"{m.contains_key(5u64)} {m.get(5u64).unwrap_or(0u64)} \
                 {m.get(18446744073709551615u64).unwrap_or(0u64)}\");",
            "true 7 9\n",
        ),
        (
            "Map[u64, u64, FxBuildHasher]",
            "let mut m: Map[u64, u64, FxBuildHasher] = Map.new();\n\
                 m.insert(5u64, 7u64);\n\
                 println(f\"{m.contains_key(5u64)} {m.get(5u64).unwrap_or(0u64)}\");",
            "true 7\n",
        ),
        (
            "Set[u64]",
            "let mut s: Set[u64] = Set.new();\n\
                 s.insert(5u64);\n\
                 println(f\"{s.contains(5u64)} {s.contains(6u64)}\");",
            "true false\n",
        ),
        (
            "SortedMap[u64, i64] iteration",
            "let mut s: SortedMap[u64, i64] = SortedMap.new();\n\
                 s.insert(5u64, 7);\n\
                 s.insert(3u64, 9);\n\
                 for (k, v) in s { println(f\"{k} {v}\"); }",
            "3 9\n5 7\n",
        ),
        (
            "Map[i64, i64] control",
            "let mut m: Map[i64, i64] = Map.new();\n\
                 m.insert(5, 7);\n\
                 println(f\"{m.contains_key(5)} {m.get(5).unwrap_or(0)}\");",
            "true 7\n",
        ),
        (
            "Map[u32, u64] control",
            "let mut m: Map[u32, u64] = Map.new();\n\
                 m.insert(5u32, 7u64);\n\
                 println(f\"{m.contains_key(5u32)} {m.get(5u32).unwrap_or(0u64)}\");",
            "true 7\n",
        ),
    ];
    for (label, body, want) in cases {
        let src = format!("fn main() {{\n{body}\n}}\n");
        assert_eq!(
            run_program(&src).as_deref(),
            Some(*want),
            "{label}: a key inserted into the map must be found again",
        );
    }
}

/// `Vector[T, N]` integer lane arithmetic must give the SAME value in both
/// backends (B-2026-08-26-8).
///
/// This row's whole class is run-vs-build: codegen's lanes are a real
/// `<N x iX>` and wrap, while the interpreter computed on its i128 carrier
/// and kept 400 for a `u8` lane sum of 200 + 200 — a value wrong under
/// wrap (144) AND under trap. Written as a TWIN comparison rather than
/// pinned constants so the two cannot drift together; the interpreter side
/// additionally pins the exact values in `tests/interpreter.rs`.
///
/// The wide-`N` case is here on purpose: design.md § Portable SIMD promises
/// "the user's source program is identical across targets — performance,
/// not correctness, is what varies", and `N = 64` is past what a native
/// vector unit covers, so codegen legalizes it into several narrower
/// vectors. A lane rule that held only at machine-native widths would break
/// exactly the promise the section makes.
#[test]
fn e2e_vector_lane_arithmetic_agrees_with_the_interpreter() {
    let cases: &[(&str, &str)] = &[
        (
            "u8 add overflow",
            "let v: Vector[u8, 4] = Vector[u8, 4].splat(200);\n\
                 println((v + v)[0] as i64);",
        ),
        (
            "i8 signed add wrap",
            "let a: Vector[i8, 4] = Vector[i8, 4].splat(100);\n\
                 println((a + a)[0] as i64);",
        ),
        (
            "u8 subtraction underflow",
            "let c: Vector[u8, 4] = Vector[u8, 4].splat(5);\n\
                 let d: Vector[u8, 4] = Vector[u8, 4].splat(10);\n\
                 println((c - d)[0] as i64);",
        ),
        (
            "i16 multiply wrap",
            "let b: Vector[i16, 4] = Vector[i16, 4].splat(300);\n\
                 println((b * b)[0] as i64);",
        ),
        (
            "wide N past a native vector unit",
            "let v: Vector[u8, 64] = Vector[u8, 64].splat(200);\n\
                 println((v + v)[0] as i64);",
        ),
    ];
    for (label, body) in cases {
        let src = format!("fn main() {{\n{body}\n}}\n");
        let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(&src);
        assert!(
            interp_errs.is_empty(),
            "{label}: interpreter errored — lane arithmetic WRAPS, it must \
                 not trap: {interp_errs:?}"
        );
        let expected = interp_out.join("");
        if let Some(aot) = run_program(&src) {
            assert_eq!(
                aot, expected,
                "{label}: AOT and the interpreter disagree on a `Vector` \
                     lane result — the divergence B-2026-08-26-8 closed",
            );
        }
    }
}

#[test]
fn e2e_u8_as_char_builds_the_character() {
    // `u8 as char` (the one infallible int→char cast) must yield the
    // CHARACTER, not its codepoint — pushed into a String and printed. Guards
    // the typecheck carve-out + codegen zext + interpreter cast_value together
    // (surfaced by the #290 Word Pattern kata: interp printed the code, build
    // printed the char — a run-vs-build divergence before the fix).
    if let Some(out) = run_program(
        "fn main() {\n\
             \x20   let mut s: String = String.new();\n\
             \x20   let bytes = \"dog\".bytes();\n\
             \x20   let mut i = 0i64;\n\
             \x20   while i < bytes.len() {\n\
             \x20       s.push(bytes[i] as char);\n\
             \x20       i = i + 1i64;\n\
             \x20   }\n\
             \x20   println(s);\n\
             \x20   let c = 100u8 as char;\n\
             \x20   println(f\"{c}\");\n\
             }",
    ) {
        assert_eq!(out, "dog\nd\n");
    }
}

/// B-2026-07-30-9 companion — `print` has no newline to fuse, so it keeps
/// the single plain `write_console` call. Pins that the fix is scoped to the
/// two-write shape and does not put every write through staging.
#[test]
fn test_ir_print_without_newline_keeps_plain_write() {
    let ir = ir_for("fn main() { print(\"hi\"); }");
    let main_body = ir
        .split("define i32 @main()")
        .nth(1)
        .and_then(|s| s.split("\n}").next())
        .unwrap_or_default();
    assert!(
        main_body.contains("call void @__karac_write_console("),
        "print should emit a plain write_console:\n{main_body}"
    );
    assert!(
        !main_body.contains("@__karac_write_console_line("),
        "print has no newline to fuse and must not stage:\n{main_body}"
    );
}

#[test]
fn test_ir_float_arithmetic() {
    let ir = ir_for("fn avg(a: f64, b: f64) -> f64 { (a + b) / 2.0 }");
    assert!(ir.contains("fadd"), "should use float add");
    assert!(ir.contains("fdiv"), "should use float div");
}

/// IR pin (phase-10 line 282): a struct's `Vec[T]` field drop sizes the
/// `karac_free_buf` recycling hint by `cap × sizeof(T)` (the erased sites
/// used a `cap × 1` under-hint that wrongly fast-rejected a mid-size
/// multi-byte-element buffer from the 1 MiB cache). A `Vec[i64]` field must
/// multiply cap by 8; also confirms `target_data` is cached (else it would
/// silently fall back to 1).
#[test]
fn ir_struct_vec_field_free_hint_uses_elem_abi_size() {
    let ir = ir_for(
        "struct Grid { cells: Vec[i64] }\n\
             fn main() {\n\
             let g = Grid { cells: Vec.filled(4i64, 7i64) };\n\
             println(g.cells.len().to_string());\n\
             }\n",
    );
    let hint_muls: Vec<&str> = ir
        .lines()
        .filter(|l| l.contains("freebuf.bytes") && l.contains("mul"))
        .collect();
    assert!(
        !hint_muls.is_empty(),
        "no `freebuf.bytes` hint mul emitted for the struct Vec field drop:\n{ir}"
    );
    assert!(
        hint_muls.iter().any(|l| l.contains(", 8")),
        "Vec[i64] field free hint not sized by elem abi size (want `mul i64 %cap, 8`, \
             got a `cap × 1` under-hint — target_data missing?): {hint_muls:?}"
    );
}

// ── Type-aware operator dispatch: signed vs unsigned ─────────
//
// Signedness-sensitive integer ops (Div/Mod/Lt/LtEq/Gt/GtEq/Shr)
// must dispatch through the operand source type, not always-signed.
// Lowering rewrites `a / b` for `b: u64` into
// `Call(Path([u64, div]), [a, b])`; the assoc-call dispatch in
// `compile_assoc_call` reads `type_name` and threads the
// is-unsigned flag through `compile_binop_typed`.

#[test]
fn test_ir_signed_int_div_mod() {
    let ir = ir_for("fn calc(a: i64, b: i64) -> i64 { (a / b) + (a % b) }");
    assert!(ir.contains("sdiv i64"), "i64 / must emit sdiv:\n{ir}");
    assert!(ir.contains("srem i64"), "i64 % must emit srem:\n{ir}");
}

#[test]
fn test_ir_unsigned_int_div_mod() {
    let ir = ir_for("fn calc(a: u64, b: u64) -> u64 { (a / b) + (a % b) }");
    assert!(ir.contains("udiv i64"), "u64 / must emit udiv:\n{ir}");
    assert!(ir.contains("urem i64"), "u64 % must emit urem:\n{ir}");
    assert!(
        !ir.contains("sdiv i64") && !ir.contains("srem i64"),
        "u64 ops must not emit signed div/rem:\n{ir}"
    );
}

#[test]
fn test_ir_signed_int_comparisons() {
    let ir = ir_for("fn cmp(a: i64, b: i64) -> bool { a < b }");
    assert!(ir.contains("icmp slt"), "i64 < must emit slt:\n{ir}");
}

#[test]
fn test_ir_unsigned_int_comparisons() {
    // Drive all four ordering predicates through usize and confirm
    // they emit the unsigned ult/ule/ugt/uge forms.
    for (op, want) in [
        ("<", "icmp ult"),
        ("<=", "icmp ule"),
        (">", "icmp ugt"),
        (">=", "icmp uge"),
    ] {
        let src = format!("fn cmp(a: u64, b: u64) -> bool {{ a {op} b }}");
        let ir = ir_for(&src);
        assert!(ir.contains(want), "u64 `{op}` must emit `{want}`:\n{ir}");
    }
}

/// B-2026-08-06-7, behaviour — the shift rules design.md § 2141-2142
/// specifies, on the surface users actually run.
///
/// Two rules, both previously unimplemented:
///   * `(1i32) << 31` is LEGAL and yields -2147483648 — "legal regardless
///     of whether it flips the sign bit". It used to yield 2147483648,
///     because the shift ran on the i64 carrier and handed back a value an
///     `i32` cannot represent.
///   * shifting by >= the DECLARED width traps. It used to emit raw LLVM
///     `shl`, which is poison there.
///
/// Arm (c) is the one that made the old behaviour a soundness hole rather
/// than a cosmetic wrong answer: a binding typed `i32` compared GREATER
/// than `i32::MAX`. Arm (d) pins the unsigned narrow case, where the
/// re-narrowing is a mask rather than a sign-extend.
#[test]
fn e2e_shift_runs_at_declared_width() {
    let Some(out) = run_program(
        "fn main() {\n\
             \x20   // (a) sign-bit flip at the declared width — legal, per spec\n\
             \x20   let a: i32 = 1i32;\n\
             \x20   println(a << 31i32);\n\
             \x20   // (b) a narrow shift stays inside the declared width\n\
             \x20   let b: i32 = 1000000i32;\n\
             \x20   let s: i32 = b << 20i32;\n\
             \x20   println(s);\n\
             \x20   // (c) …so an `i32` binding can no longer exceed i32::MAX\n\
             \x20   println(s > 2147483647i32);\n\
             \x20   // (d) unsigned narrow: re-narrowing is a mask\n\
             \x20   let u: u8 = 200u8;\n\
             \x20   println(u << 4u8);\n\
             \x20   // (e) right shift keeps its arithmetic/logical split\n\
             \x20   let n: i32 = -8i32;\n\
             \x20   println(n >> 1i32);\n\
             }\n",
    ) else {
        return;
    };
    assert_eq!(out, "-2147483648\n603979776\nfalse\n128\n-4\n");
}

/// `~` complements at the DECLARED width, not the carrier's.
///
/// `~5u8` is 250. It produced 18446744073709551610 in the compiled
/// backends and -6 in the interpreter — a value a `u8` cannot even hold —
/// so the two did not agree on the wrong answer either.
///
/// THE SPELLING IS LOAD-BEARING, and a first draft of this test missed the
/// bug entirely by getting it wrong. A SUFFIXED initializer (`let a: u8 =
/// 5u8`) gives the local a real `i8` alloca, so `xor i8` already
/// complemented 8 bits and codegen was correct. An UNSUFFIXED one (`let a:
/// u8 = 5`) leaves it in the i64 carrier, `xor i64` flips all 64, and the
/// answer is wrong. Both spellings are here on purpose: the unsuffixed
/// arms are the regression, the suffixed arms pin the path that was
/// already right against the re-narrowing introduced to fix the other.
///
/// Signed narrow was correct throughout (`~x == -x-1` never leaves a
/// narrow signed range) and so was `u64` (64 IS the carrier width), which
/// is why a test over `i32` and `u64` alone would pass in every state of
/// the code.
#[test]
fn e2e_bitwise_not_runs_at_declared_width() {
    let Some(out) = run_program(
        "fn main() {\n\
             \x20   // (a) UNSUFFIXED narrow unsigned — the broken spelling\n\
             \x20   let a: u8 = 5;\n\
             \x20   println(~a);\n\
             \x20   let b: u16 = 5;\n\
             \x20   println(~b);\n\
             \x20   let c: u32 = 5;\n\
             \x20   println(~c);\n\
             \x20   // (b) the same through a bound result, not just inline\n\
             \x20   let d: u8 = 5;\n\
             \x20   let rd: u8 = ~d;\n\
             \x20   println(rd);\n\
             \x20   // (c) SUFFIXED — already correct, must stay correct\n\
             \x20   let e: u8 = 5u8;\n\
             \x20   println(~e);\n\
             \x20   let f: u8 = 5u8;\n\
             \x20   let rf: u8 = ~f;\n\
             \x20   println(rf);\n\
             \x20   // (d) narrow signed and u64 — correct before and after\n\
             \x20   let g: i8 = 5;\n\
             \x20   println(~g);\n\
             \x20   let h: i32 = 5;\n\
             \x20   println(~h);\n\
             \x20   let i: u64 = 5;\n\
             \x20   println(~i);\n\
             \x20   // (e) edges saturate the WIDTH, not the carrier\n\
             \x20   let z: u8 = 0;\n\
             \x20   println(~z);\n\
             \x20   let m: u8 = 255;\n\
             \x20   println(~m);\n\
             \x20   let lo: i8 = -128;\n\
             \x20   println(~lo);\n\
             \x20   // (f) …so the result is a real narrow value: an equality\n\
             \x20   // against the in-range answer holds\n\
             \x20   let n: u8 = 250;\n\
             \x20   println(~n == 5);\n\
             }\n",
    ) else {
        return;
    };
    assert_eq!(
        out,
        "250\n65530\n4294967290\n250\n250\n250\n\
             -6\n-6\n18446744073709551610\n255\n0\n127\ntrue\n"
    );
}

/// B-2026-08-06-7 — every shift is guarded by an amount check.
///
/// LLVM's `shl`/`lshr`/`ashr` are POISON at or above the operand width and
/// codegen emitted them raw, so shifting by a runtime amount that could
/// reach the width was undefined behaviour in shipped binaries — measured,
/// one `let`-bound variable printed two different values in consecutive
/// `println`s of the same run and a different value on the next run.
/// design.md § 2142 specifies the trap and it was simply unimplemented:
/// the string "shift amount out of range" appeared nowhere in src/.
///
/// Pinned at the IR level rather than only by behaviour, because the
/// E2E twin below can only prove the trap fires for the amounts it
/// names — the guard has to be there for EVERY shift, including ones
/// whose amount no test happens to pick.
#[test]
fn test_ir_shift_emits_amount_check() {
    for (src, width) in [
        ("fn f(a: i64, b: i64) -> i64 { a << b }", 64),
        ("fn f(a: i64, b: i64) -> i64 { a >> b }", 64),
        ("fn f(a: u64, b: u64) -> u64 { a >> b }", 64),
        ("fn f(a: i32, b: i32) -> i32 { a << b }", 32),
        ("fn f(a: u8, b: u8) -> u8 { a << b }", 8),
    ] {
        let ir = ir_for(src);
        assert!(
            ir.contains("sh.amt.oob") || ir.contains("sh.amt.trap"),
            "`{src}` must emit the shift-amount guard:\n{ir}"
        );
        assert!(
            ir.contains(&format!("i64 {width}")) || ir.contains(&format!(", {width}")),
            "`{src}` must compare the amount against its DECLARED width {width}:\n{ir}"
        );
    }
}

/// B-2026-08-06-16: the upper half of u64 is writable as a literal, and
/// the compiled backends agree with the interpreter on its value.
///
/// It rides the i64 carrier as a wrapped bit pattern, so this is really a
/// check that the pattern survives codegen and renders UNSIGNED at the
/// print sink. Deliberately prints the binding rather than an inline
/// expression: an inline BINOP whose result exceeds i64::MAX renders signed
/// on this surface (B-2026-08-06-18, pre-existing and separate), and mixing
/// that in would make this test fail for an unrelated reason.
#[test]
fn e2e_u64_upper_half_literal_round_trips() {
    let Some(out) = run_program(
        "fn main() {\n\
             \x20   let a: u64 = 18446744073709551615u64;\n\
             \x20   println(a);\n\
             \x20   let b: u64 = 9223372036854775808u64;\n\
             \x20   println(b);\n\
             \x20   let c: u64 = 0xFFFFFFFFFFFFFFFFu64;\n\
             \x20   println(c);\n\
             \x20   println(a == u64.MAX);\n\
             \x20   println(b > 9223372036854775807u64);\n\
             \x20   let d: u64 = a / 2u64;\n\
             \x20   println(d);\n\
             }\n",
    ) else {
        return;
    };
    assert_eq!(
        out,
        "18446744073709551615\n9223372036854775808\n18446744073709551615\n\
             true\ntrue\n9223372036854775807\n"
    );
}

#[test]
fn test_ir_signed_right_shift_is_arithmetic() {
    let ir = ir_for("fn shift(a: i64, b: i64) -> i64 { a >> b }");
    assert!(ir.contains("ashr i64"), "i64 >> must emit ashr:\n{ir}");
}

#[test]
fn test_ir_unsigned_right_shift_is_logical() {
    let ir = ir_for("fn shift(a: u64, b: u64) -> u64 { a >> b }");
    assert!(ir.contains("lshr i64"), "u64 >> must emit lshr:\n{ir}");
    assert!(
        !ir.contains("ashr i64"),
        "u64 >> must not emit arithmetic shift:\n{ir}"
    );
}

#[test]
fn test_e2e_u64_ops_and_sort_unsigned_match_run() {
    // B-2026-07-04-8 parity: once the interpreter gained its u64 model,
    // codegen had to agree on unsigned print / div inside f-strings (the
    // lowering-recurse fix) and on `Vec[u64].sort()` order — a value ≥ 2⁶³
    // sorts AFTER the positives, not first as a negative i64 (the unsigned
    // default-sort thunk). Drives the whole chain end-to-end.
    let src = "fn main() {\n\
                   \x20   let hi: u64 = 1u64 << 63;\n\
                   \x20   println(f\"{hi}\");\n\
                   \x20   println(f\"{hi / 2u64}\");\n\
                   \x20   let mut xs: Vec[u64] = [1u64 << 63, 5u64, 1u64 << 62, 0u64];\n\
                   \x20   xs.sort();\n\
                   \x20   println(f\"{xs[0]},{xs[3]}\");\n\
                   }\n";
    assert_eq!(
        run_program(src).as_deref(),
        Some("9223372036854775808\n4611686018427387904\n0,9223372036854775808\n")
    );
}

// ── Cast ─────────────────────────────────────────────────────

#[test]
fn test_ir_int_to_float_cast() {
    let ir = ir_for("fn to_float(x: i64) -> f64 { x as f64 }");
    assert!(ir.contains("sitofp"), "should use sitofp for int-to-float");
}

#[test]
fn test_ir_uint_to_float_emits_uitofp() {
    // phase-8 cast slice 6 (int→float verification): an *unsigned* source
    // converts via `uitofp` (so 255u8 → 255.0, not -1.0). Pairs with
    // `test_ir_int_to_float_cast` (signed source → sitofp).
    let ir = ir_for("fn to_float(x: u32) -> f64 { x as f64 }");
    assert!(
        ir.contains("uitofp"),
        "unsigned int→float should use uitofp:\n{ir}"
    );
}

#[test]
fn test_ir_float_to_int_cast() {
    let ir = ir_for("fn to_int(x: f64) -> i64 { x as i64 }");
    assert!(ir.contains("fptosi"), "should use fptosi for float-to-int");
}

#[test]
fn test_ir_int_truncate() {
    let ir = ir_for("fn to_i32(x: i64) -> i32 { x as i32 }");
    assert!(ir.contains("trunc"));
}

#[test]
fn test_ir_int_signed_widen_emits_sext() {
    // phase-8 cast slice 5 (int→int verification): widening a *signed*
    // source sign-extends. Pairs with `test_ir_u8_cast_to_i32_emits_zext`
    // (unsigned source → zext) and `test_ir_int_truncate` (narrowing →
    // trunc) to pin all three int→int `as` lowerings.
    let ir = ir_for("fn widen(x: i8) -> i64 { x as i64 }");
    assert!(
        ir.contains("sext i8"),
        "signed widening should sign-extend:\n{ir}"
    );
}

// ── Main function ─────────────────────────────────────────────

#[test]
fn test_ir_main_returns_i32() {
    let ir = ir_for(
        r#"
fn main() {
    println(42);
}
"#,
    );
    // main must be declared as returning i32 for C ABI compatibility
    assert!(ir.contains("define i32 @main()"));
    // Should have a `ret i32 0` at the end
    assert!(ir.contains("ret i32 0"));
}

/// B-2026-08-31-24 (value half) — an over-aligned Vec element computes the
/// same thing through every container placement it can occupy.
///
/// The regression test for the bug itself is
/// `test_ir_over_aligned_vec_element_buffer_uses_the_aligned_allocator`,
/// which pins the ALLOCATION to the aligned allocator. This one guards the
/// other direction — that the aligned buffer still round-trips values — and
/// widens the shape coverage past that test's bare `Vec[Vector]`.
///
/// IT DOES NOT GO RED ON THE UNPATCHED TREE, and that is a property of the
/// bug rather than a gap in the test: the fault is heap-state dependent, so
/// whether a given build crashes is luck. `karac build` on this source
/// segfaulted; this harness's build of the same source did not, on the same
/// machine, in the same minute. That is exactly why the deterministic half
/// has to be an assertion over the emitted IR.
///
/// Twin of `tests/interpreter.rs`'s `test_vec_of_vector_operations_round_trip`,
/// pinned to the same string.
#[test]
fn e2e_vec_of_vector_operations_round_trip() {
    let Some(out) = run_program(HEAP_ALIGN_SRC) else {
        return;
    };
    assert_eq!(
        out,
        r#"lit  [Vector(1, 2, 3, 4), Vector(5, 6, 7, 8)]
idx  Vector(1, 2, 3, 4)
push [Vector(1, 2, 3, 4), Vector(5, 6, 7, 8), Vector(1, 2, 3, 4)]
for  Vector(1, 2, 3, 4)
for  Vector(5, 6, 7, 8)
for  Vector(1, 2, 3, 4)
set  [Vector(1, 2, 3, 4), Vector(5, 6, 7, 8), Vector(1, 2, 3, 4)]
pop  Vector(1, 2, 3, 4)
ins  [Vector(5, 6, 7, 8), Vector(1, 2, 3, 4), Vector(5, 6, 7, 8)]
rem  Vector(5, 6, 7, 8)
ret  [Vector(1, 2, 3, 4)]
w8   [Vector(1, 2, 3, 4, 5, 6, 7, 8)]
hs   [Holder { v: Vector(1, 2, 3, 4), n: 1 }]
es   [V(Vector(1, 2, 3, 4))]
map  {k: Vector(1, 2, 3, 4)}
os   [Some(Vector(1, 2, 3, 4))]
av   [[Vector(1, 2, 3, 4), Vector(5, 6, 7, 8)]]
sum  3520
"#
    );
}

/// B-2026-08-31-18 — a `Vector[T, N]` or `Array[T, N]` ENUM PAYLOAD survives
/// the round trip through the variant's payload words.
///
/// Both were sized as ONE word. For an array that was a pure under-count (the
/// pack side correctly produced N words, so `out.len() > num_words` heap-boxed
/// it while the unpack, recomputing the same conservative 1, read word 0 as the
/// value); for a vector it was worse, because `coerce_to_i64` has no vector arm
/// and returns a literal ZERO — the payload was not truncated, it was erased.
///
/// The `m*` rows are the ones that matter. A `match` binding is a VALUE, so
/// `E.V4(w) => w` bound `0` (or the first element, or a box pointer) with
/// nothing in the program to say so; the Display rows only make the same
/// corruption visible. `b[0]` on an array binding was a hard codegen error
/// ("Index operator applied to non-array type") because the binding rebuilt as
/// an `i64`.
///
/// EVERY ROW IS A DIFFERENT PART OF THE WORD ACCOUNTING:
///  - `d4`/`m4` — 4 lanes into a variant area wide enough to hold them inline.
///  - `d8`/`m8`/`o8` — 8 lanes. Inline in `E` (whose area is the max over
///    variants) and BOXED in `Option`, whose area is 3, so this is the pair
///    that exercises the debox path and the `malloc`-alignment fix with it.
///  - `d32`/`m32` — lanes NARROWER than a word, which the one-word-per-lane
///    convention zero-extends and the rebuild truncates back.
///  - `df`/`mf` — f64 lanes, which round-trip as bit patterns, and `dh`/`mh` +
///    `du`/`mu` — f16 and u8 lanes, SUB-WORD components unpacked at their exact
///    width. Narrow floats have been the omitted case in several hand-written
///    width lists this month, so they are pinned here.
///  - `ma`/`oa` — the array half, read through an INDEX so the binding's LLVM
///    type is asserted and not just its rendering.
///  - `hw` — a vector AND an array as FIELDS of a struct payload, the shape
///    that reached `reconstruct_payload_value`'s "unexpected multi-word
///    non-struct field" fallback and `insertvalue`d an `i64` into a
///    `<4 x i64>` slot: invalid IR that failed module verification.
///
/// The array payloads sit on a NON-derived enum deliberately: `Array[T, N]` has
/// no arm in `emit_display_fn_for_type_expr`, so a derived-Display enum
/// carrying one panics the compiler (B-2026-08-31-19) before any of this runs.
/// `Vec[Vector[T, N]]` is absent for a different reason — its element buffer is
/// `malloc`ed and accessed at the vector's natural 32-byte alignment, which
/// faults depending on heap state (B-2026-08-31-24).
///
/// Twin of `tests/interpreter.rs`'s
/// `test_vector_array_enum_payload_round_trip`, pinned to the same string.
#[test]
fn e2e_vector_array_enum_payload_round_trip() {
    let Some(out) = run_program(
        r#"#[derive(Display)]
enum E {
    V4(Vector[i64, 4]),
    V8(Vector[i64, 8]),
    V32(Vector[i32, 4]),
    Vf(Vector[f64, 2]),
    Vh(Vector[f16, 2]),
    Vu(Vector[u8, 4]),
    N,
}

struct Holder { v: Vector[i64, 4], a: Array[i64, 3], n: i64 }
enum H { W(Holder), N }

// Array payloads live on a NON-derived enum: `Array[T, N]` has no Display
// arm in codegen at any depth (B-2026-08-31-19), so a derived-Display enum
// carrying one panics the compiler before this row's reconstruction runs.
enum A { A3(Array[i64, 3]), N }

fn main() {
    let n = env.args().len() as i64;
    let v4: Vector[i64, 4] = Vector[i64, 4](n, n + 1, n + 2, n + 3);
    let v8: Vector[i64, 8] = Vector[i64, 8](n, n+1, n+2, n+3, n+4, n+5, n+6, n+7);
    let v32: Vector[i32, 4] = Vector[i32, 4](n as i32, (n+1) as i32, (n+2) as i32, (n+3) as i32);
    let vf: Vector[f64, 2] = Vector[f64, 2](n as f64 + 0.5, n as f64 + 1.5);
    let vh: Vector[f16, 2] = Vector[f16, 2]((n as f32 + 1.5f32) as f16, (n as f32 + 2.5f32) as f16);
    let vu: Vector[u8, 4] = Vector[u8, 4](n as u8, (n + 1) as u8, (n + 2) as u8, (n + 3) as u8);
    let a3: Array[i64, 3] = [n, n + 1, n + 2];

    let d4 = E.V4(v4);
    println(f"d4  {d4}");
    let d8 = E.V8(v8);
    println(f"d8  {d8}");
    let d32 = E.V32(v32);
    println(f"d32 {d32}");
    let df = E.Vf(vf);
    println(f"df  {df}");

    let dh = E.Vh(vh);
    println(f"dh  {dh}");
    let du = E.Vu(vu);
    println(f"du  {du}");

    match d4 { E.V4(w) => { println(f"m4  {w}"); } E.V8(w) => {} E.V32(w) => {} E.Vf(w) => {} E.Vh(w) => {} E.Vu(w) => {} E.N => {} }
    match d8 { E.V8(w) => { println(f"m8  {w}"); } E.V4(w) => {} E.V32(w) => {} E.Vf(w) => {} E.Vh(w) => {} E.Vu(w) => {} E.N => {} }
    match d32 { E.V32(w) => { println(f"m32 {w}"); } E.V4(w) => {} E.V8(w) => {} E.Vf(w) => {} E.Vh(w) => {} E.Vu(w) => {} E.N => {} }
    match df { E.Vf(w) => { println(f"mf  {w}"); } E.V4(w) => {} E.V8(w) => {} E.V32(w) => {} E.Vh(w) => {} E.Vu(w) => {} E.N => {} }
    match dh { E.Vh(w) => { println(f"mh  {w}"); } E.V4(w) => {} E.V8(w) => {} E.V32(w) => {} E.Vf(w) => {} E.Vu(w) => {} E.N => {} }
    match du { E.Vu(w) => { println(f"mu  {w}"); } E.V4(w) => {} E.V8(w) => {} E.V32(w) => {} E.Vf(w) => {} E.Vh(w) => {} E.N => {} }

    let da = A.A3(a3);
    match da { A.A3(b) => { println(f"ma  {b[0]} {b[1]} {b[2]}"); } A.N => {} }

    let o4: Option[Vector[i64, 4]] = Some(v4);
    match o4 { Some(w) => { println(f"o4  {w}"); } None => {} }
    let o8: Option[Vector[i64, 8]] = Some(v8);
    match o8 { Some(w) => { println(f"o8  {w}"); } None => {} }
    let oa: Option[Array[i64, 3]] = Some(a3);
    match oa { Some(b) => { println(f"oa  {b[0]} {b[2]}"); } None => {} }
    let r4: Result[Vector[i64, 4], i64] = Ok(v4);
    match r4 { Ok(w) => { println(f"r4  {w}"); } Err(x) => { println(f"re  {x}"); } }

    let h = H.W(Holder { v: v4, a: a3, n: n });
    match h { H.W(x) => { println(f"hw  {x.v} {x.a[0]} {x.a[2]} {x.n}"); } H.N => {} }
}
"#,
    ) else {
        return;
    };
    assert_eq!(
        out,
        r#"d4  V4(Vector(1, 2, 3, 4))
d8  V8(Vector(1, 2, 3, 4, 5, 6, 7, 8))
d32 V32(Vector(1, 2, 3, 4))
df  Vf(Vector(1.5, 2.5))
dh  Vh(Vector(2.5, 3.5))
du  Vu(Vector(1, 2, 3, 4))
m4  Vector(1, 2, 3, 4)
m8  Vector(1, 2, 3, 4, 5, 6, 7, 8)
m32 Vector(1, 2, 3, 4)
mf  Vector(1.5, 2.5)
mh  Vector(2.5, 3.5)
mu  Vector(1, 2, 3, 4)
ma  1 2 3
o4  Vector(1, 2, 3, 4)
o8  Vector(1, 2, 3, 4, 5, 6, 7, 8)
oa  1 3
r4  Vector(1, 2, 3, 4)
hw  Vector(1, 2, 3, 4) 1 3 1
"#
    );
}

/// B-2026-08-01-3 residual (pass-roundtrip, closed) — `e = pass(e)` /
/// `s = pass_res(s)` / `p = remake(p)`: the RHS mentions the target,
/// but owned args are caller-retains (the callee deep-copies at entry),
/// so the OLD value never moved and must be eager-freed before the
/// store orphans it. The fix is MEMORY ONLY — the NLL channel fires the
/// user bodies exactly once at the binding's last-use statement, so
/// this pin asserts the output is unchanged from the pre-fix parity
/// (the leak itself is gated by `tests/memory_sanitizer.rs`'s
/// `asan_roundtrip_reassign_frees_displaced_original`, which fails
/// pre-fix under LSan). Twin of `tests/interpreter.rs`'s
/// `test_roundtrip_reassign_single_nll_fire`.
#[test]
fn e2e_roundtrip_reassign_single_nll_fire() {
    let Some(out) = run_program(
        "struct Res { id: i64, name: String }\n\
             impl Drop for Res {\n\
             \x20   fn drop(mut ref self) {\n\
             \x20       println(f\"drop {self.id} {self.name}\")\n\
             \x20   }\n\
             }\n\
             enum Loud { Hold(Res), Quiet }\n\
             impl Drop for Loud {\n\
             \x20   fn drop(mut ref self) {\n\
             \x20       println(\"loud drop\")\n\
             \x20   }\n\
             }\n\
             fn mk_loud(n: i64) -> Loud {\n\
             \x20   return Loud.Hold(Res { id: n, name: f\"l{n}\" });\n\
             }\n\
             fn pass(b: Loud) -> Loud {\n\
             \x20   return b;\n\
             }\n\
             fn mk_res(n: i64) -> Res {\n\
             \x20   return Res { id: n, name: f\"r{n}\" };\n\
             }\n\
             fn pass_res(b: Res) -> Res {\n\
             \x20   return b;\n\
             }\n\
             fn main() {\n\
             \x20   let mut e = mk_loud(7);\n\
             \x20   println(\"a\");\n\
             \x20   e = pass(e);\n\
             \x20   println(\"b\");\n\
             \x20   let mut s = mk_res(4);\n\
             \x20   s = pass_res(s);\n\
             \x20   println(s.name);\n\
             \x20   println(\"end\");\n\
             }\n",
    ) else {
        return;
    };
    assert_eq!(out, "a\nloud drop\ndrop 7 l7\nb\nr4\ndrop 4 r4\nend\n");
}

/// B-2026-08-22-27 — an `i64`-keyed map under a non-default hasher, across
/// a resize. THE `Map[i64, V]` FAMILY IS MONOMORPHIZED and the other
/// hasher tests here are not: they use `String` keys (a different, already
/// correct lowering) and three entries, so none of them ever reached the
/// synthesized `karac_map_i64_i64_insert_old` / `_get`, and none crossed a
/// resize. That combination is exactly why this shipped.
///
/// Those two bodies called a `karac_hash_<K>` symbol baked in at emission
/// time rather than the hash the map was CONSTRUCTED with, so a
/// non-default map filed keys under one hash and probed under another.
/// Every `len` stayed right while `contains_key` / `get` went blind.
///
/// It looked like a large-N resize bug and is not one — it reproduces at
/// SIXTEEN keys. What made it look scale-dependent is that each resize
/// takes the slow path, which rehashes the whole table through the stored
/// fn and REPAIRS every misfiled key; only the keys added since the last
/// resize stay lost. So the damage is always a contiguous tail, and at
/// large N that tail begins at exactly 3/4 of capacity (196609 at
/// N=200000, 13567 lost at N=800000).
#[test]
fn an_fx_hashed_i64_map_finds_every_key_across_resizes() {
    let src = "fn probe(n: i64) -> i64 {\n\
            \x20   let mut m: Map[i64, i64, FxBuildHasher] = Map.new();\n\
            \x20   let mut i = 0;\n\
            \x20   while i < n { m.insert(i, i * 3); i = i + 1; }\n\
            \x20   let mut bad = 0;\n\
            \x20   let mut j = 0;\n\
            \x20   while j < n {\n\
            \x20       if not m.contains_key(j) { bad = bad + 1; }\n\
            \x20       if m.get(j).unwrap_or(-1) != j * 3 { bad = bad + 1; }\n\
            \x20       j = j + 1;\n\
            \x20   }\n\
            \x20   if m.len() != n { bad = bad + 1; }\n\
            \x20   return bad;\n\
            }\n\
            fn main() {\n\
            \x20   println(probe(16) + probe(64) + probe(100) + probe(1000));\n\
            }\n";
    // Pre-fix this printed 498 (3 + 15 + 3 + 231 missing, each also
    // costing a wrong `get`); 16 alone was already broken.
    assert_eq!(run_program(src).as_deref(), Some("0\n"));

    // `Map[char, V]` shares this same emitted body — `char` lowers to
    // LLVM i32 — so it desynced too, and needs even fewer keys to show it:
    // 62 chars lost 13 pre-fix.
    let chars = "fn main() {\n\
            \x20   let mut m: Map[char, i64, FxBuildHasher] = Map.new();\n\
            \x20   let src = \"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789\";\n\
            \x20   let mut i = 0;\n\
            \x20   for c in src.chars() { m.insert(c, i); i = i + 1; }\n\
            \x20   let mut bad = 0;\n\
            \x20   for c in src.chars() { if not m.contains_key(c) { bad = bad + 1; } }\n\
            \x20   println(bad);\n\
            }\n";
    assert_eq!(run_program(chars).as_deref(), Some("0\n"));
}

/// B-2026-09-15-36 — the fixture entry point REJECTS a program that does
/// not typecheck, instead of running it on the interpreter and passing.
///
/// `run_program_full` deliberately does not abort on type errors (the
/// tree-walk interpreter is dynamically typed on purpose, and
/// `tests/typechecker.rs` relies on it), and the `Vec` it returns in slot
/// 2 carries RUNTIME errors only. So an ill-typed fixture used to return
/// `errs == []` and produce output — measured on this exact source:
/// `out=["end\n"] errs=[] errs_empty=true`. Any cell whose expected
/// output is a bare trailing marker would have passed on that, pinning
/// nothing and never going red when the gate it guarded moved.
///
/// This is the source from the row: struct-literal field shorthand naming
/// a field that does not exist, which the CLI correctly rejects with two
/// errors. `should_panic` on the message rather than on any panic, so a
/// fixture that fails for some unrelated reason cannot satisfy it.
#[test]
#[should_panic(expected = "Typecheck errors in a codegen fixture")]
fn e2e_fixture_entry_point_rejects_a_program_that_does_not_typecheck() {
    let src = "struct H { f: i64 }\n\
                   fn main() {\n\
                   \x20   let a: i64 = 1;\n\
                   \x20   let h = H { a };\n\
                   \x20   println(\"end\");\n\
                   }\n";
    let _ = karac::run_program_full_checked(src);
}

/// B-2026-09-01-16 — A STRUCT WHOSE FIELD IS PASSED BY VALUE FROM INSIDE AN
/// INTERPOLATED-STRING ARGUMENT HAS ITS `Drop` DEFERRED TO SCOPE EXIT ON THE
/// COMPILED SURFACES — the sequential column.
///
/// `h`'s last use is the `readf(h.r)` call inside the `println` f-string, so
/// design.md § Drop ("Destructors fire at each binding's live-range end, not
/// at lexical scope end") puts `drop 40` after `field=43` and BEFORE `end`.
/// The row measured every compiled column printing `field=43 end drop 40`
/// while the interpreter had the order above; hoisting the call into its own
/// `let` made all four agree, which is what localized it to the admission of
/// the binding rather than the existence of the early-fire pass.
///
/// The row closed without a change of its own. Probed at its filing commit
/// (`7717d75`) the default build printed `field=43 end drop 40` while
/// `KARAC_AUTO_PAR=0` was ALREADY the interpreter's order — the same `np`
/// mismeasurement the class row's fix corrected — and `aa21ffb` (the
/// B-2026-08-31-4 / B-2026-08-31-6 fix: an auto-par group no longer swallows
/// a covered NLL drop point) took the divergence. So the row's distinction
/// from its class ("neither a `ref` arg nor a heap return") was drawn against
/// the misattributed mechanism; what selected this program was a group
/// covering the f-string statement. This pin exists so it stays gone — a
/// position-only drift like this is invisible to every count-based drop
/// gate.
#[test]
fn test_e2e_nll_drop_point_at_a_by_value_field_arg_inside_an_fstring() {
    let out = run_program(
        r#"
struct Res { id: i64, buf: Vec[i64] }
impl Drop for Res { fn drop(mut ref self) { println(f"drop {self.id}") } }
struct Holder { r: Res }

fn mk(n: i64) -> Res {
    let mut v: Vec[i64] = Vec.new();
    let mut i = 0;
    while i < 8 { v.push(n + i); i = i + 1; }
    return Res { id: n, buf: v }
}

fn hand(r: Res) -> Res { return r }
fn readf(r: Res) -> i64 { return r.buf[3] }

fn main() {
    let b = mk(20);
    let hb = hand(b);
    println(f"hand={hb.buf[1]}");
    let h = Holder { r: mk(40) };
    println(f"field={readf(h.r)}");
    println("end");
}
    "#,
    );
    if let Some(out) = out {
        assert_eq!(
            out, "hand=21\ndrop 20\nfield=43\ndrop 40\nend\n",
            "`h` must die at the f-string statement that last uses it, not at \
                 scope exit; the row measured `field=43 end drop 40`; got {out:?}"
        );
    }
}

/// B-2026-08-21-31 — `isize` did not exist as a type. design.md names it a
/// v1 numeric primitive in four normative passages (§ 2178 the four method
/// families, § 4000 the `as`-cast rule, § 5356 arithmetic traits, § 13104
/// the FFI table's `ptrdiff_t` row) and writes it into six signatures, and
/// `src/cheader.rs`, `src/deque_head.rs` and `src/wasm_glue.rs` already
/// mapped it — three back-end tables carrying a type the resolver refused.
///
/// These are AOT parity pins against the interpreter answers in
/// `tests/interpreter.rs`. The signed half is what they are for: `usize`
/// shares `isize`'s width and its LLVM type, so every lowering that
/// reinterpreted the i64 carrier as UNSIGNED would still pass a
/// width-only test and answer wrongly here.
#[test]
fn isize_lowers_as_a_signed_pointer_width_integer() {
    assert_eq!(
        run_program("fn main() { println(isize.MAX); println(isize.MIN); }"),
        Some("9223372036854775807\n-9223372036854775808\n".to_string()),
        "isize is SIGNED pointer-width — not usize's all-ones MAX"
    );
    assert_eq!(
            run_program(
                "fn f(a: isize, b: isize) -> isize { a / b }\n                 fn main() {\n                     let a: isize = -7isize;\n                     println(a);\n                     println(f(a, 2isize));\n                     println(a < 0isize);\n                     println(a.abs());\n                 }\n"
            ),
            Some("-7\n-3\ntrue\n7\n".to_string()),
            "negatives, SIGNED division and SIGNED comparison must survive lowering"
        );
    assert_eq!(
            run_program(
                "fn main() {\n                     println(isize.MAX.checked_add(1isize));\n                     println(isize.MAX.wrapping_add(1isize));\n                     println(isize.MAX.saturating_add(1isize));\n                     println(isize.MAX.overflowing_add(1isize));\n                 }\n"
            ),
            Some(
                "None\n-9223372036854775808\n9223372036854775807\n(-9223372036854775808, true)\n"
                    .to_string()
            ),
            "the four overflow method families design.md:2178 promises"
        );
    assert_eq!(
            run_program(
                "fn main() {\n                     let c: i64 = 100;\n                     let a: isize = 7isize;\n                     println(c as isize);\n                     println(a as i64);\n                     println((-1isize) as u8);\n                 }\n"
            ),
            Some("100\n7\n255\n".to_string()),
            "`as` casts both ways, including the wrapping narrow"
        );
    assert_eq!(
            run_program(
                "struct Holder { n: isize }\n                 fn main() {\n                     let o: Option[isize] = Some(-5isize);\n                     match o { Some(x) => println(x), None => println(0) }\n                     println(Holder { n: -9isize }.n);\n                     let mut v: Vec[isize] = [3isize, -1isize, 2isize];\n                     v.sort();\n                     println(f\"{v}\");\n                 }\n"
            ),
            Some("-5\n-9\n[-1, 2, 3]\n".to_string()),
            "as an Option payload, a struct field, and a SIGNED-sorted Vec element"
        );
    // Displaying the Option/Result VARIABLE ITSELF, which the `match` above
    // does not reach: destructuring reads the payload directly, while
    // `println(o)` goes through the display REGISTRATION, and that path
    // asks `type_to_type_expr` for the payload's `TypeExpr`.
    //
    // That function matches with a `_ => Error` fallback, so a missing arm
    // is SILENT — no non-exhaustive-match error, no wrong answer at the
    // type level. The payload comes back `TypeKind::Error`, the binding is
    // skipped, and `println` on a plain variable is refused with "bind a
    // struct literal or call result to a `let` first" while the interpreter
    // renders it fine: a run-vs-build divergence. That is exactly how the
    // 128-bit widths broke (B-2026-08-19-23), and the `Isize` arm sits
    // beside theirs for the same reason, so it needs its own assertion.
    assert_eq!(
        run_program(
            "fn main() {\n\
                 let o: Option[isize] = Some(-5isize);\n\
                 println(o);\n\
                 let n: Option[isize] = None;\n\
                 println(n);\n\
                 let r: Result[isize, String] = Ok(7isize);\n\
                 println(r);\n\
                 println(f\"{o}\");\n\
                 }\n"
        ),
        Some("Some(-5)\nNone\nOk(7)\nSome(-5)\n".to_string()),
        "the Option/Result VARIABLE renders, not just its destructured payload"
    );
}

/// B-2026-08-21-31 — the overflow trap must sit at the SIGNED boundary.
/// `isize.MAX + 1` is an ordinary mid-range value for the same bit width
/// unsigned, so a lowering that reused `usize`'s trap would not fire here.
#[test]
fn isize_overflow_traps_at_the_signed_boundary() {
    let out = run_program_capturing("fn main() { let a: isize = isize.MAX; println(a + 1isize); }");
    if let Some(c) = out {
        assert!(
            c.stdout.contains("integer overflow") || c.stderr.contains("integer overflow"),
            "isize must trap at the signed boundary, got stdout={:?} stderr={:?}",
            c.stdout,
            c.stderr
        );
    }
}

/// Every integer key WIDTH round-trips through the compiled map, including
/// the negative values, after B-2026-09-07-42 moved integer keys off the
/// byte path onto `karac_hash_int`.
///
/// The new path hands the key over as a VALUE, so it has to reconstruct the
/// exact bytes the pointer path used to hash: zero-extend (a negative `i8`
/// presents `0xff`, not a sign-extended word carrying seven bytes the key
/// does not have) and mask to the key's own width. Get either wrong and the
/// damage is not a crash — `-1i8` and `-1i32` would hash alike, or a key
/// would hash one way on insert and another on lookup, and the map would
/// simply MISS. Both directions are asserted for that reason: every key
/// inserted is found with its own value, and a neighbouring key that was
/// never inserted still misses.
#[test]
fn integer_key_widths_round_trip_through_the_register_hash() {
    // One map per width, each holding the extremes of its own range plus a
    // few interior values, then probed for hits AND for misses.
    let cases: [(&str, &str); 5] = [
        ("i8", "-128, -1, 0, 1, 127"),
        ("i16", "-32768, -1, 0, 1, 32767"),
        ("i32", "-2147483648, -1, 0, 1, 2147483647"),
        ("i64", "-9223372036854775807, -1, 0, 1, 9223372036854775807"),
        ("u8", "0, 1, 127, 128, 255"),
    ];
    // BOTH hashers: the seeded default and the `FxBuildHasher` opt-out
    // reach different runtime symbols through the same width logic, so a
    // zext/mask mistake could live in one arm and not the other.
    for (map_ty, arm) in [
        ("Map[{ty}, i64]", "default"),
        ("Map[{ty}, i64, FxBuildHasher]", "fx"),
    ] {
        for (ty, keys) in cases {
            let map_ty = map_ty.replace("{ty}", ty);
            let src = format!(
                "fn main() {{\n\
                     let mut m: {map_ty} = Map.new();\n\
                     let ks: Vec[{ty}] = vec![{keys}];\n\
                     let mut i = 0;\n\
                     while i < ks.len() {{\n\
                         let _ = m.insert(ks[i], (i as i64) + 100);\n\
                         i = i + 1;\n\
                     }}\n\
                     let mut hits = 0;\n\
                     let mut i2 = 0;\n\
                     while i2 < ks.len() {{\n\
                         match m.get(ks[i2]) {{\n\
                             Some(v) => {{ if v == (i2 as i64) + 100 {{ hits = hits + 1; }} }}\n\
                             None => {{}}\n\
                         }}\n\
                         i2 = i2 + 1;\n\
                     }}\n\
                     println(f\"{{hits}} {{m.len()}}\");\n\
                 }}\n"
            );
            let Some(out) = run_program(&src) else { return };
            assert_eq!(
                out.trim(),
                "5 5",
                "every {ty} key must be found with its own value on the {arm} \
                 hasher, and the map must hold exactly the 5 distinct keys \
                 inserted; a wrong extend or mask in the register hash path \
                 collapses or loses them"
            );
        }
    }
}

#[test]
fn test_e2e_volatile_read_write_roundtrip() {
    // MMIO intrinsics `volatile_write` / `volatile_read`
    // (`runtime/stdlib/intrinsics.kara`) lower to a volatile store / load
    // through a raw pointer. Exercised over a regular stack slot (volatile
    // semantics on ordinary memory behave as a normal read/write, so the
    // value round-trips): write 42 through `*mut i32`, read it back through
    // `*const i32`, expect 42. Witnesses the codegen intercept in
    // `compile_call` + the pointee-sized volatile load/store.
    let out = run_program(
        r#"
fn main() {
    let mut cell: i32 = 7;
    // Safety: pw / pr point to the live local `cell`.
    let after = unsafe {
        let pw: *mut i32 = ptr.mut(cell);
        volatile_write(pw, 42);
        let pr: *const i32 = ptr.const(cell);
        volatile_read(pr)
    };
    println(after);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "42");
    }
}

#[test]
fn test_e2e_f16_arithmetic_roundtrip() {
    // Values exactly representable in f16 (halves/quarters) so the printed
    // result is exact. 1.5 + 2.25 = 3.75; 3.0 * 4.0 = 12.
    let out = run_program(
        "fn main() {\n\
                 let x: f16 = 1.5f16;\n\
                 let y: f16 = 2.25f16;\n\
                 println(x + y);\n\
                 println(3.0f16 * 4.0f16);\n\
             }",
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "3.75\n12");
    }
}

#[test]
fn test_e2e_total_order_float_wrappers() {
    // B-2026-07-22-11 — the total-order `F32`/`F64` wrappers
    // (`struct F32 { value: f32 }`, `#[derive(Eq, Ord, Hash)]`) silently
    // miscompiled: their baked struct lived only in the synthetic prelude
    // module, dropped by codegen's super-module assembly, so construction
    // stored garbage, `.value` errored, and every comparison fell through
    // to the const-0 assoc-call tail (`a > b` → false, `a == b` → true).
    // Now the struct is seeded into codegen and comparisons emit a TOTAL
    // order (NaN last, -0 < +0, bit-equality) — construction, `.value`,
    // `<`/`>`/`==`, `Map` keys, and `sort` all work. Zero codegen tests
    // existed for the wrappers before this.
    let out = run_program(
            "fn main() {\n\
                 let a: F32 = F32 { value: 2.5 };\n\
                 let b: F32 = F32 { value: 1.5 };\n\
                 println(a > b);\n\
                 println(a < b);\n\
                 println(a == a);\n\
                 println(a == b);\n\
                 println(a.value + b.value);\n\
                 let mut m: Map[F32, i64] = Map.new();\n\
                 let _ = m.insert(F32 { value: 2.0 }, 20);\n\
                 let _ = m.insert(F32 { value: 3.0 }, 30);\n\
                 match m.get(F32 { value: 2.0 }) { Some(v) => println(v), None => println(0 - 1) }\n\
                 let mut v: Vec[F32] = Vec.new();\n\
                 v.push(F32 { value: 3.0 });\n\
                 v.push(F32 { value: 1.0 });\n\
                 v.push(F32 { value: 2.0 });\n\
                 v.sort();\n\
                 println(v[0].value);\n\
                 println(v[2].value);\n\
                 let neg0: F64 = F64 { value: 0.0 * (0.0 - 1.0) };\n\
                 let pos0: F64 = F64 { value: 0.0 };\n\
                 println(neg0 < pos0);\n\
                 println(neg0 == pos0);\n\
             }",
        );
    if let Some(out) = out {
        // a>b, a<b, a==a, a==b, .value sum, map get, sort[0], sort[2],
        // -0<+0 (total order), -0==+0 (bit-eq).
        assert_eq!(out, "true\nfalse\ntrue\nfalse\n4\n20\n1\n3\ntrue\nfalse\n");
    }
}

#[test]
fn test_e2e_float_wrapper_value_field_from_enum_payload() {
    // B-2026-07-23-2 — reading a total-order float-wrapper's `.value` field
    // off a match-arm binding extracted from a USER-ENUM payload. `f.value`
    // is genuinely `f32`, but its arm sits beside an `f64` arm (`x as f64`),
    // so the two arms had different LLVM types (`float` vs `double`) and the
    // match phi bailed to the `i64 0` placeholder → `ret i64 0` against a
    // `double` return, a module-verification failure (interp was correct).
    // Now `unify_float_match_arm_widths` widens the narrower arm up to the
    // widest present before the phi. Covers F32 (by-value + by-ref scrutinee),
    // F64, arm-ordering, and `.value` used in arithmetic — all must equal the
    // interpreter (`karac run`) output.
    let out = run_program(
        "enum Num { I(i64), F(F32) }\n\
             enum Dbl { I(i64), D(F64) }\n\
             fn show(n: Num) -> f64 { match n { I(x) => x as f64, F(f) => f.value } }\n\
             fn show_ref(n: ref Num) -> f64 { match n { I(x) => x as f64, F(f) => f.value } }\n\
             fn show_first(n: Num) -> f64 { match n { F(f) => f.value, I(x) => x as f64 } }\n\
             fn show_arith(n: Num) -> f64 { match n { I(x) => x as f64, F(f) => f.value + 1.0 } }\n\
             fn show_d(n: Dbl) -> f64 { match n { I(x) => x as f64, D(d) => d.value } }\n\
             fn main() {\n\
                 let a = Num.F(F32 { value: 2.5 });\n\
                 println(show(a));\n\
                 println(show(Num.I(7)));\n\
                 let b = Num.F(F32 { value: 4.0 });\n\
                 println(show_ref(b));\n\
                 println(show_first(Num.F(F32 { value: 1.5 })));\n\
                 println(show_arith(Num.F(F32 { value: 2.5 })));\n\
                 println(show_d(Dbl.D(F64 { value: 3.25 })));\n\
                 println(show_d(Dbl.I(9)));\n\
             }",
    );
    if let Some(out) = out {
        assert_eq!(out, "2.5\n7\n4\n1.5\n3.5\n3.25\n9\n");
    }
}

#[test]
fn e2e_mixed_int_float_branch_arms_convert_rather_than_zero() {
    // B-2026-08-30-49 — an integer arm beside a float arm made the merge's
    // all-same-type check fail, so the WHOLE construct fell through to the
    // const-`i64 0` placeholder and evaluated to `0` on both compiled
    // backends, silently, at every float width and for any value, while
    // `--interp` was correct throughout.
    //
    // The direct sibling of `test_e2e_float_wrapper_value_field_from_enum_
    // payload` above (B-2026-07-23-2): that one reconciled two float arms
    // of different WIDTHS, this one reconciles arms of different KINDS.
    // Both land in the same placeholder when unhandled.
    //
    // The three `control-` cases were measured GREEN before the fix and are
    // here to keep the assertion honest — the shape space is "arms disagree
    // at the LLVM level", and a change that made every branch convert would
    // pass the first fourteen while breaking these. The other fourteen were
    // each measured RED (`0`, or `0\n0` for the two return-position cases)
    // on the pre-fix compiler, so none of them can pass vacuously.
    //
    // `unsigned-above-i64max` is the case that pins SIGNEDNESS rather than
    // mere conversion: `uitofp` gives 18446744073709552000 where `sitofp`
    // on the same bits gives -1. It is why the conversion consults the arm
    // TAIL expression (peeled through any block wrapper) instead of
    // defaulting to signed.
    let cases: &[(&str, &str, &str)] = &[
            (
                "if-int-then",
                "fn main() { let n: i64 = 7; let a: f64 = if true { n } else { 0.0 }; println(a); }",
                "7\n",
            ),
            (
                "if-int-else",
                "fn main() { let n: i64 = 7; let a: f64 = if false { 0.0 } else { n }; println(a); }",
                "7\n",
            ),
            (
                "match-int-arm",
                "fn main() { let n: i64 = 7; let a: f64 = match 1 { 1 => n, _ => 0.0 }; println(a); }",
                "7\n",
            ),
            (
                "match-float-first",
                "fn main() { let n: i64 = 7; let a: f64 = match 1 { 0 => 0.0, _ => n }; println(a); }",
                "7\n",
            ),
            (
                "f32-annotation",
                "fn main() { let n: i64 = 7; let a: f32 = if true { n } else { 0.0 }; println(a); }",
                "7\n",
            ),
            (
                "unsigned-above-i64max",
                "fn main() { let u: u64 = 18446744073709551615u64; let a: f64 = if true { u } else { 0.0 }; println(a); }",
                "18446744073709552000\n",
            ),
            (
                "negative-int-arm",
                "fn main() { let n: i64 = -3; let a: f64 = if true { n } else { 0.0 }; println(a); }",
                "-3\n",
            ),
            (
                "if-let-payload",
                "fn opt() -> Option[i64] { Some(7) }\n\
                 fn main() { let a: f64 = if let Some(v) = opt() { v } else { 0.0 }; println(a); }",
                "7\n",
            ),
            (
                "return-position-if",
                "fn f(c: bool, n: i64) -> f64 { if c { n } else { 0.5 } }\n\
                 fn main() { println(f(true, 7)); println(f(false, 7)); }",
                "7\n0.5\n",
            ),
            (
                "return-position-match",
                "fn f(k: i64, n: i64) -> f64 { match k { 0 => n, _ => 2.5 } }\n\
                 fn main() { println(f(0, 7)); println(f(1, 7)); }",
                "7\n2.5\n",
            ),
            (
                "nested-branch",
                "fn main() { let n: i64 = 7; let a: f64 = if true { if true { n } else { 1.0 } } else { 2.0 }; println(a); }",
                "7\n",
            ),
            (
                "multi-int-arms",
                "fn main() { let n: i64 = 7; let a: f64 = match 2 { 0 => 1, 1 => 2, 2 => n, _ => 0.0 }; println(a); }",
                "7\n",
            ),
            (
                "block-bodied-arm",
                "fn main() { let a: f64 = if true { let z: i64 = 4; z } else { 0.0 }; println(a); }",
                "4\n",
            ),
            (
                "narrow-u8-arm",
                "fn main() { let b: u8 = 200; let a: f64 = if true { b } else { 0.0 }; println(a); }",
                "200\n",
            ),
            (
                "control-all-float",
                "fn main() { let a: f64 = if true { 1.5 } else { 0.0 }; println(a); }",
                "1.5\n",
            ),
            (
                "control-all-int",
                "fn main() { let n: i64 = 7; let a: i64 = if true { n } else { 0 }; println(a); }",
                "7\n",
            ),
            (
                "control-plain-let",
                "fn main() { let n: i64 = 7; let a: f64 = n; println(a); }",
                "7\n",
            ),
        ];
    for (label, src, want) in cases {
        assert_eq!(run_program(src).as_deref(), Some(*want), "{label}");
    }
}

#[test]
fn test_e2e_ptr_const_mut_place_shapes_roundtrip() {
    // `ptr.const(place)` / `ptr.mut(place)` over the full place grammar the
    // typechecker's place-validator accepts — field access, a nested field
    // chain, a tuple index, and a Vec index — not just the bare-binding /
    // deref shapes the earlier codegen slice covered. Each place: write a
    // sentinel through `ptr.mut(place)`, read it back through
    // `ptr.const(place)`, and expect the sentinel. Witnesses `ptr_place_addr`
    // GEPing to the correct in-place field / element address (owned-local
    // roots, where the same-scope round-trip is observable — the reliable
    // pattern the volatile-roundtrip test above also uses).
    let out = run_program(
        r#"
struct Reg { status: i32, control: i32 }
struct Block { inner: Reg, id: i64 }
fn main() {
    let mut r: Reg = Reg { status: 10, control: 20 };
    // Safety: every pointer addresses a live owned local for the duration of use.
    unsafe {
        let pw: *mut i32 = ptr.mut(r.status);
        volatile_write(pw, 99);
        let pr: *const i32 = ptr.const(r.status);
        println(volatile_read(pr));
    }
    let mut b: Block = Block { inner: Reg { status: 1, control: 2 }, id: 7 };
    unsafe {
        let pw: *mut i32 = ptr.mut(b.inner.control);
        volatile_write(pw, 500);
        let pr: *const i32 = ptr.const(b.inner.control);
        println(volatile_read(pr));
    }
    let mut pair: (i32, i32) = (11, 22);
    unsafe {
        let pw: *mut i32 = ptr.mut(pair.1);
        volatile_write(pw, 222);
        let pr: *const i32 = ptr.const(pair.1);
        println(volatile_read(pr));
    }
    let mut v: Vec[i32] = Vec.new();
    v.push(3);
    v.push(4);
    unsafe {
        let pw: *mut i32 = ptr.mut(v[1]);
        volatile_write(pw, 444);
    }
    println(v[1]);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "99\n500\n222\n444");
    }
}

#[test]
fn test_e2e_volatile_cell_read_write_roundtrip() {
    // Baked `VolatileCell[T]` stdlib type (`runtime/stdlib/volatile_cell.kara`,
    // prelude-visible — used here WITHOUT a local definition). Codegen lowers
    // it transparently to its inner `T` and intercepts `.read()` / `.write(v)`
    // as a volatile load / store against the binding's slot. Exercises two
    // instantiations at different widths (i32 + u8) in one program — the
    // generic-mono path fixed in B-2026-07-12-16. `0x1F` == 31.
    let out = run_program(
        r#"
fn main() {
    let mut reg: VolatileCell[i32] = VolatileCell.new(7);
    println(reg.read());
    reg.write(42);
    println(reg.read());
    reg.write(0x1F);
    println(reg.read());
    let mut flag: VolatileCell[u8] = VolatileCell.new(0);
    flag.write(1);
    println(flag.read());
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "7\n42\n31\n1");
    }
}

#[test]
fn test_e2e_interner_resolve_roundtrip() {
    // `resolve` hands back a borrowed (`cap = 0`) String view of the
    // interned bytes; it Displays through `println` directly and prints
    // the original text. Mirrors the interpreter
    // `test_interner_resolve_roundtrip`.
    let out = run_program(
        r#"
fn main() {
    let mut tab: Interner = Interner.new();
    let a = tab.intern("alpha");
    let b = tab.intern("beta");
    println(tab.resolve(a));
    println(tab.resolve(b));
    let s = tab.resolve(a);
    println(s.len());
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "alpha\nbeta\n5");
    }
}

#[test]
fn test_e2e_arena_push_get_roundtrip_i64() {
    // Phase-8 Arena codegen: bump-allocate three i64s; each `ArenaRef`
    // (a bare i64 index in codegen) resolves back via `get` (copy-out
    // through `karac_runtime_arena_get_copy`). Mirrors the interpreter
    // `test_arena_push_get_roundtrip` + `test_arena_len_tracks_pushes`.
    let out = run_program(
        r#"
fn main() {
    let a: Arena[i64] = Arena.new();
    let r0 = a.push(10);
    let r1 = a.push(20);
    let r2 = a.push(30);
    println(a.get(r0));
    println(a.get(r1));
    println(a.get(r2));
    println(a.len());
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "10\n20\n30\n3");
    }
}

#[test]
fn test_e2e_arena_checkpoint_rewind() {
    // Snapshot/restore: `high_water_mark` (erased to a bare i64 mark) +
    // `rewind_to` truncates; pre-checkpoint handles stay valid. Mirrors
    // the interpreter `test_arena_high_water_mark_and_rewind` +
    // `_rewind_keeps_pre_checkpoint_items`.
    let out = run_program(
        r#"
fn main() {
    let a: Arena[i64] = Arena.new();
    let r0 = a.push(100);
    let cp = a.high_water_mark();
    let _r1 = a.push(200);
    let _r2 = a.push(300);
    println(a.len());
    a.rewind_to(cp);
    println(a.len());
    println(a.get(r0));
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "3\n1\n100");
    }
}

#[test]
fn test_e2e_arena_foreign_checkpoint_ignored() {
    // A checkpoint minted by a DIFFERENT arena must not truncate this
    // one. With `ArenaCheckpoint` erased to a bare mark, the guard is
    // static (`arena_checkpoint_owner`): the foreign `rewind_to`
    // compiles to a no-op. ALSO regression-covers the par-group
    // handle-escape bail (2026-07-17): two independent `Arena.new()`
    // lets used to be auto-parallelized, and the branch's scope-exit
    // `FreeArenaHandle` freed the handle the parent then locked — a
    // pre-output futex hang. Mirrors the interpreter
    // `test_arena_rewind_with_foreign_checkpoint_is_ignored`.
    let out = run_program(
        r#"
fn main() {
    let a: Arena[i64] = Arena.new();
    let b: Arena[i64] = Arena.new();
    let _ra = a.push(1);
    let _rb0 = b.push(10);
    let _rb1 = b.push(20);
    let foreign = a.high_water_mark();
    b.rewind_to(foreign);
    println(b.len());
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "2");
    }
}

/// B-2026-07-11-2: indexing a `Vec[T]` (read AND write) with a NARROWER-than-i64
/// integer — here a `u8` from `String.bytes()` used directly as a count-table
/// index, the natural sliding-window / counting idiom — must lower correctly.
/// Before the fix, codegen emitted the bounds-check `icmp uge i8 %idx, i64 %len`
/// without widening the index, so LLVM module verification failed ("Both operands
/// to ICmp instruction are not of the same type!") and `karac build`/JIT aborted
/// while the tree-walk interpreter handled it — a run/build divergence. The fix
/// routes the index through `coerce_to_i64` (zext) at every collection index site.
#[test]
fn u8_byte_index_into_vec_widens_to_i64() {
    let src = "fn main() {\n\
                   \x20   let b = \"AB\".bytes();\n\
                   \x20   let c0 = b[0i64];\n\
                   \x20   let c1 = b[1i64];\n\
                   \x20   let mut v: Vec[i64] = Vec.new();\n\
                   \x20   let mut i = 0i64;\n\
                   \x20   while i < 128i64 { v.push(0i64); i = i + 1i64; }\n\
                   \x20   v[c0] = 10i64;\n\
                   \x20   v[c1] = v[c0] + 5i64;\n\
                   \x20   println(f\"{v[c1]}\");\n\
                   }\n";
    assert_eq!(run_program(src).as_deref(), Some("15\n"));
}

/// B-2026-07-03-22: a generic `-> T` return whose `T` is bound from a
/// `Slice[T]` param's ELEMENT type (not a bare `x: T` param) must resolve
/// to the concrete element type, so the returned value is formatted/typed
/// correctly at the use site. Pre-fix, `subst` inference keyed the `Slice`
/// arm only on `slice_elem_types`, so a `Vec[String]` / `Array[String]`
/// arg left `T` UNBOUND — it defaulted to `i64`, and `gsum(vs)` returned
/// the String's 8-byte heap pointer (printed as a raw integer). The
/// i64-element cases in the sibling B-9 test masked this because an unbound
/// `T` defaults to `i64`, matching those elements by luck. This test uses a
/// non-`i64` element (String) whose return value is printed directly, plus
/// a non-first index and an `Array[String]` arg. Sequential harness
/// (analysis=None); the auto-par surface is covered in par_codegen.rs.
#[test]
fn e2e_generic_slice_elem_nonint_return() {
    if let Some(out) = run_program(
            "fn gsum[T](s: Slice[T]) -> T { s[0] }\n\
             fn gat[T](s: Slice[T], i: i64) -> T { s[i] }\n\
             fn main() {\n\
             \x20   let vs: Vec[String] = [\"first-payload-long-enough-string\", \"second-payload-also-long-here\"];\n\
             \x20   let vs2: Vec[String] = [\"first-payload-long-enough-string\", \"second-payload-also-long-here\"];\n\
             \x20   let arr: Array[String, 2] = [\"array-elem-zero-long-payload\", \"array-elem-one-long-payload\"];\n\
             \x20   println(f\"{gsum(vs)}\");\n\
             \x20   println(f\"{gat(vs2, 1)}\");\n\
             \x20   println(f\"{gsum(arr)}\");\n\
             }",
        ) {
            assert_eq!(
                out,
                "first-payload-long-enough-string\n\
                 second-payload-also-long-here\n\
                 array-elem-zero-long-payload\n"
            );
        }
}

/// Built-in `f64.sqrt()` — lowers to the `llvm.sqrt` intrinsic (a single
/// `f64.sqrt` on wasm, `sqrtsd` on x86; no libm). Added for the Plume
/// flow-field dogfood's velocity normalization (`examples/plume/`); the
/// first piece of a numeric math surface (sin/cos/atan2 remain a gap).
/// Output matches the interpreter (`16.sqrt()=4`, `2.sqrt()` to full
/// f64 precision).
#[test]
fn e2e_float_sqrt() {
    if let Some(out) = run_program(
        "fn main() {\n\
                 let a = (16.0).sqrt();\n\
                 let b = (2.0).sqrt();\n\
                 let h = (3.0 * 3.0 + 4.0 * 4.0).sqrt();\n\
                 println(f\"{a}\");\n\
                 println(f\"{b}\");\n\
                 println(f\"{h}\");\n\
             }",
    ) {
        assert_eq!(out, "4\n1.4142135623730951\n5\n");
    }
}

/// Scalar transcendental + rounding math (`crate::float_math`): unary
/// `sin`/`cos`/`tan`/`exp`/`ln`/`log2`/`floor`/`ceil`/`round` and binary
/// `pow`/`atan2`. Most lower to an LLVM intrinsic; `tan`/`atan2` to a libm
/// call (`llvm.tan`/`llvm.atan2` are LLVM-19+). Exact-result inputs so the
/// assertion is platform-independent and matches the interpreter twin
/// (`tests/interpreter.rs::test_float_math_transcendental_and_rounding`).
#[test]
fn e2e_float_math_transcendental() {
    if let Some(out) = run_program(
        "fn main() {\n\
                 println(f\"{(0.0f64).sin()}\");\n\
                 println(f\"{(0.0f64).cos()}\");\n\
                 println(f\"{(0.0f64).tan()}\");\n\
                 println(f\"{(0.0f64).exp()}\");\n\
                 println(f\"{(1.0f64).ln()}\");\n\
                 println(f\"{(1024.0f64).log2()}\");\n\
                 println(f\"{(2.0f64).pow(10.0f64)}\");\n\
                 println(f\"{(0.0f64).atan2(1.0f64)}\");\n\
                 println(f\"{(2.7f64).floor()}\");\n\
                 println(f\"{(2.2f64).ceil()}\");\n\
                 println(f\"{(2.5f64).round()}\");\n\
                 println(f\"{(-2.5f64).round()}\");\n\
             }",
    ) {
        assert_eq!(out, "0\n1\n0\n1\n0\n10\n1024\n0\n2\n3\n3\n-3\n");
    }
}

/// Second wave of the `crate::float_math` surface: inverse trig
/// (`asin`/`acos`/`atan`, direct libm calls), hyperbolics
/// (`sinh`/`cosh`/`tanh`, direct libm calls), and `exp2`/`log10`/`trunc`
/// (LLVM intrinsics). Exact-result inputs so the assertion is
/// platform-independent and matches the interpreter twin
/// (`tests/interpreter.rs::test_float_math_inverse_hyperbolic_and_extras`).
#[test]
fn e2e_float_math_inverse_hyperbolic_and_extras() {
    if let Some(out) = run_program(
        "fn main() {\n\
                 println(f\"{(0.0f64).asin()}\");\n\
                 println(f\"{(1.0f64).acos()}\");\n\
                 println(f\"{(0.0f64).atan()}\");\n\
                 println(f\"{(0.0f64).sinh()}\");\n\
                 println(f\"{(0.0f64).cosh()}\");\n\
                 println(f\"{(0.0f64).tanh()}\");\n\
                 println(f\"{(3.0f64).exp2()}\");\n\
                 println(f\"{(1000.0f64).log10()}\");\n\
                 println(f\"{(2.7f64).trunc()}\");\n\
                 println(f\"{(-2.7f64).trunc()}\");\n\
             }",
    ) {
        assert_eq!(out, "0\n0\n0\n0\n1\n0\n8\n3\n2\n-2\n");
    }
}

/// Third wave of `crate::float_math`: `hypot` (binary libm call) plus the
/// inverse hyperbolics (`asinh`/`acosh`/`atanh`) and `exp_m1`/`ln_1p`
/// (libm's `expm1`/`log1p`). Exact-result inputs so the assertion is
/// platform-independent and matches the interpreter twin
/// (`tests/interpreter.rs::test_float_math_hypot_inverse_hyperbolic_exp1p`).
#[test]
fn e2e_float_math_hypot_inverse_hyperbolic_exp1p() {
    if let Some(out) = run_program(
        "fn main() {\n\
                 println(f\"{(3.0f64).hypot(4.0f64)}\");\n\
                 println(f\"{(0.0f64).asinh()}\");\n\
                 println(f\"{(1.0f64).acosh()}\");\n\
                 println(f\"{(0.0f64).atanh()}\");\n\
                 println(f\"{(0.0f64).exp_m1()}\");\n\
                 println(f\"{(0.0f64).ln_1p()}\");\n\
                 println(f\"{(0.7f64).asinh()}\");\n\
             }",
    ) {
        assert_eq!(out, "5\n0\n0\n0\n0\n0\n0.6526665660823557\n");
    }
}

/// `f32` receivers lower through the same path — the LLVM intrinsics are
/// width-overloaded and the libm fallbacks pick the `f`-suffixed symbol
/// (`tanf`/`atan2f`). Exact-result inputs.
#[test]
fn e2e_float_math_f32() {
    if let Some(out) = run_program(
        "fn main() {\n\
                 let a: f32 = (0.0f32).cos();\n\
                 let b: f32 = (0.0f32).tan();\n\
                 let c: f32 = (2.0f32).pow(3.0f32);\n\
                 let d: f32 = (0.0f32).atan2(1.0f32);\n\
                 println(f\"{a}\"); println(f\"{b}\"); println(f\"{c}\"); println(f\"{d}\");\n\
             }",
    ) {
        assert_eq!(out, "1\n0\n8\n0\n");
    }
}

/// B-2026-08-29-61 — ONE BINARY MUST NOT RETURN TWO ANSWERS FOR ONE VALUE.
///
/// LLVM constant-folds a math call whose argument it can see through by
/// running the HOST's `double` implementation and rounding to the call's
/// type, while the call it cannot see through goes to the target's
/// width-correct symbol. Two roundings against one, so `karac build`
/// printed `(2.0f32).cosh()` as 3.762195587158203 from the fold and
/// 3.7621958255767822 from the call — in the same binary, which also
/// printed `lit == dyn` as true.
///
/// EVERY ROW HERE IS A MEASURED PRE-FIX DIVERGENCE, not a plausible one:
/// these 18 are the complete set found by sweeping all 27 `float_math`
/// methods over 20 receivers each at f32 (18 of 540 rows). Seven methods
/// are involved — the row that opened this bug named three, so a fixture
/// built from the report alone would have missed `atan2`, `tanh`, `atan`
/// and `asin`.
///
/// The assertion is that the two columns AGREE, never that either equals
/// a literal: the value is the host libm's and differs across platforms,
/// but a fold and a call on one machine must not.
///
/// The last four rows are the control that keeps the fix from being a
/// blanket suppression. `floor`/`ceil`/`round`/`trunc` (and `copysign`)
/// return a value exactly representable at the receiver's width, so the
/// wider computation and the narrow one agree and they KEEP the fold —
/// see `float_math::constant_fold_is_exact`. They agreed before this fix
/// and must still agree; that they are still folded rather than called was
/// confirmed on the disassembly (no `floorf`/`ceilf` in the binary).
///
/// `one` is derived from `env.args()` so the compiler cannot see through
/// it; the first line pins it to 1 so a run with arguments fails loudly
/// instead of silently comparing different receivers.
#[test]
fn e2e_compile_time_known_float_math_is_not_folded_at_double_precision() {
    if let Some(out) = run_program(
        r#"
fn main() {
    let n = env.args().len() as i64;
    let one: f32 = (n as f32);
    println(f"one {one}");
    let a1: f32 = 0.8f32;
    let b1: f32 = 0.8f32 * one;
    println(f"asin 0.8 {a1.asin()} {b1.asin()}");
    let a2: f32 = 0.3333333f32;
    let b2: f32 = 0.3333333f32 * one;
    println(f"atan 0.3333333 {a2.atan()} {b2.atan()}");
    let a3: f32 = 7.0f32;
    let b3: f32 = 7.0f32 * one;
    println(f"sinh 7.0 {a3.sinh()} {b3.sinh()}");
    let a4: f32 = 13.0f32;
    let b4: f32 = 13.0f32 * one;
    println(f"sinh 13.0 {a4.sinh()} {b4.sinh()}");
    let a5: f32 = 1.1f32;
    let b5: f32 = 1.1f32 * one;
    println(f"sinh 1.1 {a5.sinh()} {b5.sinh()}");
    let a6: f32 = 8.5f32;
    let b6: f32 = 8.5f32 * one;
    println(f"sinh 8.5 {a6.sinh()} {b6.sinh()}");
    let a7: f32 = 2.0f32;
    let b7: f32 = 2.0f32 * one;
    println(f"cosh 2.0 {a7.cosh()} {b7.cosh()}");
    let a8: f32 = 5.0f32;
    let b8: f32 = 5.0f32 * one;
    println(f"cosh 5.0 {a8.cosh()} {b8.cosh()}");
    let a9: f32 = 10.0f32;
    let b9: f32 = 10.0f32 * one;
    println(f"cosh 10.0 {a9.cosh()} {b9.cosh()}");
    let a10: f32 = 0.7f32;
    let b10: f32 = 0.7f32 * one;
    println(f"tanh 0.7 {a10.tanh()} {b10.tanh()}");
    let a11: f32 = 3.0f32;
    let b11: f32 = 3.0f32 * one;
    println(f"log10 3.0 {a11.log10()} {b11.log10()}");
    let a12: f32 = 0.7f32;
    let b12: f32 = 0.7f32 * one;
    println(f"log10 0.7 {a12.log10()} {b12.log10()}");
    let a13: f32 = 1.3f32;
    let b13: f32 = 1.3f32 * one;
    println(f"log10 1.3 {a13.log10()} {b13.log10()}");
    let a14: f32 = 0.9f32;
    let b14: f32 = 0.9f32 * one;
    println(f"log10 0.9 {a14.log10()} {b14.log10()}");
    let a15: f32 = 1.5f32;
    let b15: f32 = 1.5f32 * one;
    let c15: f32 = 1.3f32;
    let d15: f32 = 1.3f32 * one;
    println(f"atan2 1.5,1.3 {a15.atan2(c15)} {b15.atan2(d15)}");
    let a16: f32 = 3.0f32;
    let b16: f32 = 3.0f32 * one;
    let c16: f32 = 10.0f32;
    let d16: f32 = 10.0f32 * one;
    println(f"atan2 3.0,10.0 {a16.atan2(c16)} {b16.atan2(d16)}");
    let a17: f32 = 0.1f32;
    let b17: f32 = 0.1f32 * one;
    let c17: f32 = 11.0f32;
    let d17: f32 = 11.0f32 * one;
    println(f"atan2 0.1,11.0 {a17.atan2(c17)} {b17.atan2(d17)}");
    let a18: f32 = 0.7f32;
    let b18: f32 = 0.7f32 * one;
    let c18: f32 = 13.0f32;
    let d18: f32 = 13.0f32 * one;
    println(f"atan2 0.7,13.0 {a18.atan2(c18)} {b18.atan2(d18)}");
    let a19: f32 = 2.7f32;
    let b19: f32 = 2.7f32 * one;
    println(f"floor 2.7 {a19.floor()} {b19.floor()}");
    let a20: f32 = 2.7f32;
    let b20: f32 = 2.7f32 * one;
    println(f"ceil 2.7 {a20.ceil()} {b20.ceil()}");
    let a21: f32 = 2.5f32;
    let b21: f32 = 2.5f32 * one;
    println(f"round 2.5 {a21.round()} {b21.round()}");
    let a22: f32 = 2.7f32;
    let b22: f32 = 2.7f32 * one;
    println(f"trunc 2.7 {a22.trunc()} {b22.trunc()}");
}
"#,
    ) {
        let mut lines = out.lines();
        assert_eq!(
            lines.next(),
            Some("one 1"),
            "the runtime multiplier must be exactly 1, else the two \
                 columns are not the same receiver:\n{out}"
        );
        let mut checked = 0usize;
        for line in lines {
            let f: Vec<&str> = line.split_whitespace().collect();
            assert_eq!(f.len(), 4, "unexpected row shape: {line:?}");
            assert_eq!(
                f[2], f[3],
                "{} of {}: the compile-time-known receiver folded to {} but \
                     the runtime receiver computed {} — the same value has two \
                     answers in one binary",
                f[0], f[1], f[2], f[3]
            );
            checked += 1;
        }
        assert_eq!(checked, 22, "expected 22 comparison rows, got {checked}");
    }
}

/// Integer `.pow(exp)` codegen: repeated-multiply loop, `u32` exponent,
/// `pow(0) == 1`, width-correct on narrow receivers. Mirrors the interpreter
/// (`tests/interpreter.rs::test_int_pow_values_and_zero_exponent`).
#[test]
fn e2e_int_pow_values() {
    if let Some(out) = run_program(
        "fn main() {\n\
                 println(2i64.pow(10u32));\n\
                 println(3i64.pow(0u32));\n\
                 let e: u32 = 6;\n\
                 println(2i64.pow(e));\n\
                 let b: u64 = 1000000;\n\
                 println(b.pow(2u32));\n\
                 let x: u8 = 6;\n\
                 println(x.pow(3u32));\n\
             }",
    ) {
        assert_eq!(out, "1024\n1\n64\n1000000000000\n216\n");
    }
}

/// B-2026-08-05-21: the row-major converging scan whose index adds have
/// their overflow checks elided still computes the right addresses.
///
/// Rows are ODD-width so `lo` and `hi` meet on a middle cell, and the
/// buffer is exactly `n * len`, so `base + hi_init` on the last row must
/// land on the final element — the tight case where a mis-lowered index
/// add (the risk of swapping a checked add for a plain one) would show as
/// a wrong sum rather than a crash.
#[test]
fn e2e_proven_index_add_overflow_elision_addresses_are_unchanged() {
    let src = r#"
fn main() {
    let n = 7i64;
    let len = 5i64;
    let mut v: Vec[i64] = Vec.filled(n * len, 0i64);
    let mut i = 0i64;
    while i < n {
        let base = i * len;
        let mut lo = 0i64;
        let mut hi = len - 1i64;
        while lo <= hi {
            v[base + lo] = v[base + lo] + 1i64;
            v[base + hi] = v[base + hi] + 1i64;
            lo = lo + 1i64;
            hi = hi - 1i64;
        }
        i = i + 1i64;
    }
    let mut total = 0i64;
    let mut k = 0i64;
    while k < n * len {
        total = total + v[k];
        k = k + 1i64;
    }
    println(f"{total}");
}
"#;
    // Per row the pairs are (0,4), (1,3), (2,2): every cell gets +1 and the
    // middle cell gets +1 again, so 6 per row and 6 * 7 = 42.
    if let Some(out) = run_program(src) {
        assert_eq!(out, "42\n");
    }
}

/// The gate: an index add whose bounds are NOT proven keeps its overflow
/// trap. `base` is `i64::MAX` here, so `base + lo` genuinely overflows —
/// and because `v` carries no length pin that reaches this index, the
/// elision must decline and the program must trap rather than compute a
/// wrapped address.
#[test]
fn e2e_unproven_index_add_still_traps_on_overflow() {
    let src = r#"
fn main() {
    let mut v: Vec[i64] = Vec.new();
    v.push(1i64);
    v.push(2i64);
    let mut base = 9223372036854775807i64;
    if v.len() == 0i64 { base = 0i64; }
    let mut lo = 1i64;
    if v.len() == 99i64 { lo = 0i64; }
    println(f"{v[base + lo]}");
}
"#;
    if let Some(cap) = run_program_capturing(src) {
        assert_eq!(cap.status.code(), Some(101), "stderr={:?}", cap.stderr);
        assert!(
            cap.stderr.contains("integer overflow"),
            "expected the unproven index add to KEEP its overflow trap, \
                 got stdout={:?} stderr={:?}",
            cap.stdout,
            cap.stderr
        );
    }
}

/// `pow` traps `integer overflow` at the receiver width (same as `*`):
/// `u8 16^2 = 256` overflows u8 → exit 1, no silent widening to i64.
#[test]
fn e2e_int_pow_overflow_traps() {
    if let Some(cap) = run_program_capturing("fn main() { let x: u8 = 16; println(x.pow(2u32)); }")
    {
        assert_eq!(cap.status.code(), Some(101), "stderr={:?}", cap.stderr);
        // The AOT panic handler writes to stdout (`panic at … integer overflow`).
        assert!(
            cap.stderr.contains("integer overflow"),
            "expected integer-overflow trap, got stdout={:?} stderr={:?}",
            cap.stdout,
            cap.stderr
        );
    }
}

/// `i64::{div_euclid, rem_euclid}` codegen: the signed correction (lowered
/// via `emit_int_div_guards` + `select`s) matches the interpreter oracle
/// (`tests/interpreter.rs::test_i64_div_rem_euclid`) across all four sign
/// combinations.
#[test]
fn e2e_i64_div_rem_euclid() {
    if let Some(out) = run_program(
        "fn main() {\n\
                 println((-7i64).div_euclid(3i64));\n\
                 println((-7i64).rem_euclid(3i64));\n\
                 println((7i64).div_euclid(-3i64));\n\
                 println((7i64).rem_euclid(-3i64));\n\
                 println((-7i64).div_euclid(-3i64));\n\
                 println((-7i64).rem_euclid(-3i64));\n\
                 println((6i64).div_euclid(3i64));\n\
             }",
    ) {
        assert_eq!(out, "-3\n2\n-2\n1\n3\n2\n2\n");
    }
}

/// `div_euclid` shares `/`'s trap set — a zero divisor traps `division by
/// zero` (exit 1), matching the interpreter. Built via `let mut` so the
/// fault isn't const-folded away.
#[test]
fn e2e_i64_div_euclid_zero_traps() {
    if let Some(cap) = run_program_capturing(
        "fn main() { let mut z = 3i64; z = z - 3i64; println((5i64).div_euclid(z)); }",
    ) {
        assert_eq!(cap.status.code(), Some(101), "stderr={:?}", cap.stderr);
        assert!(
            cap.stderr.contains("division by zero"),
            "expected division-by-zero trap, got stdout={:?} stderr={:?}",
            cap.stdout,
            cap.stderr
        );
    }
}

/// B-2026-07-18-36 — a CHAINED width-sensitive int intrinsic
/// (`x.leading_zeros().leading_zeros()`). The parser aliases a chained
/// call's `MethodCall.span` to its receiver's span, so both calls collide
/// on one `method_callee_types` key; `receiver_int_kind` read the OUTER
/// call's recorded width (`u32`) for the INNER call and lowered
/// `1u8.leading_zeros()` as a 32-bit ctlz (→ 31, then the outer →27)
/// instead of the width-correct 8-bit (→7, outer →29). Fixed by preferring
/// the receiver's own resolved type over the span-keyed table. Oracle:
/// the interpreter (matched below by the `interp == codegen` invariant).
#[test]
fn e2e_chained_width_sensitive_int_intrinsic() {
    if let Some(out) = run_program(
        "fn main() {\n\
                 let x: u8 = 1;\n\
                 println(x.leading_zeros());\n\
                 println(x.leading_zeros().leading_zeros());\n\
                 let y: u8 = 200;\n\
                 println(y.rotate_left(1).count_ones());\n\
                 let z: u16 = 1;\n\
                 println(z.leading_zeros().leading_zeros());\n\
             }",
    ) {
        // x=1u8: lz=7 (8-bit); 7u32.lz=29. y=200u8 rol 1 = 145 → 3 ones.
        // z=1u16: lz=15; 15u32.lz=28.
        assert_eq!(out, "7\n29\n3\n28\n");
    }
}

/// `next_power_of_two` traps `integer overflow` when the result would exceed
/// the width (`u8 129` → 256): exit 1, matching the interpreter trap
/// (`test_next_power_of_two_overflow_traps`).
#[test]
fn e2e_next_power_of_two_overflow_traps() {
    if let Some(cap) =
        run_program_capturing("fn main() { let g: u8 = 129; println(g.next_power_of_two()); }")
    {
        assert_eq!(cap.status.code(), Some(101), "stderr={:?}", cap.stderr);
        assert!(
            cap.stderr.contains("integer overflow"),
            "expected integer-overflow trap, got stdout={:?} stderr={:?}",
            cap.stdout,
            cap.stderr
        );
    }
}

#[test]
fn e2e_unqualified_struct_variant_impl_display_round_trip() {
    // B-2026-06-13-7 (the GAP-W4 origin): a user `impl Display` whose
    // `to_string` matches struct variants with UNQUALIFIED patterns —
    // exactly `examples/weave`'s `ParseError` shape — must `karac build`
    // (it already worked under `karac run`).
    if let Some(out) = run_program(
        "enum ParseError { Unexpected { got: String }, OutOfRange { value: i64 } }\n\
             impl Display for ParseError {\n\
                 fn to_string(ref self) -> String {\n\
                     match self {\n\
                         Unexpected { got } => f\"unexpected: {got}\",\n\
                         OutOfRange { value } => f\"out of range: {value}\",\n\
                     }\n\
                 }\n\
             }\n\
             fn main() {\n\
                 let e1: ParseError = ParseError.Unexpected { got: \"tok\" };\n\
                 let e2: ParseError = ParseError.OutOfRange { value: 9 };\n\
                 println(f\"{e1}\");\n\
                 println(f\"{e2}\");\n\
             }",
    ) {
        assert_eq!(out, "unexpected: tok\nout of range: 9\n");
    }
}

#[test]
fn e2e_128bit_enum_payload_round_trip() {
    // A 128-bit scalar survives an ENUM PAYLOAD round trip
    // (B-2026-08-19-19). A payload word is 64 bits, so the pack side has to
    // split a 128-bit scalar into two little-endian words and the match-arm
    // unpack has to rejoin them. Before that, the single-word fast path kept
    // only the LOW half — and every value below is picked so that half is
    // ZERO or misleading: 2^100's low 64 bits are 0, so the bug printed `0`
    // rather than a visibly corrupt number.
    //
    // Covers all three payload producers: the seeded `Option` / `Result`,
    // a user enum (including a variant whose 128-bit field is followed by a
    // 64-bit one, so the word CURSOR has to advance by two), and
    // `checked_*`, whose `Option[Self]` is built inline via phis rather
    // than through the ordinary pack path.
    if let Some(out) = run_program(
        "enum Box128 { W(i128), Pair(i128, i64), Nothing }\n\
             fn main() {\n\
             let a: Option[i128] = Some(1267650600228229401496703205376i128);\n\
             match a { Some(x) => println(x), None => println(\"none-a\") }\n\
             let b: Option[i128] = Some(-1267650600228229401496703205376i128);\n\
             match b { Some(x) => println(x), None => println(\"none-b\") }\n\
             let c: Option[u128] = Some(170141183460469231731687303715884105727u128);\n\
             match c { Some(x) => println(x), None => println(\"none-c\") }\n\
             let d: Result[i128, String] = Ok(170141183460469231731687303715884105727i128);\n\
             match d { Ok(x) => println(x), Err(e) => println(e) }\n\
             let f: Box128 = Box128.W(-170141183460469231731687303715884105727i128);\n\
             match f {\n\
             Box128.W(x) => println(x),\n\
             Box128.Pair(p, q) => println(p),\n\
             Box128.Nothing => println(\"nb\"),\n\
             }\n\
             let g: Box128 = Box128.Pair(1267650600228229401496703205376i128, 42i64);\n\
             match g {\n\
             Box128.W(x) => println(x),\n\
             Box128.Pair(p, q) => { println(p) println(q) }\n\
             Box128.Nothing => println(\"nb\"),\n\
             }\n\
             let h: Option[i128] = Some(1267650600228229401496703205376i128);\n\
             match h { Some(x) => println(x + 1i128), None => println(\"none-h\") }\n\
             let i: Option[i128] = None;\n\
             match i { Some(x) => println(x), None => println(\"none-i\") }\n\
             let mx: i128 = 170141183460469231731687303715884105727i128;\n\
             match mx.checked_add(1i128) { Some(v) => println(v), None => println(\"ovf\") }\n\
             let m: i128 = 1267650600228229401496703205376i128;\n\
             match m.checked_mul(2i128) { Some(v) => println(v), None => println(\"ovf\") }\n\
             let uu: u128 = 1267650600228229401496703205376u128;\n\
             match uu.checked_mul(2u128) { Some(v) => println(v), None => println(\"ovf\") }\n\
             }",
    ) {
        assert_eq!(
            out,
            "1267650600228229401496703205376\n\
                 -1267650600228229401496703205376\n\
                 170141183460469231731687303715884105727\n\
                 170141183460469231731687303715884105727\n\
                 -170141183460469231731687303715884105727\n\
                 1267650600228229401496703205376\n\
                 42\n\
                 1267650600228229401496703205377\n\
                 none-i\n\
                 ovf\n\
                 2535301200456458802993406410752\n\
                 2535301200456458802993406410752\n"
        );
    }
}

#[test]
fn e2e_128bit_saturating_clamps_toward_the_result_sign() {
    // `saturating_*` at 128 bits clamps toward the sign of the TRUE result
    // (B-2026-08-19-19). Two independent bugs met here, one per backend:
    // codegen computed the bounds as `((1u128 << (bits - 1)) - 1) as u64`,
    // whose cast TRUNCATES — at 128 bits SMAX came out `u64::MAX` and SMIN
    // came out `0`; the interpreter clamped by OPERATION (`sub` → MIN,
    // else MAX), which sends an overflowing NEGATIVE product to `MAX`.
    //
    // The negative-product case is what separates the two rules, so it is
    // first. The narrow widths follow because the codegen fix changed how
    // the bounds are built at EVERY width, not just 128.
    if let Some(out) = run_program(
        "fn main() {\n\
             let neg: i128 = -1267650600228229401496703205376i128;\n\
             println(neg.saturating_mul(1000000000000i128));\n\
             let mx: i128 = 170141183460469231731687303715884105727i128;\n\
             println(mx.saturating_add(1i128));\n\
             println(neg.saturating_sub(mx));\n\
             println((127i8).saturating_add(1i8));\n\
             println((-128i8).saturating_mul(2i8));\n\
             println((-2147483648i32).saturating_mul(3i32));\n\
             println((250u8).saturating_add(20u8));\n\
             println((3u8).saturating_sub(10u8));\n\
             let um: u128 = 1267650600228229401496703205376u128;\n\
             println(um.saturating_sub(1u128));\n\
             }",
    ) {
        assert_eq!(
            out,
            "-170141183460469231731687303715884105728\n\
                 170141183460469231731687303715884105727\n\
                 -170141183460469231731687303715884105728\n\
                 127\n\
                 -128\n\
                 -2147483648\n\
                 255\n\
                 0\n\
                 1267650600228229401496703205375\n"
        );
    }
}

#[test]
fn e2e_shift_runs_at_the_values_width_not_the_amounts() {
    // B-2026-08-19-26 — a SILENT MISCOMPILE on ordinary 64-bit code, found
    // while measuring 128-bit shifts.
    //
    // `compile_binop_typed` harmonizes a mixed-width int pair by truncating
    // the WIDER side, on the reasoning that such a pair is always
    // "narrow-typed operand x default-i64 literal". A shift is not that
    // shape: its amount is a `u32` whatever the value's width is, so the
    // rule truncated the VALUE to 32 bits. `let a: i64 = 2^40; a << 1u32`
    // compiled to `shl i32 0, 1` and printed 0, while `karac run` printed
    // 2199023255552 — no panic, no diagnostic, just a wrong answer. With an
    // amount >= 32 it surfaced instead as a bogus "shift amount out of
    // range" panic, because the width check then compared against 32.
    //
    // The SAME asymmetry cost the shift its signedness, one layer up: the
    // raw-`Binary` path asked "is either operand unsigned?", which is right
    // for `+`/`*`/`<` and wrong for a shift, so any shift by a `u32` amount
    // became a LOGICAL shift. `(0i32 - 8i32) >> 1u32` compiled to
    // 2147483644 against the interpreter's -4. The last two rows cover it.
    //
    // Every other value below is chosen so a 32-bit truncation is visible:
    // each is a power of two at or above 2^40, whose low 32 bits are zero.
    if let Some(out) = run_program(
        "fn main() {\n\
             let a: i64 = 1099511627776i64;\n\
             println(a << 1u32);\n\
             println(a >> 1u32);\n\
             let n: u32 = 1u32;\n\
             println(a << n);\n\
             let b: i64 = 1i64;\n\
             println(b << 40u32);\n\
             let c: u64 = 18446744073709551615u64;\n\
             println(c >> 32u32);\n\
             let d: i64 = 0i64 - 8i64;\n\
             println(d >> 1u32);\n\
             let e: i32 = 0i32 - 8i32;\n\
             println(e >> 1u32);\n\
             }",
    ) {
        assert_eq!(
            out,
            "2199023255552\n\
                 549755813888\n\
                 2199023255552\n\
                 1099511627776\n\
                 4294967295\n\
                 -4\n\
                 -4\n"
        );
    }
}

#[test]
fn e2e_128bit_shift_amounts_reach_the_full_width() {
    // A 128-bit shift may move by up to 127 (B-2026-08-19-23). The
    // interpreter's `span_int_width` had no 128-bit arms, so it answered
    // "signed 64" and rejected any amount >= 64 as out of range; codegen
    // rejected the same shifts for the separate reason above. `1i128 << 100`
    // is unrepresentable in 64 bits, so a 64-bit answer cannot be right by
    // accident.
    if let Some(out) = run_program(
        "fn main() {\n\
             let a: i128 = 1i128;\n\
             println(a << 100u32);\n\
             println(a << 127u32);\n\
             let b: i128 = 1267650600228229401496703205376i128;\n\
             println(b >> 100u32);\n\
             let m: u128 = 340282366920938463463374607431768211455u128;\n\
             println(m >> 100u32);\n\
             println(m >> 127u32);\n\
             }",
    ) {
        assert_eq!(
            out,
            "1267650600228229401496703205376\n\
                 -170141183460469231731687303715884105728\n\
                 1\n\
                 268435455\n\
                 1\n"
        );
    }
}

#[test]
fn e2e_upper_half_of_u128_end_to_end() {
    // The top half of `u128` — every value past `i128::MAX`
    // (B-2026-08-19-23). It was unwritable as a literal (the parser had no
    // room for it in the positive-magnitude path) and, once reachable by
    // arithmetic, printed with its signed reading under `karac run`.
    //
    // The literals here are all above `i128::MAX`, so a signed reading
    // flips the SIGN rather than merely losing precision — `u128::MAX`
    // reads as `-1`. The comparison and division rows are the ones that
    // silently returned a wrong ANSWER rather than a wrong rendering.
    if let Some(out) = run_program(
        "fn main() {\n\
             let m: u128 = 340282366920938463463374607431768211455u128;\n\
             let h: u128 = 200000000000000000000000000000000000000u128;\n\
             println(m);\n\
             println(f\"{m}\");\n\
             println(h > 5u128);\n\
             println(h < 5u128);\n\
             println(m >= h);\n\
             println(h / 3u128);\n\
             println(h % 7u128);\n\
             println(m / 2u128);\n\
             println(h - 5u128);\n\
             println(m.count_ones());\n\
             println(m.wrapping_add(1u128));\n\
             println(m.saturating_add(1u128));\n\
             let o: Option[u128] = Some(m);\n\
             match o { Some(v) => println(v), None => println(\"none\") }\n\
             println(m as u64);\n\
             let n: i128 = -170141183460469231731687303715884105728i128;\n\
             println(n);\n\
             }",
    ) {
        assert_eq!(
            out,
            "340282366920938463463374607431768211455\n\
                 340282366920938463463374607431768211455\n\
                 true\n\
                 false\n\
                 true\n\
                 66666666666666666666666666666666666666\n\
                 4\n\
                 170141183460469231731687303715884105727\n\
                 199999999999999999999999999999999999995\n\
                 128\n\
                 0\n\
                 340282366920938463463374607431768211455\n\
                 340282366920938463463374607431768211455\n\
                 18446744073709551615\n\
                 -170141183460469231731687303715884105728\n"
        );
    }
}

#[test]
fn e2e_chained_width_sensitive_int_methods() {
    // CHAINED width-preserving int methods resolve their receiver width
    // through the receiver itself, not the span-keyed table
    // (B-2026-08-19-8 stage 3b).
    //
    // The parser aliases a chain's `MethodCall.span` to its receiver's, so
    // `method_callee_types` holds ONE entry for the whole chain
    // (B-2026-07-18-36) and the outer call used to fall through to
    // `receiver_int_kind`'s 64-bit default. That default was invisible
    // while every carrier was 64 bits wide, and wrong the moment one was
    // not — an `i128` byte-swap chain round-tripped through a 64-bit swap
    // and lost the top half while the same expression unchained was
    // correct. `type_name_of_expr` now resolves a Self-returning int method
    // to its receiver's type.
    //
    // Pinned at the NARROW widths because that is what this change puts at
    // risk: 128-bit cannot be spelled until stage 5 lifts the type
    // rejection, but every existing width now takes the new resolution
    // path, and a mistake there would be a live miscompile today.
    if let Some(out) = run_program(
        "fn main() {\n\
             let a: i32 = 305419896i32;\n\
             println(a.swap_bytes().swap_bytes());\n\
             let b: i16 = 4660i16;\n\
             println(b.swap_bytes().swap_bytes());\n\
             let c: i32 = 1i32;\n\
             println(c.rotate_left(8u32).rotate_right(8u32));\n\
             let d: i64 = 7i64;\n\
             println(d.wrapping_add(1i64).wrapping_sub(1i64));\n\
             let e: u32 = 4294967295u32;\n\
             println(e.reverse_bits().reverse_bits());\n\
             }",
    ) {
        assert_eq!(out, "305419896\n4660\n1\n7\n4294967295\n");
    }
}

/// B-2026-08-14-11 — the compiled twin of
/// `tests/interpreter.rs::test_unsuffixed_float_literal_takes_the_destination_width`,
/// same source and same expected string.
///
/// Codegen already narrowed a bare `0.1` at a struct field, a fn argument,
/// a return, a `Vec` element, an `Array` element and a tuple element — the
/// ANNOTATED `let` was the one position it did not, because
/// `const_float_for_suffix` reads the suffix only and nothing at the
/// binding re-typed the literal. So this pins the position that was wrong
/// AND the six that were right, since the whole failure was one position
/// disagreeing with its siblings and with the interpreter.
///
/// B-2026-08-31-20 added six more positions, and on those THIS side was
/// already correct — codegen narrowed an `Option`/`Result` payload and a
/// `Vec`/`Array` literal element all along, while the interpreter kept f64
/// precision. So for the new lines this twin is the ORACLE rather than the
/// regression witness, exactly as it was the other way round for the
/// annotated `let` above. Both directions in one fixture is the point: the
/// pair pins the AGREEMENT, not either backend's behaviour.
#[test]
fn test_e2e_unsuffixed_float_literal_takes_the_destination_width() {
    assert_eq!(
        run_program(
            "struct Bx { f: f32 }\n\
                 fn takef(x: f32) -> f32 { x }\n\
                 fn retf() -> f32 { 0.1 }\n\
                 fn main() {\n\
                     let la: f32 = 0.1;\n\
                     println(la);\n\
                     let sl = Bx { f: 0.1 };\n\
                     println(sl.f);\n\
                     println(takef(0.1));\n\
                     println(retf());\n\
                     let mut v: Vec[f32] = Vec.new();\n\
                     v.push(0.1);\n\
                     println(v[0]);\n\
                     let ar: Array[f32, 1] = [0.1];\n\
                     println(ar[0]);\n\
                     let tu: (f32, f32) = (0.1, 0.2);\n\
                     println(tu.0);\n\
                     let po: Option[f32] = Option.Some(0.1);\n\
                     println(po);\n\
                     let pb: Option[f32] = Some(0.1);\n\
                     println(pb);\n\
                     let pk: Result[f32, i64] = Ok(0.1);\n\
                     println(pk);\n\
                     let pe: Result[i64, f32] = Err(0.1);\n\
                     println(pe);\n\
                     let vl: Vec[f32] = [0.1];\n\
                     println(vl[0]);\n\
                     let ap: Array[f32, 1] = Array[0.1];\n\
                     println(ap[0]);\n\
                     let h: f16 = 0.1;\n\
                     println(h);\n\
                 }"
        )
        .as_deref(),
        Some(
            "0.10000000149011612\n\
                 0.10000000149011612\n\
                 0.10000000149011612\n\
                 0.10000000149011612\n\
                 0.10000000149011612\n\
                 0.10000000149011612\n\
                 0.10000000149011612\n\
                 Some(0.10000000149011612)\n\
                 Some(0.10000000149011612)\n\
                 Ok(0.10000000149011612)\n\
                 Err(0.10000000149011612)\n\
                 0.10000000149011612\n\
                 0.10000000149011612\n\
                 0.0999755859375\n"
        ),
    );
}

/// B-2026-08-14-13 — the compiled twin of
/// `tests/interpreter.rs::test_mixed_float_arithmetic_as_cast_computes_at_the_stated_width`,
/// same source and same expected string.
///
/// An OVER-REACH GUARD, not a regression witness — every line here is
/// explicitly cast, so both operands already matched and this source
/// compiled to these values before the gate landed. It is here because the
/// gate's whole remedy is the `as`, and the two directions it offers give
/// GENUINELY DIFFERENT answers: 1.2100000262260437 at f64 against
/// 1.2100000381469727 at f32. That difference is what the old implicit
/// behaviour was silently choosing by operand order, so a reader needs to
/// see that the choice is real, that the source now makes it, and that both
/// backends agree on each. The rejection of the UNCAST spelling is asserted
/// in `tests/typechecker.rs::mixed_width_float_arithmetic_is_rejected`.
#[test]
fn test_e2e_mixed_float_arithmetic_as_cast_computes_at_the_stated_width() {
    assert_eq!(
        run_program(
            "fn main() {\n\
                     let a: f32 = 1.1f32;\n\
                     let b: f64 = 1.1;\n\
                     let wide: f64 = (a as f64) * b;\n\
                     println(wide);\n\
                     let narrow: f32 = a * (b as f32);\n\
                     println(narrow);\n\
                     let h: f16 = 1.5f16;\n\
                     let up: f32 = (h as f32) * a;\n\
                     println(up);\n\
                 }"
        )
        .as_deref(),
        Some("1.2100000262260437\n1.2100000381469727\n1.6500000953674316\n"),
    );
}

#[test]
fn test_e2e_int_to_float_widening_reaches_container_and_probe() {
    assert_eq!(
        run_program(
            "struct H { mut f: f64 }\n\
                 fn main() {\n\
                     let v = 200u8;\n\
                     let n = -5i8;\n\
                     let mut vc: Vec[f64] = Vec.new();\n\
                     vc.push(v);\n\
                     println(vc.contains(200.0));\n\
                     println(vc.contains(v));\n\
                     let mut vd: Vec[f64] = Vec.new();\n\
                     vd.push(200.0);\n\
                     println(vd.contains(v));\n\
                     let mut ve: Vec[f64] = Vec.new();\n\
                     ve.push(0.0);\n\
                     ve[0i64] = v;\n\
                     println(ve.contains(200.0));\n\
                     let mut arr: Array[f64, 2] = [0.0, 0.0];\n\
                     arr[0i64] = v;\n\
                     println(arr[0i64]);\n\
                     let mut x: f64 = 0.0;\n\
                     x = v;\n\
                     println(x == 200.0);\n\
                     let mut vn: Vec[f64] = Vec.new();\n\
                     vn.push(n);\n\
                     println(vn.contains(-5.0));\n\
                     let mut y: f64 = 0.0;\n\
                     y = n;\n\
                     println(y == -5.0);\n\
                     let mut vf: Vec[f64] = Vec.new();\n\
                     vf.push(1.5);\n\
                     println(vf.contains(1.5));\n\
                     let h = H { f: 0.0 };\n\
                     println(h.f == 0.0);\n\
                 }"
        )
        .as_deref(),
        Some("true\ntrue\ntrue\ntrue\n200\ntrue\ntrue\ntrue\ntrue\ntrue\n"),
    );
}

#[test]
fn test_e2e_implicit_int_to_float_widening_at_every_boundary() {
    // B-2026-08-13-18. The typechecker admits int→float as an implicit
    // widening, so a legal program reaches these boundaries with an `iN`
    // bound for a `double`. Two distinct failure modes were in play, which
    // is why the row's table found only one of them:
    //
    //   * boundaries that route through `coerce_scalar_to_type` FAILED
    //     MODULE VERIFICATION — `Invalid InsertValueInst operands!` at a
    //     struct-literal field and a field assignment, `Call parameter type
    //     does not match function signature!` at a call argument. Loud.
    //   * boundaries that pack into i64 words — an ENUM PAYLOAD, a MAP
    //     VALUE — reinterpreted the integer's bits as a double and printed
    //     a denormal near zero. Silent, no verifier complaint, and absent
    //     from the row's table for exactly that reason.
    //
    // The signedness half is asserted, not incidental: every `200u8` here
    // must print 200, and `sitofp` on it yields -56. The two `-5i8` lines
    // at the end are the control that the unsigned leg did not simply
    // replace one wrong extension with another.
    //
    // `m.setn(b)` is the int→int method argument. It is in this fixture
    // because the missing method-boundary coercion this row had to add is
    // the same call for both classes: B-2026-08-13-15 fixed the free-fn
    // spelling and never reached the method one, so `200u8` into
    // `fn setn(mut ref self, x: i64)` printed -56 with no float involved.
    assert_eq!(
        run_program(
            "struct W { mut a: i64, mut f: f64 }\n\
                 enum E { F(f64) }\n\
                 struct M { mut f: f64, mut n: i64 }\n\
                 impl M {\n\
                     fn setf(mut ref self, x: f64) { self.f = x; }\n\
                     fn setn(mut ref self, x: i64) { self.n = x; }\n\
                 }\n\
                 fn takesf(x: f64) -> f64 { x }\n\
                 fn retf(b: u8) -> f64 { return b; }\n\
                 fn main() {\n\
                     let b: u8 = 200u8;\n\
                     let s: i8 = -5i8;\n\
                     let w = W { a: 0i64, f: b };\n\
                     println(w.f);\n\
                     println(takesf(b));\n\
                     println(retf(b));\n\
                     let mut q = W { a: 0i64, f: 0.0 };\n\
                     q.f = b;\n\
                     println(q.f);\n\
                     let mut m = M { f: 0.0, n: 0 };\n\
                     m.setf(b);\n\
                     m.setn(b);\n\
                     println(m.f);\n\
                     println(m.n);\n\
                     let e = E.F(b);\n\
                     match e { F(x) => { println(x); } }\n\
                     let mut mp: Map[i64, f64] = Map.new();\n\
                     let _ = mp.insert(1, b);\n\
                     mp[2] = b;\n\
                     match mp.get(1) { Some(x) => { println(x); } None => { println(0.0); } }\n\
                     match mp.get(2) { Some(x) => { println(x); } None => { println(0.0); } }\n\
                     println(takesf(s));\n\
                     let ws = W { a: 0i64, f: s };\n\
                     println(ws.f);\n\
                 }"
        )
        .as_deref(),
        Some("200\n200\n200\n200\n200\n200\n200\n200\n200\n-5\n-5\n"),
    );
}

/// B-2026-08-29-24 — the OTHER wrap kinds, and the MIXED wraps that a
/// whole-walker suppression could not touch.
///
/// B-2026-08-29-19 fixed the variant constructor and nothing else: a struct
/// literal, a tuple literal and `Some(..)` doubled a param view's `Drop`
/// body exactly the same way, and a mixed wrap of any kind doubled it while
/// the fresh payload beside it needed to keep its own. Three mechanisms
/// close that, each the maskable form of a walker that used to be
/// all-or-nothing: `emit_enum_payload_user_drop_bodies_fn_skipping` (new
/// here), `emit_user_drop_field_bodies_fn_skipping` and
/// `emit_tuple_elem_user_drop_bodies_fn_skipping` (both already existed for
/// move-outs).
///
/// Every case is a body COUNT, not a memory fact — ASAN/LSan were clean on
/// these shapes before the fix as well as after
/// (`asan_wrapped_param_view_payload_frees_once`) — and every one agreed
/// across all three backends while wrong, so nothing but an absolute
/// expectation could have caught them.
/// B-2026-08-30-56 — an integer reaching a FLOAT enum payload is CONVERTED,
/// not bit-reinterpreted.
///
/// `coerce_to_payload_words` bitcasts whatever it is handed into i64 slots,
/// so a payload arriving in the wrong class was a silent wrong value rather
/// than a verifier error: `let o: Option[f64] = Some(m)` with `m: i64` read
/// back as a subnormal near 4.45e-309 — the integer's own bits as a double.
///
/// 9007199254740993 is 2^53+1, the conversion probe: it survives as itself
/// if no conversion happens and lands on ...992 once it round-trips through
/// a double. Every case here therefore asserts ...992 — the CONVERTED value
/// — and would read ...993 if the store silently skipped `sitofp`, or a
/// denormal if it bitcast.
///
/// Three paths were missing the coercion for three different reasons, which
/// is why the case list separates them: a GENERIC payload (`Option`/`Result`
/// declare theirs as `T`, so the existing skip for type parameters covered
/// every `Some`/`Ok` in the language), an enum STRUCT-variant (never called
/// the coercion at all), and the QUALIFIED spelling of a generic user enum
/// (the span that carries the instantiation reached only the bare form).
/// `T.W(f64)`, a plain struct field and a `Vec[f64]` literal were already
/// correct and are kept as CONTROLS — they are what localized each gap.
#[test]
fn e2e_int_into_float_enum_payload_converts() {
    let hdr = "enum T { W(f64) }\n\
                   enum S { V { f: f64 } }\n\
                   enum G[X] { A(X), B }\n\
                   struct Plain { f: f64 }\n";
    for (label, src, want) in [
            (
                "option-generic-payload",
                format!(
                    "{hdr}fn main() {{\n\
                     \x20   let m: i64 = 9007199254740993;\n\
                     \x20   let o: Option[f64] = Some(m);\n\
                     \x20   match o {{ Some(x) => println(f\"{{x}}\"), None => println(\"none\") }}\n\
                     }}"
                ),
                "9007199254740992\n",
            ),
            (
                "result-generic-payload",
                format!(
                    "{hdr}fn main() {{\n\
                     \x20   let m: i64 = 9007199254740993;\n\
                     \x20   let r: Result[f64, i64] = Ok(m);\n\
                     \x20   match r {{ Ok(x) => println(f\"{{x}}\"), Err(_) => println(\"err\") }}\n\
                     }}"
                ),
                "9007199254740992\n",
            ),
            (
                "enum-struct-variant-field",
                format!(
                    "{hdr}fn main() {{\n\
                     \x20   let m: i64 = 9007199254740993;\n\
                     \x20   let s = S.V {{ f: m }};\n\
                     \x20   match s {{ S.V {{ f }} => println(f\"{{f}}\") }}\n\
                     }}"
                ),
                "9007199254740992\n",
            ),
            (
                "user-generic-enum-qualified",
                format!(
                    "{hdr}fn main() {{\n\
                     \x20   let m: i64 = 9007199254740993;\n\
                     \x20   let g: G[f64] = G.A(m);\n\
                     \x20   match g {{ G.A(x) => println(f\"{{x}}\"), G.B => println(\"b\") }}\n\
                     }}"
                ),
                // The QUALIFIED spelling. Its bare sibling below was fixed by
                // the same threading one call site earlier, and the two
                // disagreeing is what showed this was a plumbing gap rather
                // than a missing conversion.
                "9007199254740992\n",
            ),
            (
                "user-generic-enum-bare",
                format!(
                    "{hdr}fn main() {{\n\
                     \x20   let m: i64 = 9007199254740993;\n\
                     \x20   let g: G[f64] = A(m);\n\
                     \x20   match g {{ G.A(x) => println(f\"{{x}}\"), G.B => println(\"b\") }}\n\
                     }}"
                ),
                "9007199254740992\n",
            ),
            (
                "concrete-tuple-variant-control",
                format!(
                    "{hdr}fn main() {{\n\
                     \x20   let m: i64 = 9007199254740993;\n\
                     \x20   let t = T.W(m);\n\
                     \x20   match t {{ T.W(x) => println(f\"{{x}}\") }}\n\
                     }}"
                ),
                // CONTROL: correct since B-2026-08-13-18, and the row's own
                // localizer — structurally the same construction as `Some(m)`,
                // differing only in that its payload type is declared
                // concretely.
                "9007199254740992\n",
            ),
            (
                "plain-struct-field-control",
                format!(
                    "{hdr}fn main() {{\n\
                     \x20   let m: i64 = 9007199254740993;\n\
                     \x20   let p = Plain {{ f: m }};\n\
                     \x20   println(f\"{{p.f}}\");\n\
                     }}"
                ),
                "9007199254740992\n",
            ),
            (
                "vec-literal-element-control",
                format!(
                    "{hdr}fn main() {{\n\
                     \x20   let m: i64 = 9007199254740993;\n\
                     \x20   let v: Vec[f64] = [m];\n\
                     \x20   println(f\"{{v[0]}}\");\n\
                     }}"
                ),
                "9007199254740992\n",
            ),
        ] {
            assert_eq!(run_program(&src).as_deref(), Some(want), "case {label}");
        }
}

#[test]
fn e2e_int_into_float_enum_payload_signedness_width_and_passthrough() {
    // B-2026-08-30-56 companion to `e2e_int_into_float_enum_payload_converts`
    // above, which fixed the defect. That test covers WHICH CONSTRUCTION
    // PATHS reach the coercion; this one covers WHAT THE COERCION DOES once
    // reached, on four axes it does not exercise. Written after that fix
    // landed, so it is coverage rather than a regression test for it — every
    // case below passes on the commit that closed the row.
    //
    //  * SIGNEDNESS. Every case there sources from `i64`, where `sitofp` and
    //    `uitofp` agree. `u64::MAX` is where they do not: 1.8446744e19 under
    //    `uitofp` against -1 under `sitofp`. Pre-fix this shape printed NaN
    //    (the bit pattern as a double), so it was never a passing case whose
    //    signedness happened to be right — it is newly covered.
    //  * TARGET WIDTH. All eight of those are `f64`. `Option[f32]` proves the
    //    conversion follows the payload's declared width and not a fixed
    //    double.
    //  * THE B-2026-08-19-19 GUARD. That row's whole point is that coercing a
    //    generic payload to the all-i64 base TRUNCATES `Option[i128]` to its
    //    low word, printing 0. The fix rests on resolving `T` to the real type
    //    instead, and the surrounding prose says so — but no case pinned it.
    //    This is that row's exact repro; it fails on any future "fix" that
    //    reaches the generic skip by deleting it.
    //  * NON-SCALAR AND ABSENT PAYLOADS. `coerce_enum_payload_scalar` returns
    //    early unless the value is an int or a float, and a `None` has no
    //    payload word at all. Both must pass through the newly-reached call
    //    untouched; a `String` payload silently coerced would be a pointer
    //    reinterpreted as a number.
    //
    // `enum-struct-variant-narrow` is the same-class control for the
    // struct-variant path that fix newly routed through the coercion: `u8`
    // into a `u8` field must stay 200, not be re-widened or re-narrowed.
    let cases: &[(&str, &str, &str)] = &[
            (
                "option-unsigned-src",
                "fn main() { let u: u64 = 18446744073709551615u64; let o: Option[f64] = Some(u); match o { Some(x) => println(x), None => println(-1.0) } }",
                "18446744073709552000\n",
            ),
            (
                "option-f32-payload",
                "fn main() { let n: i64 = 7; let o: Option[f32] = Some(n); match o { Some(x) => println(x), None => println(-1.0) } }",
                "7\n",
            ),
            (
                "control-option-i128",
                "fn main() { let big: i128 = 1267650600228229401496703205376i128; let o: Option[i128] = Some(big); match o { Some(x) => println(x), None => println(-1) } }",
                "1267650600228229401496703205376\n",
            ),
            (
                "control-option-string",
                "fn main() { let s: Option[String] = Some(\"hi\"); match s { Some(x) => println(x), None => println(\"no\") } }",
                "hi\n",
            ),
            (
                "control-option-none",
                "fn main() { let n: Option[f64] = None; match n { Some(x) => println(x), None => println(\"none\") } }",
                "none\n",
            ),
            (
                "enum-struct-variant-narrow",
                "enum N { V { b: u8 } }\n\
                 fn main() { match N.V { b: 200 } { V { b } => println(b) } }",
                "200\n",
            ),
        ];
    for (label, src, want) in cases {
        assert_eq!(run_program(src).as_deref(), Some(*want), "{label}");
    }
}

/// B-2026-08-30-48's compiled twin — the ORACLE half.
///
/// That row is an interpreter defect: an int reaching a float slot through
/// an aggregate or a variant payload converted as SIGNED (a tuple / array
/// literal has no integer type of its own, so the source width lookup
/// answered `None`), or did not convert at all. Both compiled backends were
/// already correct at every shape, which is what made them the oracle, and
/// `interp_int_reaching_a_float_slot_through_an_aggregate_converts` in
/// `tests/interpreter.rs` asserts exactly the values below.
///
/// Pinning them here is the point: without it a codegen regression would
/// silently redefine the reference the interpreter test is measured against,
/// and the pair would agree on a wrong answer. `u64::MAX` separates `uitofp`
/// (18446744073709552000) from `sitofp` (-1); 2^53+1 separates a real
/// conversion (...992) from a skipped one (...993).
#[test]
fn e2e_int_into_float_through_an_aggregate_is_the_interpreter_oracle() {
    let cases: &[(&str, &str, &str)] = &[
        (
            "tuple-u64",
            "fn main() { let u: u64 = 18446744073709551615u64;\n\
                 let t: (f64, i64) = (u, 1); println(t.0); }",
            "18446744073709552000\n",
        ),
        (
            "tuple-mixed-signs",
            "fn main() { let u: u64 = 18446744073709551615u64; let s: i64 = -3;\n\
                 let t: (f64, f64) = (u, s); println(f\"{t.0} {t.1}\"); }",
            "18446744073709552000 -3\n",
        ),
        (
            "array-u64",
            "fn main() { let u: u64 = 18446744073709551615u64;\n\
                 let a: Array[f64, 2] = [u, 1.0]; println(a[0]); }",
            "18446744073709552000\n",
        ),
        (
            "option-payload-u64",
            "fn main() { let u: u64 = 18446744073709551615u64;\n\
                 let o: Option[f64] = Some(u);\n\
                 match o { Some(x) => println(x), None => println(-1.0) } }",
            "18446744073709552000\n",
        ),
        (
            "tuple-i64-2p53",
            "fn main() { let m: i64 = 9007199254740993;\n\
                 let t: (f64, i64) = (m, 1); println(t.0); }",
            "9007199254740992\n",
        ),
        (
            "array-i64-2p53",
            "fn main() { let m: i64 = 9007199254740993;\n\
                 let a: Array[f64, 2] = [m, 1.0]; println(a[0]); }",
            "9007199254740992\n",
        ),
        (
            "control-tuple-all-float",
            "fn main() { let t: (f64, f64) = (1.5, 2.5); println(f\"{t.0} {t.1}\"); }",
            "1.5 2.5\n",
        ),
        (
            "control-vec-i64",
            "fn main() { let v: Vec[i64] = [7]; println(v[0]); }",
            "7\n",
        ),
        (
            "map-insert-2p53",
            "fn main() { let m: i64 = 9007199254740993;\n\
                 let mut mp: Map[i64, f64] = Map.new(); mp.insert(1, m);\n\
                 match mp.get(1) { Some(x) => println(x), None => println(-1.0) } }",
            "9007199254740992\n",
        ),
        (
            "map-insert-u64",
            "fn main() { let u: u64 = 18446744073709551615u64;\n\
                 let mut mp: Map[i64, f64] = Map.new(); mp.insert(1, u);\n\
                 match mp.get(1) { Some(x) => println(x), None => println(-1.0) } }",
            "18446744073709552000\n",
        ),
        (
            "map-insert-f32",
            "fn main() { let m: u32 = 4294967295u32;\n\
                 let mut mp: Map[i64, f32] = Map.new(); mp.insert(1, m);\n\
                 match mp.get(1) { Some(x) => println(x), None => println(-1.0) } }",
            "4294967296\n",
        ),
    ];
    for (label, src, want) in cases {
        assert_eq!(run_program(src).as_deref(), Some(*want), "{label}");
    }
}

#[test]
fn test_ir_f16_scalar_float_helper_block_emits_no_half_arithmetic() {
    // The mechanism pin for the E2E above: after widening, the whole
    // `recip` / `to_degrees` / `to_radians` / `fract` block computes at
    // `float`, so an f16 receiver must produce NO `half` arithmetic at
    // all — only the `fptrunc` back at the end. A regression that stops
    // widening f16 shows up here as `fmul half`, naming the cause
    // directly instead of as twenty wrong digits.
    let src = r#"
fn main() {
    let a: f16 = 7.80078125f16;
    println(f"{a.to_degrees()} {a.to_radians()} {a.recip()} {a.fract()}");
}
"#;
    let mut parsed = karac::parse(src);
    assert!(
        parsed.errors.is_empty(),
        "parse errors: {:?}",
        parsed.errors
    );
    karac::prepare_for_resolve(&mut parsed.program);
    let resolved = karac::resolve(&parsed.program);
    let typed = karac::typecheck(&parsed.program, &resolved);
    karac::lower(&mut parsed.program, &typed);
    let ir = compile_to_ir(&parsed.program, None, None).expect("codegen failed");
    for line in ir.lines() {
        for op in ["fmul half", "fdiv half", "fsub half", "fadd half"] {
            assert!(
                !line.contains(op),
                "`{op}` emitted: the float-helper block computed at the \
                     receiver's width, so an irrational constant was rounded \
                     into f16 before the arithmetic (B-2026-08-30-5): {line}"
            );
        }
    }
    assert!(
        ir.contains("fmul float"),
        "expected the widened f32 multiply; IR:\n{ir}"
    );
}

#[test]
fn test_e2e_float_if_return_phi_width() {
    // Float sibling of the int if-width bug: an `f32`-returning fn whose body
    // is `if c { x } else { 0.0 }` mismatched the phi operands (an f32 branch
    // beside the default-f64 literal) and fell through to the `i64 0`
    // placeholder → `ret i64 0` against `float`, failing module verification.
    // `unify_float_branch_widths` truncates the f64 literal to the sibling f32
    // before the phi. (Surfaced by GPU-LBM-4 allowing `if` in `#[gpu]` scalar
    // kernels, but the bug is general — a plain float `if`-return.)
    if let Some(out) = run_program(
        "fn relu(x: f32) -> f32 { if x > 0.0 { x } else { 0.0 } }\n\
             fn main() {\n\
                 println(f\"{relu(3.0)}\");   // 3\n\
                 println(f\"{relu(-2.0)}\");  // 0\n\
             }",
    ) {
        assert_eq!(out, "3\n0\n");
    }
}

#[test]
fn e2e_sorted_set_int_iter_min_max_codegen() {
    // B-2026-07-09-16: `SortedSet[i64]` iterates in ASCENDING order (backed
    // by `KaracMap` storage + a `karac_map_sorted_keys` materialize at the
    // for-loop / min / max observation points). Byte-identical to the
    // `karac run` interpreter (`BTreeMap`) oracle. `try_insert` (dup 1)
    // routes through the shared `compile_set_method` and is now unblocked.
    if let Some(out) = run_program(
        "fn main() {\n\
                 let mut s: SortedSet[i64] = SortedSet.new();\n\
                 let _ = s.insert(5_i64); let _ = s.insert(1_i64);\n\
                 let _ = s.insert(3_i64); let _ = s.try_insert(1_i64);\n\
                 let mut out: String = \"\";\n\
                 for x in s { out.push_str(f\"{x},\"); }\n\
                 println(out);\n\
                 match s.min() { Some(v) => println(v), None => println(\"none\") }\n\
                 match s.max() { Some(v) => println(v), None => println(\"none\") }\n\
                 let mut e: SortedSet[i64] = SortedSet.new();\n\
                 match e.min() { Some(v) => println(v), None => println(\"none\") }\n\
             }",
    ) {
        assert_eq!(out, "1,3,5,\n1\n5\nnone\n");
    }
}

#[test]
fn e2e_sorted_map_int_keys_values_entries_codegen() {
    // B-2026-07-09-17: `SortedMap[i64, String]` observes ASCENDING key order
    // at `keys()` / `values()` / `entries()` / `for (k,v)`. Crucially
    // `values()` is emitted in KEY order (it carries no key to post-sort by),
    // via a sorted-key walk + `karac_map_get` per key. Byte-identical to the
    // `karac run` (BTreeMap) oracle. `try_insert`/`get` reuse compile_map_method.
    if let Some(out) = run_program(
        "fn main() {\n\
                 let mut m: SortedMap[i64, String] = SortedMap.new();\n\
                 let _ = m.insert(5_i64, \"five\"); let _ = m.insert(1_i64, \"one\");\n\
                 let _ = m.insert(3_i64, \"three\"); let _ = m.try_insert(1_i64, \"ONE\");\n\
                 let mut ks: String = \"\";\n\
                 for k in m.keys() { ks.push_str(f\"{k},\"); }\n\
                 println(ks);\n\
                 let mut vs: String = \"\";\n\
                 for v in m.values() { vs.push_str(f\"{v},\"); }\n\
                 println(vs);\n\
                 let mut es: String = \"\";\n\
                 for (k, v) in m { es.push_str(f\"{k}={v};\"); }\n\
                 println(es);\n\
                 match m.get(3_i64) { Some(v) => println(v), None => println(\"none\") }\n\
             }",
    ) {
        assert_eq!(
            out,
            "1,3,5,\nONE,three,five,\n1=ONE;3=three;5=five;\nthree\n"
        );
    }
}

/// IEEE-754 bit reinterpretation in codegen: `f64.to_bits()` / `.to_bits32()`
/// and the inverse `i64.bits_as_f64()` / `.bits_as_f32()`. These had an
/// interpreter + typechecker implementation but no codegen arm, so a program
/// that round-tripped an f64 through its bits ran under `karac run` but failed
/// `karac build` with "no handler for method 'to_bits'" — a run/build
/// divergence surfaced by the LeetCode #50 Pow(x, n) benchmark's XOR-fold
/// sink (ledger B-2026-07-03-1). Now lowered to pure LLVM bitcasts. Covers
/// `to_bits` (positive, +0.0, and the sign-bit-only -0.0 pattern), a
/// `bits_as_f64` round-trip, `to_bits32` + `bits_as_f32` round-trip, and the
/// XOR-fold an integer sink for a float kernel relies on — every value
/// byte-identical to the interpreter oracle (true as of B-2026-08-11-20;
/// the `-0.0` line disagreed with the interpreter before it).
#[test]
fn e2e_float_to_bits_codegen() {
    if let Some(out) = run_program(
        "fn main() {\n\
                 println((2.1_f64).to_bits());\n\
                 println((0.0_f64).to_bits());\n\
                 println((-0.0_f64).to_bits());\n\
                 let b: i64 = (2.1_f64).to_bits() as i64;\n\
                 println(b.bits_as_f64());\n\
                 println((1.5_f64).to_bits32());\n\
                 let b32: i64 = (1.5_f64).to_bits32();\n\
                 println(b32.bits_as_f32());\n\
                 let mut acc: i64 = 0i64;\n\
                 acc = acc ^ (2.0_f64).to_bits();\n\
                 acc = acc ^ (3.0_f64).to_bits();\n\
                 println(acc);\n\
             }",
    ) {
        // B-2026-08-11-20: the `-0.0` line read `-9223372036854775808`
        // until this row. `to_bits` is declared `-> u64`, and the
        // interpreter has always printed the unsigned `9223372036854775808`
        // for it — so despite the "byte-identical to the interpreter
        // oracle" claim above, this expectation was pinning a real
        // interp-vs-codegen divergence rather than agreement. It was the
        // ONE value in this test with the high bit set, which is the only
        // place the two renderings differ.
        assert_eq!(
            out,
            "4611911198408756429\n0\n9223372036854775808\n2.1\n1069547520\n1.5\n2251799813685248\n"
        );
    }
}

#[test]
fn e2e_u8_ascii_predicates_codegen() {
    // ASCII byte-classification on `u8` (inline range checks): must match
    // the interpreter (test_u8_ascii_predicates_interpreter). The
    // self-hosting lexer's AOT byte-indexed scan depends on this.
    if let Some(out) = run_program(
            "fn main() {\n\
                 let s: String = \"aZ9_ f\";\n\
                 for b in s.bytes() {\n\
                     println(f\"{b.is_ascii_digit()} {b.is_ascii_alphabetic()} {b.is_ascii_hexdigit()}\");\n\
                 }\n\
             }",
        ) {
            assert_eq!(
                out,
                "false true true\n\
                 false true false\n\
                 true false true\n\
                 false false false\n\
                 false false false\n\
                 false true true\n"
            );
        }
}

#[test]
fn e2e_with_capacity_count_overflow_panics() {
    // `(1 << 61) + 1` elements of 8 bytes wraps the u64 byte-count
    // multiply. Before the checked multiply this allocated 8 bytes while
    // recording `cap = 2^61 + 1` — a heap overflow on the first pushes.
    // Must now panic `capacity overflow` up front.
    if let Some(cap) = run_program_capturing(
        "fn main() {\n\
                 let v: Vec[i64] = Vec.with_capacity(2305843009213693953);\n\
                 println(v.len());\n\
             }",
    ) {
        assert_eq!(
            cap.status.code(),
            Some(101),
            "stdout={:?} stderr={:?}",
            cap.stdout,
            cap.stderr
        );
        assert!(
            cap.stderr.contains("capacity overflow"),
            "expected capacity-overflow panic, got stdout={:?} stderr={:?}",
            cap.stdout,
            cap.stderr
        );
    }
}

#[test]
fn test_e2e_print_integer() {
    let out = run_program("fn main() { println(42); }");
    if let Some(out) = out {
        assert_eq!(out.trim(), "42");
    }
}

#[test]
fn test_e2e_print_bool() {
    let out = run_program("fn main() { println(true); }");
    if let Some(out) = out {
        assert_eq!(out.trim(), "true");
    }
}

#[test]
fn test_e2e_float_shortest_roundtrip_formatting() {
    // AOT floats render with Rust's shortest-round-trip `{}` (via
    // `karac_runtime_f64_to_str`), matching the interpreter — not C
    // `printf("%g")`'s 6 significant figures. Covers all four lowering
    // sites: `println`, f-string interpolation, struct `Display`, plus
    // the special values (`%g` rendered `nan` lowercase). Each line is
    // exactly what `karac run` prints for the same program.
    let out = run_program(
        "struct P { a: f64, b: f32 }\n\
             fn main() {\n\
                 println(7.0 / 3.0);\n\
                 println(1.0);\n\
                 println(100.0);\n\
                 println(0.1);\n\
                 println(f\"x={2.0 / 3.0}\");\n\
                 let p = P { a: 3.141592653589793, b: 1.5 };\n\
                 println(f\"{p.a} {p.b}\");\n\
                 let nan: f64 = 0.0 / 0.0;\n\
                 println(nan);\n\
                 let inf: f64 = 1.0 / 0.0;\n\
                 println(inf);\n\
             }\n",
    );
    if let Some(out) = out {
        assert_eq!(
            out,
            "2.3333333333333335\n1\n100\n0.1\nx=0.6666666666666666\n\
                 3.141592653589793 1.5\nNaN\ninf\n",
            "AOT float formatting must match the interpreter's shortest-round-trip output",
        );
    }
}

#[test]
fn test_e2e_ambient_rand_next_u64_advances_state() {
    // `rand.next_u64()` lowers to the `karac_runtime_rand_next_u64` FFI
    // (xorshift64), the codegen counterpart of the interpreter's
    // `("RandomSource", "next_u64")` arm. Output is seeded from
    // wall-clock nanoseconds so no specific value is assertable — but
    // two consecutive draws differing is a sharp witness that the FFI
    // fired and state advanced. Mirrors the interpreter's
    // `test_ambient_random_source_next_u64_advances_state`. Regression
    // guard for the "ambient resource method 'RandomSource.next_u64' is
    // not yet lowered (interpreter-only)" codegen error.
    let out = run_program(
        r#"
fn main() reads(RandomSource) {
    let a = rand.next_u64();
    let b = rand.next_u64();
    if a != b { println("rand-advanced"); } else { println("rand-stuck"); }
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "rand-advanced");
    }
}

#[test]
fn test_e2e_stdout_print_and_flush() {
    // `Stdout.print` (no newline) + `Stdout.flush()` + `Stdout.println`.
    // flush lowers to `fflush(NULL)` and must run without crashing; the
    // accumulated output is the concatenation with the final newline.
    let out = run_program(
        r#"
fn main() {
    Stdout.print("x");
    Stdout.flush();
    Stdout.println("y");
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out, "xy\n");
    }
}

#[test]
fn test_e2e_with_provider_override_rand_next_u64_scalar() {
    // `with_provider[RandomSource]` override of `next_u64` — a method with
    // NO vtable slot before this slice (it errored loudly at codegen,
    // `test_ambient_override_of_nonvtable_method_errors_loudly`). Now it
    // gets a slot + a runtime override-vs-default branch. Scalar (i64)
    // phi shape. The override returns a fixed 777, observed BOTH directly
    // and cross-boundary (`draw()` is a separate fn, so dispatch is via
    // the runtime provider stack, not lexical scope). After the scope
    // pops, the real FFI default resumes (two draws differ → "false").
    // `karac run` of the same source matches.
    let out = run_program(
        r#"
struct FakeRng { v: i64 }
impl FakeRng { fn next_u64(ref self) -> i64 { self.v } }
fn draw() -> i64 reads(RandomSource) { rand.next_u64() as i64 }
fn main() reads(RandomSource) {
    with_provider[RandomSource](FakeRng { v: 777 }, || {
        println(draw());
        println(rand.next_u64());
    });
    println(rand.next_u64() == rand.next_u64());
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "777\n777\nfalse");
    }
}

/// IR pin for the same fix: the explicit `return;` in `main` lowers to
/// `ret i32 0`. A bare `fn main() { return; }` would otherwise verify-
/// fail, so reaching this assertion at all already proves the fix; the
/// grep pins the exact instruction.
#[test]
fn test_ir_explicit_return_in_main_is_ret_i32_zero() {
    let ir = ir_for("fn main() { return; }");
    let body = function_body(&ir, "main").expect("main body");
    assert!(
        body.contains("ret i32 0"),
        "explicit return in main should emit `ret i32 0`; body was:\n{}",
        body
    );
    assert!(
        !body.contains("ret void"),
        "main must not emit `ret void`; body was:\n{}",
        body
    );
}

// ── println signedness round-trips ───────────────────────────
//
// Pre-fix `println(x: i32)` passed the raw i32 to printf "%lld";
// LLVM zero-padded the slot to 64 bits and `%lld` read the high
// bits as 0, producing the *unsigned* representation on negatives
// (e.g. `-123` printed as `4_294_967_173`). Fix routes narrow ints
// through `sext + %lld` (signed) or `zext + %llu` (unsigned) based
// on the source-level type, mirroring `synth_display`. These
// regression tests pin the signed and unsigned arms at i8 / i16 /
// i32 / i64 / u8 / u16 / u32 / u64 plus a u64 value > 2^32 to
// exercise the wide-unsigned arm that pre-fix also misprinted.

#[test]
fn test_e2e_print_i32_negative_prints_signed() {
    let out = run_program("fn main() { let x: i32 = -123i32; println(x); }");
    if let Some(out) = out {
        assert_eq!(out.trim(), "-123");
    }
}

#[test]
fn test_e2e_print_i16_negative_prints_signed() {
    let out = run_program("fn main() { let x: i16 = -7i16; println(x); }");
    if let Some(out) = out {
        assert_eq!(out.trim(), "-7");
    }
}

#[test]
fn test_e2e_print_i8_negative_prints_signed() {
    let out = run_program("fn main() { let x: i8 = -7i8; println(x); }");
    if let Some(out) = out {
        assert_eq!(out.trim(), "-7");
    }
}

#[test]
fn test_e2e_print_i64_negative_prints_signed() {
    let out = run_program("fn main() { let x: i64 = -123i64; println(x); }");
    if let Some(out) = out {
        assert_eq!(out.trim(), "-123");
    }
}

#[test]
fn test_e2e_print_u8_prints_unsigned() {
    let out = run_program("fn main() { let x: u8 = 200u8; println(x); }");
    if let Some(out) = out {
        assert_eq!(out.trim(), "200");
    }
}

#[test]
fn test_e2e_u8_cast_to_i32_zero_extends() {
    // 0xff as u8 widened to i32 must yield 255, not -1. Pre-fix
    // `compile_cast` used `build_int_cast` which sign-extends on widening;
    // the load+cast path for `Vec[u8]` / `Slice[u8]` elements produced
    // negative results for any byte ≥ 128. Hot-loop impact on kata-91
    // (`(bytes[i] as i32) - (zero as i32)` for digit math): the extra
    // sext+mask sequence cost ~2 inst/iter vs rust's single ldrb.
    let out = run_program(
        r#"
fn main() {
    let b: u8 = 255u8;
    let x: i32 = b as i32;
    println(x);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "255");
    }
}

#[test]
fn test_e2e_int_to_int_cast_spec_cases() {
    // phase-8 cast slice 5 (int→int verification): the design.md test
    // vectors. Narrowing keeps the low bits (`trunc`); widening sign- or
    // zero-extends per source signedness. Compiled output must match the
    // interpreter (and these documented values).
    let out = run_program(
        r#"
fn main() {
    println(0x1FFi32 as u8);   // narrow: low 8 bits -> 255
    println(-1i32 as u8);      // narrow: 0xFF -> 255
    println(300i32 as i8);     // narrow: 300 & 0xFF = 0x2C -> 44
    println(-1i8 as u8);       // same width reinterpret -> 255
    let a: i8 = -5;
    println(a as i64);         // signed widen (sext) -> -5
    let u: u8 = 200;
    println(u as i64);         // unsigned widen (zext) -> 200
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "255\n255\n44\n255\n-5\n200");
    }
}

#[test]
fn test_e2e_int_to_float_casts() {
    // phase-8 cast slice 6 (int→float verification): signed via sitofp,
    // unsigned via uitofp, never panics. Large values round to nearest —
    // shown via a round-trip through a value not representable in f64
    // (2^60 + 1 → 2^60), which prints as a stable integer (no float-format
    // dependence).
    let out = run_program(
        r#"
fn main() {
    println(5i64 as f64);
    println(255u8 as f64);
    let x: i64 = 1152921504606846977;
    println((x as f64) as i64);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "5\n255\n1152921504606846976");
    }
}

#[test]
fn test_e2e_float_to_float_casts() {
    // phase-8 cast slice 7 (float→float verification): widening is implicit
    // (f32→f64 is value-preserving), narrowing via `as` rounds, and
    // narrowing overflow produces ±Infinity per IEEE 754 (not a trap).
    let out = run_program(
        r#"
fn main() {
    let f: f32 = 2.5;
    println(f as f64);
    println(1.5f64 as f32);
    println(1e300f64 as f32);
    println(-1e300f64 as f32);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "2.5\n1.5\ninf\n-inf");
    }
}

#[test]
fn test_e2e_u8_slice_index_cast_to_i32_zero_extends() {
    // Same fix exercised through the kata-91 shape — `bytes: Vec[u8]`,
    // `bytes[i] as i32`. The `expr_is_unsigned_int` `Index` arm reads
    // the element TypeExpr off `var_elem_type_exprs` and drives the
    // zext-widening lane.
    let out = run_program(
        r#"
fn main() {
    let mut bs: Vec[u8] = Vec.new();
    bs.push(200u8);
    let x: i32 = (bs[0] as i32) - (b'0' as i32);
    println(x);
}
"#,
    );
    if let Some(out) = out {
        // 200 - 48 ('0') = 152. Pre-fix the sext load + sext cast made
        // bs[0] = -56 (i8) → -56 - 48 = -104.
        assert_eq!(out.trim(), "152");
    }
}

#[test]
fn test_ir_u8_cast_to_i32_emits_zext() {
    // IR-level pin: the cast must lower to a zext, not a sext. ARM
    // backend fuses `load i8` + `zext i8 to i32` into `ldrb` — that's
    // the assembly-level win we're chasing in kata-91's hot loop.
    let ir = ir_for(
        r#"
fn main() {
    let b: u8 = 200u8;
    let x: i32 = b as i32;
    println(x);
}
"#,
    );
    assert!(
        ir.contains("zext i8") || ir.contains("zext nneg i8"),
        "expected zext widening from i8 in IR, got no match in:\n{}",
        ir
    );
}

#[test]
fn test_e2e_print_u16_prints_unsigned() {
    let out = run_program("fn main() { let x: u16 = 60000u16; println(x); }");
    if let Some(out) = out {
        assert_eq!(out.trim(), "60000");
    }
}

#[test]
fn test_e2e_print_u32_above_i32_max_prints_unsigned() {
    // 4_000_000_000 > 2^31 - 1; pre-fix accidentally printed
    // correctly via zero-padding, but the path now uses %llu and
    // this pins it.
    let out = run_program("fn main() { let x: u32 = 4000000000u32; println(x); }");
    if let Some(out) = out {
        assert_eq!(out.trim(), "4000000000");
    }
}

#[test]
fn test_e2e_print_u64_prints_unsigned() {
    let out = run_program("fn main() { let x: u64 = 5000000000u64; println(x); }");
    if let Some(out) = out {
        assert_eq!(out.trim(), "5000000000");
    }
}

#[test]
fn test_e2e_print_i32_suffixed_literal_directly() {
    // `println(-123i32)` directly — exercises the literal-suffix
    // arm of `expr_is_unsigned_int` (the Unary(Neg, Integer)
    // shape falls through to the default-signed path).
    let out = run_program("fn main() { println(-123i32); }");
    if let Some(out) = out {
        assert_eq!(out.trim(), "-123");
    }
}

#[test]
fn test_e2e_print_u32_suffixed_literal_directly() {
    let out = run_program("fn main() { println(4000000000u32); }");
    if let Some(out) = out {
        assert_eq!(out.trim(), "4000000000");
    }
}

// ── Layout introspection intrinsics ──────────────────────────
//
// `size_of[T]()` lowers to inkwell's `BasicTypeEnum::size_of()`
// (compile-time constant); `align_of[T]()` queries the host
// `TargetData::get_abi_alignment()`. Both return `usize` (i64 on
// the 64-bit-only target). Slice 1b NO_KNOWN_SIZE pull.

#[test]
fn test_e2e_size_of_i64_is_8() {
    let out = run_program("fn main() { println(size_of[i64]()); }");
    if let Some(out) = out {
        assert_eq!(out.trim(), "8");
    }
}

#[test]
fn test_e2e_size_of_i32_is_4() {
    let out = run_program("fn main() { println(size_of[i32]()); }");
    if let Some(out) = out {
        assert_eq!(out.trim(), "4");
    }
}

#[test]
fn test_e2e_size_of_i8_is_1() {
    let out = run_program("fn main() { println(size_of[i8]()); }");
    if let Some(out) = out {
        assert_eq!(out.trim(), "1");
    }
}

#[test]
fn test_e2e_align_of_i64_is_8() {
    let out = run_program("fn main() { println(align_of[i64]()); }");
    if let Some(out) = out {
        assert_eq!(out.trim(), "8");
    }
}

#[test]
fn test_e2e_align_of_i32_is_4() {
    let out = run_program("fn main() { println(align_of[i32]()); }");
    if let Some(out) = out {
        assert_eq!(out.trim(), "4");
    }
}

#[test]
fn test_e2e_align_of_i8_is_1() {
    let out = run_program("fn main() { println(align_of[i8]()); }");
    if let Some(out) = out {
        assert_eq!(out.trim(), "1");
    }
}

#[test]
fn test_e2e_union_field_read_write_round_trip() {
    // Two-field same-shape union pins the literal-construction +
    // unsafe-read path without dragging the float formatter in
    // (we want the printed value to match the input verbatim, not
    // an f32 scientific-notation render). Stores the low slot,
    // reads back through the high slot — must see the same
    // 32-bit pattern because both fields share the storage cell.
    let out = run_program(
        "#[repr(C)] union BitsLR { l: u32, r: u32 }\n\
             fn main() {\n\
                 let u = BitsLR { l: 4242u32 };\n\
                 let v = unsafe { u.r };\n\
                 println(v);\n\
             }",
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "4242");
    }
}

#[test]
fn test_e2e_ptr_addr_round_trips_through_int_storage() {
    // End-to-end: receive a usize masquerading as `*const i64` (via
    // the i64-pointer ABI), call `ptr.addr` to recover the bits,
    // confirm the round-trip via `ptr.with_addr` and `ptr.addr` is
    // observation-equivalent. Doesn't require a real heap pointer
    // because the ABI carries the value as i64 throughout.
    let src = "fn round_trip(p: *const i64) -> bool { \
                       let a: usize = ptr.addr(p); \
                       let q: *const i64 = ptr.with_addr(p, a); \
                       ptr.addr(q) == a \
                   } \
                   fn main() {}";
    let ir = ir_for(src);
    assert!(
        ir.contains("@round_trip"),
        "round_trip fn should be emitted; got IR:\n{ir}"
    );
}

#[test]
fn test_e2e_ptr_null_is_null_round_trips_to_true() {
    // ptr.is_null(ptr.null()) must observe true. Pins the
    // null-pointer constant + the EQ-against-zero compare.
    let src = "fn main() { \
                       let p: *const i32 = ptr.null(); \
                       if ptr.is_null(p) { println(1); } else { println(0); } \
                   }";
    let out = run_program(src);
    if let Some(out) = out {
        assert_eq!(out, "1\n");
    }
}

#[test]
fn test_e2e_string_marshaling_round_trips_through_cstring() {
    // The complete FFI String-marshaling loop (phase-8 "FFI — String
    // marshaling"): a runtime-built Kāra `String` crosses the C boundary as
    // a NUL-terminated `char*` and comes back losslessly. Outbound via the
    // OWNING `CString` (`to_cstring` appends the NUL, owns the heap buffer)
    // + `as_ptr`; inbound via `CStr.from_ptr` (borrows the caller's memory,
    // libc `strlen` recomputes the length) + `to_string` (copies, validates
    // UTF-8). The two halves are exercised individually elsewhere; this pins
    // that they COMPOSE into a String→char*→String identity. `cs` outlives
    // the borrowed pointer (drops at end of `main`), so `p` stays valid.
    let src = r#"
fn main() {
    let s = "hello, " + "world";
    let cs = s.to_cstring().unwrap();
    let p = cs.as_ptr();
    // Safety: `p` points into `cs`'s NUL-terminated buffer, live until main exits.
    let back = unsafe { CStr.from_ptr(p) }.to_string().unwrap();
    println(back);
    println(back.len());
}
"#;
    let out = run_program(src);
    if let Some(out) = out {
        assert_eq!(out, "hello, world\n12\n");
    }
}

#[test]
fn test_e2e_cstr_from_ptr_round_trips_len_and_bytes() {
    // The inbound raw-pointer constructor (LLVM-C FFI spike sub-q 4):
    // a c"..." literal -> as_ptr (raw *const u8) -> CStr.from_ptr
    // (libc `strlen` recomputes the length) -> the borrowed surface
    // (len / is_empty / as_bytes) reads the same bytes back. Proves the
    // `{ptr, strlen(ptr)}` aggregate the assoc-call lowering builds is a
    // well-formed CStr indistinguishable from the literal it came from.
    let src = r#"
fn main() {
    let original = c"hello, world";
    let p = original.as_ptr();
    // Safety: `p` is a NUL-terminated rodata pointer from a c"..." literal.
    let rebuilt = unsafe { CStr.from_ptr(p) };
    println(rebuilt.len());
    println(rebuilt.is_empty());
    let bytes = rebuilt.as_bytes();
    println(bytes[0]);
    println(bytes[4]);
    // An empty C string round-trips to len 0 / is_empty true.
    let ep = c"".as_ptr();
    let rebuilt_empty = unsafe { CStr.from_ptr(ep) };
    println(rebuilt_empty.is_empty());
}
"#;
    let out = run_program(src);
    if let Some(out) = out {
        // len=12, not empty, bytes[0]='h'(104), bytes[4]='o'(111), empty=true
        assert_eq!(out, "12\nfalse\n104\n111\ntrue\n");
    }
}

#[test]
fn test_e2e_raw_ptr_deref_store_round_trip() {
    // B-2026-06-11-3 (store side): `*p = val` on a `*mut T` used to store
    // into the pointer variable's own alloca (`get_data_ptr`'s owned-local
    // branch) rather than through the pointer — clobbering `p` instead of
    // the pointee. The store arm now compiles the raw-pointer operand to its
    // address value and stores through it. Round-trip: write 90, read 90.
    let src = r#"
fn main() {
    let mut a: Array[u8, 3] = [65u8, 66u8, 67u8];
    let p = a.as_mut_ptr();
    // Safety: `p` addresses element 0 of the live mutable owned array.
    unsafe { *p = 90u8; }
    let b: u8 = unsafe { *p };
    println(b);
    // The write lands in the array itself, observable via indexing.
    println(a[0]);
}
"#;
    let out = run_program(src);
    if let Some(out) = out {
        assert_eq!(out, "90\n90\n");
    }
}

#[test]
fn test_e2e_two_field_i64_struct_reassign_not_gpu_buffer() {
    // B-2026-07-18-7: a 2-field all-`i64` user struct lowers to the anonymous
    // `{i64, i64}` that is ALSO the GPU-buffer handle type, so the reassign
    // arm's `vs.ty == gpu_buffer_type()` check misfired — `p = P { … }`
    // routed old-value cleanup through `karac_runtime_gpu_free_soa`, so both
    // `karac run` (JIT symbol-not-found) and `karac build` (gpu-archive link
    // demand) failed for this non-GPU program. The reassign now gates on the
    // authoritative `gpu_buffer_vars` set, so a plain struct reassign takes
    // the normal path and references no GPU symbol.
    let out = run_program(
        r#"
struct P { x: i64, y: i64 }
fn main() {
    let mut p = P { x: 1, y: 2 };
    p = P { x: 10, y: 20 };
    println((p.x + p.y).to_string());
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "30");
    }
}

#[test]
fn test_ir_generic_float_specialization() {
    let ir = ir_for(
        r#"
fn double_val[T](x: T) -> T { x + x }
fn main() {
    let y = double_val(2.5);
    println(y);
}
"#,
    );
    assert!(
        ir.contains("double_val$f64"),
        "should contain f64 specialization"
    );
}

/// Multi-arg + float `Fn(...)` parameters: the env-first trampoline ABI
/// forwards every user arg, regardless of arity or scalar width.
#[test]
fn fn_value_fn_typed_param_multiarg_and_float_run() {
    let multiarg = run_program(
        "fn combine(a: i64, b: i64) -> i64 { a * 10i64 + b }\n\
             fn apply2(f: Fn(i64, i64) -> i64, x: i64, y: i64) -> i64 { f(x, y) }\n\
             fn main() { println(f\"{apply2(combine, 4i64, 2i64)}\"); }\n",
    );
    assert_eq!(multiarg.as_deref(), Some("42\n"));

    let float = run_program(
        "fn half(x: f64) -> f64 { x / 2.0 }\n\
             fn applyf(f: Fn(f64) -> f64, x: f64) -> f64 { f(x) }\n\
             fn main() { println(f\"{applyf(half, 84.0)}\"); }\n",
    );
    assert_eq!(float.as_deref(), Some("42\n"));
}

/// SPEC'D FLOAT HOLES: the whole arm, and the WIDE case that used to read
/// past its buffer.
///
/// There was no codegen coverage of spec'd float rendering at all before
/// this, which is how B-2026-09-07-46 survived: codegen called `snprintf`
/// and used its return value as the rendered length, but C returns the
/// length it WOULD have written. `f"{x:.2}"` on `f64::MAX` is 312 bytes
/// against what was then a 64-byte buffer, so the `String` ran ~245 bytes
/// past the written region and the program printed uninitialized STACK --
/// different bytes on every run, which is also why no fixed expected value
/// could have caught it by accident.
///
/// The `H[...]` line is therefore the load-bearing one, and it asserts
/// CONTENT rather than length: the length was already 312 before the fix.
/// That is the whole defect -- a correct length over a buffer that never
/// held that many bytes.
///
/// The rest is the ordinary matrix, which pins that moving the arm off
/// `snprintf` (B-2026-09-07-39) changed cost and not output: precision,
/// width, align, zero-pad between sign and digits, and the two shapes
/// `needs_runtime_formatter()` diverts.
#[test]
fn e2e_spec_float_holes_render_exactly_and_never_past_the_buffer() {
    if let Some(out) = run_program(
        r#"
fn main() {
    let x: f64 = 1.5;
    let y: f64 = 3.0;
    let z: f64 = 1.23456;
    let n: f64 = -2.5;
    let b: f64 = 1234567.891;
    let huge: f64 = 1.7976931348623157e308;
    println(f"[{z:.2}][{z:.0}][{z:.5}]");
    println(f"[{z:10.2}][{z:<10.2}][{z:010.2}]");
    println(f"[{n:.2}][{n:08.2}][{n:<8.2}]");
    println(f"[{y:.1}][{y:.3}][{b:.2}]");
    println(f"[{x}][{y}][{z}][{n}]");
    println(f"[{z:^10.2}][{z:*>10.2}]");
    println(f"H[{huge:.2}]");
}
"#,
    ) {
        let want = "[1.23][1][1.23456]\n\
                        [      1.23][1.23      ][0000001.23]\n\
                        [-2.50][-0002.50][-2.50   ]\n\
                        [3.0][3.000][1234567.89]\n\
                        [1.5][3][1.23456][-2.5]\n\
                        [   1.23   ][******1.23]\n\
                        H[179769313486231570814527423731704356798070567525844996598917476803157260780028538760589558632766878171540458953514382464234321326889464182768467546703537516986049910576551282076245490090389328944075868508455133942304583236903222948165808559332123348274797826204144723168738177180919299881250404026184124858368.00]\n";
        assert_eq!(out, want, "spec'd float rendering drifted");
    }
}

/// B-2026-09-05-23 — the replacement formatter must agree with the old
/// `snprintf` bytes at the boundaries, on every integer display path.
///
/// `i64::MIN` is the value a hand-rolled itoa gets wrong: negating it as an
/// `i64` overflows and wraps straight back to itself, so a naive `-v`
/// prints it as a positive number or loops. The upper half of `u64` is the
/// other side — read as signed it renders negative.
#[test]
fn e2e_i64_formatter_renders_the_boundaries() {
    if let Some(out) = run_program(
        r#"
fn main() {
    let min: i64 = -9223372036854775808;
    let max: i64 = 9223372036854775807;
    let ubig: u64 = 18446744073709551615u64;
    let umid: u64 = 9223372036854775808u64;
    println(min);
    println(max);
    println(ubig);
    println(umid);
    println(f"{min} {max} {ubig} {umid}");
    let i8v: i8 = -128;
    let i32v: i32 = -2147483648;
    let u32v: u32 = 4294967295;
    println(f"{i8v} {i32v} {u32v}");
    println(0);
    println(f"{0}");
    let v: Vec[i64] = [0, -1, -9223372036854775808, 9223372036854775807];
    println(f"{v}");
}
"#,
    ) {
        let want = "-9223372036854775808\n\
                        9223372036854775807\n\
                        18446744073709551615\n\
                        9223372036854775808\n\
                        -9223372036854775808 9223372036854775807 \
                        18446744073709551615 9223372036854775808\n\
                        -128 -2147483648 4294967295\n\
                        0\n\
                        0\n\
                        [0, -1, -9223372036854775808, 9223372036854775807]\n";
        assert_eq!(out, want, "integer boundary rendering drifted");
    }
}

#[test]
fn test_ir_vector_adjacent_vec_load_fuses() {
    // B-2026-07-21-3 (contiguous leg): `Vector[f64, 2](v[p], v[p + 1])`
    // over one plain Vec whose element type equals the lane type lowers
    // as ONE `load <2 x double>` (elem-aligned) from the element pointer
    // at `p` — not two checked scalar loads + two insertelements (which
    // the wasm backend never re-fuses; Prism's vertical Lanczos pass
    // measured ~4.7x slower on the chain form). A non-adjacent / mixed
    // construction keeps the insertelement chain.
    let ir = ir_for(
        r#"
fn pair_sum(v: Vec[f64], p: i64) -> f64 {
    let pair = Vector[f64, 2](v[p], v[p + 1]);
    return pair.reduce_sum();
}
fn mixed(v: Vec[f64]) -> f64 {
    let q = Vector[f64, 2](v[0], 5.0);
    return q.reduce_sum();
}
fn main() {
    let v: Vec[f64] = [1.0, 2.0, 3.0];
    println(pair_sum(v, 1));
    println(mixed(v));
}
"#,
    );
    let fused = function_body(&ir, "pair_sum").unwrap_or_else(|| {
        panic!("pair_sum body not found in IR:\n{}", ir);
    });
    assert!(
        fused.contains("load <2 x double>") && fused.contains("vsimd"),
        "expected a single fused `load <2 x double>` (vsimd) in pair_sum; body was:\n{}",
        fused
    );
    assert!(
        !fused.contains("vec.ins"),
        "adjacent-lane construction should not emit an insertelement chain; body was:\n{}",
        fused
    );
    let chain = function_body(&ir, "mixed").unwrap_or_else(|| {
        panic!("mixed body not found in IR:\n{}", ir);
    });
    assert!(
        chain.contains("vec.ins"),
        "mixed (non-adjacent) construction must keep the insertelement chain; body was:\n{}",
        chain
    );
}

#[test]
fn test_e2e_vector_adjacent_vec_load_shapes() {
    // B-2026-07-21-3 output leg: the fused adjacent-load construction is
    // byte-identical to the insertelement chain across identifier bases
    // in a loop, literal bases, i64 lanes, and a mixed (unfused)
    // construction. The IR twin asserts the fusion actually happens.
    let output = run_program(
        "fn vsum(v: Vec[f64]) -> f64 {\n\
                 let mut acc = Vector[f64, 2](0.0, 0.0);\n\
                 let mut p: i64 = 0;\n\
                 while p + 1 < v.len() {\n\
                     let pair = Vector[f64, 2](v[p], v[p + 1]);\n\
                     acc = acc + pair;\n\
                     p = p + 2;\n\
                 }\n\
                 return acc.reduce_sum();\n\
             }\n\
             fn main() {\n\
                 let mut v: Vec[f64] = Vec.new();\n\
                 let mut i: i64 = 0;\n\
                 while i < 8 {\n\
                     v.push(i.to_f64() + 0.5);\n\
                     i = i + 1;\n\
                 }\n\
                 println(vsum(v));\n\
                 let w: Vec[f64] = [10.0, 20.0, 30.0];\n\
                 println(Vector[f64, 2](w[0], w[1]).reduce_sum());\n\
                 println(Vector[f64, 2](w[1], w[2]).reduce_sum());\n\
                 let a: Vec[i64] = [7, 8, 9];\n\
                 println(Vector[i64, 2](a[1], a[2]).reduce_sum());\n\
                 let q = Vector[f64, 4](w[0], w[1], w[2], 5.0);\n\
                 println(q.reduce_sum());\n\
             }",
    )
    .expect("compile + run failed");
    assert_eq!(output, "32\n30\n50\n17\n65\n");
}

#[test]
fn test_e2e_vector_reduction_as_chain_receiver() {
    // B-2026-07-29-7: a `Vector[T, N]` reduction used as the RECEIVER of
    // another call. The parser sets `MethodCall.span == receiver.span`, so
    // `v.reduce_sum().to_string()` puts both links at one
    // `method_callee_types` key; the outer `f32.to_string` insert clobbered
    // the inner `Vector.reduce_sum`, and codegen's method-segment guard
    // then (correctly) refused to let `to_string` drive the inner call —
    // leaving it with no dispatch key at all. The vector dispatch fell
    // through and the build died with "no handler for method 'reduce_sum'".
    //
    // `v.reduce_sum()` on its own was always fine, which is why the whole
    // Vector surface looked healthy: every existing test prints the
    // reduction directly. One `.to_string()` was enough to break it.
    //
    // Covers every reduction plus a chained unary (`sqrt`) and a lane
    // permute (`reverse`) as inner links, over float and integer lanes,
    // and a reduction inside an arithmetic expression.
    let output = run_program(
        "fn main() {\n\
                 let a = Vector[f64, 4](1.0, 2.0, 3.0, 4.0);\n\
                 let b = Vector[f64, 4](2.0, 2.0, 2.0, 2.0);\n\
                 println(a.reduce_sum().to_string());\n\
                 println(a.reduce_product().to_string());\n\
                 println(a.reduce_min().to_string());\n\
                 println(a.reduce_max().to_string());\n\
                 println(a.dot(b).to_string());\n\
                 println(a.reverse().reduce_max().to_string());\n\
                 let c = Vector[i64, 4](1, 2, 3, 4);\n\
                 println(c.reduce_and().to_string());\n\
                 println(c.reduce_or().to_string());\n\
                 println(c.reduce_xor().to_string());\n\
                 println((a.reduce_sum() + b.reduce_sum()).to_string());\n\
             }",
    )
    .expect("compile + run failed");
    assert_eq!(output, "10\n24\n1\n4\n20\n4\n0\n7\n4\n18\n");
}

#[test]
fn test_e2e_vector_call_initialized_binding_and_arith() {
    // B-2026-07-29-7 sibling shapes, from the entry's own repro: a
    // `Vector`-typed binding whose initializer is a CALL, and
    // `acc = acc + x * y` accumulation. Both were reported as broken and
    // both in fact work — the entry's repro only failed because it printed
    // through `.to_string()`, which is the chain bug above. Pinned here so
    // that stays true, and so the entry's exact program has a test.
    let output = run_program(
            "fn zero8() -> Vector[f32, 8] {\n\
                 Vector[f32, 8](0.0f32, 0.0f32, 0.0f32, 0.0f32, 0.0f32, 0.0f32, 0.0f32, 0.0f32)\n\
             }\n\
             fn main() {\n\
                 let z: Vector[f32, 8] = zero8();\n\
                 println(z.reduce_sum().to_string());\n\
                 let mut acc: Vector[f32, 8] = zero8();\n\
                 let x = Vector[f32, 8](1.0f32, 2.0f32, 1.0f32, 2.0f32, 1.0f32, 2.0f32, 1.0f32, 2.0f32);\n\
                 acc = acc + x * x;\n\
                 println(acc.reduce_sum().to_string());\n\
             }",
        )
        .expect("compile + run failed");
    assert_eq!(output, "0\n20\n");
}

#[test]
fn test_ir_proven_index_add_skips_its_overflow_check() {
    // B-2026-08-05-21: when BCE has proven `0 <= base + i < v.len()`, the
    // add that computed the index provably cannot overflow, so its trap is
    // dead and must not be emitted.
    //
    // Asserted as a DIFFERENTIAL against the reordered variant rather than
    // an absolute count: both programs contain the same five adds
    // (`acc + ..`, `base + lo`, `base + hi`, `lo + 1`, `i + 1`), and only
    // the two index adds are provable. So the proven form must carry
    // exactly two fewer intrinsics — an absolute count would silently pass
    // if an unrelated add were added to or removed from the fixture.
    let proven = count_add_overflow_intrinsics(&ir_for(CONV_TWO_POINTER_SRC));
    let unproven = count_add_overflow_intrinsics(&ir_for(&conv_two_pointer_unproven_src()));
    assert_eq!(
        unproven,
        proven + 2,
        "expected the two proven index adds to drop their overflow checks \
             (proven={proven}, unproven={unproven})"
    );
    assert!(
        proven > 0,
        "the unprovable adds (`lo + 1`, `i + 1`, `acc + ..`) must KEEP \
             their checks — a zero count would mean the elision is firing far \
             too widely, got {proven}"
    );
}

#[test]
fn test_ir_unproven_index_add_keeps_its_overflow_check() {
    // The gate, stated positively: with the bounds proof defeated by the
    // reordering, BOTH the bounds check and the overflow check must
    // survive. Pins that the elision rides on the proof rather than on the
    // syntactic `v[a + b]` shape.
    let ir = ir_for(&conv_two_pointer_unproven_src());
    assert!(
        ir.contains("vidx.ok"),
        "expected the unproven loop to keep its bounds check, got:\n{ir}"
    );
    assert!(
        count_add_overflow_intrinsics(&ir) >= 5,
        "expected every add to keep its overflow check when nothing is \
             proven, got {} in:\n{ir}",
        count_add_overflow_intrinsics(&ir)
    );
}

#[test]
fn test_ir_binsearch_sum_midpoint_trunc_div_gets_no_assume() {
    // B-2026-08-30-6 — `(lo + hi) / 2` must NOT get the midpoint assumes.
    //
    // `assume(mid < hi)` needs the halve to round DOWN. `/` truncates
    // toward ZERO, which rounds a NEGATIVE sum UP, and at `hi == lo + 1`
    // that lands exactly on `hi`: `lo = -3, hi = -2` gives `(-5)/2 == -2`,
    // so the emitted fact was `assume(-2 < -2)` — `assume(false)`. The
    // compiled program then ran on injected UB and SIGBUS'd where the
    // interpreter ran it correctly.
    //
    // The difference form is unaffected and keeps its assumes (pinned by
    // `test_ir_binsearch_midpoint_emits_assumes`), because its dividend
    // `hi - lo >= 1` is positive under the guard, where the two roundings
    // agree.
    let ir = ir_for(
        r#"
fn bisect(nums: ref Vec[i64], len: i64, target: i64) -> i64 {
    let mut lo = 0i64;
    let mut hi = len;
    while lo < hi {
        let mid = (lo + hi) / 2i64;
        if nums[mid] < target { lo = mid + 1i64; } else { hi = mid; }
    }
    lo
}
fn main() {
    let mut v: Vec[i64] = Vec.new();
    v.push(1i64); v.push(3i64); v.push(5i64);
    println(f"{bisect(v, 3i64, 3i64)}");
}
"#,
    );
    assert!(
        !ir.contains("bs.mid.lt.hi"),
        "a truncating `(lo + hi) / 2` must not assert `mid < hi` — that \
             fact is false for a negative odd sum"
    );
    assert!(
        !ir.contains("bs.mid.ge.lo"),
        "the truncating sum form must emit no midpoint assumes at all"
    );
}

#[test]
fn test_ir_binsearch_sum_midpoint_floor_shift_keeps_assume() {
    // The other half of B-2026-08-30-6: `(lo + hi) >> 1` FLOORS, so
    // `lo <= mid <= hi - 1` holds for every `lo < hi` including negative
    // sums, and this spelling is recognised. Without this the fix would
    // read as "the sum form is unsupported" rather than "the sum form
    // requires a flooring halve", and a later session could restore the
    // unsound arm believing it was merely a missing feature.
    let ir = ir_for(
        r#"
fn bisect(nums: ref Vec[i64], len: i64, target: i64) -> i64 {
    let mut lo = 0i64;
    let mut hi = len;
    while lo < hi {
        let mid = (lo + hi) >> 1i64;
        if nums[mid] < target { lo = mid + 1i64; } else { hi = mid; }
    }
    lo
}
fn main() {
    let mut v: Vec[i64] = Vec.new();
    v.push(1i64); v.push(3i64); v.push(5i64);
    println(f"{bisect(v, 3i64, 3i64)}");
}
"#,
    );
    assert!(
        ir.contains("bs.mid.ge.lo") && ir.contains("bs.mid.lt.hi"),
        "a flooring `(lo + hi) >> 1` midpoint should still get both assumes"
    );
}

#[test]
fn test_e2e_negative_bisect_midpoint_does_not_miscompile() {
    // The B-2026-08-30-6 repro end to end. A bisection over NEGATIVE
    // bounds is a well-defined program; before the fix the compiled binary
    // died with SIGBUS while the interpreter printed the right answer.
    // Asserting the VALUE (not merely that it exits) is what makes this a
    // miscompile test rather than a crash test.
    let src = r#"
fn main() {
    let mut lo = 0i64 - 3i64;
    let mut hi = 0i64 - 2i64;
    let mut steps = 0i64;
    while lo < hi {
        let mid = (lo + hi) / 2i64;
        steps = steps + 1i64;
        println(f"mid {mid}");
        if steps > 3i64 { break; }
        lo = mid + 1i64;
    }
    println(f"steps {steps}");
}
"#;
    // `(-3 + -2) / 2` truncates to -2, so `mid` is -2 and the loop makes
    // exactly one pass (`lo` becomes -1, which is not < -2).
    assert_eq!(
        run_program(src).as_deref(),
        Some("mid -2\nsteps 1\n"),
        "negative-bounds bisection must compile to the interpreter's result"
    );
}

#[test]
fn test_ir_binsearch_midpoint_emits_assumes() {
    // Binary-search midpoint BCE (control_flow_bce.rs § midpoint): a
    // `let mid = lo + (hi - lo) / 2` under a strict `while lo < hi`
    // guard emits `assume(mid >= lo)` + `assume(mid < hi)` so LLVM
    // folds the `nums[mid]` bounds check. Pin both `llvm.assume`s and
    // the named comparisons appear.
    let ir = ir_for(
        r#"
fn lower_bound(nums: ref Vec[i64], len: i64, target: i64) -> i64 {
    let mut lo = 0i64;
    let mut hi = len;
    while lo < hi {
        let mid = lo + (hi - lo) / 2i64;
        if nums[mid] < target { lo = mid + 1i64; } else { hi = mid; }
    }
    lo
}
fn main() {
    let mut v: Vec[i64] = Vec.new();
    v.push(1i64); v.push(3i64); v.push(5i64);
    println(f"{lower_bound(v, 3i64, 3i64)}");
}
"#,
    );
    assert!(
        ir.contains("bs.mid.ge.lo"),
        "expected midpoint lower-bound assume (mid >= lo); IR had none"
    );
    assert!(
        ir.contains("bs.mid.lt.hi"),
        "expected midpoint upper-bound assume (mid < hi); IR had none"
    );
    assert!(
        ir.contains("@llvm.assume"),
        "expected an llvm.assume call for the midpoint facts"
    );
}

#[test]
fn test_ir_non_midpoint_binding_no_binsearch_assume() {
    // Negative gate: a non-midpoint `let` under a `while lo < hi` guard
    // (`mid = lo + 1`, not the midpoint form) must NOT emit the
    // binary-search assumes — the recognition is shape-exact.
    let ir = ir_for(
        r#"
fn scan(nums: ref Vec[i64], len: i64) -> i64 {
    let mut lo = 0i64;
    let mut hi = len;
    let mut acc = 0i64;
    while lo < hi {
        let mid = lo + 1i64;
        acc = acc + nums[lo];
        lo = mid;
    }
    acc
}
fn main() {
    let mut v: Vec[i64] = Vec.new();
    v.push(7i64); v.push(9i64);
    println(f"{scan(v, 2i64)}");
}
"#,
    );
    assert!(
        !ir.contains("bs.mid.ge.lo") && !ir.contains("bs.mid.lt.hi"),
        "non-midpoint binding must not emit binary-search midpoint assumes"
    );
}

#[test]
fn test_e2e_binsearch_midpoint_assume_is_sound() {
    // Soundness E2E: the midpoint assumes must not corrupt results. A
    // lower/upper-bound search over a sorted Vec with duplicate runs is
    // exercised across hit / miss / boundary targets; output must match
    // the hand-computed [first, last] pairs exactly.
    let out = run_program(
        r#"
fn lower_bound(nums: ref Vec[i64], len: i64, target: i64) -> i64 {
    let mut lo = 0i64;
    let mut hi = len;
    while lo < hi {
        let mid = lo + (hi - lo) / 2i64;
        if nums[mid] < target { lo = mid + 1i64; } else { hi = mid; }
    }
    lo
}
fn upper_bound(nums: ref Vec[i64], len: i64, target: i64) -> i64 {
    let mut lo = 0i64;
    let mut hi = len;
    while lo < hi {
        let mid = lo + (hi - lo) / 2i64;
        if nums[mid] <= target { lo = mid + 1i64; } else { hi = mid; }
    }
    lo
}
fn report(nums: ref Vec[i64], len: i64, target: i64) {
    let lo = lower_bound(nums, len, target);
    if lo == len or nums[lo] != target {
        println(f"{target}: -1 -1");
    } else {
        println(f"{target}: {lo} {upper_bound(nums, len, target) - 1i64}");
    }
}
fn main() {
    let mut v: Vec[i64] = Vec.new();
    // [5, 7, 7, 8, 8, 10]
    v.push(5i64); v.push(7i64); v.push(7i64); v.push(8i64); v.push(8i64); v.push(10i64);
    report(v, 6i64, 8i64);   // 3 4
    report(v, 6i64, 7i64);   // 1 2
    report(v, 6i64, 5i64);   // 0 0
    report(v, 6i64, 10i64);  // 5 5
    report(v, 6i64, 6i64);   // -1 -1
    report(v, 6i64, 11i64);  // -1 -1
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(
            out.trim(),
            "8: 3 4\n7: 1 2\n5: 0 0\n10: 5 5\n6: -1 -1\n11: -1 -1"
        );
    }
}

#[test]
fn test_e2e_soa_pop_front_shifts_each_group() {
    // `pop_front` materializes the head element, then memmoves the
    // tail of every hot group + cold group left by one slot. After
    // the shift, the new head must match what was at index 1.
    // Multiple pops verify the per-group shift is consistent across
    // calls (a bug that shifted only the first group would surface
    // as misaligned reads on later pops).
    let src = r#"
struct Entity { x: i64, y: i64, label: i64 }
layout entities: Vec[Entity] {
    group physics { x, y }
    cold { label }
}
fn main() {
    let mut entities: Vec[Entity] = Vec.new();
    entities.push(Entity { x: 1, y: 10, label: 100 });
    entities.push(Entity { x: 2, y: 20, label: 200 });
    entities.push(Entity { x: 3, y: 30, label: 300 });
    entities.push(Entity { x: 4, y: 40, label: 400 });
    match entities.pop_front() {
        Some(e) => println(e.x),
        None => println(-1),
    }
    let head = entities[0];
    println(head.x);
    println(head.y);
    println(head.label);
    match entities.pop_front() {
        Some(e) => println(e.x),
        None => println(-1),
    }
    let head2 = entities[0];
    println(head2.x);
    println(head2.label);
    println(entities.len());
}
"#;
    if let Some(out) = run_program(src) {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["1", "2", "20", "200", "2", "3", "300", "2"]);
    }
}

#[test]
fn test_e2e_soa_remove_at_index_shifts_tail() {
    // `remove(idx)` materializes the element at idx, then memmoves
    // the (len-1-idx) tail elements down by one across every group
    // + cold. Returns the removed element directly (no Option
    // wrap) per plain `Vec.remove`'s contract.
    let src = r#"
struct Entity { x: i64, y: i64, label: i64 }
layout entities: Vec[Entity] {
    group physics { x, y }
    cold { label }
}
fn main() {
    let mut entities: Vec[Entity] = Vec.new();
    entities.push(Entity { x: 1, y: 10, label: 100 });
    entities.push(Entity { x: 2, y: 20, label: 200 });
    entities.push(Entity { x: 3, y: 30, label: 300 });
    entities.push(Entity { x: 4, y: 40, label: 400 });
    let removed = entities.remove(1);
    println(removed.x);
    println(removed.y);
    println(removed.label);
    let now0 = entities[0];
    let now1 = entities[1];
    let now2 = entities[2];
    println(now0.x);
    println(now1.x);
    println(now1.label);
    println(now2.x);
    println(now2.label);
    println(entities.len());
}
"#;
    if let Some(out) = run_program(src) {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(
            lines,
            vec!["2", "20", "200", "1", "3", "300", "4", "400", "3"]
        );
    }
}

#[test]
fn test_e2e_function_returning_vec_i64_no_double_free() {
    // Sibling: `Vec[i64]` return — same fix applies. The
    // primitive case happened to work pre-fix due to
    // use-after-free reading stable data, but is now correct
    // by construction (cleanup skipped at the move site, caller
    // owns the buffer cleanly).
    let out = run_program(
        r#"
fn make_vec(n: i64) -> Vec[i64] {
    let mut v: Vec[i64] = Vec.new();
    let mut i = 0i64;
    while i < n {
        v.push(i * 10);
        i = i + 1;
    }
    v
}
fn main() {
    let f: Vec[i64] = make_vec(4);
    println(f.len());
    println(f[3]);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "4\n30");
    }
}

/// B-2026-08-20-34 — stack exhaustion SAYS SO instead of dying silently.
///
/// A Kāra binary has its own `main`, so Rust's `lang_start` never runs and
/// std's guard-page handler was never installed: exhausting the stack
/// killed the process with a bare SIGSEGV, exit 139, and ZERO bytes on
/// stderr — nothing naming recursion as the cause. Measured against the
/// mirrors on the same machine, Kāra matched `cc -O0` and lost to both
/// comparators it otherwise benchmarks against; unoptimized `rustc` prints
/// `thread 'main' has overflowed its stack` and exits 134.
///
/// The fixture recurses without allocating, so it faults in single-digit
/// milliseconds rather than churning the heap on the way down.
#[test]
fn e2e_stack_overflow_reports_itself() {
    let Some(run) = run_program_capturing(
        "fn down(n: i64) -> i64 {\n\
             \x20   if n <= 0 {\n\
             \x20       return 0;\n\
             \x20   }\n\
             \x20   let sub = down(n - 1);\n\
             \x20   return sub + n;\n\
             }\n\
             fn main() {\n\
             \x20   println(f\"{down(50000000i64)}\");\n\
             }\n",
    ) else {
        return;
    };
    assert!(
        !run.status.success(),
        "the fixture must actually overflow; got a clean exit with stdout {:?}",
        run.stdout
    );
    assert!(
        run.stderr.contains("stack overflow"),
        "stack exhaustion must name itself, got stderr {:?} status {:?}",
        run.stderr,
        run.status
    );
    // The hint is the half that makes it actionable — a bare "stack
    // overflow" would still leave a reader guessing what to change.
    assert!(
        run.stderr.contains("recursive") && run.stderr.contains("ulimit"),
        "expected the cause + remedy notes, got stderr {:?}",
        run.stderr
    );
}

#[test]
fn test_e2e_vec_filled_i64_primitive() {
    // `Vec.filled(n, val)` for a primitive element type — malloc +
    // fill loop emit the {data, len, cap} aggregate. Before the
    // fix, the assoc-call fell through to the default i64 zero
    // return, the let-binding allocated an i64 alloca for a Vec-
    // typed binding, and any later method dispatch GEP'd past it
    // into stack garbage (SIGTRAP at runtime, "Built" at build).
    let out = run_program(
        r#"
fn main() {
    let v: Vec[i64] = Vec.filled(3, 42);
    println(v.len());
    println(v[0]);
    println(v[2]);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "3\n42\n42");
    }
}

#[test]
fn test_e2e_vec_nested_indexed_write_round_trip() {
    // `rows[r][c] = val` on `Vec[Vec[T]]`. Pre-fix this errored
    // at codegen with "Index assignment target must be a
    // variable" (the kata 6 _faster.kara workaround uses a flat
    // single-buffer layout to avoid this). The new arm in
    // compile_index_store + compile_nested_vec_vec_index_store
    // GEPs to the inner Vec aggregate, loads its data ptr, GEPs
    // by the inner index, and stores.
    let out = run_program(
        r#"
fn main() {
    let mut rows: Vec[Vec[i64]] = Vec.new();
    let r0: Vec[i64] = Vec.filled(3, 0);
    let r1: Vec[i64] = Vec.filled(3, 0);
    rows.push(r0);
    rows.push(r1);
    rows[0][1] = 42;
    rows[1][2] = 99;
    println(rows[0][0]);
    println(rows[0][1]);
    println(rows[1][2]);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "0\n42\n99");
    }
}

#[test]
fn test_e2e_vec_extend_from_slice_triggers_grow() {
    // dst has cap=2 and 1 element, src has 4 elems — extend
    // must grow mid-flight and copy elements correctly.
    let out = run_program(
        r#"
fn main() {
    let src: Vec[i64] = Vec.filled(4, 5);
    let mut dst: Vec[i64] = Vec.with_capacity(2);
    dst.push(1);
    dst.extend_from_slice(src);
    println(dst.len());
    println(dst[0]);
    println(dst[4]);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "5\n1\n5");
    }
}

#[test]
fn test_e2e_vec_deque_push_front_shifts_storage_right() {
    // `push_front` shifts existing elements right by 1 via
    // `llvm.memmove` and stores the new element at index 0. Iter
    // yields front-to-back: [front=5, then 10, 20].
    let out = run_program(
        r#"
fn main() {
    let mut q: VecDeque[i64] = VecDeque.new();
    q.push_back(10);
    q.push_back(20);
    q.push_front(5);
    let mut sum = 0i64;
    for x in q.iter() { sum = sum + x; }
    println(sum);
    println(q.len());
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "35\n3");
    }
}

#[test]
fn test_e2e_float_recip_and_angle_conversions() {
    // `recip` → `fdiv`, `to_degrees`/`to_radians` → `fmul` by Rust's exact
    // constants. AOT output must match the interpreter oracle
    // (`test_float_recip_and_angle_conversions`) bit-for-bit, including the
    // irrational conversions.
    if let Some(out) = run_program(
        r#"
fn main() {
    println((4.0f64).recip());
    println((0.5f64).recip());
    println((0.0f64).to_degrees());
    println((0.0f64).to_radians());
    println((1.0f64).to_radians());
    println((1.0f64).to_degrees());
}
"#,
    ) {
        assert_eq!(
            out,
            "0.25\n2\n0\n0\n0.017453292519943295\n57.29577951308232\n"
        );
    }
}

#[test]
fn test_e2e_float_copysign_and_fract() {
    // `copysign` → `llvm.copysign`, `fract` → `fsub x, llvm.trunc(x)`. Both
    // are exact IEEE ops, so AOT output matches the interpreter oracle
    // (`test_float_copysign_and_fract`) bit-for-bit, including negative
    // `fract` (sign-preserving) and the irrational `0.1.fract()`.
    if let Some(out) = run_program(
        r#"
fn main() {
    println((3.5f64).copysign(-1.0f64));
    println((-3.5f64).copysign(1.0f64));
    println((2.75f64).fract());
    println((-2.75f64).fract());
    println((5.0f64).fract());
    println((0.1f64).fract());
}
"#,
    ) {
        assert_eq!(out, "-3.5\n3.5\n0.75\n-0.75\n0\n0.1\n");
    }
}

#[test]
fn test_e2e_signum_signed_int_and_float() {
    // `x.signum()`: ints lower to a nested `select` (signed `icmp`), floats
    // to `copysign(1.0, x)` guarded by a NaN check. AOT output must match
    // the interpreter oracle (`test_signum_signed_int_and_float`),
    // including the signed-zero and NaN edges.
    if let Some(out) = run_program(
        r#"
fn main() {
    println((42i64).signum());
    println((-42i64).signum());
    println((0i64).signum());
    println((0i32 - 7i32).signum());
    println((3.5f64).signum());
    println((-3.5f64).signum());
    println((0.0f64).signum());
    let z: f64 = 0.0 * (0.0 - 1.0);
    println(z.signum());
    let n: f64 = (0.0 - 1.0).sqrt();
    println(n.signum());
}
"#,
    ) {
        assert_eq!(out, "1\n-1\n0\n-1\n1\n-1\n1\n-1\nNaN\n");
    }
}

#[test]
fn test_e2e_int_float_min_max() {
    // `a.min(b)` / `a.max(b)` on numeric scalars: ints lower to `select` on
    // signed/unsigned `icmp`, floats to `llvm.minnum`/`llvm.maxnum`. The AOT
    // output must match the interpreter oracle
    // (`test_int_float_min_max`).
    if let Some(out) = run_program(
        r#"
fn main() {
    println(7i64.min(3i64));
    println(7i64.max(3i64));
    println((0 - 5i64).max(0i64));
    let x: f64 = 1.5;
    let y: f64 = 2.5;
    println(x.min(y));
    println(x.max(y));
    let u: u8 = 200;
    println(u.min(100u8));
    let w: u32 = 4000000000;
    println(w.max(1u32));
}
"#,
    ) {
        assert_eq!(out, "3\n7\n0\n1.5\n2.5\n100\n4000000000\n");
    }
}

#[test]
fn test_e2e_int_float_clamp_method() {
    // `v.clamp(lo, hi)` lowers to nested `select`s (icmp for ints, ordered
    // fcmp for floats). AOT output must match the interpreter oracle
    // (`test_int_float_clamp_method`), including the inverted-range
    // low-wins case (`7.clamp(10, 5)` → 10).
    if let Some(out) = run_program(
        r#"
fn main() {
    println(15i64.clamp(0i64, 10i64));
    println((0 - 3i64).clamp(0i64, 10i64));
    println(5i64.clamp(0i64, 10i64));
    println(7i64.clamp(10i64, 5i64));
    let x: f64 = 1.5;
    println(x.clamp(2.0, 3.0));
    let y: f64 = 2.5;
    println(y.clamp(0.0, 2.0));
    let u: u8 = 200;
    println(u.clamp(0u8, 100u8));
    let w: u32 = 4000000000;
    println(w.clamp(1u32, 4294967295u32));
}
"#,
    ) {
        assert_eq!(out, "10\n0\n5\n10\n2\n2\n100\n4000000000\n");
    }
}

#[test]
fn test_e2e_abs_int_min_traps() {
    // `iN::MIN.abs()` is the one input with no representable result —
    // it must trap as integer overflow, matching the interpreter.
    // Built via `let mut` + reassignment so the fault isn't const-folded.
    let captured = run_program_capturing(
        r#"
fn main() {
    let mut x = -9223372036854775807;
    x = x - 1;
    println(x.abs());
}
"#,
    );
    if let Some(c) = captured {
        assert!(
            c.stderr.contains("integer overflow"),
            "expected integer-overflow trap on iN::MIN.abs(), got stdout={:?}",
            c.stdout
        );
        assert!(
            !c.stdout.contains("-9223372036854775808"),
            "the un-negated MIN value must not print"
        );
    }
}

// ── Float↔int conversion codegen (phase-8 cast slice 4) ────────────

#[test]
fn test_ir_float_as_int_is_saturating() {
    // `f as iN` must lower to the saturating intrinsic, not raw `fptosi`
    // (which is poison on out-of-range). Signed → fptosi.sat, unsigned →
    // fptoui.sat.
    let ir_s = ir_for("fn f(x: f64) -> i32 { x as i32 }");
    assert!(
        ir_s.contains("llvm.fptosi.sat.i32.f64"),
        "signed float→int as-cast should use fptosi.sat:\n{ir_s}"
    );
    let ir_u = ir_for("fn f(x: f64) -> u8 { x as u8 }");
    assert!(
        ir_u.contains("llvm.fptoui.sat.i8.f64"),
        "unsigned float→int as-cast should use fptoui.sat:\n{ir_u}"
    );
}

#[test]
fn test_ir_saturating_to_uses_sat_intrinsic() {
    let ir = ir_for("fn f(x: f64) -> i32 { x.saturating_to_i32() }");
    assert!(ir.contains("llvm.fptosi.sat.i32.f64"), "{ir}");
    let ir_u = ir_for("fn f(x: f64) -> u8 { x.saturating_to_u8() }");
    assert!(ir_u.contains("llvm.fptoui.sat.i8.f64"), "{ir_u}");
}

#[test]
fn test_e2e_float_as_int_saturates() {
    // The headline: `f as iN` saturates (was raw fptosi / UB pre-slice-4).
    let out = run_program(
        r#"
fn main() {
    println(1e30f64 as i32);
    println(-1e30f64 as i32);
    println(1e30f64 as u8);
    println(-1.0f64 as u8);
    println(f64.NAN as i32);
    println(f64.INFINITY as i64);
    println(f64.NEG_INFINITY as i64);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(
            out.trim(),
            "2147483647\n-2147483648\n255\n0\n0\n9223372036854775807\n-9223372036854775808"
        );
    }
}

#[test]
fn test_e2e_saturating_to_int_family() {
    let out = run_program(
        r#"
fn main() {
    println((3.7f64).saturating_to_i32());
    println((1e30f64).saturating_to_i32());
    println((-1e30f64).saturating_to_i32());
    println((1e30f64).saturating_to_u8());
    println((-1.0f64).saturating_to_u8());
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "3\n2147483647\n-2147483648\n255\n0");
    }
}

#[test]
fn test_e2e_overflow_arith_family() {
    // C2 (B-2026-06-19-10): `{checked,saturating,overflowing}_{add,sub,mul}`
    // now lower in codegen via `llvm.{s,u}{op}.with.overflow.iN`. This is the
    // exact program (and expected output) of the interpreter test
    // `test_checked_saturating_overflowing_arith` — A/B parity across all
    // three families, widths (i32/i64/u8/u32), and signedness.
    let out = run_program(
        r#"
fn main() {
    let a = 2000000000i32;
    match a.checked_add(2000000000i32) { Some(v) => println(v), None => println(-1i32) }
    match a.checked_add(100i32) { Some(v) => println(v), None => println(-1i32) }
    println(a.saturating_add(2000000000i32));
    let u: u8 = 3u8;
    println(u.saturating_sub(10u8));
    let pair = a.overflowing_add(2000000000i32);
    println(pair.0);
    if pair.1 { println(1i32); } else { println(0i32); }
    let big = 9000000000000000000i64;
    match big.checked_add(big) { Some(v) => println(v), None => println(-7i64) }
    let w: u32 = 4000000000u32;
    println(w.checked_mul(2u32).is_none());
    println(w.saturating_add(1000000000u32));
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(
            out.trim(),
            "-1\n2000000100\n2147483647\n0\n-294967296\n1\n-7\ntrue\n4294967295"
        );
    }
}

#[test]
fn test_e2e_overflow_arith_signed_saturating_signs() {
    // The trickiest C2 path: signed saturating_{mul,sub,add} pick SMAX vs
    // SMIN by the sign of the true result. Pins each branch in codegen.
    let out = run_program(
        r#"
fn main() {
    println((100i8).saturating_mul(2i8));
    println((-100i8).saturating_mul(2i8));
    println((-100i8).saturating_mul(-2i8));
    println((100i8).saturating_mul(-2i8));
    println((-100i8).saturating_sub(100i8));
    println((100i8).saturating_sub(-100i8));
    println((-100i8).saturating_add(-100i8));
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "127\n-128\n127\n-128\n-128\n127\n-128");
    }
}

#[test]
fn test_e2e_wrapping_to_int_family() {
    // Modular truncation: 300 → 44 in i8, 256 → 0 / 257 → 1 in u8.
    let out = run_program(
        r#"
fn main() {
    println((300.0f64).wrapping_to_i8());
    println((256.0f64).wrapping_to_u8());
    println((257.9f64).wrapping_to_u8());
    println((-3.7f64).wrapping_to_i32());
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "44\n0\n1\n-3");
    }
}

#[test]
fn test_e2e_checked_to_int_family() {
    // `Some(trunc)` in range; `None` on out-of-range / NaN / negative→unsigned.
    let out = run_program(
        r#"
fn main() {
    let a: Option[i32] = (1.5f64).checked_to_i32();
    match a { Some(v) => println(v), None => println(-1) };
    let b: Option[i32] = (1e30f64).checked_to_i32();
    match b { Some(v) => println(v), None => println(-1) };
    let c: Option[i32] = (f64.NAN).checked_to_i32();
    match c { Some(v) => println(v), None => println(-1) };
    let d: Option[u8] = (-1.0f64).checked_to_u8();
    match d { Some(v) => println(v), None => println(-1) };
    let e: Option[u8] = (200.0f64).checked_to_u8();
    match e { Some(v) => println(v), None => println(-1) };
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "1\n-1\n-1\n-1\n200");
    }
}

#[test]
fn test_e2e_struct_display_print_and_to_string() {
    if let Some(out) = run_program(
        r#"
#[derive(Display)]
struct Cell { tag: char, n: u32, f: f64 }
fn main() {
    let c = Cell { tag: 'Z', n: 42, f: 1.5 };
    println(c);
    println(c.to_string());
    println(f"c={c}");
}
"#,
    ) {
        assert_eq!(
            out,
            "Cell { tag: Z, n: 42, f: 1.5 }\n\
                 Cell { tag: Z, n: 42, f: 1.5 }\n\
                 c=Cell { tag: Z, n: 42, f: 1.5 }\n"
        );
    }
}

#[test]
fn test_e2e_int_overflow_add_traps() {
    let captured = run_program_capturing(
        r#"
fn main() {
    let mut x = 9223372036854775806;
    x = x + 1;
    x = x + 1;
    println(x);
}
"#,
    );
    if let Some(c) = captured {
        assert!(
            c.stderr.contains("integer overflow"),
            "expected integer-overflow panic, got stdout={:?} stderr={:?}",
            c.stdout,
            c.stderr
        );
        assert!(
            !c.stdout.contains("-9223372036854775808"),
            "wrapped value must not print"
        );
    }
}

#[test]
fn test_e2e_int_overflow_sub_traps() {
    let captured = run_program_capturing(
        r#"
fn main() {
    let mut x = -9223372036854775807;
    x = x - 1;
    x = x - 1;
    println(x);
}
"#,
    );
    if let Some(c) = captured {
        assert!(
            c.stderr.contains("integer overflow"),
            "expected integer-overflow panic on MIN - 1, got stdout={:?}",
            c.stdout
        );
    }
}

#[test]
fn test_e2e_int_overflow_mul_traps() {
    let captured = run_program_capturing(
        r#"
fn main() {
    let mut x = 4611686018427387904;
    x = x * 2;
    println(x);
}
"#,
    );
    if let Some(c) = captured {
        assert!(
            c.stderr.contains("integer overflow"),
            "expected integer-overflow panic on 2^62 * 2, got stdout={:?}",
            c.stdout
        );
    }
}

#[test]
fn test_e2e_min_div_neg_one_traps_as_overflow() {
    let captured = run_program_capturing(
        r#"
fn main() {
    let mut m = -9223372036854775807;
    m = m - 1;
    let mut d = -1;
    d = d + 0;
    let q = m / d;
    println(q);
}
"#,
    );
    if let Some(c) = captured {
        assert!(
            c.stderr.contains("integer overflow"),
            "expected integer-overflow panic on MIN / -1 (design.md: same \
                 error family as MAX + 1, distinct from division by zero), \
                 got stdout={:?}",
            c.stdout
        );
        assert!(
            !c.stdout.contains("division by zero"),
            "MIN / -1 must report overflow, not division by zero"
        );
    }
}

#[test]
fn test_e2e_min_mod_neg_one_traps_as_overflow() {
    let captured = run_program_capturing(
        r#"
fn main() {
    let mut m = -9223372036854775807;
    m = m - 1;
    let mut d = -1;
    d = d + 0;
    let r = m % d;
    println(r);
}
"#,
    );
    if let Some(c) = captured {
        assert!(
            c.stderr.contains("integer overflow"),
            "expected integer-overflow panic on MIN % -1, got stdout={:?}",
            c.stdout
        );
    }
}

#[test]
fn test_ir_add_emits_checked_overflow_intrinsic() {
    let ir = ir_for(
        r#"
fn main() {
    let mut x = 1;
    x = x + 41;
    println(x);
}
"#,
    );
    assert!(
        ir.contains("sadd.with.overflow"),
        "signed add must lower through llvm.sadd.with.overflow, IR:\n{ir}"
    );
    assert!(
        ir.contains("add.ovf.trap"),
        "overflow trap block must be emitted, IR:\n{ir}"
    );
}

#[test]
fn test_e2e_monotone_var_overflow_traps_before_assume_misleads() {
    // Soundness pin: the monotone update that would wrap MUST trap —
    // the assume is only sound because the wrapped value never exists.
    // k is index-used (so the assume IS emitted) and driven to
    // overflow by a large literal step.
    let captured = run_program_capturing(
        r#"
fn main() {
    let mut v: Vec[i64] = Vec.filled(4, 7);
    let mut k = 0;
    let mut i = 0;
    while i < 100 {
        if k < 4 {
            println(v[k]);
        }
        k = k + 4611686018427387904;
        i = i + 1;
    }
    println(k);
}
"#,
    );
    if let Some(c) = captured {
        assert!(
            c.stderr.contains("integer overflow"),
            "wrapping monotone update must trap, got stdout={:?}",
            c.stdout
        );
    }
}

// ── Level 2 crash diagnostics (design.md § Crash diagnostics) ──────
// When a source filename is threaded into codegen (the CLI build/run
// path), panics report `panic at <file>:<line>:<col> in <fn>: <msg>`.
// The location operands are compile-time constants — no runtime DWARF
// walk / symbolizer (which would re-add the dead-stripped ~57 KiB
// gimli/addr2line tree). Without a filename (most `run_program` tests),
// the bare `panic: <msg>` form is preserved — see the existing
// `test_e2e_vec_indexed_write_oob_panics` above.

/// B-2026-08-14-31 — a `Map` or `Set` reached through anything but a bound
/// name renders like a bound one, instead of printing its control pointer.
///
/// The identifier arms key off per-variable side tables, so a field, a call
/// result, a tuple element and an element of a `Vec[Map[..]]` all fell
/// through to the value-kind arms — where a Map/Set is one pointer and
/// nothing distinguishes it from any other pointer, so it printed AS one.
/// `f"{b.m}"` rendered `94259731420368` where `--interp` rendered
/// `{k: 1}`, with no diagnostic anywhere and a different number each run.
/// `compile_print`'s own header comment predicted this ("Map gets printed
/// as a raw address"); B-2026-07-28-12 closed it for `Vec` and left the
/// siblings open.
///
/// Every spelling that printed an address is here, in both the f-string and
/// the bare-`println` form, plus the bound local as the control they have
/// to match. The last line repeats the first — B-2026-08-14-30 is what
/// happens when a display arm takes ownership of a place expression, so a
/// field printed TWICE is the cheap standing check that this one does not.
#[test]
fn test_e2e_print_a_map_or_set_place_expression() {
    assert_eq!(
            run_program(
                "struct B { m: Map[String, i64], s: Set[String] }\n\
                 fn mkm() -> Map[String, i64] { let mut m: Map[String, i64] = Map.new(); m.insert(\"k\", 1); m }\n\
                 fn mks() -> Set[String] { let mut s: Set[String] = Set.new(); s.insert(\"e\"); s }\n\
                 fn main() {\n\
                     let mut m: Map[String, i64] = Map.new();\n\
                     m.insert(\"k\", 1);\n\
                     let mut st: Set[String] = Set.new();\n\
                     st.insert(\"e\");\n\
                     let b = B { m: m, s: st };\n\
                     println(f\"{b.m}\");\n\
                     println(b.m);\n\
                     println(f\"{b.s}\");\n\
                     println(b.s);\n\
                     println(f\"{mkm()}\");\n\
                     println(f\"{mks()}\");\n\
                     let mut m2: Map[String, i64] = Map.new();\n\
                     m2.insert(\"k\", 1);\n\
                     let t = (m2, 1);\n\
                     println(f\"{t.0}\");\n\
                     let mut m3: Map[String, i64] = Map.new();\n\
                     m3.insert(\"k\", 1);\n\
                     let v: Vec[Map[String, i64]] = [m3];\n\
                     println(f\"{v[0]}\");\n\
                     println(f\"{b.m}\");\n\
                 }"
            )
            .as_deref(),
            Some("{k: 1}\n{k: 1}\nSet{e}\nSet{e}\n{k: 1}\nSet{e}\n{k: 1}\n{k: 1}\n{k: 1}\n"),
        );
}

/// B-2026-08-14-30 — printing a `Vec` read out of a PLACE prints it, instead
/// of freeing the container's buffer out from under it.
///
/// Both Vec display paths materialized the value into a temp and then took
/// ownership of it, on the reasoning that the identifier arm handles a bound
/// `Vec` so everything else must be a fresh temporary. A place expression is
/// neither: `b.xs`, `b.nested[0]` and a shared node's field all read storage
/// something else owns, and `compile_expr` yields that container's own
/// `{ptr, len, cap}`. So the buffer was freed twice — `println(b.xs)` on a
/// `Vec[i64]` field aborted with `free(): double free detected`, and a
/// `Vec[String]` or nested `Vec` SEGFAULTED because the deep drain walked
/// elements it did not own.
///
/// Every line here is a shape that crashed, plus the two PRODUCERS (a
/// literal and a call result) that must still be dropped — the fix
/// enumerates producers rather than excluding places, so this asserts both
/// halves. The last line repeats the first: printing a place TWICE was the
/// shape that turned the double free into a segfault, and it proves the
/// field is still intact after the first print.
#[test]
fn test_e2e_print_a_vec_place_expression() {
    assert_eq!(
        run_program(
            "struct B { xs: Vec[String], ns: Vec[i64], nested: Vec[Vec[i64]] }\n\
                 shared struct S { xs: Vec[String] }\n\
                 fn mk() -> Vec[String] { [\"x\", \"y\"] }\n\
                 fn main() {\n\
                     let b = B { xs: [\"a\", \"b\"], ns: [1, 2, 3], nested: [[1, 2], [3]] };\n\
                     let s = S { xs: [\"p\", \"q\"] };\n\
                     println(b.xs);\n\
                     println(f\"{b.xs}\");\n\
                     println(b.ns);\n\
                     println(f\"{b.nested}\");\n\
                     println(f\"{b.nested[0]}\");\n\
                     println(f\"{s.xs}\");\n\
                     println([9, 8]);\n\
                     println(mk());\n\
                     println(f\"{b.xs}\");\n\
                 }"
        )
        .as_deref(),
        Some("[a, b]\n[a, b]\n[1, 2, 3]\n[[1, 2], [3]]\n[1, 2]\n[p, q]\n[9, 8]\n[x, y]\n[a, b]\n"),
    );
}

/// B-2026-08-14-19 — a `String.substring` cut that lands INSIDE a codepoint
/// faults, instead of handing back the raw bytes.
///
/// The run-vs-build split this closes was not about which garbage came
/// back: the two garbages had DIFFERENT LENGTHS. `"日本語".substring(0, 2)`
/// measured `3 3 1` under `--interp` (one U+FFFD) against `2 2 2` compiled
/// (two truncated bytes, with the continuation byte counted as a codepoint),
/// so a loop that sliced and measured terminated differently on the two
/// backends — and the compiled side put invalid UTF-8 on stdout, which
/// design.md's UTF-8 `String` is not allowed to hold.
///
/// Rejecting is what reconciles them. The interpreter raises the same error
/// (`tests/interpreter.rs::test_substring_non_codepoint_boundary_faults`),
/// so run and build agree by both refusing rather than by one adopting the
/// other's garbage.
#[test]
fn test_e2e_substring_non_codepoint_boundary_panics() {
    let captured = run_program_capturing_with_filename(
        "fn main() {\n\
                 let s = \"\u{65e5}\u{672c}\u{8a9e}\";\n\
                 let a = s.substring(0i64, 2i64);\n\
                 println(a.len());\n\
             }",
        "cut.kara",
    );
    if let Some(c) = captured {
        assert!(
            c.stderr.contains("panic at ") && c.stderr.contains("cut.kara:"),
            "expected a located panic, got stdout={:?} stderr={:?}",
            c.stdout,
            c.stderr
        );
        assert!(
            c.stderr.contains("not a UTF-8 codepoint boundary"),
            "expected the boundary diagnosis, got stdout={:?}",
            c.stdout
        );
        // The fault must happen INSTEAD of the read, not after it.
        assert!(
            !c.stdout.contains("\n2\n"),
            "substring returned a value before faulting: stdout={:?}",
            c.stdout
        );
    }
}

#[test]
fn test_e2e_char_literal_value_round_trip() {
    // Regression guard for the pre-fix `CharLit → 0` gap: the
    // codepoint cast to i64 must be the actual value (65 for 'A'),
    // not zero. Uses an explicit cast so we're checking the value
    // rather than the print path.
    let out = run_program(
        r#"
fn main() {
    let c: char = 'A';
    let n: i64 = c as i64;
    println(n);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "65");
    }
}

#[test]
fn test_e2e_array_for_print_each() {
    let out = run_program(
        r#"
fn main() {
    let a = [1, 2, 3];
    for x in a {
        println(x);
    }
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["1", "2", "3"]);
    }
}

#[test]
fn test_e2e_int_from_widening() {
    let out = run_program(
        r#"
fn main() {
    let x: i32 = 7;
    let y: i64 = i64.from(x);
    println(y);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "7");
    }
}

#[test]
fn test_e2e_question_triggers_scope_cleanup() {
    // A Vec on the stack must be freed before ? returns early.
    // We can't directly observe the free, but we can verify the program
    // does not crash and the early-return path does run.
    let out = run_program(
        r#"
fn boom(flag: bool) -> Result[i64, i64] {
    if flag { Ok(1_i64) } else { Err(7_i64) }
}
fn use_vec(flag: bool) -> Result[i64, i64] {
    let mut v: Vec[i64] = Vec.new();
    v.push(10_i64);
    v.push(20_i64);
    v.push(30_i64);
    let _ = boom(flag)?;
    Ok(v.len() as i64)
}
fn main() {
    match use_vec(false) {
        Ok(n) => println(n),
        Err(e) => println(e),
    }
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "7");
    }
}

// ── Map LLVM codegen E2E (Task 6) ─────────────────────────────────────────

#[test]
fn test_e2e_map_i64_insert_get_none() {
    // get on missing key → None (no output, just no crash)
    let out = run_program(
        r#"
fn main() {
    let mut m: Map[i64, i64] = Map.new();
    let v = m.get(42_i64);
    match v {
        Some(x) => println(x),
        None => println(0_i64),
    }
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "0");
    }
}

#[test]
fn test_e2e_map_i64_insert_get_some() {
    let out = run_program(
        r#"
fn main() {
    let mut m: Map[i64, i64] = Map.new();
    m.insert(10_i64, 99_i64);
    let v = m.get(10_i64);
    match v {
        Some(x) => println(x),
        None => println(0_i64),
    }
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "99");
    }
}

#[test]
fn test_e2e_map_i64_insert_returns_old() {
    // First insert → None; second insert with same key → Some(old)
    let out = run_program(
        r#"
fn main() {
    let mut m: Map[i64, i64] = Map.new();
    let first = m.insert(7_i64, 10_i64);
    match first {
        Some(x) => println(x),
        None => println(0_i64),
    }
    let second = m.insert(7_i64, 20_i64);
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
fn test_e2e_map_i64_remove_some_none() {
    let out = run_program(
        r#"
fn main() {
    let mut m: Map[i64, i64] = Map.new();
    m.insert(5_i64, 55_i64);
    let r1 = m.remove(5_i64);
    match r1 {
        Some(x) => println(x),
        None => println(0_i64),
    }
    let r2 = m.remove(5_i64);
    match r2 {
        Some(x) => println(x),
        None => println(0_i64),
    }
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["55", "0"]);
    }
}

#[test]
fn test_e2e_map_i64_contains_key() {
    let out = run_program(
        r#"
fn main() {
    let mut m: Map[i64, i64] = Map.new();
    m.insert(3_i64, 30_i64);
    println(m.contains_key(3_i64));
    println(m.contains_key(4_i64));
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["true", "false"]);
    }
}

#[test]
fn test_e2e_map_i64_len_is_empty() {
    let out = run_program(
        r#"
fn main() {
    let mut m: Map[i64, i64] = Map.new();
    println(m.is_empty());
    println(m.len());
    m.insert(1_i64, 10_i64);
    m.insert(2_i64, 20_i64);
    println(m.is_empty());
    println(m.len());
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["true", "0", "false", "2"]);
    }
}

#[test]
fn test_ir_map_i64_i64_len_has_linkonce_odr() {
    // §3.2 locked decision: every monomorphized collection symbol
    // gets `LinkOnceODR` linkage so cross-crate / cross-TU dupes
    // collapse at link time.
    let ir = ir_for(
        r#"
fn main() {
    let mut m: Map[i64, i64] = Map.new();
    println(m.len());
}
"#,
    );
    // LLVM IR shape: `define linkonce_odr i64 @karac_map_i64_i64_len(ptr ...)`.
    let define_line = ir
        .lines()
        .find(|l| l.contains("@karac_map_i64_i64_len") && l.starts_with("define"))
        .unwrap_or_else(|| panic!("could not find define for mono len; IR:\n{}", ir));
    assert!(
        define_line.contains("linkonce_odr"),
        "mono len should have linkonce_odr linkage; saw: {}",
        define_line
    );
}

#[test]
fn test_ir_map_i64_i64_len_body_is_direct_field_load() {
    // Slice 1b.1 — the mono len body drops the wrapper call to
    // `karac_map_len` and reads the KaracMap.len field directly
    // (offset 24, `#[repr(C)]` layout pinned by the runtime-side
    // `karac_map_field_offsets_match_codegen` unit test). The IR
    // for the mono len's body should contain a load i64 and no
    // call to the erased `karac_map_len` extern.
    let ir = ir_for(
        r#"
fn main() {
    let mut m: Map[i64, i64] = Map.new();
    println(m.len());
}
"#,
    );
    // Walk just the mono len's define block.
    let mut in_body = false;
    let mut body_lines: Vec<&str> = Vec::new();
    for line in ir.lines() {
        if line.starts_with("define") && line.contains("@karac_map_i64_i64_len") {
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
        "could not extract mono len body; IR:\n{}",
        ir
    );
    let body = body_lines.join("\n");
    assert!(
        body.contains("load i64"),
        "mono len should load the len field directly; body:\n{}",
        body
    );
    assert!(
        !body.contains("call") || !body.contains("@karac_map_len"),
        "mono len should not call the erased karac_map_len extern; body:\n{}",
        body
    );
}

#[test]
fn test_ir_map_char_uses_i32_key_size() {
    // Slice 2.0 — `llvm_type_for_name` now recognizes `"char"` as
    // i32 (Unicode scalar value, 4 bytes). Prior to this fix,
    // `Map[char, V].new()` allocated 8-byte key slots and the
    // runtime memcpy'd 4 bytes of stack-neighbor garbage with
    // each char key. This test pins the corrected emission.
    let ir = ir_for(
        r#"
fn main() {
    let mut m: Map[char, i64] = Map.new();
    m.insert('a', 1_i64);
    println(m.len());
}
"#,
    );
    let new_line = ir
        .lines()
        .find(|l| l.contains("call ptr @karac_map_new"))
        .unwrap_or_else(|| panic!("no karac_map_new call site; IR:\n{}", ir));
    // sizeof(i32) = 4 bytes is rendered as `ptrtoint (ptr
    // getelementptr (i32, ptr null, i32 1) to i64)` by inkwell's
    // size_of() codegen; sizeof(i64) = 8 as the `i64` form. Pin
    // both — key is i32, val is i64.
    assert!(
        new_line.contains("getelementptr (i32, ptr null, i32 1)"),
        "char-key Map.new() should pass key_size = sizeof(i32); saw: {}",
        new_line
    );
    assert!(
        new_line.contains("getelementptr (i64, ptr null, i32 1)"),
        "i64-value Map.new() should pass val_size = sizeof(i64); saw: {}",
        new_line
    );
}

#[test]
fn test_ir_map_i64_i64_len_emitted_once_per_module() {
    // Multiple `m.len()` sites on Map[i64, i64] should share a
    // single emission (the side-table cache returns the cached
    // FunctionValue on second hit).
    let ir = ir_for(
        r#"
fn main() {
    let mut a: Map[i64, i64] = Map.new();
    let mut b: Map[i64, i64] = Map.new();
    println(a.len());
    println(b.len());
}
"#,
    );
    let define_count = ir
        .lines()
        .filter(|l| l.contains("@karac_map_i64_i64_len") && l.starts_with("define"))
        .count();
    assert_eq!(
        define_count, 1,
        "mono len should be defined exactly once; IR:\n{}",
        ir
    );
}

#[test]
fn test_e2e_map_i64_for_loop_sum() {
    // Sum all values; key sum is deterministic (single entry)
    let out = run_program(
        r#"
fn main() {
    let mut m: Map[i64, i64] = Map.new();
    m.insert(1_i64, 100_i64);
    m.insert(2_i64, 200_i64);
    m.insert(3_i64, 300_i64);
    let mut total: i64 = 0;
    for (k, v) in m {
        total = total + v;
    }
    println(total);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "600");
    }
}

#[test]
fn test_e2e_map_index_get_existing_i64() {
    let out = run_program(
        r#"
fn main() {
    let mut m: Map[i64, i64] = Map.new();
    m.insert(7_i64, 42_i64);
    println(m[7_i64]);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "42");
    }
}

// ── Compound-key Map (List 2, item 2) ────────────────────────
// `Map[(K1, K2, …), V]` — codegen emits per-field-recursive hash and
// eq functions so each tuple component is hashed/compared via its
// own per-type fn (String hashes contents, i64 hashes raw bytes, …).

#[test]
fn test_e2e_map_tuple_string_int_key() {
    let out = run_program(
        r#"
fn main() {
    let mut m: Map[(String, i64), i64] = Map.new();
    m.insert(("alice", 1_i64), 100_i64);
    m.insert(("alice", 2_i64), 200_i64);
    m.insert(("bob",   1_i64), 300_i64);
    println(m.len());
    let v1 = m.get(("alice", 1_i64));
    match v1 {
        Some(x) => println(x),
        None => println(0_i64 - 1_i64),
    }
    let v2 = m.get(("bob", 1_i64));
    match v2 {
        Some(x) => println(x),
        None => println(0_i64 - 1_i64),
    }
    let v3 = m.get(("alice", 9_i64));
    match v3 {
        Some(x) => println(x),
        None => println(0_i64 - 1_i64),
    }
}
"#,
    );
    let out = out.expect("tuple-key codegen should not bail");
    let lines: Vec<&str> = out.trim().lines().collect();
    assert_eq!(lines, vec!["3", "100", "300", "-1"]);
}

#[test]
fn test_e2e_map_tuple_int_int_key() {
    let out = run_program(
        r#"
fn main() {
    let mut m: Map[(i64, i64), i64] = Map.new();
    m.insert((1_i64, 2_i64), 12_i64);
    m.insert((3_i64, 4_i64), 34_i64);
    m.insert((1_i64, 4_i64), 14_i64);
    println(m.len());
    println(m[(1_i64, 2_i64)]);
    println(m[(3_i64, 4_i64)]);
    println(m[(1_i64, 4_i64)]);
}
"#,
    );
    let out = out.expect("tuple-key codegen should not bail");
    let lines: Vec<&str> = out.trim().lines().collect();
    assert_eq!(lines, vec!["3", "12", "34", "14"]);
}

#[test]
fn test_e2e_map_prefix_literal_int_keys() {
    let out = run_program(
        r#"
fn main() {
    let m: Map[i64, i64] = Map[1_i64: 100_i64, 2_i64: 200_i64];
    println(m.len());
    println(m[2_i64]);
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["2", "200"]);
    }
}

// ── Display for collections (recursive codegen) ─────────────────
//
// Subtask 8 of the canonical Display bullet (phase-7-codegen.md §
// Phase 7.2). Each test exercises `compile_print`'s collection
// dispatch landed in subtask 7 against the per-type Display fns
// emitted by subtasks 1-6. Format expectations match the
// interpreter's `Value::Display` impl at `src/interpreter.rs:206`.
//
// Map iteration order is unspecified per `design.md` line 1588 — the
// codegen runtime walks the bucket array directly, so multi-entry
// map tests would be order-dependent. The map tests below stick to
// single-entry maps; multi-entry coverage is left to interpreter
// tests where the iteration is over an ordered Vec.

#[test]
fn test_e2e_display_vec_i64() {
    let out = run_program(
        r#"
fn main() {
    let mut v: Vec[i64] = Vec.new();
    v.push(1);
    v.push(2);
    v.push(3);
    println(v);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "[1, 2, 3]");
    }
}

#[test]
fn test_e2e_display_map_string_i64_singleton() {
    let out = run_program(
        r#"
fn main() {
    let mut m: Map[String, i64] = Map.new();
    m.insert("k", 42_i64);
    println(m);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "{k: 42}");
    }
}

#[test]
fn test_e2e_display_map_i64_i64_singleton() {
    let out = run_program(
        r#"
fn main() {
    let mut m: Map[i64, i64] = Map.new();
    m.insert(7_i64, 99_i64);
    println(m);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "{7: 99}");
    }
}

#[test]
fn test_e2e_display_vec_tuple_i64_i64() {
    // Vec[(i64, i64)] — exercises tuple Display recursion via the
    // Vec body's element dispatcher. Single-entry map keeps the
    // expected output deterministic.
    let out = run_program(
        r#"
fn main() {
    let mut m: Map[i64, i64] = Map.new();
    m.insert(1_i64, 10_i64);
    let es: Vec[(i64, i64)] = m.entries();
    println(es);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "[(1, 10)]");
    }
}

#[test]
fn test_e2e_display_vec_tuple_i64_string() {
    // Vec[(i64, String)] — heap-bearing field on the value side of a
    // tuple element. The tuple Display fn GEPs to the String slot at
    // offset 8 (after the i64 field) and recurses into String Display.
    let out = run_program(
        r#"
fn main() {
    let mut m: Map[i64, String] = Map.new();
    m.insert(1_i64, "hi");
    let es: Vec[(i64, String)] = m.entries();
    println(es);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "[(1, hi)]");
    }
}

// ── Set[T] LLVM codegen ─────────────────────────────────────────
//
// Subtask 6 of the canonical Set codegen bullet (phase-8-stdlib-floor.md
// search `Set[T] LLVM codegen`). Set[T] lowers to Map[T, ()] at codegen
// and reuses karac_map_*; tests cover insert / contains / remove / len /
// is_empty / clear / for-loop iteration. The union / intersection /
// difference methods (subtask 5) are deferred — they need per-type
// clone fn infrastructure for non-Copy elements — so the matching
// tests (`test_e2e_set_union`, etc.) are not yet present.

#[test]
fn test_e2e_set_i64_insert_contains() {
    let out = run_program(
        r#"
fn main() {
    let mut s: Set[i64] = Set.new();
    s.insert(1_i64);
    s.insert(2_i64);
    println(s.contains(1_i64));
    println(s.contains(2_i64));
    println(s.contains(99_i64));
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["true", "true", "false"]);
    }
}

#[test]
fn test_e2e_set_i64_insert_returns_bool() {
    // Set.insert returns true on fresh insert, false when value already
    // present (matches Rust HashSet::insert).
    let out = run_program(
        r#"
fn main() {
    let mut s: Set[i64] = Set.new();
    let a = s.insert(1_i64);
    let b = s.insert(1_i64);
    println(a);
    println(b);
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["true", "false"]);
    }
}

#[test]
fn test_e2e_set_i64_remove() {
    // Set.remove returns true when value existed, false otherwise.
    let out = run_program(
        r#"
fn main() {
    let mut s: Set[i64] = Set.new();
    s.insert(7_i64);
    let r1 = s.remove(7_i64);
    let r2 = s.remove(7_i64);
    println(r1);
    println(r2);
    println(s.contains(7_i64));
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["true", "false", "false"]);
    }
}

#[test]
fn test_e2e_set_i64_len_is_empty() {
    let out = run_program(
        r#"
fn main() {
    let mut s: Set[i64] = Set.new();
    println(s.is_empty());
    println(s.len());
    s.insert(1_i64);
    s.insert(2_i64);
    s.insert(3_i64);
    println(s.is_empty());
    println(s.len());
    s.insert(2_i64);
    println(s.len());
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["true", "0", "false", "3", "3"]);
    }
}

#[test]
fn test_e2e_set_i64_for_loop_sum() {
    // for x in s — iteration order is unspecified, so test against the
    // sum (which is order-independent).
    let out = run_program(
        r#"
fn main() {
    let mut s: Set[i64] = Set.new();
    s.insert(10_i64);
    s.insert(20_i64);
    s.insert(30_i64);
    let mut sum: i64 = 0;
    for x in s {
        sum = sum + x;
    }
    println(sum);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "60");
    }
}

#[test]
fn test_e2e_display_set_i64_singleton() {
    // Display subtask 5 of the canonical Display bullet (closed by Set
    // codegen subtasks 1-4). Format `Set{...}` matches the interpreter
    // at `src/interpreter.rs:292`. Single-entry set keeps the expected
    // output deterministic — multi-entry iteration order is unspecified.
    let out = run_program(
        r#"
fn main() {
    let mut s: Set[i64] = Set.new();
    s.insert(42_i64);
    println(s);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "Set{42}");
    }
}

#[test]
fn test_e2e_set_union_i64() {
    // Membership-based assertions (rather than printing the result set)
    // — runtime iteration order is unspecified for Map-backed sets.
    let out = run_program(
        r#"
fn main() {
    let mut a: Set[i64] = Set.new();
    a.insert(1_i64);
    a.insert(2_i64);
    a.insert(3_i64);
    let mut b: Set[i64] = Set.new();
    b.insert(3_i64);
    b.insert(4_i64);
    b.insert(5_i64);
    let u: Set[i64] = a.union(b);
    println(u.len());
    println(u.contains(1_i64));
    println(u.contains(2_i64));
    println(u.contains(3_i64));
    println(u.contains(4_i64));
    println(u.contains(5_i64));
    println(u.contains(99_i64));
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(
            lines,
            vec!["5", "true", "true", "true", "true", "true", "false"]
        );
    }
}

#[test]
fn test_e2e_set_intersection_i64() {
    let out = run_program(
        r#"
fn main() {
    let mut a: Set[i64] = Set.new();
    a.insert(1_i64);
    a.insert(2_i64);
    a.insert(3_i64);
    let mut b: Set[i64] = Set.new();
    b.insert(2_i64);
    b.insert(3_i64);
    b.insert(4_i64);
    let i: Set[i64] = a.intersection(b);
    println(i.len());
    println(i.contains(1_i64));
    println(i.contains(2_i64));
    println(i.contains(3_i64));
    println(i.contains(4_i64));
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["2", "false", "true", "true", "false"]);
    }
}

#[test]
fn test_e2e_set_difference_i64() {
    let out = run_program(
        r#"
fn main() {
    let mut a: Set[i64] = Set.new();
    a.insert(1_i64);
    a.insert(2_i64);
    a.insert(3_i64);
    let mut b: Set[i64] = Set.new();
    b.insert(2_i64);
    b.insert(3_i64);
    b.insert(4_i64);
    let d: Set[i64] = a.difference(b);
    println(d.len());
    println(d.contains(1_i64));
    println(d.contains(2_i64));
    println(d.contains(3_i64));
    println(d.contains(4_i64));
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["1", "true", "false", "false", "false"]);
    }
}

/// B-2026-08-14-20 — the `String -> bytes -> String` round trip, plus the
/// `Slice[T].to_vec()` that makes any borrowed view reachable to an
/// owned-container consumer.
///
/// Line 01 is the row's literal repro, which did not typecheck at all:
/// `String.bytes()` hands back a `Slice[u8]` and `String.from_utf8` was
/// signed over `Vec[u8]`, with nothing bridging them. 03 pins that the
/// widening did not cost the old spelling — an owned `Vec[u8]` still
/// passes, through the general `Slice[T] <- Vec[T]` call coercion — and 04
/// that invalid bytes still reach the `Err` arm rather than being silently
/// dropped on the floor.
///
/// The rest is `to_vec` shape coverage. 06 is the one that would catch a
/// header-aliasing implementation: the copy is written and the SOURCE is
/// read back, so returning a view of the same buffer fails here and
/// nowhere else. 09 is its nested twin (the interpreter's `Arc`-shared
/// cells make the shallow copy the easy mistake on that surface). 11
/// covers a `chunks` element and 13 a `Slice` PARAMETER, where the
/// interpreter's receiver is a type-erased `Value::Array` rather than a
/// `Value::Slice`. 07 and 12 are the range-index and `as_slice_mut`
/// receivers, both non-identifier: the whole `Slice` method block is
/// identifier-keyed, so a chained receiver reached no arm at all.
#[test]
fn test_e2e_string_bytes_round_trip_and_slice_to_vec() {
    let src = r#"
fn sum_slice(xs: Slice[i64]) -> i64 {
    let v = xs.to_vec();
    let mut t = 0i64;
    let mut i = 0i64;
    while i < v.len() { t = t + v[i]; i = i + 1i64; }
    t
}

fn main() {
    let s = "café";
    match String.from_utf8(s.bytes()) { Ok(t) => println(f"01 {t}"), Err(_) => println("01 bad") }
    match String.from_utf8(s.bytes().to_vec()) { Ok(t) => println(f"02 {t}"), Err(_) => println("02 bad") }

    let mut ov: Vec[u8] = Vec.new();
    ov.push(104u8); ov.push(105u8);
    match String.from_utf8(ov) { Ok(t) => println(f"03 {t}"), Err(_) => println("03 bad") }

    let mut bad: Vec[u8] = Vec.new();
    bad.push(255u8); bad.push(254u8);
    match String.from_utf8(bad.as_slice()) { Ok(t) => println(f"04 {t}"), Err(_) => println("04 err") }

    let nums: Vec[i64] = [10i64, 20i64, 30i64, 40i64];
    let sl = nums.as_slice();
    let copy = sl.to_vec();
    println(f"05 {copy.len()} {copy[0i64]} {copy[3i64]}");

    let mut copy2 = nums.as_slice().to_vec();
    copy2[0i64] = 99i64;
    println(f"06 {copy2[0i64]} {nums[0i64]}");

    let win = nums[1..3].to_vec();
    println(f"07 {win.len()} {win[0i64]} {win[1i64]}");

    let words: Vec[String] = ["alpha", "beta", "gamma"];
    let mut wc = words.as_slice().to_vec();
    wc[0i64] = "zulu";
    println(f"08 {wc.len()} {wc[0i64]} {wc[2i64]} {words[0i64]}");

    let rows: Vec[Vec[i64]] = [[1i64, 2i64], [3i64, 4i64]];
    let mut rc = rows.as_slice().to_vec();
    rc[0i64][0i64] = 77i64;
    println(f"09 {rc[0i64][0i64]} {rows[0i64][0i64]}");

    let empty = nums[2..2].to_vec();
    println(f"10 {empty.len()}");

    let cs = nums.as_slice().chunks(2i64);
    let c0 = cs[0i64];
    println(f"11 {c0.to_vec().len()} {cs[1i64].to_vec().len()}");

    let mut m: Vec[i64] = [7i64, 8i64];
    let mc = m.as_slice_mut().to_vec();
    println(f"12 {mc.len()} {mc[1i64]}");

    println(f"13 {sum_slice(nums)}");
}
"#;
    assert_eq!(
        run_program(src).as_deref(),
        Some(
            "01 café\n\
                 02 café\n\
                 03 hi\n\
                 04 err\n\
                 05 4 10 40\n\
                 06 99 10\n\
                 07 2 20 30\n\
                 08 3 zulu gamma alpha\n\
                 09 77 1\n\
                 10 0\n\
                 11 2 2\n\
                 12 2 8\n\
                 13 100\n"
        ),
    );
}

// ── Compound-payload enum codegen ─────────────────────────────
//
// Slice CP (Phase 7.2 — 2026-05-09) lights up multi-word payload
// round-trip for `enum E { V(String) }`, `enum E { V(Vec[T]) }`,
// user-struct payloads, and tag-gated mixed-width variants. Before
// this slice the construction path collapsed any non-primitive
// payload to a single zero word via `coerce_to_i64`'s catch-all.
// The 8 tests below pin the layout machinery: (1) `String`
// round-trip, (2) `Vec[i64]` round-trip via function dispatch
// (because pattern-bound Vec methods don't yet have elem-type
// re-registration — see CP slice's "Out of scope, still open"),
// (3) `Vec[(String, i64)]` (the Slice F `Json.Object` shape),
// (4) user-struct payload, (5) mixed-width V1 narrow path,
// (6) mixed-width V2 wide path, (7) two-string-variant payload-
// area sharing, (8) regression gate for `IoError.Other(String)`.

#[test]
fn test_compound_enum_string_payload_round_trip() {
    let out = run_program(
        r#"
enum E { V(String) }
fn main() {
    let e = V("alice");
    match e {
        V(s) => println(s),
    }
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "alice");
    }
}

#[test]
fn test_compound_enum_vec_payload_round_trip() {
    // Method dispatch on the bound `xs` is not registered with
    // `vec_elem_types` at match-arm bind time (the typechecker's
    // `pattern_binding_types` map is name-only, not parameterized);
    // route the Vec through a `ref Vec[i64]` parameter so the
    // existing function-arg path registers the elem type.
    let out = run_program(
        r#"
enum E { V(Vec[i64]) }
fn sum(xs: ref Vec[i64]) -> i64 {
    let mut acc = 0;
    for x in xs { acc = acc + x; }
    acc
}
fn main() {
    let mut v: Vec[i64] = Vec.new();
    v.push(7);
    v.push(8);
    let e = V(v);
    match e {
        V(xs) => println(sum(xs)),
    }
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "15");
    }
}

#[test]
fn test_compound_enum_vec_of_tuples_payload_round_trip() {
    // Slice F's `Json.Object` shape: `Vec[(String, i64)]`.
    // Tuples are compound aggregates, so this exercises the
    // recursive payload-word computation through the Vec layer
    // (Vec → 3 words; tuple-element type is ignored at the Vec
    // level since heap memory is the elem buffer).
    let out = run_program(
        r#"
enum E { V(Vec[(String, i64)]) }
fn count(xs: ref Vec[(String, i64)]) -> i64 {
    xs.len()
}
fn main() {
    let mut v: Vec[(String, i64)] = Vec.new();
    v.push(("alpha", 1));
    v.push(("beta", 2));
    let e = V(v);
    match e {
        V(xs) => println(count(xs)),
    }
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "2");
    }
}

#[test]
fn test_compound_enum_user_struct_payload_round_trip() {
    let out = run_program(
        r#"
struct Point { x: i64, y: i64 }
enum E { V(Point) }
fn main() {
    let p = Point { x: 3, y: 4 };
    let e = V(p);
    match e {
        V(q) => {
            println(q.x);
            println(q.y);
        }
    }
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["3", "4"]);
    }
}

#[test]
fn test_io_error_other_string_round_trip() {
    // The regression gate for Slice CP. Pins the previously-latent
    // gap where `coerce_to_i64`'s catch-all silently zeroed any
    // multi-word payload. The IoError prelude type isn't spliced
    // into `program.items` for parser-mode tests, so we mirror its
    // shape with `MyIoErr` to stand in for the round-trip
    // semantics. If this test ever regresses, the latent gap has
    // returned and the slice CP layout machinery has drifted.
    let out = run_program(
        r#"
enum MyIoErr {
    NotFound,
    PermissionDenied,
    Other(String),
}
fn main() {
    let e = MyIoErr.Other("disk full");
    match e {
        Other(msg) => println(msg),
        _ => println("wrong variant"),
    }
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "disk full");
    }
}

// ── Compound-payload tuple-payload destructure ──
//
// Theme 5 (2026-05-10) — the Tuple arm in `bind_pattern_values` +
// `reconstruct_payload_value` lights up `match e { V((a, b)) => ... }`
// for variant-payload tuples of arbitrary primitive / aggregate /
// recursive-tuple shape. The headline test above
// (`test_pattern_bound_nested_tuple_vec_payload`) pins the original
// `#[ignore]`'d cross-check; the four below exercise the full grid
// of element shapes (primitive×primitive, heap×primitive, nested
// tuples, three-element tuples).

#[test]
fn test_compound_tuple_payload_int_int() {
    // Smallest non-trivial case: two-i64 tuple. Verifies per-element
    // word-offset dispatch handles primitive payloads correctly
    // without depending on heap-bearing aggregates.
    let out = run_program(
        r#"
enum E { V((i64, i64)) }
fn main() {
    let e = V((7, 35));
    match e {
        V((a, b)) => println(a + b),
    }
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "42");
    }
}

// ── Primitive-type associated constants ──────────────────────
//
// Theme 7 (2026-05-10) — `i64.MAX` / `f64.INFINITY` / `usize.MAX`
// etc. dispatch through the shared `PRIMITIVE_CONSTS` table at
// `src/prelude.rs`. Codegen intercepts the `FieldAccess` arm at
// `compile_field_access` before falling through to the generic
// field-access path. Float widths preserved (f32 vs f64).

#[test]
fn test_codegen_primitive_const_i64_max() {
    let out = run_program("fn main() { let x = i64.MAX; println(x); }");
    if let Some(out) = out {
        assert_eq!(out.trim(), "9223372036854775807");
    }
}

#[test]
fn test_codegen_primitive_const_i64_min() {
    let out = run_program("fn main() { let x = i64.MIN; println(x); }");
    if let Some(out) = out {
        assert_eq!(out.trim(), "-9223372036854775808");
    }
}

#[test]
fn test_codegen_primitive_const_u64_max_bit_pattern_preserved() {
    // u64.MAX bit pattern is 0xFFFF_FFFF_FFFF_FFFF, and it now PRINTS as
    // 18446744073709551615.
    //
    // This test previously asserted "-1", with a comment calling the signed
    // rendering "a separate concern". It was not separate — it was
    // B-2026-08-11-21, and pinning it here is how a wrong answer survived:
    // `let x = u64.MAX` is an un-annotated binding, exactly the shape whose
    // type was never recorded, so codegen's `%llu`/`%lld` classifier fell
    // back to signed. The interpreter printed the right number the whole
    // time, so this assertion also pinned a run-vs-build divergence as
    // expected behaviour.
    //
    // Strict `assert_eq!` rather than the tolerant `if let Some(out)` it
    // used to have: a stale runtime archive must fail this loudly instead of
    // asserting nothing (CLAUDE.md).
    assert_eq!(
        run_program("fn main() { let x = u64.MAX; println(x); }").as_deref(),
        Some("18446744073709551615\n"),
    );
}

#[test]
fn test_codegen_primitive_const_usize_max() {
    // v1 is 64-bit only — usize.MAX == u64.MAX. Same correction as the u64
    // test above (B-2026-08-11-21); this one had pinned "-1" too.
    assert_eq!(
        run_program("fn main() { let x = usize.MAX; println(x); }").as_deref(),
        Some("18446744073709551615\n"),
    );
}

/// Phase-7 line 14 — object-file + linked-binary roundtrip. The
/// symbol must survive backend codegen, linking, and (where the
/// platform supports it) `--gc-sections`/`-dead_strip`. Skipped
/// gracefully when `libkarac_runtime.a` isn't built; the IR-shape
/// test above is the unconditional safety net.
#[test]
fn test_jit_template_section_roundtrip() {
    use karac::codegen::{compile_to_object, link_executable};
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);

    let src = r#"
fn main() {}
"#;
    let mut parsed = karac::parse(src);
    assert!(
        parsed.errors.is_empty(),
        "parse failed: {:?}",
        parsed.errors
    );
    let resolved = karac::resolve(&parsed.program);
    let typed = karac::typecheck(&parsed.program, &resolved);
    karac::lower(&mut parsed.program, &typed);

    let id = COUNTER.fetch_add(1, Ordering::Relaxed);
    let obj_path = format!("/tmp/karac_jit_tmpl_{}_{}.o", std::process::id(), id);
    let exe_path = format!("/tmp/karac_jit_tmpl_{}_{}", std::process::id(), id);

    compile_to_object(&parsed.program, &obj_path, None, None).expect("codegen failed");

    // Layer 2 — symbol visible in the object file.
    let nm_obj = std::process::Command::new("nm")
        .arg(&obj_path)
        .output()
        .expect("nm should be on PATH");
    let nm_obj_stdout = String::from_utf8_lossy(&nm_obj.stdout);
    assert!(
        nm_obj_stdout.contains("karac_jit_template_manifest"),
        "expected manifest symbol in object file; nm stdout:\n{nm_obj_stdout}",
    );

    // Layer 3 — symbol survives linking. Soft-skip if the runtime
    // archive isn't built (the link will fail without it).
    if link_executable(&obj_path, &exe_path).is_err() {
        let _ = std::fs::remove_file(&obj_path);
        eprintln!("jit-template roundtrip: link skipped (libkarac_runtime.a missing?)");
        return;
    }
    let nm_exe = std::process::Command::new("nm")
        .arg(&exe_path)
        .output()
        .expect("nm should be on PATH");
    let nm_exe_stdout = String::from_utf8_lossy(&nm_exe.stdout);
    assert!(
        nm_exe_stdout.contains("karac_jit_template_manifest"),
        "expected manifest symbol in linked executable; nm stdout:\n{nm_exe_stdout}",
    );

    // Layer 4 (Mach-O only) — the manifest must live inside
    // `__TEXT`, not a fresh `__KARA` segment. Regression guard for
    // the 2026-05-25 fix that reclaimed 16 KiB per binary by
    // parking the 4-byte manifest in `__TEXT` instead of letting
    // it allocate its own page-aligned segment. If anyone moves
    // the manifest back into a custom segment, the `__KARA`
    // segment will reappear in `otool -l` and this assertion
    // catches it. Soft-skip if `otool` isn't on PATH (e.g. CI
    // running ELF cross-compile).
    if cfg!(target_vendor = "apple") {
        if let Ok(otool) = std::process::Command::new("otool")
            .arg("-l")
            .arg(&exe_path)
            .output()
        {
            let otool_stdout = String::from_utf8_lossy(&otool.stdout);
            assert!(
                !otool_stdout.contains("segname __KARA"),
                "manifest must live in `__TEXT`, not a fresh `__KARA` \
                     segment (fresh segments cost 16 KiB per binary for a \
                     4-byte payload). otool -l stdout:\n{otool_stdout}",
            );
            assert!(
                otool_stdout.contains("sectname __jittmpl"),
                "expected `__jittmpl` section in linked binary; otool -l \
                     stdout:\n{otool_stdout}",
            );
        }
    }

    let _ = std::fs::remove_file(&obj_path);
    let _ = std::fs::remove_file(&exe_path);
}

#[test]
fn test_state_struct_type_primitive_typed_param_uses_i64_fallback() {
    // A primitive-typed param (`i64`) has no recorded `type_name` in
    // `pattern_binding_types`, so the layout's `type_name` is `None`
    // and codegen falls back to `i64`. State struct = `{ i32, i64 }`.
    let ir = ir_for_with_state_struct_layouts(
        "effect resource Network;
             pub fn fetch() with sends(Network) receives(Network) {}
             fn driver(n: i64) { fetch(); }",
    );
    let line = ir
        .lines()
        .find(|l| l.starts_with("%kara.state.driver = type {"))
        .unwrap_or_else(|| panic!("no driver state struct type def:\n{ir}"));
    assert!(
        line.contains("i32, i64"),
        "expected tag + i64 fallback for primitive param: {line}"
    );
}

// ── Phase 6 line 26 slice 8i: non-unit returns through terminal field ─
//
// When a network-boundary function has a non-unit return type, the
// state struct gains a terminal field appended after the captured-
// local fields, the terminal arm of the poll-fn writes a placeholder
// into that field before Ready, and caller-side intercepts load the
// field as the call's return value. v1 records `i64` returns only;
// other return types stay on the unit-return path until follow-on
// slices widen the supported set.

#[test]
fn test_return_value_state_struct_includes_terminal_i64_field() {
    // `fn driver() -> i64 with sends(Network) receives(Network) { fetch(); 0 }`
    // — the state struct gains a terminal i64 field after the
    // captured-local fields (none here, so the struct is { i32 tag,
    // i64 return }).
    let ir = ir_for_with_state_struct_layouts(
        "effect resource Network;
             pub fn fetch() with sends(Network) receives(Network) {}
             fn driver() -> i64 with sends(Network) receives(Network) { fetch(); 0 }",
    );
    let line = ir
        .lines()
        .find(|l| l.starts_with("%kara.state.driver = type {"))
        .unwrap_or_else(|| panic!("no driver state struct in IR:\n{ir}"));
    // Tag + terminal i64 — the struct definition line should be
    // `%kara.state.driver = type { i32, i64 }`.
    assert!(
        line.contains("i32, i64"),
        "state struct must include i32 tag + i64 terminal:\n{line}"
    );
}

// ── Phase 6 line 26 slice 8ai: widened state-machine return types ─────
//
// Slice 8i registered only `i64`-returning network-boundary fns into
// `state_machine_return_types`. Slice 8ai widens the supported set to
// every integer / float primitive, `bool`, `char`, `Vec[T]` /
// `VecDeque[T]` / `String` / `str` (`{ptr, i64, i64}` slice
// descriptor), `Slice[T]` (`{ptr, i64}`), and concrete user structs.
// Each affected fn's state struct gains a terminal field sized to
// the registered type; the terminal-arm placeholder fallback is now
// a typed `const_zero` rather than a hardcoded `i64 0`. The caller-
// side intercept (slice 8d / 8g) already loads through the typed
// entry — no change there. Tests verify state-struct shape + typed
// placeholder per type class.

#[test]
fn test_8ai_i32_return_state_struct_terminal_field_is_i32() {
    // Sources `x: i32` through a parameter rather than a bare
    // literal because the integer-literal inference path lowers
    // `0` to `i64` regardless of expected return-type context —
    // an orthogonal typechecker / codegen gap not in slice 8ai's
    // scope. The parameter form bypasses literal-inference and
    // pins what slice 8ai actually tests: the registered
    // terminal-field type for an `i32`-returning fn is `i32`.
    let ir = ir_for_with_state_struct_layouts(
        "effect resource Network;
             pub fn fetch() with sends(Network) receives(Network) {}
             fn driver(x: i32) -> i32 with sends(Network) receives(Network) { fetch(); x }",
    );
    let line = ir
        .lines()
        .find(|l| l.starts_with("%kara.state.driver = type {"))
        .unwrap_or_else(|| panic!("no driver state struct in IR:\n{ir}"));
    assert!(
        line.contains(", i32 }") && line.starts_with("%kara.state.driver = type { i32"),
        "i32 return: state struct must terminate with the i32 terminal field:\n{line}"
    );
    let body = extract_fn_ir(&ir, "__kara_poll_driver");
    // Final expression `x` is a captured-local i32 — slice 8ah
    // recognises it as a `Slot` and emits a slot-load + store.
    assert!(
        body.contains("store i32 %x.return, ptr %kara.return.field_ptr"),
        "i32 return: terminal arm must store typed i32 from x.slot:\n{body}"
    );
}

// ── Phase 6 line 26 slice 8k: args-bearing free-fn body-splitting ─
//
// Extends slice 8h's free-function body-splitting to accept calls
// whose args are recognised shapes (integer literal or captured-
// local identifier reference). The per-arm slot map from slice 8j
// provides the variable backing store for identifier args.

// ── Phase 6 line 26 slice 8m: arm-local let-bindings ─────────────────
//
// Lets introduced inside an arm body (between yields) get an arm-local
// alloca slot, with the binding name registered into the per-arm slot
// map so subsequent calls in the same arm can reference it. v1 lowers
// every slot as `i64` (state-struct primitive fallback); let-bindings
// don't survive across yields without state-struct write-back (a
// follow-on slice). RHS shapes follow the slice-8k `BodyArg`
// discipline (integer literal or in-scope identifier).

#[test]
fn test_body_splitting_8m_let_int_lit_then_call_uses_slot() {
    // `fn driver() with sends(Network) { fetch(); let x = 42; take(x); }`
    // — terminal arm allocates `%x.slot`, stores 42, then `take(x)`
    // loads the slot and passes it as the call arg.
    let ir = ir_for_with_state_struct_layouts(
        "effect resource Network;
             pub fn fetch() with sends(Network) receives(Network) {}
             fn take(n: i64) {}
             fn driver() with sends(Network) receives(Network) {
                 fetch();
                 let x = 42;
                 take(x);
             }",
    );
    let body = extract_fn_ir(&ir, "__kara_poll_driver");
    // Alloca + store + load + call shape.
    assert!(
        body.contains("%x.slot = alloca i64"),
        "let x must alloca an i64 slot:\n{body}"
    );
    assert!(
        body.contains("store i64 42, ptr %x.slot"),
        "let x = 42 must store literal into slot:\n{body}"
    );
    assert!(
        body.contains("call void @take(i64 %x.arg)"),
        "take(x) must pass the loaded slot value:\n{body}"
    );
}

#[test]
fn test_body_splitting_8r_bitwise_compound_assign_silently_dropped() {
    // `n &= 1;` — bitwise compound op outside slice-8r's recognised
    // set. The walker drops the statement; no `and i64` / `or i64`
    // / `xor i64` / `shl i64` / `ashr i64` appears in the poll-fn
    // body. The slice-8n writeback still fires for the untouched
    // slot (a value-equivalent no-op via slice 8a's reload).
    let ir = ir_for_with_state_struct_layouts(
        "effect resource Network;
             pub fn fetch() with sends(Network) receives(Network) {}
             fn driver(n: i64) with sends(Network) receives(Network) {
                 n &= 1;
                 fetch();
             }",
    );
    let body = extract_fn_ir(&ir, "__kara_poll_driver");
    assert!(
        !body.contains("and i64"),
        "bitwise-AND compound-assign must skip lowering:\n{body}"
    );
    // No binary-result store into n.slot beyond the slice-8a reload
    // store. The reload store carries the value `%n.reload`; the
    // skipped compound-assign would have stored `%binop.assign_rhs`.
    assert!(
        !body.contains("%binop.assign_rhs"),
        "skipped compound-assign must not materialise a binop:\n{body}"
    );
}

#[test]
fn test_body_splitting_8s_let_int_lit_alloca_stays_i64() {
    // Regression guard: integer-literal RHS still alloca's i64 —
    // slice 8s is value-driven, and IntLit materialises to i64
    // const, so the slot type stays i64. Reuses slice 8m's
    // `let x = 42` shape.
    let ir = ir_for_with_state_struct_layouts(
        "effect resource Network;
             pub fn fetch() with sends(Network) receives(Network) {}
             fn driver() with sends(Network) receives(Network) {
                 fetch();
                 let x = 42;
             }",
    );
    let body = extract_fn_ir(&ir, "__kara_poll_driver");
    assert!(
        body.contains("%x.slot = alloca i64"),
        "let x = 42 must still alloca an i64 slot (IntLit defaults to i64):\n{body}"
    );
    assert!(
        body.contains("store i64 42, ptr %x.slot"),
        "let x = 42 must store the i64 literal:\n{body}"
    );
}

#[test]
fn test_body_splitting_8s_let_integer_binary_alloca_stays_i64() {
    // Regression guard: `let m = n + 1` where n is i64 —
    // materialise_body_arg's Binary arm emits an i64 add, so
    // value.get_type() is i64 and the slot stays i64. Reuses
    // slice 8q's shape.
    let ir = ir_for_with_state_struct_layouts(
        "effect resource Network;
             pub fn fetch() with sends(Network) receives(Network) {}
             fn driver(n: i64) with sends(Network) receives(Network) {
                 fetch();
                 let m = n + 1;
             }",
    );
    let body = extract_fn_ir(&ir, "__kara_poll_driver");
    assert!(
        body.contains("%m.slot = alloca i64"),
        "let m = n + 1 must alloca an i64 slot (binary result is i64):\n{body}"
    );
    assert!(
        body.contains("%binop.let_rhs = add i64 %n.let_rhs, 1"),
        "binary RHS must materialise as `add i64`:\n{body}"
    );
    assert!(
        body.contains("store i64 %binop.let_rhs, ptr %m.slot"),
        "binary result must store into m.slot as i64:\n{body}"
    );
}

// ── Phase 6 line 26 slice 8o: terminal-arm final-expression value ─────
//
// The terminal-arm store into the state-struct terminal field now
// uses the user's `body.final_expr` value when it's a recognised
// `BodyArg` shape (slice-8k discipline: integer literal or in-scope
// identifier). Slice 8i's placeholder `i64 0` survives only as a
// fallback for unrecognised final-exprs or absent final-exprs.

#[test]
fn test_terminal_return_8o_uses_int_literal_final_expr() {
    // `fn driver() -> i64 ... { fetch(); 42 }` — the user's
    // trailing `42` becomes the stored terminal value.
    let ir = ir_for_with_state_struct_layouts(
        "effect resource Network;
             pub fn fetch() with sends(Network) receives(Network) {}
             fn driver() -> i64 with sends(Network) receives(Network) { fetch(); 42 }",
    );
    let body = extract_fn_ir(&ir, "__kara_poll_driver");
    assert!(
        body.contains("store i64 42, ptr %kara.return.field_ptr"),
        "terminal arm must store user's literal 42 into terminal field:\n{body}"
    );
}

// ── Phase 6 line 26 slice 8l: args-bearing method-call body-splitting ─
//
// Mirrors slice 8k's free-fn arg compilation for `MethodCall` shapes:
// method args go through the same `BodyArg` recognition (literal int
// or captured-local identifier) and the same per-arm slot map. The
// receiver claims call position 0; args follow at 1..=N.

#[test]
fn test_body_splitting_8l_emits_method_with_int_literal_arg() {
    // `fn driver() { let h = Hub { count: 0 }; h.take(42); fetch(); }`
    // — `h.take(42)` lowers to `call void @Hub.take(ptr %h.slot, i64 42)`.
    let ir = ir_for_with_state_struct_layouts(
        "effect resource Network;
             pub fn fetch() with sends(Network) receives(Network) {}
             struct Hub { count: i64 }
             impl Hub { fn take(ref self, n: i64) {} }
             fn driver() with sends(Network) receives(Network) {
                 let h = Hub { count: 0 };
                 h.take(42);
                 fetch();
             }",
    );
    let body = extract_fn_ir(&ir, "__kara_poll_driver");
    assert!(
        body.contains("call void @Hub.take(ptr %h.slot, i64 42)"),
        "h.take(42) must pass receiver + literal arg in order:\n{body}"
    );
}

#[test]
fn test_body_splitting_8k_emits_int_literal_arg_call() {
    // `fn driver() { take(42); fetch(); }` — `take(42)` runs in
    // state_0; the literal `42` is materialised as an `i64` const
    // and passed to `@take` ahead of the tag-store.
    let ir = ir_for_with_state_struct_layouts(
        "effect resource Network;
             pub fn fetch() with sends(Network) receives(Network) {}
             fn take(n: i64) {}
             fn driver() with sends(Network) receives(Network) {
                 take(42);
                 fetch();
             }",
    );
    let body = extract_fn_ir(&ir, "__kara_poll_driver");
    assert!(
        body.contains("call void @take(i64 42)"),
        "take(42) literal arg must lower to `call void @take(i64 42)`:\n{body}"
    );
    let call_pos = body.find("call void @take(i64 42)").unwrap();
    let tag_store_pos = body
        .find("store i32 1, ptr %state_0.next_tag_ptr")
        .expect("state_0 must store next tag = 1");
    assert!(
        call_pos < tag_store_pos,
        "take(42) call must precede the tag-store in state_0:\n{body}"
    );
}

// ── `ref T` arg from a non-place rvalue ──────────────────────
//
// Pre-fix, `compile_call` only handled the `ExprKind::Identifier`
// shape inside its `is_ref` branch; literals, function returns,
// arithmetic results — anything without a binding — fell through
// to the raw-value path and emitted `call @f(i32 42)` / `call
// @f({ptr,i64,i64} %s)` against a `ptr` parameter. LLVM module
// verification rejected the mismatch. Surfaced by kara-katas
// leetcode #8 atoi, where each `main` test case had to be bound
// to a local (`let c1 = "42"; report(c1)`) to work around the
// bug. Fix materializes the rvalue into an entry-block alloca
// and passes its pointer so the callee sees the same ABI as the
// identifier fast-path. See `compile_call` in
// `codegen/call_dispatch.rs`.

#[test]
fn test_ir_ref_param_with_int_literal_rvalue() {
    let ir = ir_for(
        "fn take(n: ref i32) -> i32 { *n }\n\
             fn app_main() -> i32 { take(42i32) }",
    );
    // Callee declares ref i32 → first param is ptr.
    assert!(
        ir.contains("@take(ptr "),
        "callee should take a pointer for `ref i32`:\n{ir}"
    );
    // Caller materializes the literal and passes the alloca pointer
    // rather than the raw `i32 42` value the pre-fix path emitted.
    assert!(
        ir.contains("%ref_rvalue_arg0"),
        "ref-rvalue arg should be materialized to a named entry alloca:\n{ir}"
    );
    assert!(
        ir.contains("store i32 42, ptr %ref_rvalue_arg0"),
        "literal value should be stored into the temp before the call:\n{ir}"
    );
    assert!(
        ir.contains("call i32 @take(ptr %ref_rvalue_arg0)"),
        "call should pass the temp's pointer, not the raw integer:\n{ir}"
    );
}

#[test]
fn test_ir_string_bytes_indexing_reads_i8() {
    // Element type is u8 (i8 in LLVM IR). Indexing `bs[i]` GEPs
    // into the slice's data pointer with i8 stride and loads a
    // single byte. Regression guard for the slice-elem
    // inference that ties `let bs = s.bytes()` to u8 — without
    // it, indexing would fall back to i64 stride and read 8
    // bytes per index.
    let ir = ir_for(
        "fn f() -> i64 {\n\
                 let s = \"hello\";\n\
                 let bs = s.bytes();\n\
                 bs[0] as i64\n\
             }",
    );
    assert!(
        ir.contains("load i8"),
        "indexing a `Slice[u8]` should load i8, not i64:\n{ir}"
    );
}

#[test]
fn test_ir_match_on_string_uses_memcmp_not_int_cmp() {
    // Regression guard against the int-path fall-through. The
    // panic site used to be `lhs.into_int_value()` on the
    // String struct; after the fix, codegen routes through
    // `compile_string_binop` which emits `memcmp` calls for the
    // per-arm equality test. Find at least one `memcmp` in the
    // generated IR for the match body.
    let ir = ir_for(
        "fn pick(s: String) -> i64 {\n\
                 match s {\n\
                     \"x\" => 1,\n\
                     _ => 0,\n\
                 }\n\
             }",
    );
    assert!(
        ir.contains("call i32 @memcmp") || ir.contains("call ptr @memcmp"),
        "match-on-String should emit memcmp via compile_string_binop:\n{ir}"
    );
}

#[test]
fn test_e2e_i64_parse_basic() {
    let output = run_program(
        "fn main() {\n\
                 match i64.parse(\"42\") { Some(n) => println(n), None => println(-1) }\n\
                 match i64.parse(\"not a number\") { Some(n) => println(n), None => println(-1) }\n\
                 match i64.parse(\"-7\") { Some(n) => println(n), None => println(-1) }\n\
                 match i64.parse(\"  100  \") { Some(n) => println(n), None => println(-1) }\n\
                 match i64.parse(\"\") { Some(n) => println(n), None => println(-1) }\n\
             }",
    )
    .expect("compile + run failed");
    assert_eq!(output, "42\n-1\n-7\n100\n-1\n");
}

#[test]
fn test_e2e_numeric_try_from() {
    // Built-in numeric narrowing `T.try_from(x) -> Result[T, String]`
    // (design.md § Conversion Traits). Build==run parity with the
    // interpreter sibling `test_numeric_try_from_interpreter`. Covers
    // in-range Ok, narrowing Err, sign-change (negative → unsigned) Err,
    // widening always Ok, an unsigned target correctly printing a value that
    // overflows the signed target (a signedness-of-payload check), and the
    // `.try_into()` desugar. The `Err` String is a static (`cap=0`) value —
    // valgrind-clean (verified by hand).
    let output = run_program(
        "fn main() {\n\
                 match i8.try_from(100) { Ok(v) => println(v), Err(e) => println(e) }\n\
                 match i8.try_from(300) { Ok(v) => println(v), Err(e) => println(e) }\n\
                 match u8.try_from(-1) { Ok(v) => println(v), Err(e) => println(e) }\n\
                 match i64.try_from(42) { Ok(v) => println(v), Err(e) => println(e) }\n\
                 let big: i64 = 3000000000;\n\
                 match u32.try_from(big) { Ok(v) => println(v), Err(e) => println(e) }\n\
                 match i32.try_from(big) { Ok(v) => println(v), Err(e) => println(e) }\n\
                 let n: i32 = 70000;\n\
                 let r: Result[i16, String] = n.try_into();\n\
                 match r { Ok(v) => println(v), Err(e) => println(e) }\n\
             }",
    )
    .expect("compile + run failed");
    assert_eq!(
            output,
            "100\nout of range for i8\nout of range for u8\n42\n3000000000\nout of range for i32\nout of range for i16\n"
        );
}

#[test]
fn test_e2e_i64_from_str_radix_basic() {
    // Radix parse (hex/bin/oct + reject + invalid radix) — the
    // self-hosting lexer's hex/binary/octal literal path. Must match the
    // interpreter (test_i64_from_str_radix_interpreter).
    let output = run_program(
        "fn pr(o: Option[i64]) { match o { Some(n) => println(n), None => println(-1) } }\n\
             fn main() {\n\
                 pr(i64.from_str_radix(\"ff\", 16));\n\
                 pr(i64.from_str_radix(\"1010\", 2));\n\
                 pr(i64.from_str_radix(\"17\", 8));\n\
                 pr(i64.from_str_radix(\"zz\", 16));\n\
                 pr(i64.from_str_radix(\"7f\", 16));\n\
             }",
    )
    .expect("compile + run failed");
    assert_eq!(output, "255\n10\n15\n-1\n127\n");
}

#[test]
fn test_ir_i64_from_str_radix_calls_runtime_extern() {
    let ir = ir_for(
        "fn h(s: String) -> i64 {\n\
                 match i64.from_str_radix(s, 16) {\n\
                     Some(n) => n,\n\
                     None => -1,\n\
                 }\n\
             }",
    );
    assert!(
        ir.contains("call i8 @karac_runtime_parse_i64_radix"),
        "i64.from_str_radix should call the runtime extern:\n{ir}"
    );
}

#[test]
fn test_ir_i64_parse_calls_runtime_extern() {
    let ir = ir_for(
        "fn parse_id(s: String) -> i64 {\n\
                 match i64.parse(s) {\n\
                     Some(n) => n,\n\
                     None => -1,\n\
                 }\n\
             }",
    );
    assert!(
        ir.contains("call i8 @karac_runtime_parse_i64"),
        "i64.parse should call the runtime extern:\n{ir}"
    );
}

// ── Json.parse codegen (phase-8 line 435 slice 2) ─────────────
//
// Round-trip tests pair `Json.parse(s)` with `.stringify()` so the
// assertion runs against the parser's output verbatim. Each test
// covers one variant of the FFI→Kāra walker
// (`__karac_json_ffi_to_kara`): scalar variants (Null, Bool,
// Number, String) plus the recursive Array and Object arms. A
// dedicated error-path test exercises the Result.Err return.

#[test]
fn test_e2e_json_parse_null_roundtrip() {
    let out = run_program(
        "fn main() {\n\
                 match Json.parse(\"null\") {\n\
                     Ok(j) => println(j.stringify()),\n\
                     Err(_) => println(\"err\"),\n\
                 }\n\
             }",
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "null");
    }
}

#[test]
fn test_e2e_json_parse_bool_true_roundtrip() {
    let out = run_program(
        "fn main() {\n\
                 match Json.parse(\"true\") {\n\
                     Ok(j) => println(j.stringify()),\n\
                     Err(_) => println(\"err\"),\n\
                 }\n\
             }",
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "true");
    }
}

#[test]
fn test_e2e_json_parse_bool_false_roundtrip() {
    let out = run_program(
        "fn main() {\n\
                 match Json.parse(\"false\") {\n\
                     Ok(j) => println(j.stringify()),\n\
                     Err(_) => println(\"err\"),\n\
                 }\n\
             }",
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "false");
    }
}

#[test]
fn test_e2e_json_parse_number_roundtrip() {
    let out = run_program(
        "fn main() {\n\
                 match Json.parse(\"42.5\") {\n\
                     Ok(j) => println(j.stringify()),\n\
                     Err(_) => println(\"err\"),\n\
                 }\n\
             }",
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "42.5");
    }
}

#[test]
fn test_e2e_json_parse_string_roundtrip() {
    let out = run_program(
        "fn main() {\n\
                 match Json.parse(\"\\\"hello\\\"\") {\n\
                     Ok(j) => println(j.stringify()),\n\
                     Err(_) => println(\"err\"),\n\
                 }\n\
             }",
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "\"hello\"");
    }
}

#[test]
fn test_e2e_json_parse_empty_string_roundtrip() {
    // Empty payload exercises the `str_len == 0` fast path in the
    // lift walker — no malloc fires for the data pointer.
    let out = run_program(
        "fn main() {\n\
                 match Json.parse(\"\\\"\\\"\") {\n\
                     Ok(j) => println(j.stringify()),\n\
                     Err(_) => println(\"err\"),\n\
                 }\n\
             }",
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "\"\"");
    }
}

#[test]
fn test_e2e_json_parse_array_roundtrip() {
    // Recursive walk over arr_items[0..N] — exercises both the
    // outer Json.Array repack and the per-child self-call in
    // `__karac_json_ffi_to_kara`.
    let out = run_program(
        "fn main() {\n\
                 match Json.parse(\"[1,2,3]\") {\n\
                     Ok(j) => println(j.stringify()),\n\
                     Err(_) => println(\"err\"),\n\
                 }\n\
             }",
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "[1,2,3]");
    }
}

#[test]
fn test_e2e_json_parse_empty_array_roundtrip() {
    // Zero-length Array exercises the malloc-skip empty-buffer
    // path (`arr_len == 0` → null data ptr, cap = 0).
    let out = run_program(
        "fn main() {\n\
                 match Json.parse(\"[]\") {\n\
                     Ok(j) => println(j.stringify()),\n\
                     Err(_) => println(\"err\"),\n\
                 }\n\
             }",
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "[]");
    }
}

#[test]
fn test_e2e_json_parse_object_roundtrip() {
    // Object arm — exercises the parallel obj_keys / obj_vals
    // walk, the strlen-based key-copy path, and the 56-byte tuple
    // stride between the String key (offset 0) and Json value
    // (offset 24) in the synthesized buffer.
    let out = run_program(
        "fn main() {\n\
                 match Json.parse(\"{\\\"a\\\":1,\\\"b\\\":true}\") {\n\
                     Ok(j) => println(j.stringify()),\n\
                     Err(_) => println(\"err\"),\n\
                 }\n\
             }",
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "{\"a\":1,\"b\":true}");
    }
}

#[test]
fn test_e2e_json_parse_nested_roundtrip() {
    // Triple-level nesting — exercises both Array→Object and
    // Object→Array recursion paths through
    // `__karac_json_ffi_to_kara`'s self-call sites.
    let out = run_program(
        "fn main() {\n\
                 match Json.parse(\"{\\\"items\\\":[1,2,3],\\\"meta\\\":{\\\"v\\\":true}}\") {\n\
                     Ok(j) => println(j.stringify()),\n\
                     Err(_) => println(\"err\"),\n\
                 }\n\
             }",
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "{\"items\":[1,2,3],\"meta\":{\"v\":true}}");
    }
}

/// B-2026-07-30-15 — `Json.Int(i64)` under codegen: a constructed Int
/// stringifies without `.0`; 2^53 + 1 (unrepresentable in f64) survives a
/// parse + stringify round-trip exactly; float SYNTAX still round-trips as
/// `Number` with its fractional form; and the seventh variant participates
/// in `match` (both the make_int lowering arm and the offset-72 `int_val`
/// read in the lift walker). Twinned with `tests/json.rs`'s
/// `test_json_int_variant_exact_roundtrip` / `_match_destructure`.
#[test]
fn test_e2e_json_int_variant_exact_roundtrip() {
    let Some(out) = run_program(
        "fn describe(j: Json) -> String {\n\
                 match j {\n\
                     Json.Null => \"null\",\n\
                     Json.Bool(b) => \"bool\",\n\
                     Json.Number(f) => \"num\",\n\
                     Json.Int(i) => \"int:\" + i.to_string(),\n\
                     Json.String(s) => \"str\",\n\
                     Json.Array(xs) => \"arr\",\n\
                     Json.Object(kv) => \"obj\",\n\
                 }\n\
             }\n\
             fn main() {\n\
                 let o: Json = Json.Object(Vec[(\"id\", Json.Int(1))]);\n\
                 println(o.stringify());\n\
                 match Json.parse(\"{\\\"n\\\":9007199254740993}\") {\n\
                     Ok(j) => println(j.stringify()),\n\
                     Err(_) => println(\"err\"),\n\
                 }\n\
                 match Json.parse(\"[7, 2.5, 1.0]\") {\n\
                     Ok(j) => println(j.stringify()),\n\
                     Err(_) => println(\"err\"),\n\
                 }\n\
                 println(Json.Int(-42).stringify());\n\
                 match Json.parse(\"41\") {\n\
                     Ok(j) => println(describe(j)),\n\
                     Err(_) => println(\"err\"),\n\
                 }\n\
                 match Json.parse(\"4.5\") {\n\
                     Ok(j) => println(describe(j)),\n\
                     Err(_) => println(\"err\"),\n\
                 }\n\
             }\n",
    ) else {
        return;
    };
    assert_eq!(
        out,
        "{\"id\":1}\n{\"n\":9007199254740993}\n[7,2.5,1.0]\n-42\nint:41\nnum\n"
    );
}

#[test]
fn test_e2e_modbind_float_immutable_let() {
    let output = run_program(
        "let PI: f64 = 3.14;\n\
             fn main() { println(PI); }",
    )
    .expect("compile + run failed");
    assert_eq!(output, "3.14\n");
}

#[test]
fn test_e2e_modbind_negated_int_literal() {
    // `let X = -42;` — the AST shape is `Unary { Neg, Integer }`,
    // not `Integer(-42)`. The slice-9 surface lowers it
    // recursively so the negative-init case works.
    let output = run_program(
        "let TEMP: i64 = -42;\n\
             fn main() { println(TEMP); }",
    )
    .expect("compile + run failed");
    assert_eq!(output, "-42\n");
}

#[test]
fn test_e2e_modbind_map_new_int_keys() {
    // Int-keyed module-scope Map — a different hash/eq emission path
    // (i64 key vs the String key above). Two inserts + an overwrite to
    // prove the single global handle persists across writes.
    let src = "let mut SCORES: Map[i64, i64] = Map.new();\n\
                   fn main() {\n\
                       SCORES.insert(1, 10);\n\
                       SCORES.insert(2, 20);\n\
                       SCORES.insert(1, 11);\n\
                       println(SCORES.get(1).unwrap_or(0));\n\
                       println(SCORES.get(2).unwrap_or(0));\n\
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
        "int-keyed module-scope Map.new() must typecheck clean, got: {:?}",
        typed.errors.iter().map(|e| &e.message).collect::<Vec<_>>(),
    );
    let output = run_program(src).expect("compile + run failed");
    assert_eq!(output, "11\n20\n");
}

// ── Type-changing shadows (phase-5-diagnostics "codegen
//    type-changing-shadow"). A `let` that re-binds an in-scope name with
//    a different type/class used to be rejected by a `bind_pattern` guard
//    because the per-variable sidecar metadata (string/collection class
//    tags) survived the rebind and mis-dispatched a later use. The
//    `shadow.rs` take/restore dance now purges the old tags. Each test
//    soft-skips when the runtime archive is unavailable (run_program ->
//    None), matching the rest of the E2E suite. ──

#[test]
fn test_e2e_shadow_string_to_int_via_old_binding() {
    // `let s = s.len()` — collection→scalar shadow whose RHS references
    // the OLD binding. The dance must keep `s` dispatching as a String
    // while the RHS compiles, then drop the String tag so `println(s)`
    // formats an i64 (not a String, which would trap).
    if let Some(out) = run_program(
        "fn main() {\n\
             let s = \"hello\";\n\
             let s = s.len();\n\
             println(s);\n\
             }",
    ) {
        assert_eq!(out, "5\n");
    }
}

#[test]
fn test_e2e_shadow_int_to_string() {
    // int→String shadow. The new String tag must be installed; the old
    // (absent) scalar metadata leaves nothing to purge.
    if let Some(out) = run_program(
        "fn main() {\n\
             let s = 5i64;\n\
             let s = \"world\";\n\
             println(s);\n\
             }",
    ) {
        assert_eq!(out, "world\n");
    }
}

#[test]
fn test_e2e_fs_write_then_read_round_trips() {
    // `FileSystem.write(path, contents) -> Result[Unit, IoError]`
    // (L646 slice 4). Write a file, then read it back with
    // `FileSystem.read_to_string` and print — a write→read round-trip
    // that pins both the Unit-Ok unpack (write) and that the bytes
    // actually landed on disk. Lowers to `karac_runtime_fs_write` +
    // `lower_kara_io_result(FileOkKind::Unit)`. Uses the host temp dir
    // (per-name, cleaned around the run) so it doesn't race other tests.
    let tmp = std::env::temp_dir().join("karac_e2e_fs_write_rt.txt");
    let _ = std::fs::remove_file(&tmp);
    let path = tmp.to_str().unwrap().replace('\\', "\\\\");
    let src = format!(
        r#"
fn main() with writes(FileSystem) reads(FileSystem) {{
    match FileSystem.write("{path}", "hello-fs-write") {{
        Ok(_) => match FileSystem.read_to_string("{path}") {{
            Ok(s) => println(s),
            Err(_) => println("read-err"),
        }},
        Err(_) => println("write-err"),
    }}
}}
"#
    );
    let out = run_program(&src);
    let _ = std::fs::remove_file(&tmp);
    if let Some(out) = out {
        assert_eq!(out.trim(), "hello-fs-write");
    }
}

#[test]
fn test_e2e_lowercase_fs_write_then_read_round_trips() {
    // Lowercase `fs.write` / `fs.read_to_string` disk round-trip — the
    // lowercase counterpart of `test_e2e_fs_write_then_read_round_trips`.
    // These route through the ambient-alias codegen path
    // (`ambient_resource_for_alias("fs")` → `compile_ambient_ffi`'s
    // FileSystem arms → the `compile_fs_write_vals` /
    // `compile_file_read_to_string_val` value-cores), distinct from the
    // capitalized associated-call path — regression guard for the
    // "FileSystem.write is not yet lowered" error the ambient path hit
    // before this slice added its FileSystem arms.
    let tmp = std::env::temp_dir().join("karac_e2e_lc_fs_write_rt.txt");
    let _ = std::fs::remove_file(&tmp);
    let path = tmp.to_str().unwrap().replace('\\', "\\\\");
    let src = format!(
        r#"
fn main() with writes(FileSystem) reads(FileSystem) {{
    match fs.write("{path}", "hello-lc-fs") {{
        Ok(_) => match fs.read_to_string("{path}") {{
            Ok(s) => println(s),
            Err(_) => println("read-err"),
        }},
        Err(_) => println("write-err"),
    }}
}}
"#
    );
    let out = run_program(&src);
    let _ = std::fs::remove_file(&tmp);
    if let Some(out) = out {
        assert_eq!(out.trim(), "hello-lc-fs");
    }
}

// ── Phase 8 File handle slice F6 — broader E2E coverage ─────────
//
// F1-F5 wired up the surface and the basic propagation contracts.
// F6 adds the full round-trip and lifecycle coverage:
//
//   - create + write + flush + reopen + read produces the
//     expected bytes on disk and round-trips them through the
//     Kāra read path,
//   - open + write + drop (no explicit flush) still produces the
//     correct bytes — the FreeFileHandle cleanup action's call to
//     karac_runtime_file_close flushes via std::fs::File's Drop,
//   - File.open of a nonexistent path produces an IoError.NotFound
//     Err arm that the user can match on directly (the slice-F5
//     ?-propagation tests covered the propagated variant; this
//     test pins the direct-match shape),
//   - large read returns the correct byte count (>4KiB to ensure
//     multiple syscalls aren't an issue).

#[test]
fn test_e2e_file_create_write_flush_reopen_read_full_roundtrip() {
    // Round-trip: write three bytes through Kāra, reopen, read
    // them back through Kāra, assert the on-disk contents and the
    // read byte count.
    let tmp = std::env::temp_dir().join("karac_e2e_file_f6_roundtrip.txt");
    let _ = std::fs::remove_file(&tmp);
    let path = tmp.to_str().unwrap().replace('\\', "\\\\");
    let src = format!(
        r#"
fn main() with reads(FileSystem) writes(FileSystem) {{
    match File.create("{path}") {{
        Ok(f) => {{
            let mut data: Vec[u8] = Vec.new();
            data.push(72u8); data.push(73u8); data.push(10u8);
            match f.write(data) {{
                Ok(_) => println("wrote"),
                Err(_) => println("write-err"),
            }}
            match f.flush() {{
                Ok(_) => println("flushed"),
                Err(_) => println("flush-err"),
            }}
        }}
        Err(_) => println("create-err"),
    }}
    match File.open("{path}") {{
        Ok(f) => {{
            let mut buf: Vec[u8] = Vec.with_capacity(8);
            buf.push(0u8); buf.push(0u8); buf.push(0u8); buf.push(0u8);
            match f.read(mut buf) {{
                Ok(_) => println("read"),
                Err(_) => println("read-err"),
            }}
        }}
        Err(_) => println("open-err"),
    }}
}}
"#
    );
    let out = run_program(&src);
    if let Some(out) = out {
        assert_eq!(out.trim(), "wrote\nflushed\nread");
        let contents = std::fs::read(&tmp).expect("read tempfile");
        assert_eq!(contents, b"HI\n");
    }
    let _ = std::fs::remove_file(&tmp);
}

#[test]
fn test_e2e_file_large_write_and_read_round_trip() {
    // Write more bytes than one Vec.push call's amortized growth
    // can fit in a single allocation; verifies the read-write
    // buffer transfer handles multi-page payloads. We push 256
    // bytes (well within libc's single-syscall read/write
    // capability on every supported platform, but enough to
    // exercise the buffer-management correctness independent of
    // initial Vec capacity).
    let tmp = std::env::temp_dir().join("karac_e2e_file_f6_large.bin");
    let _ = std::fs::remove_file(&tmp);
    let path = tmp.to_str().unwrap().replace('\\', "\\\\");
    let src = format!(
        r#"
fn main() with reads(FileSystem) writes(FileSystem) {{
    match File.create("{path}") {{
        Ok(f) => {{
            let mut data: Vec[u8] = Vec.with_capacity(256);
            let mut i: i64 = 0;
            while i < 256 {{
                data.push(65u8);
                i = i + 1;
            }}
            match f.write(data) {{
                Ok(_) => println("wrote"),
                Err(_) => println("err"),
            }}
            match f.flush() {{
                Ok(_) => println("flushed"),
                Err(_) => println("flush-err"),
            }}
        }}
        Err(_) => println("create-err"),
    }}
}}
"#
    );
    let out = run_program(&src);
    if let Some(out) = out {
        assert_eq!(out.trim(), "wrote\nflushed");
        let contents = std::fs::read(&tmp).expect("read tempfile");
        assert_eq!(contents.len(), 256);
        assert!(contents.iter().all(|&b| b == 65));
    }
    let _ = std::fs::remove_file(&tmp);
}

#[test]
fn test_e2e_read_to_string_roundtrip() {
    // Write a file outside Kāra, read it back through
    // FileSystem.read_to_string, print the contents.
    let tmp = std::env::temp_dir().join("karac_e2e_read_to_string.txt");
    let _ = std::fs::remove_file(&tmp);
    std::fs::write(&tmp, b"alpha\nbeta\n").expect("seed temp");
    let path = tmp.to_str().unwrap().replace('\\', "\\\\");
    let src = format!(
        r#"
fn main() with reads(FileSystem) {{
    match FileSystem.read_to_string("{path}") {{
        Ok(s) => print(s),
        Err(_) => println("read-err"),
    }}
}}
"#
    );
    let out = run_program(&src);
    if let Some(out) = out {
        assert_eq!(out, "alpha\nbeta\n");
    }
    let _ = std::fs::remove_file(&tmp);
}

#[test]
fn test_e2e_vec_sort_by_mono_i64_ascending() {
    // Slice 6.1 — Vec[i64].sort_by(|a, b| a.cmp(b)) routes through the
    // monomorphized fast path `emit_sort_by_mono_i64`: insertion sort
    // body emitted into the user binary, comparator inlined at the
    // inner compare, no `karac_vec_sort_by` callback. Canonical
    // ascending shape (kata 15 + 16 idiom).
    let out = run_program(
        r#"
fn main() {
    let mut v: Vec[i64] = Vec.new();
    v.push(30); v.push(10); v.push(20); v.push(40); v.push(15);
    v.sort_by(|a, b| a.cmp(b));
    for x in v.iter() { println(x); }
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["10", "15", "20", "30", "40"]);
    }
}

#[test]
fn test_e2e_vec_sort_by_mono_i64_descending() {
    // Slice 6.1 — the comparator controls the ORDER, not just the
    // value extraction. `b.cmp(a)` inverts the ascending shape and
    // the mono path must respect it (the closure body is inlined; if
    // we hardcoded ascending in the sort, this test would fail).
    let out = run_program(
        r#"
fn main() {
    let mut v: Vec[i64] = Vec.new();
    v.push(30); v.push(10); v.push(20); v.push(40);
    v.sort_by(|a, b| b.cmp(a));
    for x in v.iter() { println(x); }
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["40", "30", "20", "10"]);
    }
}

#[test]
fn test_e2e_vec_sort_by_mono_i64_computed_key() {
    // Slice 6.1 — closure body with arithmetic on the params. Pins
    // that the param-binding step (a = data[jj], b = key) correctly
    // feeds the closure body's computation; if either binding were
    // swapped, this would produce a wrong order. Sorts by squared
    // value, so the sign of the input is irrelevant.
    let out = run_program(
        r#"
fn main() {
    let mut v: Vec[i64] = Vec.new();
    v.push(-3i64); v.push(1i64); v.push(-2i64); v.push(4i64); v.push(0i64);
    v.sort_by(|a, b| (a * a).cmp(b * b));
    for x in v.iter() { println(x); }
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        // Squared: 9, 1, 4, 16, 0 — sorted ascending: 0(0), 1(1), -2(4), -3(9), 4(16)
        assert_eq!(lines, vec!["0", "1", "-2", "-3", "4"]);
    }
}

#[test]
fn test_e2e_vec_sort_by_mono_i64_with_duplicates() {
    // Slice 6.1 — equal values sit adjacent after sort; the mono path
    // must not lose any element. Insertion-sort is stable, but stability
    // isn't pinned here (sort_by has no stable contract); we only
    // assert the multiset and order.
    let out = run_program(
        r#"
fn main() {
    let mut v: Vec[i64] = Vec.new();
    v.push(3); v.push(1); v.push(3); v.push(2); v.push(1); v.push(3);
    v.sort_by(|a, b| a.cmp(b));
    for x in v.iter() { println(x); }
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["1", "1", "2", "3", "3", "3"]);
    }
}

#[test]
fn test_e2e_vec_sort_by_mono_i64_empty_and_single() {
    // Slice 6.1 — edge cases. Empty Vec (len=0) and single-element
    // Vec (len=1) must both produce no-op behavior: the outer-chk
    // condition (ii=1 < len) is false at entry, the body never runs,
    // we return immediately. If the BB chain were wrong, either would
    // crash or corrupt the (one-element) buffer.
    let out = run_program(
        r#"
fn main() {
    let mut empty: Vec[i64] = Vec.new();
    empty.sort_by(|a, b| a.cmp(b));
    println(empty.len());

    let mut one: Vec[i64] = Vec.new();
    one.push(42);
    one.sort_by(|a, b| a.cmp(b));
    println(one[0i64]);
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["0", "42"]);
    }
}

#[test]
fn test_e2e_vec_sort_by_mono_named_struct_int_field() {
    // Slice 6.4 — `struct S { v: i64 }` rides the mono path when its
    // fields are all integers. The caller plumbs the elem Kāra type
    // name through, the mono emitter registers var_type_names for
    // closure params, and the body's `.v` named-field access
    // resolves through the existing struct-field-extract path.
    // Without the var_type_names registration this output would be
    // `[30, 10, 20]` (sort no-op'd, input order preserved); with it,
    // the sort works (surfaced 2026-05-29 by an earlier failing
    // version of this test).
    let out = run_program(
        r#"
struct Score { v: i64 }
fn main() {
    let mut v: Vec[Score] = Vec.new();
    v.push(Score { v: 30 });
    v.push(Score { v: 10 });
    v.push(Score { v: 20 });
    v.sort_by(|a, b| a.v.cmp(b.v));
    for s in v.iter() { println(s.v); }
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["10", "20", "30"]);
    }
}

#[test]
fn test_e2e_vec_sort_by_key_struct_all_int_fields() {
    // `derive(Ord)` on a struct with all-integer fields. Pins that the
    // single-field-int case still works (it could have routed through
    // either the all-int StructValue cascade or the struct-aware
    // dispatch — the struct-aware path takes precedence and must
    // produce the same result).
    let out = run_program(
        r#"
#[derive(PartialEq, Eq, PartialOrd, Ord)]
struct Pt { x: i64, y: i64 }

fn main() {
    let mut v: Vec[Pt] = Vec.new();
    v.push(Pt { x: 3i64, y: 1i64 });
    v.push(Pt { x: 1i64, y: 9i64 });
    v.push(Pt { x: 3i64, y: 0i64 });
    v.push(Pt { x: 1i64, y: 2i64 });
    v.sort_by_key(|p| p);
    for p in v.iter() {
        println(p.x);
        println(p.y);
    }
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        // Sorted ascending by (x, y): (1,2), (1,9), (3,0), (3,1).
        assert_eq!(lines, vec!["1", "2", "1", "9", "3", "0", "3", "1"]);
    }
}

#[test]
fn test_e2e_vec_sort_by_key_float_total_cmp_ascending() {
    // Float keys go through `karac_float_cmp` (Rust's `f64::total_cmp`
    // semantics: sign-flip the bit pattern, integer-compare). The
    // typechecker accepts floats as a sort_by_key-scoped concession
    // (other Ord consumers still reject them).
    let out = run_program(
        r#"
fn main() {
    let mut v: Vec[f64] = Vec.new();
    v.push(3.5); v.push(-1.2); v.push(2.7); v.push(-0.5);
    v.sort_by_key(|x| x);
    for x in v.iter() { println(x); }
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["-1.2", "-0.5", "2.7", "3.5"]);
    }
}

#[test]
fn test_e2e_vec_sort_by_key_float_nan_sorts_largest() {
    // NaN sorts as the largest value under total_cmp semantics. Pins the
    // NaN-handling policy and guards against a regression to IEEE 754
    // unordered semantics (where NaN would compare unordered with
    // everything and produce a non-deterministic permutation).
    //
    // B-2026-08-11-17: the interpreter twin of this test now exists
    // (`test_sort_by_key_float_nan_sorts_largest_interp_parity`) and
    // asserts the SAME order. Only this compiled side was pinned, which
    // is how the interpreter shipped an incoherent float sort — it
    // ordered with `partial_cmp(…).unwrap_or(Equal)`, so any NaN made
    // every comparison "equal". Keep the two in lockstep.
    //
    // Note the NaN here is CONSTANT-FOLDED, which only exercises one of
    // the two provenances — see
    // `test_e2e_sort_by_key_float_nan_provenance_independent` below for
    // the runtime-produced one, which is what actually split the backends.
    let out = run_program(
        r#"
fn main() {
    let mut v: Vec[f64] = Vec.new();
    let nan: f64 = 0.0 / 0.0;
    v.push(3.5); v.push(nan); v.push(1.2); v.push(-2.0); v.push(2.7);
    v.sort_by_key(|x| x);
    println(v.len());
    for x in v.iter() { println(x); }
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        // NaN now prints "NaN" (Rust `{}` via karac_runtime_f64_to_str),
        // matching the interpreter — was lowercase "nan" under C `%g`.
        assert_eq!(lines, vec!["5", "-2", "1.2", "2.7", "3.5", "NaN"]);
    }
}

#[test]
fn test_e2e_sort_by_key_float_nan_provenance_independent() {
    // B-2026-08-11-17 — a float sort key must order the same whatever
    // produced its NaN. `karac_float_cmp` is Rust's `total_cmp` transform,
    // which is sign-sensitive: a NEGATIVE NaN sorts before `-Infinity`
    // while a POSITIVE one sorts after `+Infinity`. Nothing at the source
    // level chooses between them — an x86 runtime `z / z` yields a
    // negative NaN and LLVM's constant folder a positive one — so the same
    // program sorted NaN-first under the JIT and NaN-LAST under AOT, which
    // inlines `zero()` and folds the division. Fixed by canonicalizing
    // every NaN inside `karac_float_cmp` (and identically in the
    // interpreter's `value_compare`), so provenance stops being
    // observable.
    //
    // `zero()` is what defeats the folding; a literal `0.0 / 0.0` would
    // give both vectors the same constant NaN and assert nothing.
    let out = run_program(
        r#"
fn zero() -> f64 { return 0.0; }
fn main() {
    let z = zero();
    let rt = z / z;
    let ct = 0.0 / 0.0;
    let mut a: Vec[f64] = Vec.new();
    a.push(1.0); a.push(rt); a.push(-1.0);
    a.sort_by_key(|x| x);
    let mut b: Vec[f64] = Vec.new();
    b.push(1.0); b.push(ct); b.push(-1.0);
    b.sort_by_key(|x| x);
    for x in a.iter() { println(x); }
    for x in b.iter() { println(x); }
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["-1", "1", "NaN", "-1", "1", "NaN"]);
    }
}

#[test]
fn e2e_partial_cmp_only_rejection_names_a_workaround_that_works() {
    // The rejection for an `impl PartialOrd` without a `cmp` prescribes
    // "add `impl Ord for T { fn cmp(...) }`". This compiles and runs exactly
    // that program, so the advice cannot rot into a lie the way the wording
    // it replaced did (`#[derive(PartialOrd)]`, which compiles and then
    // compares by declaration order without calling the impl).
    //
    // The `cmp` reverses, so `true` would mean the derive-style comparator
    // answered and `false` means the prescribed impl did.
    let out = run_program(
        r#"
struct Item { id: i64 }
impl PartialEq for Item { fn eq(ref self, other: ref Item) -> bool { self.id == other.id } }
impl Eq for Item {}
impl PartialOrd for Item { fn partial_cmp(ref self, other: ref Item) -> Option[Ordering] { Some(other.id.cmp(self.id)) } }
impl Ord for Item { fn cmp(ref self, other: ref Item) -> Ordering { other.id.cmp(self.id) } }
fn main() {
    let a = Item { id: 1 };
    let b = Item { id: 5 };
    println(a < b);
}
"#,
    );
    let out = out.expect("the prescribed workaround must build");
    assert_eq!(out.trim(), "false");
}

#[test]
fn test_e2e_u64_sorted_now_supported() {
    // Both `Column` and `Tensor` now sort every numeric width, u64
    // INCLUDED: the scratch sort keys unsigned integers with `ugt`
    // (B-2026-07-07-2, lifting the old loud rejection this test used to
    // assert). u64 was previously blocked to avoid a run/build divergence
    // while the interpreter had no u64 model; that model landed in
    // B-2026-07-04-8, so build now matches run. (The ≥ 2^63 ordering that
    // actually distinguishes signed from unsigned keys is exercised in
    // `test_e2e_u64_column_tensor_sort_unsigned_match_run`.)
    let col_u64 = r#"
fn main() {
    let c: Column[u64] = Column.from_vec([5, 9, 3, 1]);
    let s: Vec[u64] = c.sorted();
    println(f"{s[0]},{s[3]}");
}
"#;
    ir_result(col_u64).expect("a u64 column sort must now compile");
    assert_eq!(run_program(col_u64).as_deref(), Some("1,9\n"));

    let tensor_u64 = r#"
fn main() {
    let t: Tensor[u64, [4]] = Tensor.from([4, 2, 7, 1]);
    let s: Vec[u64] = t.sorted();
    println(f"{s[0]},{s[3]}");
}
"#;
    ir_result(tensor_u64).expect("a u64 tensor sort must now compile");
    assert_eq!(run_program(tensor_u64).as_deref(), Some("1,7\n"));
}

#[test]
fn test_assert_eq_int_pass() {
    let out = run_program(
        r#"
fn main() {
    assert_eq(1 + 1, 2)
    println(42)
}
"#,
    );
    if let Some(s) = out {
        assert_eq!(s.trim(), "42");
    }
}

#[test]
fn test_assert_eq_int_fail_formats_operands() {
    let captured = run_program_capturing(
        r#"
fn main() {
    assert_eq(1, 2)
    println(99)
}
"#,
    );
    if let Some(c) = captured {
        assert!(
            c.stderr.contains("KARAC_TEST_FAILURE "),
            "expected failure marker; got stderr={:?}",
            c.stderr
        );
        assert!(
            c.stderr
                .contains("\"message\":\"assertion failed: left != right\""),
            "expected assert_eq message; got stderr={:?}",
            c.stderr
        );
        assert!(
            c.stderr.contains("\"left\":\"1\"") && c.stderr.contains("\"right\":\"2\""),
            "expected formatted left=1, right=2; got stderr={:?}",
            c.stderr
        );
        assert!(
            !c.stdout.contains("99"),
            "stdout should not include trailing println; got {:?}",
            c.stdout
        );
    }
}

#[test]
fn test_e2e_unsigned_refinement_above_signed_midpoint_accepts() {
    // A value that is itself above the base's signed midpoint: 4e9 fits
    // `u32` but reads negative as `i32`, so both operands exercise the
    // unsigned reading, not just the bound.
    let out = run_program(
        r#"
distinct type Big = u32 where self <= 4294967295;
fn main() {
    let b = Big(4000000000);
    println("built");
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "built");
    }
}

#[test]
fn test_e2e_distinct_float_base_layout() {
    // A non-`i64` base lowers to the correct layout: a distinct type
    // over `f64` round-trips `3.5` losslessly (would print garbage if it
    // hit the `i64` fall-through default).
    let out = run_program(
        r#"
distinct type Meters = f64;
fn main() {
    let m = Meters(3.5);
    println(m.raw());
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "3.5");
    }
}

#[test]
fn test_e2e_distinct_roundtrips_through_function() {
    // A distinct value passes through a function call typed in the
    // distinct type and unwraps back to its base on the far side.
    let out = run_program(
        r#"
distinct type UserId = i64;
fn identity(id: UserId) -> UserId { id }
fn main() {
    let u = identity(UserId(7));
    println(u.raw());
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "7");
    }
}

#[test]
fn test_vector_ir_emits_simd_type_and_ops() {
    // Use function params so the optimizer-free `compile_to_ir` keeps the
    // vector ops (literal-only inputs would still survive here since
    // compile_to_ir does not run LLVM passes, but params make the intent
    // explicit and the test robust to any future folding).
    let ir = ir_for(
        r#"
fn lane0_of_sum(p: i64, q: i64) -> i64 {
    let a: Vector[i64, 4] = Vector[i64, 4](p, p, p, p);
    let b: Vector[i64, 4] = Vector[i64, 4](q, q, q, q);
    let c = a + b;
    c[0]
}
fn main() { println(lane0_of_sum(1, 10)); }
"#,
    );
    assert!(
        ir.contains("<4 x i64>"),
        "expected `<4 x i64>` SIMD vector type in IR; got:\n{}",
        ir
    );
    assert!(
        ir.contains("insertelement"),
        "expected `insertelement` (vector construction) in IR; got:\n{}",
        ir
    );
    assert!(
        ir.contains("extractelement"),
        "expected `extractelement` (lane read) in IR; got:\n{}",
        ir
    );
}

#[test]
fn test_vector_i64_construct_add_index() {
    let out = run_program(
        r#"
fn main() {
    let a: Vector[i64, 4] = Vector[i64, 4](1, 2, 3, 4);
    let b: Vector[i64, 4] = Vector[i64, 4](10, 20, 30, 40);
    let c = a + b;
    println(c[0]);
    println(c[3]);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out, "11\n44\n");
    }
}

/// Bare numeric literals as vector lanes lower at the literal default
/// width (f64 / i64) and must be boundary-coerced to the element type
/// (`coerce_scalar_to_type`) at every scalar entry point — construction,
/// `splat`, `from_array` (array-literal arm), `replace`. Without the
/// coercion, `Vector[f32, 4](0.5, …)` mislowered as `<4 x double>` and
/// the module failed LLVM verification at the first op against a
/// correctly-typed operand (surfaced 2026-06-07 by the WASM SIMD-128
/// slice's E2E fixture; target-independent). Also pins `println(f32)`'s
/// varargs promotion (fpext to double — the C default-argument rule);
/// the missing fpext printed garbage on wasm32's args-buffer varargs.
#[test]
fn test_vector_f32_literal_lanes_coerce_to_element_type() {
    let out = run_program(
        r#"
fn construct(x: f32) -> f32 {
    let a: Vector[f32, 4] = Vector[f32, 4](x, x, x, x);
    let b: Vector[f32, 4] = Vector[f32, 4](0.5, 0.25, 0.5, 0.25);
    let c = a * b;
    c[0] + c[1] + c[2] + c[3]
}

fn splat_lit(y: f32) -> f32 {
    let m: Vector[f32, 4] = Vector[f32, 4].splat(0.5);
    let a: Vector[f32, 4] = Vector[f32, 4](y, y, y, y);
    let c = a * m;
    c[0] + c[3]
}

fn from_array_replace(y: f32) -> f32 {
    let m: Vector[f32, 4] = Vector[f32, 4].from_array([0.5, 0.5, 0.25, 0.25]);
    let r = m.replace(0, 1.5);
    let a: Vector[f32, 4] = Vector[f32, 4](y, y, y, y);
    let c = a * r;
    c[0] + c[1] + c[2] + c[3]
}

fn int_lanes(x: i32) -> i32 {
    let a: Vector[i32, 8] = Vector[i32, 8].splat(x);
    let b: Vector[i32, 8] = Vector[i32, 8](1, 2, 3, 4, 5, 6, 7, 8);
    let c = a + b;
    c[0] + c[7]
}

fn main() {
    println(construct(2.0));
    println(splat_lit(4.0));
    println(from_array_replace(2.0));
    println(int_lanes(10));
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out, "3\n4\n5\n29\n");
    }
}

#[test]
fn test_vector_i64_mul_and_sub() {
    let out = run_program(
        r#"
fn main() {
    let a: Vector[i64, 4] = Vector[i64, 4](2, 3, 4, 5);
    let b: Vector[i64, 4] = Vector[i64, 4](10, 10, 10, 10);
    let prod = a * b;
    let diff = b - a;
    println(prod[1]);
    println(diff[2]);
}
"#,
    );
    if let Some(out) = out {
        // prod = [20, 30, 40, 50] -> [1] == 30; diff = [8,7,6,5] -> [2] == 6
        assert_eq!(out, "30\n6\n");
    }
}

#[test]
fn test_vector_inferred_binding_type() {
    // No annotation on the construction binding — synthesis-mode inference
    // resolves the binding's type to Vector[i64, 2].
    let out = run_program(
        r#"
fn main() {
    let a = Vector[i64, 2](7, 8);
    let b = Vector[i64, 2](100, 200);
    let c = a + b;
    println(c[1]);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out, "208\n");
    }
}

#[test]
fn test_vector_f64_elementwise_div() {
    let out = run_program(
        r#"
fn main() {
    let a: Vector[f64, 2] = Vector[f64, 2](10.0, 9.0);
    let b: Vector[f64, 2] = Vector[f64, 2](2.0, 3.0);
    let q = a / b;
    println(q[0]);
    println(q[1]);
}
"#,
    );
    if let Some(out) = out {
        // 10.0/2.0 == 5, 9.0/3.0 == 3 (float print format is the runtime's)
        assert_eq!(out, "5\n3\n");
    }
}

#[test]
fn test_vector_typechecks_clean() {
    let errs = vector_typecheck_errors(
        r#"
fn main() {
    let a: Vector[i64, 4] = Vector[i64, 4](1, 2, 3, 4);
    let b: Vector[i64, 4] = Vector[i64, 4](5, 6, 7, 8);
    let c = a + b;
    let _ = c[0];
}
"#,
    );
    assert!(
        errs.is_empty(),
        "expected a clean typecheck for a valid Vector program; got: {:?}",
        errs
    );
}

#[test]
fn test_vector_lane_count_mismatch_is_type_error() {
    let errs = vector_typecheck_errors("fn main() { let _ = Vector[i64, 4](1, 2, 3); }");
    assert!(
        !errs.is_empty(),
        "Vector[i64, 4] built from 3 lanes must be a type error"
    );
}

#[test]
fn test_vector_non_numeric_element_is_type_error() {
    let errs =
        vector_typecheck_errors("fn main() { let _ = Vector[bool, 4](true, false, true, false); }");
    assert!(
        !errs.is_empty(),
        "Vector[bool, N] (non-numeric element) must be a type error"
    );
}

#[test]
fn test_vector_zero_lanes_is_type_error() {
    let errs = vector_typecheck_errors("fn f(v: Vector[i64, 0]) {} fn main() {}");
    assert!(
        !errs.is_empty(),
        "Vector[i64, 0] (non-positive lane count) must be a type error"
    );
}

#[test]
fn test_vector_scalar_mix_is_type_error() {
    let errs = vector_typecheck_errors(
        r#"
fn main() {
    let a: Vector[i64, 4] = Vector[i64, 4](1, 2, 3, 4);
    let _ = a + 5;
}
"#,
    );
    assert!(
        !errs.is_empty(),
        "vector + scalar must be a type error (no implicit broadcast)"
    );
}

// ── Vector slice 2 — reductions (dot, reduce_sum) ────────────────────

#[test]
fn test_vector_reduce_sum_ir_uses_extractelement() {
    let ir = ir_for(
        r#"
fn sum4(p: i64, q: i64, r: i64, s: i64) -> i64 {
    let v: Vector[i64, 4] = Vector[i64, 4](p, q, r, s);
    v.reduce_sum()
}
fn main() { println(sum4(1, 2, 3, 4)); }
"#,
    );
    assert!(
        ir.contains("extractelement"),
        "reduce_sum should fold lanes via extractelement; got:\n{}",
        ir
    );
}

#[test]
fn test_vector_reduce_sum_i64() {
    let out =
        run_program("fn main() { let v = Vector[i64, 4](1, 2, 3, 4); println(v.reduce_sum()); }");
    if let Some(out) = out {
        assert_eq!(out, "10\n");
    }
}

#[test]
fn test_vector_reduce_sum_f64() {
    let out =
        run_program("fn main() { let v = Vector[f64, 2](1.5, 2.5); println(v.reduce_sum()); }");
    if let Some(out) = out {
        assert_eq!(out, "4\n");
    }
}

#[test]
fn test_ir_vector_sqrt_uses_vector_intrinsic() {
    // `std.simd.math` (phase-11): `v.sqrt()` on a `Vector[f32, 4]` lowers
    // to the overloaded LLVM VECTOR intrinsic `@llvm.sqrt.v4f32` (one
    // hardware `sqrtps`), not a scalarized per-lane call.
    let ir = ir_for(
        r#"
fn f(a: f32, b: f32, c: f32, d: f32) -> f32 {
    let v: Vector[f32, 4] = Vector[f32, 4](a, b, c, d);
    let r = v.sqrt();
    r[0]
}
fn main() { println(f(4.0f32, 9.0f32, 16.0f32, 25.0f32)); }
"#,
    );
    assert!(
        ir.contains("@llvm.sqrt.v4f32"),
        "v.sqrt() should lower to the vector intrinsic @llvm.sqrt.v4f32; got:\n{ir}"
    );
}

#[test]
fn test_e2e_vector_simd_math_transcendentals() {
    // sqrt/exp/ln/sigmoid/tanh on float vectors (std.simd.math). Exact /
    // saturating oracles: sqrt([4,9,16,25])=[2,3,4,5]; exp(0)=1; ln(1)=0;
    // sigmoid(0)=0.5; tanh(0)=0.
    let out = run_program(
        r#"
fn main() {
    let v = Vector[f32, 4].from_array([4.0f32, 9.0f32, 16.0f32, 25.0f32]);
    let s = v.sqrt();
    println(s[0]);
    println(s[3]);
    let z = Vector[f32, 4].splat(0.0f32);
    let ex = z.exp();
    println(ex[0]);
    let sg = z.sigmoid();
    println(sg[0]);
    let th = z.tanh();
    println(th[0]);
    let o = Vector[f32, 4].splat(1.0f32);
    let l = o.ln();
    println(l[0]);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out, "2\n5\n1\n0.5\n0\n0\n");
    }
}

#[test]
fn test_e2e_vector_unary_math_lane_width_matches_the_interpreter() {
    // The COMPILED half of the oracle pair for B-2026-08-29-40. The bug was
    // the interpreter's (it kept f64 bits in a bf16 lane); codegen was right
    // all along, computing each lane at the element width. So this does not
    // fail without that fix — its job is to nail the compiled values down so
    // the two halves cannot drift apart again, and it is character-identical
    // to `tests/interpreter.rs::test_vector_unary_math_rounds_lanes_to_element_width`.
    //
    // Read that test for why all five non-integral methods are pinned rather
    // than `sqrt` alone, and why an f32 and an f64 line bracket them.
    // Verified byte-identical under `karac run --interp`, `karac run`,
    // `karac build`, and `KARAC_AUTO_PAR=0 karac build`.
    assert_eq!(
        run_program(
            r#"
fn main() {
    let v: Vector[bf16, 4] = Vector[bf16, 4](1.0bf16, 2.0bf16, 3.0bf16, 7.0bf16);
    println(v.sqrt()[1]);
    println(v.sqrt().reduce_sum());
    println(v.exp()[1]);
    println(v.ln()[2]);
    println(v.tanh()[0]);
    println(v.sigmoid()[1]);
    let f: Vector[f32, 4] = Vector[f32, 4](2.0f32, 3.0f32, 5.0f32, 7.0f32);
    println(f.sqrt()[0]);
    let d: Vector[f64, 2] = Vector[f64, 2](2.0, 3.0);
    println(d.sqrt()[0]);
}
"#,
        ),
        Some(
            "1.4140625\n6.75\n7.375\n1.1015625\n0.76171875\n0.87890625\n\
                 1.4142135381698608\n1.4142135623730951\n"
                .to_string()
        )
    );
}

#[test]
fn test_e2e_vector_f16_transcendentals_match_the_scalar_and_the_interpreter() {
    // B-2026-08-29-53. `apply_vector_float_unary` widened bf16 lanes to f32
    // for the whole body but left f16 narrow, so the exp-derived formulas
    // computed their INTERMEDIATES at f16. `tanh` is `(e^2ˣ-1)/(e^2ˣ+1)`,
    // and f16's largest finite value is 65504, which `e^2ˣ` crosses at
    // x ≈ 5.545: above it the intermediate is `+inf` and the ratio is NaN.
    //
    // Two things this pins that the row did not name. The defect is not
    // only the NaN: at x = 4, well under the threshold, both `e^2ˣ ± 1`
    // round to the SAME f16, so the quotient is exactly 1 where the answer
    // is 0.99951171875 — a wrong number rather than a visibly wrong one,
    // and the half that a NaN-hunting fixture would miss. And `sigmoid` was
    // affected too (the row listed it as unobserved-but-not-cleared): it
    // never overflows, being 1/(1+e^-x), but it lost an f16 ULP at x = 2
    // and x = 6 for the same reason.
    //
    // The `st`/`se` scalar lines are the load-bearing half of the oracle.
    // Pre-fix the compiled binary DISAGREED WITH ITSELF — `v.tanh()[i]` was
    // NaN where `v[i].tanh()` on the same value in the same binary returned
    // the correct 1 — and there is no reading of a language under which one
    // function of one value has two answers inside one program. That is
    // what made this a defect rather than a precision preference.
    //
    // The `bf` and `f32` lines are controls at the two boundaries: bf16
    // must keep the values `test_e2e_vector_unary_math_lane_width_matches_the_interpreter`
    // already pins (the widening it relies on is still there), and f32 must
    // NOT move (it is not a reduced-precision format and is not widened —
    // a "generalization" of this fix that touched it would show up here).
    //
    // Pre-fix this test fails on the `vt` and `sg` lines only:
    //     vt 1 NaN NaN NaN          sg 0.88037109375 0.9970703125
    // Receivers derive from `env.args().len()` (= 1 under this harness) so
    // nothing folds to a constant — a fixture built from literals measures
    // LLVM's constant folder, not the emitted arithmetic (B-2026-08-29-61).
    // Verified byte-identical under `karac run --interp`, `karac run`,
    // `karac build`, and `KARAC_AUTO_PAR=0 karac build`.
    assert_eq!(
        run_program(
            r#"
fn main() {
    let n = env.args().len() as i64;
    let a: f16 = ((n + 3) as f32) as f16;
    let b: f16 = ((n + 5) as f32) as f16;
    let c: f16 = ((n + 6) as f32) as f16;
    let d: f16 = ((n + 19) as f32) as f16;
    let v: Vector[f16, 4] = Vector[f16, 4](a, b, c, d);
    println(f"vt {v.tanh()[0]} {v.tanh()[1]} {v.tanh()[2]} {v.tanh()[3]}");
    println(f"st {a.tanh()} {b.tanh()} {c.tanh()} {d.tanh()}");
    let e: f16 = ((n + 1) as f32) as f16;
    let s: Vector[f16, 4] = Vector[f16, 4](e, b, c, d);
    println(f"sg {s.sigmoid()[0]} {s.sigmoid()[1]}");
    println(f"ve {v.exp()[0]} {v.ln()[1]} {v.sqrt()[2]}");
    println(f"se {a.exp()} {b.ln()} {c.sqrt()}");
    let g: Vector[bf16, 4] = Vector[bf16, 4](1.0bf16, 2.0bf16, 3.0bf16, 7.0bf16);
    println(f"bf {g.tanh()[0]} {g.sigmoid()[1]}");
    let f: Vector[f32, 4] = Vector[f32, 4](2.0f32, 3.0f32, 5.0f32, 7.0f32);
    println(f"f32 {f.tanh()[0]} {f.sigmoid()[1]}");
}
"#,
        ),
        Some(
            "vt 0.99951171875 1 1 1\n\
                 st 0.99951171875 1 1 1\n\
                 sg 0.880859375 0.99755859375\n\
                 ve 54.59375 1.7919921875 2.646484375\n\
                 se 54.59375 1.7919921875 2.646484375\n\
                 bf 0.76171875 0.87890625\n\
                 f32 0.9640275835990906 0.9525741338729858\n"
                .to_string()
        )
    );
}

#[test]
fn test_ir_vector_f16_unary_math_computes_in_f32() {
    // The STRUCTURAL half of B-2026-08-29-53: the f16 vector body must be
    // widened once on entry and rounded once on exit, with the formula in
    // between carried at `<4 x float>`. A value test alone would pass on a
    // fix that special-cased `tanh` with a saturation cutoff instead, which
    // would leave every other multi-step f16 lowering to be found one at a
    // time; this asserts the shape that makes the whole family right.
    //
    // The `fdiv <4 x half>` absence is the specific pre-fix artifact — that
    // instruction, fed `inf` on both sides, is where the NaN was born.
    let ir = ir_for(
        r#"
fn f(a: f16) -> f16 {
    let v: Vector[f16, 4] = Vector[f16, 4](a, a, a, a);
    let t = v.tanh();
    t[0]
}
fn main() { println(f(6.0f16)); }
"#,
    );
    assert!(
        ir.contains("fpext <4 x half>") && ir.contains("fptrunc <4 x float>"),
        "f16 v.tanh() should widen to <4 x float> and round back once; got:\n{ir}"
    );
    assert!(
        !ir.contains("fdiv <4 x half>"),
        "the tanh quotient must not be evaluated at f16 (that is where the \
             NaN came from); got:\n{ir}"
    );
}

#[test]
fn test_ir_vector_exp_uses_polynomial_not_intrinsic() {
    // `v.exp()` on an f32 vector is the hand-written Cephes polynomial
    // (guaranteed SIMD — see compile_vector_exp), NOT `@llvm.exp` (which
    // scalarizes where libmvec is absent). The IR proves it: the range
    // reduction emits `@llvm.floor.v4f32` and the `2^n` assembly emits a
    // `shl <4 x i32>` + bitcast, and there is NO `@llvm.exp` call.
    let ir = ir_for(
        r#"
fn f(a: f32) -> f32 {
    let v: Vector[f32, 4] = Vector[f32, 4](a, a, a, a);
    let e = v.exp();
    e[0]
}
fn main() { println(f(1.0f32)); }
"#,
    );
    assert!(
        ir.contains("@llvm.floor.v4f32")
            && ir.contains("shl <4 x i32>")
            && !ir.contains("@llvm.exp"),
        "f32 v.exp() should be the polynomial (floor + shl, no @llvm.exp); got:\n{ir}"
    );
}

#[test]
fn test_e2e_vector_exp_polynomial_accuracy() {
    // `v.exp()` is the hand-written Cephes expf polynomial (f32, ~1 ULP).
    // Verify the codegen output against libm across the range to a tight
    // relative tolerance, and that sigmoid / tanh route through it. NOTE:
    // this DEVIATES from the interpreter (f64 libm) by more than f32
    // rounding — the documented approximation — so we assert against the
    // TRUE math values, not interp parity.
    let Some(out) = run_program(
        r#"
fn main() {
    let x = Vector[f32, 4].from_array([1.0f32, 2.0f32, -3.0f32, 5.0f32]);
    let e = x.exp();
    println(e[0]); println(e[1]); println(e[2]); println(e[3]);
    let s = x.sigmoid(); println(s[0]);
    let t = x.tanh(); println(t[0]);
}
"#,
    ) else {
        return;
    };
    let got: Vec<f64> = out.lines().map(|l| l.parse::<f64>().unwrap()).collect();
    let want = [
        1.0_f64.exp(),
        2.0_f64.exp(),
        (-3.0_f64).exp(),
        5.0_f64.exp(),
        1.0 / (1.0 + (-1.0_f64).exp()), // sigmoid(1)
        1.0_f64.tanh(),
    ];
    assert_eq!(got.len(), want.len(), "output line count; got:\n{out}");
    for (g, w) in got.iter().zip(want.iter()) {
        let tol = w.abs() * 1e-4 + 1e-6;
        assert!(
            (g - w).abs() <= tol,
            "vector exp/sigmoid/tanh off: got {g}, want {w} (tol {tol})\nfull:\n{out}"
        );
    }
}

#[test]
fn test_ir_vector_ln_uses_polynomial_not_intrinsic() {
    // `v.ln()` on an f32 vector is the hand-written Cephes logf polynomial
    // (guaranteed SIMD — see compile_vector_ln), NOT `@llvm.log`. The IR
    // proves it: the branchless frexp emits the exponent extraction
    // (`sitofp` of the integer exponent) and there is NO `@llvm.log` call.
    let ir = ir_for(
        r#"
fn f(a: f32) -> f32 {
    let v: Vector[f32, 4] = Vector[f32, 4](a, a, a, a);
    let l = v.ln();
    l[0]
}
fn main() { println(f(2.0f32)); }
"#,
    );
    assert!(
        ir.contains("sitofp") && !ir.contains("@llvm.log"),
        "f32 v.ln() should be the polynomial (sitofp, no @llvm.log); got:\n{ir}"
    );
}

#[test]
fn test_e2e_vector_ln_polynomial_accuracy() {
    // `v.ln()` is the hand-written Cephes logf polynomial (f32, ~1 ULP).
    // Verify against libm across the range to a tight relative tolerance,
    // and the domain: ln(0) = -inf, ln(-1) = NaN (matching @llvm.log). As
    // for exp, this deviates from the interpreter (f64 libm) beyond f32
    // rounding — assert against the true math values, not interp parity.
    let Some(out) = run_program(
        r#"
fn main() {
    let x = Vector[f32, 4].from_array([1.0f32, 2.0f32, 10.0f32, 0.5f32]);
    let l = x.ln();
    println(l[0]); println(l[1]); println(l[2]); println(l[3]);
    let d = Vector[f32, 4].from_array([0.0f32, -1.0f32, 1.0f32, 1.0f32]);
    let ld = d.ln();
    println(ld[0]); println(ld[1]);
}
"#,
    ) else {
        return;
    };
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.len(), 6, "output line count; got:\n{out}");
    let want = [1.0_f64.ln(), 2.0_f64.ln(), 10.0_f64.ln(), 0.5_f64.ln()];
    for (l, w) in lines[..4].iter().zip(want.iter()) {
        let g: f64 = l.parse().unwrap();
        let tol = w.abs() * 1e-4 + 1e-6;
        assert!(
            (g - w).abs() <= tol,
            "vector ln off: got {g}, want {w} (tol {tol})\nfull:\n{out}"
        );
    }
    let z0: f64 = lines[4].parse().unwrap();
    assert!(
        z0.is_infinite() && z0 < 0.0,
        "ln(0) should be -inf, got {}",
        lines[4]
    );
    let zn: f64 = lines[5].parse().unwrap();
    assert!(zn.is_nan(), "ln(-1) should be NaN, got {}", lines[5]);
}

#[test]
fn test_ir_vector_exp_ln_f64_use_rationals_not_intrinsics() {
    // The f64 `v.exp()` / `v.ln()` are the Cephes double-precision RATIONAL
    // forms (a float `fdiv` of two polynomials), NOT `@llvm.exp` /
    // `@llvm.log`. IR proof: a `fdiv <2 x double>` is present and neither
    // intrinsic is called.
    let ir_e = ir_for(
        r#"
fn f(a: f64) -> f64 {
    let v: Vector[f64, 2] = Vector[f64, 2](a, a);
    let e = v.exp();
    e[0]
}
fn main() { println(f(1.0)); }
"#,
    );
    assert!(
        ir_e.contains("fdiv <2 x double>") && !ir_e.contains("@llvm.exp"),
        "f64 v.exp() should be the rational (fdiv, no @llvm.exp); got:\n{ir_e}"
    );
    let ir_l = ir_for(
        r#"
fn f(a: f64) -> f64 {
    let v: Vector[f64, 2] = Vector[f64, 2](a, a);
    let l = v.ln();
    l[0]
}
fn main() { println(f(2.0)); }
"#,
    );
    assert!(
        ir_l.contains("fdiv <2 x double>") && !ir_l.contains("@llvm.log"),
        "f64 v.ln() should be the rational (fdiv, no @llvm.log); got:\n{ir_l}"
    );
}

#[test]
fn test_e2e_vector_exp_ln_f64_accuracy() {
    // f64 `v.exp()` / `v.ln()` are the Cephes double-precision rationals —
    // near machine precision, much tighter than the f32 single polynomials.
    // Verify against libm to a tight relative tolerance + the ln domain.
    let Some(out) = run_program(
        r#"
fn main() {
    let x = Vector[f64, 4].from_array([1.0, 2.0, 5.0, -1.0]);
    let e = x.exp();
    println(e[0]); println(e[1]); println(e[2]); println(e[3]);
    let y = Vector[f64, 4].from_array([2.0, 10.0, 0.001, 1000000.0]);
    let l = y.ln();
    println(l[0]); println(l[1]); println(l[2]); println(l[3]);
    let d = Vector[f64, 2].from_array([0.0, -2.0]);
    let ld = d.ln();
    println(ld[0]); println(ld[1]);
}
"#,
    ) else {
        return;
    };
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.len(), 10, "output line count; got:\n{out}");
    let want = [
        1.0_f64.exp(),
        2.0_f64.exp(),
        5.0_f64.exp(),
        (-1.0_f64).exp(),
        2.0_f64.ln(),
        10.0_f64.ln(),
        0.001_f64.ln(),
        1_000_000.0_f64.ln(),
    ];
    for (l, w) in lines[..8].iter().zip(want.iter()) {
        let g: f64 = l.parse().unwrap();
        let tol = w.abs() * 1e-10 + 1e-12;
        assert!(
            (g - w).abs() <= tol,
            "f64 exp/ln off: got {g}, want {w} (tol {tol})\nfull:\n{out}"
        );
    }
    let z0: f64 = lines[8].parse().unwrap();
    assert!(
        z0.is_infinite() && z0 < 0.0,
        "ln(0) should be -inf, got {}",
        lines[8]
    );
    let zn: f64 = lines[9].parse().unwrap();
    assert!(zn.is_nan(), "ln(-2) should be NaN, got {}", lines[9]);
}

#[test]
fn test_ir_vector_floor_uses_vector_intrinsic() {
    // `std.simd.math` rounding (phase-11): `v.floor()` on a `Vector[f32, 4]`
    // lowers to the overloaded LLVM VECTOR intrinsic `@llvm.floor.v4f32`
    // (one hardware `roundps`), not a scalarized per-lane call.
    let ir = ir_for(
        r#"
fn f(a: f32, b: f32, c: f32, d: f32) -> f32 {
    let v: Vector[f32, 4] = Vector[f32, 4](a, b, c, d);
    let r = v.floor();
    r[0]
}
fn main() { println(f(1.5f32, 2.5f32, 3.5f32, 4.5f32)); }
"#,
    );
    assert!(
        ir.contains("@llvm.floor.v4f32"),
        "v.floor() should lower to the vector intrinsic @llvm.floor.v4f32; got:\n{ir}"
    );
}

#[test]
fn test_e2e_vector_simd_math_rounding() {
    // floor/ceil/round/trunc on a float vector (std.simd.math). `round` is
    // half-away-from-zero (llvm.round). Lanes [2.5, -2.5] pin the distinct
    // rounding directions: floor→[2,-3], ceil→[3,-2], round→[3,-3] (ties
    // away from zero, NOT to-even), trunc→[2,-2]. Matches the interpreter
    // twin tests/interpreter.rs::test_vector_simd_math_rounding.
    let out = run_program(
        r#"
fn main() {
    let v = Vector[f32, 4].from_array([2.5f32, -2.5f32, 2.7f32, -2.3f32]);
    let fl = v.floor();
    println(fl[0]); println(fl[1]);
    let ce = v.ceil();
    println(ce[0]); println(ce[1]);
    let ro = v.round();
    println(ro[0]); println(ro[1]);
    let tr = v.trunc();
    println(tr[0]); println(tr[1]);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out, "2\n-3\n3\n-2\n3\n-3\n2\n-2\n");
    }
}

#[test]
fn test_ir_vector_to_bits_uses_bitcast() {
    // `std.simd.math` bit-reinterpretation (phase-11): `v.to_bits()` on a
    // `Vector[f32, 4]` lowers to a single LLVM vector `bitcast`
    // `<4 x float>` → `<4 x i32>` (no per-lane scalarization).
    let ir = ir_for(
        r#"
fn f(a: f32, b: f32, c: f32, d: f32) -> i32 {
    let v: Vector[f32, 4] = Vector[f32, 4](a, b, c, d);
    let bits = v.to_bits();
    bits[0]
}
fn main() { println(f(1.0f32, 2.0f32, 3.0f32, 4.0f32)); }
"#,
    );
    assert!(
        ir.contains("bitcast <4 x float>") && ir.contains("to <4 x i32>"),
        "v.to_bits() should lower to a <4 x float> -> <4 x i32> bitcast; got:\n{ir}"
    );
}

#[test]
fn test_e2e_vector_simd_math_bits() {
    // Element-wise IEEE-754 bitcast round-trip on float vectors
    // (std.simd.math). Known patterns: 1.0f32 = 0x3F800000 = 1065353216;
    // 1.0f64 = 0x3FF0000000000000 = 4607182418800017408. to_bits ->
    // bits_as_f* recovers the original. Byte-identical to the interpreter
    // twin tests/interpreter.rs::test_vector_simd_math_bits_roundtrip.
    let out = run_program(
        r#"
fn main() {
    let v = Vector[f32, 4].from_array([1.0f32, 2.0f32, 0.0f32, -1.0f32]);
    let b = v.to_bits();
    println(b[0]);
    let r = b.bits_as_f32();
    println(r[3]);
    let w = Vector[f64, 2].from_array([1.0, 2.0]);
    let wb = w.to_bits();
    println(wb[0]);
    let wr = wb.bits_as_f64();
    println(wr[1]);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out, "1065353216\n-1\n4607182418800017408\n2\n");
    }
}

#[test]
fn test_ir_vector_shift_signed_unsigned() {
    // `std.simd.math` (phase-11): `<<` lowers to `shl <N x iX>`; `>>` is
    // `lshr` (logical) on an unsigned-lane vector and `ashr` (arithmetic)
    // on a signed-lane vector — matching the scalar shift semantics.
    let ir_u = ir_for(
        r#"
fn f(a: u32) -> u32 {
    let v: Vector[u32, 4] = Vector[u32, 4](a, a, a, a);
    let amt: Vector[u32, 4] = Vector[u32, 4](1u32, 1u32, 1u32, 1u32);
    let l = v << amt;
    let r = l >> amt;
    r[0]
}
fn main() { println(f(8u32)); }
"#,
    );
    assert!(
        ir_u.contains("shl <4 x i32>") && ir_u.contains("lshr <4 x i32>"),
        "unsigned vector shift should emit shl + lshr; got:\n{ir_u}"
    );
    let ir_s = ir_for(
        r#"
fn f(a: i32) -> i32 {
    let v: Vector[i32, 4] = Vector[i32, 4](a, a, a, a);
    let amt: Vector[i32, 4] = Vector[i32, 4](1, 1, 1, 1);
    let r = v >> amt;
    r[0]
}
fn main() { println(f(-8)); }
"#,
    );
    assert!(
        ir_s.contains("ashr <4 x i32>"),
        "signed vector >> should emit ashr; got:\n{ir_s}"
    );
}

#[test]
fn test_e2e_vector_integer_shift() {
    // Element-wise `<<` / `>>` on integer vectors (std.simd.math). `>>` is
    // logical on unsigned lanes, arithmetic on signed. The last line
    // exercises the Sleef 2^n idiom: bits_as_f32((n + 127) << 23), n=3 →
    // 8.0. Byte-identical to the interpreter twin
    // tests/interpreter.rs::test_vector_integer_shift.
    let out = run_program(
        r#"
fn main() {
    let v = Vector[u32, 4].from_array([1u32, 2u32, 3u32, 4u32]);
    let l = v << Vector[u32, 4].splat(3u32);
    println(l[0]);
    println(l[3]);
    let r = Vector[u32, 4].splat(2147483648u32) >> Vector[u32, 4].splat(4u32);
    println(r[0]);
    let ar = Vector[i32, 4].splat(-16) >> Vector[i32, 4].splat(2);
    println(ar[0]);
    let expo = (Vector[u32, 4].splat(3u32) + Vector[u32, 4].splat(127u32))
        << Vector[u32, 4].splat(23u32);
    let pow = expo.bits_as_f32();
    println(pow[0]);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out, "8\n32\n134217728\n-4\n8\n");
    }
}

#[test]
fn test_vector_dot_i64() {
    let out = run_program(
        r#"
fn main() {
    let a = Vector[i64, 4](1, 2, 3, 4);
    let b = Vector[i64, 4](10, 20, 30, 40);
    println(a.dot(b));
}
"#,
    );
    if let Some(out) = out {
        // 10 + 40 + 90 + 160 = 300
        assert_eq!(out, "300\n");
    }
}

#[test]
fn test_vector_dot_f64() {
    let out = run_program(
        r#"
fn main() {
    let a = Vector[f64, 2](2.0, 3.0);
    let b = Vector[f64, 2](4.0, 5.0);
    println(a.dot(b));
}
"#,
    );
    if let Some(out) = out {
        // 8 + 15 = 23
        assert_eq!(out, "23\n");
    }
}

#[test]
fn test_vector_dot_mismatched_type_is_type_error() {
    let errs = vector_typecheck_errors(
        r#"
fn main() {
    let a = Vector[i64, 4](1, 2, 3, 4);
    let b = Vector[i64, 2](1, 2);
    let _ = a.dot(b);
}
"#,
    );
    assert!(
        !errs.is_empty(),
        "dot between Vector[i64,4] and Vector[i64,2] must be a type error"
    );
}

#[test]
fn test_vector_unknown_method_is_type_error() {
    let errs = vector_typecheck_errors(
        "fn main() { let a = Vector[i64, 2](1, 2); let _ = a.frobnicate(); }",
    );
    assert!(
        !errs.is_empty(),
        "an unknown Vector method must be a type error"
    );
}

// ── Vector slice 2b — product + bitwise reductions ───────────────────

#[test]
fn test_vector_reduce_product_i64() {
    let out = run_program(
        "fn main() { let v = Vector[i64, 4](1, 2, 3, 4); println(v.reduce_product()); }",
    );
    if let Some(out) = out {
        assert_eq!(out, "24\n");
    }
}

#[test]
fn test_vector_reduce_and_i64() {
    let out =
        run_program("fn main() { let v = Vector[i64, 4](15, 7, 3, 1); println(v.reduce_and()); }");
    if let Some(out) = out {
        assert_eq!(out, "1\n");
    }
}

#[test]
fn test_vector_reduce_or_i64() {
    let out =
        run_program("fn main() { let v = Vector[i64, 4](1, 2, 4, 8); println(v.reduce_or()); }");
    if let Some(out) = out {
        assert_eq!(out, "15\n");
    }
}

#[test]
fn test_vector_reduce_xor_i64() {
    let out =
        run_program("fn main() { let v = Vector[i64, 4](1, 2, 4, 8); println(v.reduce_xor()); }");
    if let Some(out) = out {
        assert_eq!(out, "15\n");
    }
}

#[test]
fn test_vector_reduce_and_on_float_is_type_error() {
    let errs = vector_typecheck_errors(
        "fn main() { let v = Vector[f64, 2](1.0, 2.0); let _ = v.reduce_and(); }",
    );
    assert!(
        !errs.is_empty(),
        "bitwise reduce_and on a float vector must be a type error"
    );
}

// ── Vector slice 2c — min/max (signed-int + float) ───────────────────

#[test]
fn test_vector_reduce_min_max_i64() {
    let out = run_program(
        r#"
fn main() {
    let v = Vector[i64, 4](3, 1, 4, 2);
    println(v.reduce_min());
    println(v.reduce_max());
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out, "1\n4\n");
    }
}

#[test]
fn test_vector_reduce_min_max_i64_negative() {
    // Signed compare: -5 min, 2 max.
    let out = run_program(
        r#"
fn main() {
    let v = Vector[i64, 3](-5, 2, -3);
    println(v.reduce_min());
    println(v.reduce_max());
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out, "-5\n2\n");
    }
}

#[test]
fn test_vector_reduce_min_max_f64() {
    let out = run_program(
        r#"
fn main() {
    let v = Vector[f64, 2](2.5, 1.5);
    println(v.reduce_min());
    println(v.reduce_max());
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out, "1.5\n2.5\n");
    }
}

#[test]
fn test_vector_reduce_min_max_u32_unsigned() {
    // Unsigned compare (`ult`/`ugt`). Slice 2e-ii: the `unsigned_vector_exprs`
    // side-table flags the u32 element so codegen picks the unsigned
    // predicate. `3000000000` is the most-negative value as a signed i32,
    // so a signed `slt`/`sgt` would (wrongly) make it the min and pick `10`
    // as the max — `3000000000\n10\n`. The unsigned compare yields the
    // correct `5\n4000000000\n`, which is what distinguishes this test.
    let out = run_program(
        r#"
fn main() {
    let v = Vector[u32, 4](3000000000, 5, 10, 4000000000);
    println(v.reduce_min());
    println(v.reduce_max());
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out, "5\n4000000000\n");
    }
}

#[test]
fn test_vector_reduce_min_max_u8_unsigned() {
    // u8 breadth. `200` and `255` have the high bit set → negative as i8,
    // so a signed compare would invert min/max. Unsigned: min=10, max=255.
    let out = run_program(
        r#"
fn main() {
    let v = Vector[u8, 4](200, 10, 50, 255);
    println(v.reduce_min());
    println(v.reduce_max());
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out, "10\n255\n");
    }
}

/// `println(v[i])` on an unsigned-element vector must print the lane
/// unsigned: the lane is a genuine narrow uint since the literal-width
/// lane coercion (2026-06-07), so the print path needs the receiver's
/// element signedness — recorded by the typechecker at the Index span
/// (`vector_method_receivers`, folded into `unsigned_vector_exprs`),
/// since the Index node's `expr_types` entry holds the scalar element
/// type. Pre-coercion the lanes were i64-wide positives and printed
/// correctly by accident.
#[test]
fn test_vector_u8_lane_read_prints_unsigned() {
    let out = run_program(
        r#"
fn main() {
    let v = Vector[u8, 4](200, 10, 50, 255);
    println(v[0]);
    println(v[3]);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out, "200\n255\n");
    }
}

// ── Vector slice 2c — cross product (Vector[T, 3] only) ──────────────

#[test]
fn test_vector_cross_i64() {
    // (2,3,4) × (5,6,7): c0 = 3*7-4*6 = -3; c1 = 4*5-2*7 = 6;
    // c2 = 2*6-3*5 = -3. Asserting all three lanes pins the component
    // ordering and signs (a single lane wouldn't catch a swap).
    let out = run_program(
        r#"
fn main() {
    let a: Vector[i64, 3] = Vector[i64, 3](2, 3, 4);
    let b: Vector[i64, 3] = Vector[i64, 3](5, 6, 7);
    let c = a.cross(b);
    println(c[0]);
    println(c[1]);
    println(c[2]);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out, "-3\n6\n-3\n");
    }
}

#[test]
fn test_vector_cross_f64_orthonormal() {
    // x̂ × ŷ = ẑ: (1,0,0) × (0,1,0) = (0,0,1).
    let out = run_program(
        r#"
fn main() {
    let a: Vector[f64, 3] = Vector[f64, 3](1.0, 0.0, 0.0);
    let b: Vector[f64, 3] = Vector[f64, 3](0.0, 1.0, 0.0);
    let c = a.cross(b);
    println(c[0]);
    println(c[1]);
    println(c[2]);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out, "0\n0\n1\n");
    }
}

#[test]
fn test_vector_cross_non_three_lane_is_type_error() {
    let errs = vector_typecheck_errors(
        r#"
fn main() {
    let a = Vector[i64, 4](1, 2, 3, 4);
    let b = Vector[i64, 4](5, 6, 7, 8);
    let _ = a.cross(b);
}
"#,
    );
    assert!(
        !errs.is_empty(),
        "cross on a 4-lane vector must be a type error (3D only)"
    );
}

#[test]
fn test_vector_cross_mismatched_type_is_type_error() {
    let errs = vector_typecheck_errors(
        r#"
fn main() {
    let a = Vector[i64, 3](1, 2, 3);
    let b = Vector[f64, 3](1.0, 2.0, 3.0);
    let _ = a.cross(b);
}
"#,
    );
    assert!(
        !errs.is_empty(),
        "cross between differently-typed vectors must be a type error"
    );
}

// ── Vector slice 2d — splat (scalar broadcast) ───────────────────────

#[test]
fn test_vector_splat_i64() {
    let out = run_program(
        r#"
fn main() {
    let v = Vector[i64, 4].splat(7);
    println(v[0]);
    println(v[3]);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out, "7\n7\n");
    }
}

#[test]
fn test_vector_splat_enables_scalar_broadcast_arithmetic() {
    // [1,2,3,4] + splat(10) = [11,12,13,14] — the canonical splat use.
    let out = run_program(
        r#"
fn main() {
    let v: Vector[i64, 4] = Vector[i64, 4](1, 2, 3, 4);
    let r = v + Vector[i64, 4].splat(10);
    println(r[0]);
    println(r[3]);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out, "11\n14\n");
    }
}

#[test]
fn test_vector_splat_arity_is_type_error() {
    let errs = vector_typecheck_errors("fn main() { let _ = Vector[i64, 4].splat(1, 2); }");
    assert!(
        !errs.is_empty(),
        "splat with more than one argument must be a type error"
    );
}

// ── Vector slice 2e — from_array (fixed-array construction) ───────────

#[test]
fn test_vector_from_array_i64() {
    // Vector[i64, 4].from_array([10, 20, 30, 40]) → lanes in order.
    let out = run_program(
        r#"
fn main() {
    let v = Vector[i64, 4].from_array([10, 20, 30, 40]);
    println(v[0]);
    println(v[3]);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out, "10\n40\n");
    }
}

#[test]
fn test_vector_from_array_feeds_arithmetic() {
    // from_array participates in element-wise vector ops:
    // [1,2,3,4] + [10,20,30,40] = [11,22,33,44].
    let out = run_program(
        r#"
fn main() {
    let a = Vector[i64, 4].from_array([1, 2, 3, 4]);
    let b = Vector[i64, 4].from_array([10, 20, 30, 40]);
    let r = a + b;
    println(r[0]);
    println(r[3]);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out, "11\n44\n");
    }
}

#[test]
fn test_vector_from_array_f64() {
    let out = run_program(
        r#"
fn main() {
    let v = Vector[f64, 2].from_array([1.5, 2.5]);
    println(v[0]);
    println(v[1]);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out, "1.5\n2.5\n");
    }
}

// ── Vector slice 2e-iii — from_slice (runtime-length construction) ────

#[test]
fn test_vector_from_slice_i64() {
    // Slice header `{ptr, i64 len}` → `<4 x i64>`. The `len == N` runtime
    // guard passes (4 == 4), then each lane is loaded from `data[i]`.
    let out = run_program(
        r#"
fn main() {
    let a: Array[i64, 4] = [10, 20, 30, 40];
    let v = Vector[i64, 4].from_slice(a.as_slice());
    println(v.reduce_sum());
    println(v[0]);
    println(v[3]);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out, "100\n10\n40\n");
    }
}

#[test]
fn test_vector_from_slice_subslice_offset() {
    // A range-indexed sub-slice `a[1..5]` carries a data pointer offset to
    // the 2nd element — proves codegen loads from the slice's own `data`
    // pointer (window {2,3,4,5}), not the source array base.
    let out = run_program(
        r#"
fn main() {
    let a: Array[i64, 6] = [1, 2, 3, 4, 5, 6];
    let v = Vector[i64, 4].from_slice(a[1..5]);
    println(v[0]);
    println(v[3]);
    println(v.reduce_sum());
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out, "2\n5\n14\n");
    }
}

// ── Vector slice 3a — bitwise & | ^ (binary) and ~ (unary) ───────────

#[test]
fn test_vector_bitwise_and_or_xor() {
    // `build_and`/`build_or`/`build_xor` lower directly on `<4 x i64>`.
    let out = run_program(
        r#"
fn main() {
    let a = Vector[i64, 4](12, 10, 15, 3);
    let b = Vector[i64, 4](10, 6, 1, 3);
    let band = a & b;
    let bor = a | b;
    let bxor = a ^ b;
    println(band[0]); // 12 & 10 = 8
    println(bor[1]);  // 10 | 6  = 14
    println(bxor[2]); // 15 ^ 1  = 14
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out, "8\n14\n14\n");
    }
}

#[test]
fn test_vector_bitnot() {
    // Unary `~v` lowers via `build_not` on the `<4 x i64>` operand.
    let out = run_program(
        r#"
fn main() {
    let a = Vector[i64, 4](0, 3, -1, 255);
    let n = ~a;
    println(n[0]); // ~0   = -1
    println(n[1]); // ~3   = -4
    println(n[2]); // ~-1  = 0
    println(n[3]); // ~255 = -256
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out, "-1\n-4\n0\n-256\n");
    }
}

// ── Vector slice 3b — comparison → Mask[N] + select ──────────────────

#[test]
fn test_vector_compare_mask() {
    // `<`/`==` lower to `build_int_compare` on the vector → `<4 x i1>`;
    // `m[i]` extractelements an i1 (== Kāra bool).
    let out = run_program(
        r#"
fn main() {
    let a = Vector[i64, 4](1, 5, 3, 8);
    let b = Vector[i64, 4](4, 2, 3, 6);
    let lt = a < b;
    let eq = a == b;
    println(lt[0]); println(lt[1]); println(eq[2]); println(eq[0]);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out, "true\nfalse\ntrue\nfalse\n");
    }
}

#[test]
fn test_vector_compare_unsigned_mask() {
    // Unsigned predicate (`ult`): 3000000000 is the most-negative i32, so a
    // signed compare would wrongly make it `< 10`. The mask must read false.
    let out = run_program(
        r#"
fn main() {
    let a = Vector[u32, 2](3000000000, 5);
    let b = Vector[u32, 2](10, 10);
    let m = a < b;
    println(m[0]); println(m[1]);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out, "false\ntrue\n");
    }
}

// ── Slice 6a — lane permutations (reverse / rotate_lanes_*) ──────────

#[test]
fn test_vector_reverse() {
    // reverse reverses lane order: (1,2,3,4) -> (4,3,2,1).
    let out = run_program(
        r#"
fn main() {
    let a = Vector[i64, 4](1, 2, 3, 4);
    let r = a.reverse();
    println(r[0]); println(r[1]); println(r[2]); println(r[3]);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out, "4\n3\n2\n1\n");
    }
}

#[test]
fn test_vector_rotate_lanes_left() {
    // rotate left by 1: result lane i = source lane (i+1) mod 4.
    // (10,20,30,40) -> (20,30,40,10).
    let out = run_program(
        r#"
fn main() {
    let a = Vector[i64, 4](10, 20, 30, 40);
    let r = a.rotate_lanes_left(1);
    println(r[0]); println(r[1]); println(r[2]); println(r[3]);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out, "20\n30\n40\n10\n");
    }
}

#[test]
fn test_vector_rotate_lanes_right() {
    // rotate right by 1: result lane i = source lane (i+N-1) mod 4.
    // (10,20,30,40) -> (40,10,20,30).
    let out = run_program(
        r#"
fn main() {
    let a = Vector[i64, 4](10, 20, 30, 40);
    let r = a.rotate_lanes_right(1);
    println(r[0]); println(r[1]); println(r[2]); println(r[3]);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out, "40\n10\n20\n30\n");
    }
}

#[test]
fn test_vector_rotate_wraps_modulo_lanes() {
    // A rotate amount >= N wraps: rotate_left(5) on 4 lanes == rotate_left(1).
    let out = run_program(
        r#"
fn main() {
    let a = Vector[i64, 4](10, 20, 30, 40);
    let r = a.rotate_lanes_left(5);
    println(r[0]); println(r[1]); println(r[2]); println(r[3]);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out, "20\n30\n40\n10\n");
    }
}

#[test]
fn test_vector_rotate_non_literal_is_type_error() {
    let errs = vector_typecheck_errors(
        r#"
fn main() {
    let a = Vector[i64, 4](1, 2, 3, 4);
    let n = 2;
    let _ = a.rotate_lanes_left(n);
}
"#,
    );
    assert!(
        !errs.is_empty(),
        "a non-literal rotate amount must be a type error"
    );
}

#[test]
fn test_vector_reverse_with_arg_is_type_error() {
    let errs = vector_typecheck_errors(
        r#"
fn main() {
    let a = Vector[i64, 4](1, 2, 3, 4);
    let _ = a.reverse(2);
}
"#,
    );
    assert!(!errs.is_empty(), "reverse takes no arguments");
}

// ── Slice 6d — lane replace (v.replace(i, x)) ───────────────────────

#[test]
fn test_vector_replace() {
    // replace(2, 99) sets lane 2 in a new vector; the original is unchanged
    // (value semantics — a[2] still reads 3).
    let out = run_program(
        r#"
fn main() {
    let a = Vector[i64, 4](1, 2, 3, 4);
    let r = a.replace(2, 99);
    println(r[0]); println(r[1]); println(r[2]); println(r[3]);
    println(a[2]);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out, "1\n2\n99\n4\n3\n");
    }
}

#[test]
fn test_vector_replace_runtime_index() {
    // A runtime (non-literal) index lowers to insertelement with a dynamic
    // index; the bounds check passes for an in-range index.
    let out = run_program(
        r#"
fn main() {
    let a = Vector[f64, 4](1.0, 2.0, 3.0, 4.0);
    let i = 3;
    let r = a.replace(i, 9.5);
    println(r[3]);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out, "9.5\n");
    }
}

#[test]
fn test_vector_replace_out_of_bounds_panics() {
    // An out-of-range lane index traps (UGE bounds check → panic), exactly
    // like the `v[i]` lane read.
    let captured = run_program_capturing(
        r#"
fn main() {
    let a = Vector[i64, 4](1, 2, 3, 4);
    let r = a.replace(9, 0);
    println(r[0]);
}
"#,
    );
    if let Some(c) = captured {
        assert!(
            c.stderr.contains("vector lane index out of bounds"),
            "expected vector lane OOB panic, got stdout={:?} stderr={:?}",
            c.stdout,
            c.stderr
        );
    }
}

#[test]
fn test_vector_replace_wrong_arity_is_type_error() {
    let errs = vector_typecheck_errors(
        r#"
fn main() {
    let a = Vector[i64, 4](1, 2, 3, 4);
    let _ = a.replace(0);
}
"#,
    );
    assert!(!errs.is_empty(), "replace takes exactly two arguments");
}

// ── Slice 6b — lane shuffle (v.shuffle([..])) ───────────────────────

#[test]
fn test_vector_shuffle_permute() {
    // shuffle gathers source lanes by index: ([0,2,1,3]) reorders.
    // (10,20,30,40) -> (10,30,20,40).
    let out = run_program(
        r#"
fn main() {
    let a = Vector[i64, 4](10, 20, 30, 40);
    let r = a.shuffle([0, 2, 1, 3]);
    println(r[0]); println(r[1]); println(r[2]); println(r[3]);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out, "10\n30\n20\n40\n");
    }
}

#[test]
fn test_vector_shuffle_widening_with_repeats() {
    // The index list length M may differ from the source N, and indices may
    // repeat: a 2-lane source shuffled into a 4-lane result.
    let out = run_program(
        r#"
fn main() {
    let a = Vector[i64, 2](7, 9);
    let r = a.shuffle([1, 0, 1, 0]);
    println(r[0]); println(r[1]); println(r[2]); println(r[3]);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out, "9\n7\n9\n7\n");
    }
}

#[test]
fn test_vector_shuffle_out_of_range_index_is_type_error() {
    let errs = vector_typecheck_errors(
        r#"
fn main() {
    let a = Vector[i64, 4](1, 2, 3, 4);
    let _ = a.shuffle([0, 4, 1, 2]);
}
"#,
    );
    assert!(
        !errs.is_empty(),
        "shuffle index 4 is out of range for a 4-lane source"
    );
}

#[test]
fn test_vector_shuffle_non_literal_index_is_type_error() {
    let errs = vector_typecheck_errors(
        r#"
fn main() {
    let a = Vector[i64, 4](1, 2, 3, 4);
    let n = 2;
    let _ = a.shuffle([0, n]);
}
"#,
    );
    assert!(
        !errs.is_empty(),
        "shuffle indices must be compile-time integer literals"
    );
}

#[test]
fn test_vector_shuffle_non_array_arg_is_type_error() {
    let errs = vector_typecheck_errors(
        r#"
fn main() {
    let a = Vector[i64, 4](1, 2, 3, 4);
    let _ = a.shuffle(0);
}
"#,
    );
    assert!(
        !errs.is_empty(),
        "shuffle requires an array-literal index list"
    );
}

// ── Slice 6e — masked load (Vector::load_masked(slice, mask)) ────────

#[test]
fn test_vector_load_masked_tail() {
    // Tail handling: a 2-element slice loaded into a 4-lane vector with a
    // mask true for the first two lanes. Active lanes load slice[i],
    // inactive lanes read 0 — no out-of-bounds access past the slice.
    let out = run_program(
        r#"
fn main() {
    let a: Array[i64, 6] = [10, 20, 30, 40, 50, 60];
    let tail = a[0..2];
    let idx = Vector[i64, 4](0, 1, 2, 3);
    let lim = Vector[i64, 4](2, 2, 2, 2);
    let m = idx < lim;
    let v = Vector[i64, 4].load_masked(tail, m);
    println(v[0]); println(v[1]); println(v[2]); println(v[3]);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out, "10\n20\n0\n0\n");
    }
}

#[test]
fn test_vector_load_masked_all_active() {
    // An all-true mask over a full-length slice loads every lane.
    let out = run_program(
        r#"
fn main() {
    let a: Array[i64, 4] = [5, 6, 7, 8];
    let idx = Vector[i64, 4](0, 1, 2, 3);
    let lim = Vector[i64, 4](9, 9, 9, 9);
    let m = idx < lim;
    let v = Vector[i64, 4].load_masked(a.as_slice(), m);
    println(v[0]); println(v[1]); println(v[2]); println(v[3]);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out, "5\n6\n7\n8\n");
    }
}

#[test]
fn test_vector_load_masked_float() {
    // Float lanes: masked-off lanes read 0.0 (typed zero).
    let out = run_program(
        r#"
fn main() {
    let a: Array[f64, 4] = [1.5, 2.5, 3.5, 4.5];
    let idx = Vector[i64, 4](0, 1, 2, 3);
    let lim = Vector[i64, 4](2, 2, 2, 2);
    let m = idx < lim;
    let v = Vector[f64, 4].load_masked(a.as_slice(), m);
    println(v[0]); println(v[1]); println(v[2]); println(v[3]);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out, "1.5\n2.5\n0\n0\n");
    }
}

#[test]
fn test_vector_load_masked_active_oob_panics() {
    // An active lane whose index is past the slice length traps, like the
    // `v[i]` lane read (a 1-element slice with lane 1 active).
    let captured = run_program_capturing(
        r#"
fn main() {
    let a: Array[i64, 4] = [10, 20, 30, 40];
    let one = a[0..1];
    let idx = Vector[i64, 4](0, 1, 2, 3);
    let lim = Vector[i64, 4](2, 2, 2, 2);
    let m = idx < lim;
    let v = Vector[i64, 4].load_masked(one, m);
    println(v[0]);
}
"#,
    );
    if let Some(c) = captured {
        assert!(
            c.stderr
                .contains("load_masked: active lane index out of bounds"),
            "expected active-lane OOB panic, got stdout={:?} stderr={:?}",
            c.stdout,
            c.stderr
        );
    }
}

#[test]
fn test_vector_load_masked_wrong_mask_type_is_type_error() {
    // The mask must be a Vector[bool, N] — an i64 vector is rejected.
    let errs = vector_typecheck_errors(
        r#"
fn main() {
    let a: Array[i64, 4] = [1, 2, 3, 4];
    let bad = Vector[i64, 4](1, 0, 1, 0);
    let _ = Vector[i64, 4].load_masked(a.as_slice(), bad);
}
"#,
    );
    assert!(!errs.is_empty(), "load_masked mask must be Vector[bool, N]");
}

#[test]
fn test_vector_load_masked_wrong_arity_is_type_error() {
    let errs = vector_typecheck_errors(
        r#"
fn main() {
    let a: Array[i64, 4] = [1, 2, 3, 4];
    let _ = Vector[i64, 4].load_masked(a.as_slice());
}
"#,
    );
    assert!(!errs.is_empty(), "load_masked takes exactly two arguments");
}

#[test]
fn test_vector_load_masked_non_slice_is_type_error() {
    let errs = vector_typecheck_errors(
        r#"
fn main() {
    let idx = Vector[i64, 4](0, 1, 2, 3);
    let lim = Vector[i64, 4](2, 2, 2, 2);
    let m = idx < lim;
    let _ = Vector[i64, 4].load_masked(7, m);
}
"#,
    );
    assert!(
        !errs.is_empty(),
        "load_masked first argument must be a slice"
    );
}

// ── Slice 6e — masked store (v.store_masked(slice_mut, mask)) ────────

#[test]
fn test_vector_store_masked_partial() {
    // Writes active lanes through a `mut Slice[i64]`; inactive lanes leave
    // the destination untouched (lanes 0,1 written, 2,3 preserved).
    let out = run_program(
        r#"
fn fill(xs: mut Slice[i64]) {
    let v = Vector[i64, 4](10, 20, 30, 40);
    let idx = Vector[i64, 4](0, 1, 2, 3);
    let lim = Vector[i64, 4](2, 2, 2, 2);
    let m = idx < lim;
    v.store_masked(xs, m);
}
fn main() {
    let mut a: Array[i64, 4] = [1, 2, 3, 4];
    fill(mut a);
    println(a[0]); println(a[1]); println(a[2]); println(a[3]);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out, "10\n20\n3\n4\n");
    }
}

#[test]
fn test_vector_store_masked_float() {
    let out = run_program(
        r#"
fn fill(xs: mut Slice[f64]) {
    let v = Vector[f64, 4](1.5, 2.5, 3.5, 4.5);
    let idx = Vector[i64, 4](0, 1, 2, 3);
    let lim = Vector[i64, 4](1, 1, 1, 1);
    let m = idx < lim;
    v.store_masked(xs, m);
}
fn main() {
    let mut a: Array[f64, 4] = [0.0, 0.0, 0.0, 0.0];
    fill(mut a);
    println(a[0]); println(a[1]);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out, "1.5\n0\n");
    }
}

#[test]
fn test_vector_store_masked_active_oob_panics() {
    // A 2-element destination with an all-true mask: lanes 2,3 are active
    // but past the slice length, so the store traps.
    let captured = run_program_capturing(
        r#"
fn fill(xs: mut Slice[i64]) {
    let v = Vector[i64, 4](10, 20, 30, 40);
    let idx = Vector[i64, 4](0, 1, 2, 3);
    let lim = Vector[i64, 4](9, 9, 9, 9);
    let m = idx < lim;
    v.store_masked(xs, m);
}
fn main() {
    let mut a: Array[i64, 2] = [0, 0];
    fill(mut a);
    println(a[0]);
}
"#,
    );
    if let Some(c) = captured {
        assert!(
            c.stdout
                .contains("store_masked: active lane index out of bounds")
                || c.stderr
                    .contains("store_masked: active lane index out of bounds"),
            "expected active-lane OOB panic, got stdout={:?} stderr={:?}",
            c.stdout,
            c.stderr
        );
    }
}

#[test]
fn test_vector_store_masked_immutable_slice_is_type_error() {
    // The destination must be a `mut Slice[T]`; an immutable `Slice[T]`
    // param is rejected.
    let errs = vector_typecheck_errors(
        r#"
fn fill(xs: Slice[i64]) {
    let v = Vector[i64, 4](10, 20, 30, 40);
    let idx = Vector[i64, 4](0, 1, 2, 3);
    let lim = Vector[i64, 4](2, 2, 2, 2);
    let m = idx < lim;
    v.store_masked(xs, m);
}
fn main() {
    let a: Array[i64, 4] = [1, 2, 3, 4];
    fill(a);
}
"#,
    );
    assert!(
        !errs.is_empty(),
        "store_masked destination must be a mut Slice[T]"
    );
}

#[test]
fn test_vector_store_masked_wrong_mask_type_is_type_error() {
    let errs = vector_typecheck_errors(
        r#"
fn fill(xs: mut Slice[i64]) {
    let v = Vector[i64, 4](10, 20, 30, 40);
    let bad = Vector[i64, 4](1, 0, 1, 0);
    v.store_masked(xs, bad);
}
fn main() {
    let mut a: Array[i64, 4] = [1, 2, 3, 4];
    fill(mut a);
}
"#,
    );
    assert!(
        !errs.is_empty(),
        "store_masked mask must be Vector[bool, N]"
    );
}

// ── Slice 6f — gather (Vector::gather(slice, indices)) ───────────────

#[test]
fn test_vector_gather_permuted_indices() {
    // gather reads slice[indices[i]] per lane: idx (5,0,3,1) over
    // [10,20,30,40,50,60] → (60,10,40,20).
    let out = run_program(
        r#"
fn main() {
    let a: Array[i64, 6] = [10, 20, 30, 40, 50, 60];
    let idx = Vector[i64, 4](5, 0, 3, 1);
    let v = Vector[i64, 4].gather(a.as_slice(), idx);
    println(v[0]); println(v[1]); println(v[2]); println(v[3]);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out, "60\n10\n40\n20\n");
    }
}

#[test]
fn test_vector_gather_repeated_indices_float() {
    // Indices may repeat; float element type.
    let out = run_program(
        r#"
fn main() {
    let a: Array[f64, 3] = [1.5, 2.5, 3.5];
    let idx = Vector[i64, 4](2, 2, 0, 1);
    let v = Vector[f64, 4].gather(a.as_slice(), idx);
    println(v[0]); println(v[1]); println(v[2]); println(v[3]);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out, "3.5\n3.5\n1.5\n2.5\n");
    }
}

#[test]
fn test_vector_gather_out_of_bounds_panics() {
    // An index past the slice length traps (UGE bounds check).
    let captured = run_program_capturing(
        r#"
fn main() {
    let a: Array[i64, 4] = [10, 20, 30, 40];
    let idx = Vector[i64, 4](0, 1, 9, 2);
    let v = Vector[i64, 4].gather(a.as_slice(), idx);
    println(v[0]);
}
"#,
    );
    if let Some(c) = captured {
        assert!(
            c.stdout.contains("gather: index out of bounds")
                || c.stderr.contains("gather: index out of bounds"),
            "expected gather OOB panic, got stdout={:?} stderr={:?}",
            c.stdout,
            c.stderr
        );
    }
}

#[test]
fn test_vector_gather_non_integer_indices_is_type_error() {
    let errs = vector_typecheck_errors(
        r#"
fn main() {
    let a: Array[i64, 4] = [10, 20, 30, 40];
    let idx = Vector[f64, 4](0.0, 1.0, 2.0, 3.0);
    let _ = Vector[i64, 4].gather(a.as_slice(), idx);
}
"#,
    );
    assert!(!errs.is_empty(), "gather indices must be an integer vector");
}

#[test]
fn test_vector_gather_wrong_lane_count_is_type_error() {
    let errs = vector_typecheck_errors(
        r#"
fn main() {
    let a: Array[i64, 4] = [10, 20, 30, 40];
    let idx = Vector[i64, 2](0, 1);
    let _ = Vector[i64, 4].gather(a.as_slice(), idx);
}
"#,
    );
    assert!(
        !errs.is_empty(),
        "gather indices must match the result lane count"
    );
}

// ── Slice 6f — scatter (v.scatter(slice_mut, indices)) ───────────────

#[test]
fn test_vector_scatter_permuted_indices() {
    // scatter writes slice[indices[i]] = v[i]: v (10,20,30,40) at idx
    // (3,1,0,2) → a = [30,20,40,10].
    let out = run_program(
        r#"
fn fill(xs: mut Slice[i64]) {
    let v = Vector[i64, 4](10, 20, 30, 40);
    let idx = Vector[i64, 4](3, 1, 0, 2);
    v.scatter(xs, idx);
}
fn main() {
    let mut a: Array[i64, 4] = [0, 0, 0, 0];
    fill(mut a);
    println(a[0]); println(a[1]); println(a[2]); println(a[3]);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out, "30\n20\n40\n10\n");
    }
}

#[test]
fn test_vector_scatter_partial_indices_float() {
    // Scatter into a subset of slots (indices need not cover the slice);
    // unwritten slots keep their prior value. Float element type.
    let out = run_program(
        r#"
fn fill(xs: mut Slice[f64]) {
    let v = Vector[f64, 2](1.5, 2.5);
    let idx = Vector[i64, 2](0, 2);
    v.scatter(xs, idx);
}
fn main() {
    let mut a: Array[f64, 3] = [9.0, 9.0, 9.0];
    fill(mut a);
    println(a[0]); println(a[1]); println(a[2]);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out, "1.5\n9\n2.5\n");
    }
}

#[test]
fn test_vector_scatter_out_of_bounds_panics() {
    let captured = run_program_capturing(
        r#"
fn fill(xs: mut Slice[i64]) {
    let v = Vector[i64, 4](10, 20, 30, 40);
    let idx = Vector[i64, 4](0, 1, 9, 2);
    v.scatter(xs, idx);
}
fn main() {
    let mut a: Array[i64, 4] = [0, 0, 0, 0];
    fill(mut a);
    println(a[0]);
}
"#,
    );
    if let Some(c) = captured {
        assert!(
            c.stdout.contains("scatter: index out of bounds")
                || c.stderr.contains("scatter: index out of bounds"),
            "expected scatter OOB panic, got stdout={:?} stderr={:?}",
            c.stdout,
            c.stderr
        );
    }
}

#[test]
fn test_vector_scatter_immutable_slice_is_type_error() {
    let errs = vector_typecheck_errors(
        r#"
fn fill(xs: Slice[i64]) {
    let v = Vector[i64, 4](10, 20, 30, 40);
    let idx = Vector[i64, 4](0, 1, 2, 3);
    v.scatter(xs, idx);
}
fn main() {
    let a: Array[i64, 4] = [1, 2, 3, 4];
    fill(a);
}
"#,
    );
    assert!(
        !errs.is_empty(),
        "scatter destination must be a mut Slice[T]"
    );
}

#[test]
fn test_vector_scatter_non_integer_indices_is_type_error() {
    let errs = vector_typecheck_errors(
        r#"
fn fill(xs: mut Slice[i64]) {
    let v = Vector[i64, 4](10, 20, 30, 40);
    let idx = Vector[f64, 4](0.0, 1.0, 2.0, 3.0);
    v.scatter(xs, idx);
}
fn main() {
    let mut a: Array[i64, 4] = [1, 2, 3, 4];
    fill(mut a);
}
"#,
    );
    assert!(
        !errs.is_empty(),
        "scatter indices must be an integer vector"
    );
}

// ── Slice 6c — element cast (Vector[U, N].cast_from(v)) ──────────────

#[test]
fn test_vector_cast_from_float_to_int() {
    // f64 → i64 truncates toward zero per lane (fptosi).
    let out = run_program(
        r#"
fn main() {
    let f = Vector[f64, 4](1.7, 2.2, 3.9, 4.0);
    let i = Vector[i64, 4].cast_from(f);
    println(i[0]); println(i[1]); println(i[2]); println(i[3]);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out, "1\n2\n3\n4\n");
    }
}

#[test]
fn test_vector_cast_from_int_to_float() {
    // i64 → f64 per lane (sitofp).
    let out = run_program(
        r#"
fn main() {
    let i = Vector[i64, 4](1, 2, 3, 4);
    let f = Vector[f64, 4].cast_from(i);
    println(f[0]); println(f[3]);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out, "1\n4\n");
    }
}

#[test]
fn test_vector_cast_from_unsigned_to_float() {
    // u8 → f64 uses uitofp (not sitofp): 200/255 must not become negative.
    let out = run_program(
        r#"
fn main() {
    let u = Vector[u8, 2](200, 255);
    let f = Vector[f64, 2].cast_from(u);
    println(f[0]); println(f[1]);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out, "200\n255\n");
    }
}

#[test]
fn test_vector_cast_from_wrong_lane_count_is_type_error() {
    let errs = vector_typecheck_errors(
        r#"
fn main() {
    let f = Vector[f64, 2](1.0, 2.0);
    let _ = Vector[i64, 4].cast_from(f);
}
"#,
    );
    assert!(
        !errs.is_empty(),
        "cast_from source must match the target lane count"
    );
}

// ── Slice 4 — first-class Numeric trait + lane-literal ergonomics ─────

#[test]
fn test_numeric_generic_arithmetic() {
    // `[T: Numeric]` arithmetic monomorphizes to concrete int/float codegen.
    let out = run_program(
        r#"
fn add3[T: Numeric](a: T, b: T, c: T) -> T { a + b + c }
fn neg[T: Numeric](x: T) -> T { -x }
fn main() {
    println(add3(1, 2, 3));
    println(add3(1.5, 2.5, 3.0));
    println(neg(5));
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out, "6\n7\n-5\n");
    }
}

#[test]
fn test_vector_f32_suffixless_lanes() {
    // f64 lane literals coerce to f32 lanes; `<4 x float>` arithmetic.
    let out = run_program(
        r#"
fn main() {
    let a = Vector[f32, 4](1.0, 2.0, 3.0, 4.0);
    let b = Vector[f32, 4](0.5, 0.5, 0.5, 0.5);
    let c = a * b;
    println(c[0]); println(c[3]);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out, "0.5\n2\n");
    }
}

#[test]
fn test_vector_i32_suffixless_lanes() {
    // i64 lane literals coerce to i32 lanes; `<4 x i32>` arithmetic.
    let out = run_program(
        r#"
fn main() {
    let a = Vector[i32, 4](1, 2, 3, 4);
    let b = Vector[i32, 4](10, 20, 30, 40);
    let c = a + b;
    println(c[0]); println(c[3]);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out, "11\n44\n");
    }
}

// ── Sub-64-bit scalar widths at ABI boundaries ──────────────────
//
// Codegen's internal convention is default-width scalars (unsuffixed
// int literals and annotated `let` slots are i64, float literals
// f64) while fn signatures lower at their declared width. The
// boundary coercion (`coerce_scalar_to_type` + the binop width
// harmonization) is what makes sub-64-bit signatures usable at all —
// before it, `fn f() -> i32 { return 0; }` emitted `ret i64 0`,
// `f(5)` against an i8 param emitted `call i8 @f(i64 5)`, and
// `x + 1` on an i8 param emitted `add nsw i8 %x, i64 1` (every one
// a module-verification failure; surfaced 2026-06-05 by the
// browser-WASM slice's smoke programs, filed + fixed via bugs.md).

#[test]
fn test_ir_sub64_int_return_coerces_to_declared_width() {
    let ir = ir_for(
        "fn f() -> i32 {\n    return 0;\n}\n\
             fn main() { println(f()); }\n",
    );
    let f_start = ir
        .find("define internal i32 @f(")
        .or_else(|| ir.find("define i32 @f("))
        .expect("fn f must lower at its declared i32 width");
    let f_body = &ir[f_start..ir[f_start..].find("\n}").map(|i| f_start + i).unwrap()];
    assert!(
        f_body.contains("ret i32"),
        "f's return must be coerced to the declared i32 width: {f_body}",
    );
    assert!(
        !f_body.contains("ret i64"),
        "no i64-width ret may survive in f: {f_body}",
    );
}

#[test]
fn test_e2e_sub64_struct_field_widths_and_print_signedness() {
    // The field-store leg of the boundary-coercion family
    // (follow-up to the ret/call/binop fix): a default-width
    // literal against a narrower declared field built a malformed
    // aggregate (plain struct — `s.b` read back 0) or stored 8
    // bytes over a 1-byte heap slot (shared struct — corrupting
    // the NEIGHBOR field, hence the `tail` integrity pins). Covers
    // struct-literal init, field assignment, mut-ref-param store,
    // and shared-struct init + assignment. The 200/199 values
    // double as print-signedness pins for the FieldAccess and
    // MethodCall arms of `expr_is_unsigned_int` (u8 results
    // sign-extended to -56 before those arms existed).
    let out = run_program(
        "struct S { mut b: u8, mut tail: i64 }\n\
             \n\
             shared struct Sh { mut b: u8, mut tail: i64 }\n\
             \n\
             impl S {\n\
                 fn get_b(ref self) -> u8 {\n        return self.b;\n    }\n\
             }\n\
             \n\
             fn poke(s: mut ref S) {\n    s.b = 201;\n}\n\
             \n\
             fn main() {\n\
                 let mut s = S { b: 1, tail: 7 };\n\
                 s.b = 200;\n\
                 println(s.b);\n\
                 println(s.tail);\n\
                 println(s.get_b());\n\
                 poke(mut s);\n\
                 println(s.b);\n\
                 let sh = Sh { b: 200, tail: 9 };\n\
                 println(sh.b);\n\
                 sh.b = 199;\n\
                 println(sh.b);\n\
                 println(sh.tail);\n\
             }\n",
    );
    if let Some(out) = out {
        assert_eq!(
            out, "200\n7\n200\n201\n200\n199\n9\n",
            "narrow struct fields must store at their declared width \
                 (neighbor fields intact) and print with their declared \
                 signedness",
        );
    }
}

/// B-2026-08-24-10 — a `break` value's slot is typed by the VALUE, not
/// hardcoded to i64. A float break used to store nothing at all (the
/// store was guarded on `is_int_value()`) and load whatever the slot
/// happened to hold: `fn f() -> f64 { loop { break 2.5 } }` printed 0
/// from an AOT binary and a garbage integer from the JIT, against 2.5
/// from the interpreter. A silent wrong answer, no diagnostic anywhere.
#[test]
fn test_e2e_loop_break_float_value_round_trips() {
    let out = run_program(
        r#"
fn pick() -> f64 {
    let mut i = 0;
    loop { i = i + 1; if i == 2 { break 2.5 } }
}

fn main() { println(pick()); }
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "2.5");
    }
}

/// B-2026-08-24-13 — a NON-scalar break value now travels correctly.
///
/// This test previously asserted the opposite: aggregates were refused,
/// because storing a `{ptr, i64, i64}` String header into the slot gave
/// its buffer a second owner and the loop body's drain freed it out from
/// under the receiver ("free(): double free detected in tcache 2").
/// `compile_break` now suppresses the source's scope-exit free between the
/// store and the drain — the break-site twin of what
/// `suppress_cleanup_for_tail_return` does for a function tail — so the
/// value leaves with exactly one owner.
#[test]
fn test_e2e_loop_break_string_value_round_trips() {
    let out = run_program(
        r#"
fn pick() -> String {
    let mut i = 0;
    loop { i = i + 1; if i == 2 { break f"got{i}" } }
}

fn main() { println(pick()); }
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "got2");
    }
}

/// B-2026-08-24-21 — a `shared` value carried out of a loop by `break`.
///
/// An RC'd value moves out by RETAIN, not by suppression: the source's
/// queued dec still fires and `suppress_source_vec_cleanup_for_arg` emits
/// the balancing `+1`, so it leaves at net +1 for the receiver — the same
/// shape the function tail emits for `fn f() -> Node { let n = ...; n }`.
/// Disarming the source instead (the Map/Set treatment) hangs, because the
/// zero-store lands before the retain reads the slot.
#[test]
fn test_e2e_loop_break_shared_struct_round_trips() {
    let out = run_program(
        r#"
shared struct Node { v: i64 }

fn pick() -> Node {
    let mut i = 0;
    loop {
        i = i + 1;
        let n = Node { v: i * 11 };
        if i == 3 { break n }
    }
}

fn main() { println(pick().v); }
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "33");
    }
}

/// The `shared enum` sibling — a different heap layout through the same
/// transfer, and the shape that bus-errored under the wrong mechanism.
#[test]
fn test_e2e_loop_break_shared_enum_round_trips() {
    let out = run_program(
        r#"
shared enum Tree { Leaf(i64), Node(i64, i64) }

fn pick() -> Tree {
    let mut i = 0;
    loop {
        i = i + 1;
        let t = Tree.Node(i, i * 2);
        if i == 2 { break t }
    }
}

fn main() { match pick() { Leaf(a) => println(a), Node(a, b) => println(a + b) } }
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "6");
    }
}

/// B-2026-08-24-19 — a `Map` handle carried out of a loop by `break`.
///
/// Map/Set cleanup is queue-driven (`FreeMapHandle`) with no in-slot
/// sentinel, so the existing move suppressor retracts the queued action at
/// COMPILE time. That is wrong at a conditional break, so this disarms by
/// ZEROING the slot at runtime instead and `FreeMapHandle` gained the
/// null-guard that makes the queued action inert exactly on that path.
#[test]
fn test_e2e_loop_break_map_handle_round_trips() {
    let out = run_program(
        r#"
fn pick() -> Map[String, i64] {
    let mut i = 0;
    loop {
        i = i + 1;
        let mut m: Map[String, i64] = Map.new();
        m.insert(f"k{i}", i * 7);
        if i == 3 { break m }
    }
}

fn main() { let m = pick(); println(m.get(f"k3").unwrap()); }
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "21");
    }
}

/// The Identifier carrier, which needs the OTHER suppressor: here the
/// buffer belongs to a tracked binding rather than an f-string
/// accumulator, and a fresh one is allocated on EVERY iteration — so the
/// iterations that do not break must still free theirs. A leak here would
/// be the mirror-image bug of the double free this fix removed.
#[test]
fn test_e2e_loop_break_string_binding_round_trips() {
    let out = run_program(
        r#"
fn pick() -> String {
    let mut i = 0;
    loop {
        i = i + 1;
        let s = f"row{i}";
        if i == 2 { break s }
    }
}

fn main() { println(pick()); }
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "row2");
    }
}

/// B-2026-08-25-1 — an RVALUE `break` value that owns heap: a `shared`
/// struct literal built directly in the break, with no binding anywhere.
///
/// Every mechanism the three preceding rows added is keyed on a source
/// BINDING — the disarm needs a slot to null, the retain reads the handle
/// back out of one — so this shape fell through the pointer gate and left
/// the loop's result slot unwritten. That surfaced as
/// `Module verification failed: ret i64 0`, not as a wrong answer.
///
/// The transfer needs NO ownership action at all, which is what the
/// `return` twin proves: `return Node { v: 1 }` in the same loop emits a
/// `malloc`, a `store i64 1` into the refcount, and a bare `ret` — nothing
/// retained, nothing queued. The temporary is born owned, so `break` only
/// ever needed permission to store it.
#[test]
fn test_e2e_loop_break_shared_struct_rvalue_round_trips() {
    let out = run_program(
        r#"
shared struct Node { v: i64 }

fn pick() -> Node {
    let mut i = 0;
    loop {
        i = i + 1;
        if i == 2 { break Node { v: i * 11 } }
    }
}

fn main() { println(pick().v); }
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "22");
    }
}

/// The `shared enum` rvalue sibling. A variant construction parses as a
/// CALL, so it reaches the fix through the call arm rather than the
/// struct-literal one — a different route to the same permission.
#[test]
fn test_e2e_loop_break_shared_enum_rvalue_round_trips() {
    let out = run_program(
        r#"
shared enum Tree { Leaf(i64), Node(i64, i64) }

fn pick() -> Tree {
    let mut i = 0;
    loop {
        i = i + 1;
        if i == 2 { break Tree.Node(i, i * 2) }
    }
}

fn main() { match pick() { Leaf(a) => println(a), Node(a, b) => println(a + b) } }
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "6");
    }
}

/// The `Map` rvalue — a call RETURN rather than a literal, so the handle
/// is manufactured one frame down and arrives already owned. The `Set`
/// surface shares this path (`Set` lowers to `Map[T, ()]`).
#[test]
fn test_e2e_loop_break_map_rvalue_round_trips() {
    let out = run_program(
        r#"
fn make_map(n: i64) -> Map[String, i64] {
    let mut m: Map[String, i64] = Map.new();
    m.insert("a", n);
    m
}

fn pick() -> Map[String, i64] {
    let mut i = 0;
    loop {
        i = i + 1;
        if i == 2 { break make_map(i * 7) }
    }
}

fn main() { println(pick().get("a").unwrap()); }
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "14");
    }
}

/// A `String` / `Vec` rvalue carrier ALREADY worked, and this pins why so
/// the pointer-gate story is not mistaken for the whole story: those
/// lower to a `{ptr, i64, i64}` STRUCT, which the slot has accepted since
/// B-2026-08-24-13. Only handle types — `shared`, `Map`, `Set` — lower to
/// a bare pointer and hit the ownership gate this row opened.
#[test]
fn test_e2e_loop_break_string_and_vec_rvalues_round_trip() {
    let out = run_program(
        r#"
fn pick_str() -> String {
    let mut i = 0;
    loop {
        i = i + 1;
        if i == 2 { break f"x{i}" }
    }
}

fn pick_vec() -> Vec[i64] {
    let mut i = 0;
    loop {
        i = i + 1;
        if i == 2 { break [1, 2, i] }
    }
}

fn main() { println(pick_str()); println(pick_vec()[2]); }
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "x2\n2");
    }
}

/// B-2026-08-25-6 — a BRANCHING carrier: `break if c { Node { .. } } else
/// { Node { .. } }`. Exactly one tail runs, but the slot is written once
/// for all of them, so the allowlist recurses with an ALL-tails rule.
#[test]
fn test_e2e_loop_break_if_else_rvalue_round_trips() {
    let out = run_program(
        r#"
shared struct Node { v: i64 }

fn pick() -> Node {
    let mut i = 0;
    loop {
        i = i + 1;
        if i == 2 { break if i > 1 { Node { v: 4 } } else { Node { v: 5 } } }
    }
}

fn main() { println(pick().v); }
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "4");
    }
}

/// The `match` sibling, plus a BLOCK tail — the same recursion through two
/// other nodes.
#[test]
fn test_e2e_loop_break_match_and_block_rvalues_round_trip() {
    let out = run_program(
        r#"
shared struct Node { v: i64 }

fn pick_match() -> Node {
    let mut i = 0;
    loop {
        i = i + 1;
        if i == 2 { break match i { 2 => Node { v: 7 }, _ => Node { v: 9 } } }
    }
}

fn pick_block() -> Node {
    let mut i = 0;
    loop {
        i = i + 1;
        if i == 2 { break { let k = i * 3; Node { v: k } } }
    }
}

fn main() { println(pick_match().v); println(pick_block().v); }
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "7\n6");
    }
}

/// B-2026-08-30-3 — the VALUE half of the f-string arm-tail fix.
///
/// `branch_tail_mints_fresh_owned_temp_inner` now admits an
/// `InterpolatedStringLit` tail, so all seven consuming gates free the
/// rendered buffer at the use site instead of stranding it. The leak is
/// asserted by `asan_branch_tail_fstring_arm_frees_once`; this is the twin
/// that pins the VALUES, because the failure mode a use-site free can
/// introduce is reading freed memory, and a wrong value is how that shows
/// up when it does not crash outright.
///
/// `p8` is read twice on purpose: the `let` is an OWNING destination that
/// was already correct, and the second read is what would print empty if
/// the new free were applied there too.
#[test]
fn test_e2e_branch_tail_fstring_arm_values_round_trip() {
    assert_eq!(
            run_program(
                "fn use_s(s: String) -> i64 { return s.len(); }\n\
                 fn main() {\n\
                 let n: i64 = \"ab\".len();\n\
                 let c = n > 0;\n\
                 let b = if c { f\"x{n}\" } else { f\"y{n}\" }.contains(\"x\");\n\
                 let d = use_s(if c { f\"x{n}\" } else { f\"y{n}\" });\n\
                 println(f\"b={b} d={d}\");\n\
                 let p1 = { f\"p{n}\" }.contains(\"p\");\n\
                 let p2 = { { f\"q{n}\" } }.contains(\"q\");\n\
                 let p3 = match n { 0 => f\"m{n}\", _ => f\"o{n}\" }.contains(\"o\");\n\
                 let p4 = if n < 0 { f\"u{n}\" } else if c { f\"v{n}\" } else { f\"w{n}\" }.contains(\"v\");\n\
                 println(f\"p1={p1} p2={p2} p3={p3} p4={p4}\");\n\
                 let p5 = if c { f\"r{n}\" } else { f\"s{n}\" }.len();\n\
                 let p6 = f\"[{if c { f\"t{n}\" } else { f\"z{n}\" }}]\";\n\
                 let p7 = if c { f\"c{n}\" } else { f\"e{n}\" } + \"-tail\";\n\
                 println(f\"p5={p5} p6={p6} p7={p7}\");\n\
                 let mut v: Vec[String] = [];\n\
                 v.push(if c { f\"k{n}\" } else { f\"l{n}\" });\n\
                 let p8 = if c { f\"g{n}\" } else { f\"h{n}\" };\n\
                 println(f\"v0={v[0]} p8={p8} again={p8}\");\n\
                 }\n",
            ),
            Some(
                "b=true d=2\np1=true p2=true p3=true p4=true\np5=2 p6=[t2] p7=c2-tail\nv0=k2 p8=g2 again=g2\n"
                    .to_string()
            )
        );
}

// ── Wrapping integer arithmetic (wrapping_add/sub/mul) ───────────────
//
// The non-trapping sibling of `+`/`-`/`*`: lowers to a bare add/sub/mul
// with NO `with.overflow` intrinsic and no per-element trap branch. The
// straight-line loop body is what lets LLVM auto-vectorize integer slice
// kernels (the trap branch is the proven vectorization blocker).
#[test]
fn wrapping_arith_lowers_without_overflow_trap() {
    let wrapping = ir_for(
        r#"
fn wadd(a: i64, b: i64) -> i64 { return a.wrapping_add(b); }
fn wsub(a: i64, b: i64) -> i64 { return a.wrapping_sub(b); }
fn wmul(a: i64, b: i64) -> i64 { return a.wrapping_mul(b); }
fn main() { print(3.wrapping_add(4)); }
"#,
    );
    assert!(
        !wrapping.contains("with.overflow"),
        "wrapping_* must not emit the checked-overflow intrinsic:\n{wrapping}"
    );
    // Sanity: the trapping `+` DOES emit the intrinsic, so the absence
    // above is a meaningful signal (not just an unsupported method).
    let trapping = ir_for(
        r#"fn add2(a: i64, b: i64) -> i64 { return a + b; }
fn main() { print(1); }"#,
    );
    assert!(
        trapping.contains("sadd.with.overflow"),
        "trapping `+` should still emit the checked-overflow intrinsic:\n{trapping}"
    );
}

// Narrow widths (B-2026-08-19-1). Two operand shapes have to agree, and the
// first cut of the widening got both wrong: a narrow function PARAMETER is
// a real LLVM `i32`, while a narrow LOCAL is normalized to an i64 carrier
// (`compile_narrow_int_binop`). Mixing them emitted `add i32 %x, i64 1` and
// failed module verification outright; computing at i64 without reducing
// gave 2147483648 for `i32::MAX.wrapping_add(1)` — no wrap at all.
#[test]
fn wrapping_arith_reduces_to_the_receiver_width() {
    let out = run_program(
        r#"
fn w32(x: i32) -> i32 { return x.wrapping_add(1); }
fn m32(x: i32) -> i32 { return x.wrapping_mul(100000); }
fn main() {
    print(w32(2147483647));
    print(m32(100000));
    let a: i32 = 2147483647;
    print(a.wrapping_add(1));
    let b: u32 = 4294967295;
    print(b.wrapping_add(1));
    let c: i8 = 127;
    print(c.wrapping_add(1));
    let d: i16 = -32768;
    print(d.wrapping_sub(1));
}
"#,
    );
    // param i32, param i32 mul, local i32, local u32, local i8, local i16
    assert_eq!(
        out,
        Some("-21474836481410065408-21474836480-12832767".to_string())
    );
}

#[test]
fn e2e_wrapping_arithmetic_semantics() {
    // Two's-complement wraparound, no trap, on the 64-bit widths.
    let out = run_program_capturing(
        r#"
fn main() {
    let big: i64 = 9223372036854775807;   // i64::MAX
    println(big.wrapping_add(1));          // wraps to i64::MIN
    let a: i64 = 100;
    println(a.wrapping_sub(250));          // -150
    println(a.wrapping_mul(3));            // 300
    let u: u64 = 5;
    println(u.wrapping_add(2));            // 7 (literal arg promotes to u64)
}
"#,
    );
    if let Some(c) = out {
        assert_eq!(c.stdout.trim(), "-9223372036854775808\n-150\n300\n7");
    }
}

#[test]
fn e2e_subword_vec_bool_push_no_heap_overflow() {
    // Vec[bool] is also 1-byte; a computed bool push past the cap boundary
    // hit the same overflow. Count the trues over 200 elements.
    if let Some(out) = run_program(
        "fn main() {\n\
                 let mut v: Vec[bool] = Vec.new();\n\
                 let mut a = 0i64;\n\
                 while a < 200i64 { v.push((a % 2i64) == 0i64); a = a + 1i64; }\n\
                 let mut trues = 0i64;\n\
                 let mut k = 0i64;\n\
                 while k < v.len() { if v[k] { trues = trues + 1i64; } k = k + 1i64; }\n\
                 println(f\"{v.len()} {trues}\");\n\
             }",
    ) {
        assert_eq!(out, "200 100\n");
    }
}

#[test]
fn test_e2e_let_elem_hint_does_not_leak_into_call_arg_literals() {
    // B-2026-07-02-13: `let s: String = tail(vec![100, 200, 300]);`
    // packed the ARGUMENT literal's elements as i8 (the let annotation's
    // String elem width leaked via `pending_let_elem_type` into every
    // literal nested in the RHS) — the callee read garbage, silently.
    // The literal's own span-recorded type now wins over the ambient
    // pending-let hint. No generics involved.
    let out = run_program(
        "fn tail_str(xs: Vec[i64]) -> String {\n\
                 let mut out = \"\";\n\
                 for x in xs {\n\
                     out = f\"{out}{x}\";\n\
                 }\n\
                 return out;\n\
             }\n\
             fn main() {\n\
                 let s: String = tail_str(vec![100, 200, 300]);\n\
                 println(s);\n\
             }\n",
    );
    if let Some(out) = out {
        assert_eq!(out, "100200300\n");
    }
}

/// B-2026-07-28-3 — a plain (non-`shared`) struct that reaches ITSELF
/// through a `Vec` field used to overflow the compiler's stack.
///
/// `aggregate_param_copy_supported_struct` guards against cyclic types
/// with a `stack` of struct names, but `field_copy_supported`'s `Vec`
/// arm returned `true` without inspecting the element type, so the walk
/// stopped at the `Vec` and the guard never saw `Node` a second time.
/// The EMITTER does descend there (B-2026-07-04-9(a)'s per-element deep
/// copy), so the by-value param was made callee-owned and codegen then
/// recursed struct → Vec-element → struct with no base case. Since the
/// element copy is unrolled at emission, no finite emission exists for a
/// self-referential type — the analysis has to decline it instead.
///
/// This is `examples/tangle/src/cross_graph.kara`'s shape (an adjacency
/// list — `struct GraphNode { mut edges: Vec[GraphNode] }`), reduced. A
/// regression aborts the process on stack overflow rather than failing
/// an assertion, so merely reaching the end of this test is the check;
/// the output comparison additionally pins that declining the copy still
/// produces a correct program.
#[test]
fn test_e2e_self_referential_struct_through_vec_does_not_overflow_codegen() {
    let src = r#"
struct Node {
    val: i64,
    mut kids: Vec[Node],
}

impl Node {
    fn adopt(mut ref self, child: Node) {
        self.kids.push(child);
    }
}

fn total(n: Node) -> i64 {
    let mut sum = n.val;
    for k in n.kids {
        sum = sum + total(k);
    }
    sum
}

fn main() {
    let mut root = Node { val: 1, kids: Vec.new() };
    let leaf = Node { val: 2, kids: Vec.new() };
    root.adopt(leaf);
    let t = total(root);
    println(f"total: {t}");
}
"#;
    // IR emission alone is the crash surface — assert it terminates.
    let parsed = karac::parse(src);
    assert!(
        parsed.errors.is_empty(),
        "repro must parse: {:?}",
        parsed.errors
    );
    let ir = compile_to_ir(&parsed.program, None, None)
        .expect("self-referential struct must reach codegen without overflowing");
    assert!(
        ir.contains("define"),
        "expected real IR for the self-referential program"
    );

    if let Some(out) = run_program(src) {
        assert_eq!(out, "total: 3\n");
    }
}

/// B-2026-08-27-9 — the compiled twin of
/// `a_float_field_compares_by_ieee_semantics_not_by_bits`, same bytes.
///
/// Every TYPE-DIRECTED comparator routed a float field to
/// `emit_eq_fn_for_type`'s byte loop. Bit equality differs from float
/// equality in exactly the two places IEEE-754 defines specially, so `==`
/// was wrong in BOTH directions: `0.0` vs `-0.0` (equal, different bits)
/// answered false, and NaN vs NaN (unequal, identical bits) answered true.
///
/// THE OPERANDS COME FROM OPAQUE FUNCTIONS ON PURPOSE, and this is the
/// part of the test that is load-bearing. Written inline as `0.0 / 0.0`,
/// LLVM constant-folds the entire comparison and yields the RIGHT answer
/// without the emitted comparator ever running — which is precisely how
/// the first draft of this fix appeared to work while the runtime path was
/// untouched. Keep `nan64` / `negzero` opaque, or this test stops testing
/// codegen and starts testing the constant folder.
///
/// Four shapes, four routes into the comparator family: a shared struct's
/// direct field; a `Vec[f64]` ELEMENT; a PLAIN struct with a Vec field
/// (per B-2026-08-12-5 a Vec field is what pulls a plain struct onto the
/// type-directed path at all, so "plain structs are unaffected" is only
/// true of a DIRECT float field); and a plain struct nested in a shared
/// one. Operands are BOUND to locals rather than compared as inline struct
/// literals: comparing two non-binding operands is a separate,
/// already-diagnosed codegen gap, and letting it fire would mean this test
/// never reached the comparator at all.
///
/// `negzero` is `-z`, NOT `0.0 - z`. The latter yields `+0.0` in IEEE, so
/// every negzero assertion here would compare `0.0` to `0.0` and hold
/// whatever the comparator did. It was written that way first and the A/B
/// against the unfixed compiler is what caught it: the negzero lines
/// passed before the fix.
#[test]
fn test_e2e_float_field_compares_by_ieee() {
    assert_eq!(
        run_program(
            r#"
#[derive(PartialEq)]
shared struct Sf { x: f64 }
#[derive(PartialEq)]
shared struct Sv { v: Vec[f64] }
#[derive(PartialEq)]
struct Pv { v: Vec[f64] }
#[derive(PartialEq)]
struct Inner { x: f64 }
#[derive(PartialEq)]
shared struct Outer { i: Inner }
fn nan64(z: f64) -> f64 { return z / z; }
fn negzero(z: f64) -> f64 { return -z; }
fn one(x: f64) -> Vec[f64] {
    let mut v: Vec[f64] = Vec.new();
    v.push(x);
    return v;
}
fn main() {
    let p = 0.0;
    let n = negzero(p);
    let q = nan64(p);

    println(f"scalar-negzero={p == n}");
    println(f"scalar-nan={q == q}");

    let sa = Sf { x: p };
    let sb = Sf { x: n };
    let sq1 = Sf { x: q };
    let sq2 = Sf { x: q };
    println(f"shared-negzero={sa == sb}");
    println(f"shared-nan={sq1 == sq2}");

    let va = Sv { v: one(p) };
    let vb = Sv { v: one(n) };
    let vq1 = Sv { v: one(q) };
    let vq2 = Sv { v: one(q) };
    println(f"sharedvec-negzero={va == vb}");
    println(f"sharedvec-nan={vq1 == vq2}");

    let pa = Pv { v: one(p) };
    let pb = Pv { v: one(n) };
    let pq1 = Pv { v: one(q) };
    let pq2 = Pv { v: one(q) };
    println(f"plainvec-negzero={pa == pb}");
    println(f"plainvec-nan={pq1 == pq2}");

    let o1 = Outer { i: Inner { x: p } };
    let o2 = Outer { i: Inner { x: n } };
    println(f"nested-negzero={o1 == o2}");
}
"#
        ),
        Some(
            "scalar-negzero=true\nscalar-nan=false\n\
                 shared-negzero=true\nshared-nan=false\n\
                 sharedvec-negzero=true\nsharedvec-nan=false\n\
                 plainvec-negzero=true\nplainvec-nan=false\n\
                 nested-negzero=true\n"
                .to_string()
        )
    );
}

/// B-2026-08-27-9, the DISTINCTION the fix turns on, asserted in one
/// program so the contrast cannot drift apart.
///
/// A genuine float field compares by IEEE `==`: `-0.0` equals `0.0`, NaN
/// does not equal itself. A TOTAL-ORDER WRAPPER field compares BY BITS,
/// deliberately and oppositely: `-0.0` differs from `0.0`, and two
/// canonicalized NaNs are equal. Bit equality is the whole reason the
/// wrapper type exists — it is what gives floats a total order, hence what
/// lets one be a `Map` key or be sorted.
///
/// Both directions matter and neither implies the other. Widening the IEEE
/// arm over the wrapper splits its hash from its eq (its hash hashes bits),
/// so two canonical-NaN keys stop comparing equal while still hashing
/// together and a `Map[F64, V]` silently grows a second entry for one key.
/// That is what happened when this fix was first written, and it is why
/// the `wrapper-*` lines live here next to the `plain-*` ones rather than
/// only in the wrapper's own test.
#[test]
fn test_e2e_ieee_float_field_and_total_order_wrapper_differ() {
    assert_eq!(
        run_program(
            r#"
#[derive(PartialEq)]
shared struct Sf { x: f64 }
fn nan64(z: f64) -> f64 { return z / z; }
fn negzero(z: f64) -> f64 { return -z; }
fn main() {
    let p = 0.0;
    let n = negzero(p);
    let q = nan64(p);

    let a = Sf { x: p };
    let b = Sf { x: n };
    let q1 = Sf { x: q };
    let q2 = Sf { x: q };
    println(f"plain-negzero-eq={a == b}");
    println(f"plain-nan-eq={q1 == q2}");

    let wp: F64 = F64 { value: p };
    let wn: F64 = F64 { value: n };
    let wq1: F64 = F64 { value: q };
    let wq2: F64 = F64 { value: q };
    println(f"wrapper-negzero-eq={wp == wn}");
    println(f"wrapper-nan-eq={wq1 == wq2}");

    let mut m: Map[F64, i64] = Map.new();
    let _ = m.insert(wq1, 1);
    let _ = m.insert(wq2, 2);
    println(f"wrapper-key-collides={m.len()}");
    match m.get(wq2) { Some(x) => println(f"wrapper-key-get={x}"), None => println(f"wrapper-key-get=miss") }
}
"#
        ),
        Some(
            "plain-negzero-eq=true\nplain-nan-eq=false\n\
                 wrapper-negzero-eq=false\nwrapper-nan-eq=true\n\
                 wrapper-key-collides=1\nwrapper-key-get=2\n"
                .to_string()
        )
    );
}

/// B-2026-08-29-23 — NO `bf16` OPERATION MAY LOWER TO A NATIVE `bfloat`
/// LLVM NODE. This is the IR-level twin of
/// `e2e_bf16_operations_lower_without_a_native_bfloat_node` below, and it
/// is the assertion that actually pins the bug.
///
/// WHY AN IR TEST AND NOT JUST AN OUTPUT TEST. LLVM 18's x86 backend
/// legalizes every scalar `bfloat` node happily; aarch64 (every `-mcpu`
/// measured) and wasm32 select NONE of them and abort the process with
/// `LLVM ERROR: Cannot select` and no program output. So the defect is
/// INVISIBLE to any output-based test on the x86 machines this compiler
/// is developed and CI-tested on — `karac_eq_bf16` shipped a raw
/// `fcmp oeq bfloat` for exactly that reason. An output assertion here
/// would pass against the broken compiler; only looking at the emitted
/// node catches it.
///
/// The safe set is `alloca` / `load` / `store` / `bitcast`: a bf16 may be
/// COPIED natively, never computed on. Everything else routes through
/// `f32` (`build_float_cast_bf16_safe` and its compare/neg siblings).
#[test]
fn bf16_operations_emit_no_native_bfloat_node() {
    let ir = ir_for(
        r#"
#[derive(PartialEq)]
shared struct Sb { x: bf16 }
fn main() {
    let n = env.args().len() as i64;
    let a: bf16 = (n as f32) as bf16;
    let b: bf16 = ((n + 1) as f32) as bf16;
    println(f"cmp={a < b} {a <= b} {a > b} {a >= b} {a == b} {a != b}");
    println(f"arith={a + b} {a - b} {a * b} {a / b}");
    println(f"unary={-a} {a.abs()}");
    println(f"minmax={a.min(b)} {a.max(b)}");
    println(f"clamp={b.clamp(a, b)}");
    let s1 = Sb { x: a };
    let s2 = Sb { x: a };
    println(f"eq={s1 == s2}");
    let v: Vec[bf16] = [b, a, b];
    println(f"contains={v.contains(a)}");
    let mut m: Map[Bf16, i64] = Map.new();
    let _ = m.insert(Bf16.from(a), 1);
    println(f"map={m.len()}");
    println(f"fmt={a} back={a as f32}");
}
"#,
    );
    // Every native bf16 shape LLVM 18 cannot select off x86. `fcmp` is
    // matched by predicate-agnostic prefix so a new predicate cannot slip
    // through; the intrinsic form is matched by the `.bf16` suffix LLVM
    // mangles onto an overloaded declaration.
    let mut offenders: Vec<&str> = Vec::new();
    for line in ir.lines() {
        let t = line.trim();
        // Anchored on `= <opcode> `, not a bare substring: an LLVM value
        // NAME derived from an op trips the loose form
        // (`%fneg = bitcast i16 %fneg.bf.flip to bfloat` contains
        // "fneg " twice and is a perfectly safe bitcast). The opcode can
        // only appear immediately after the assignment.
        let native_op = t.contains(" bfloat")
            && [
                "= fadd ",
                "= fsub ",
                "= fmul ",
                "= fdiv ",
                "= frem ",
                "= fneg ",
                "= fcmp ",
                "= fptrunc ",
                "= fpext ",
                "= fptosi ",
                "= fptoui ",
                "= sitofp ",
                "= uitofp ",
            ]
            .iter()
            .any(|op| t.contains(op));
        // `llvm.minnum.bf16` was a real offender: an intrinsic is a native
        // bf16 op wearing a call's clothing, and it is what broke wasm32
        // after the plain-instruction sites were already fixed.
        let native_intrinsic = t.contains("@llvm.") && t.contains(".bf16");
        if native_op || native_intrinsic {
            offenders.push(line);
        }
    }
    assert!(
        offenders.is_empty(),
        "emitted native `bfloat` node(s) — unselectable on aarch64 and wasm32:\n{}",
        offenders.join("\n")
    );
    // Anti-vacuity: the program must actually have produced bf16 values,
    // or the assertion above is satisfied by an empty search. A bf16
    // `alloca` is the shape that survives the fix.
    assert!(
        ir.contains("alloca bfloat"),
        "fixture stopped exercising bf16 at all — the no-native-node \
             assertion above would then hold vacuously"
    );
}

/// B-2026-08-29-23, the behavioural half: every `bf16` operation still
/// computes the right answer after being rerouted through `f32`.
///
/// The widening is exact in both directions (bf16 is a truncated f32), so
/// these answers are the same ones a native bf16 op would give — that is
/// the property this pins. Its sibling above is what catches the
/// regression; this one catches a *wrong* widening.
#[test]
fn e2e_bf16_operations_lower_without_a_native_bfloat_node() {
    assert_eq!(
        run_program(
            r#"
#[derive(PartialEq)]
shared struct Sb { x: bf16 }
fn main() {
    let a: bf16 = 1.5bf16;
    let b: bf16 = 2.5bf16;
    println(f"lt={a < b} gt={a > b} eq={a == b} ne={a != b}");
    println(f"add={a + b} sub={a - b} mul={a * b} div={b / a}");
    println(f"neg={-a} abs={(-a).abs()}");
    println(f"min={a.min(b)} max={a.max(b)}");
    println(f"clamp={b.clamp(a, a)}");
    let s1 = Sb { x: a };
    let s2 = Sb { x: a };
    let s3 = Sb { x: b };
    println(f"seq={s1 == s2} sne={s1 == s3}");
    let v: Vec[bf16] = [b, a, b];
    println(f"contains={v.contains(a)} missing={v.contains(0.25bf16)}");
}
"#
        ),
        Some(
            "lt=true gt=false eq=false ne=true\n\
                 add=4 sub=-1 mul=3.75 div=1.6640625\n\
                 neg=-1.5 abs=1.5\n\
                 min=1.5 max=2.5\n\
                 clamp=1.5\n\
                 seq=true sne=false\n\
                 contains=true missing=false\n"
                .to_string()
        )
    );
}

/// B-2026-08-29-34 — NO `Vector[bf16, N]` LANE OP MAY LOWER TO A NATIVE
/// `<N x bfloat>` LLVM NODE. The vector twin of
/// `bf16_operations_emit_no_native_bfloat_node`, and it needs to exist
/// separately for a reason the scalar test cannot cover.
///
/// THE IR HERE CONTAINS NO SCALAR `bfloat` NODE AT ALL. It is the AArch64
/// *vector* legalizer that scalarizes `<4 x bfloat> fadd` down into the
/// scalar `bf16 fadd` that then cannot be selected, so the scalar scan
/// reports a clean module for a program that aborts at ISel. wasm32 dies
/// one step earlier, on `fp_to_bf16`.
///
/// Measured with llc-18 (`-mtriple` reproduces the target's ISel exactly
/// from an x86 host): before the fix this fixture's shape aborted with
/// `Cannot select: bf16 = fadd` on `aarch64-unknown-linux-gnu` and
/// `aarch64-apple-darwin`, and `Cannot select: i32 = fp_to_bf16` on
/// `wasm32-unknown-wasi`.
///
/// WHY THE FIXTURE IS BIG, AND WHY A SMALL ONE WOULD LIE. A two-line
/// `a + b` plus a lane read SELECTS on aarch64 at every opt level — not
/// because the node is selectable but because llc CONSTANT-FOLDS the add
/// away, so zero `fadd`s reach ISel. Operands here are derived from
/// `env.args().len()` so nothing folds, and the surface is swept wide
/// enough that any one unrouted site shows up.
#[test]
fn vector_bf16_operations_emit_no_native_bfloat_node() {
    let ir = ir_for(
        r#"
fn main() {
    let n = env.args().len() as i64;
    let s: bf16 = (n as f32) as bf16;
    let t: bf16 = ((n + 1) as f32) as bf16;
    let a: Vector[bf16, 4] = Vector[bf16, 4](s, t, s, t);
    let b: Vector[bf16, 4] = Vector[bf16, 4].splat(t);
    println(f"arith={(a + b).reduce_sum()} {(a - b).reduce_sum()} {(a * b).reduce_sum()} {(a / b).reduce_sum()} {(a % b).reduce_sum()}");
    println(f"cmp={(a < b)[0]} {(a == b)[0]} {(a != b)[0]} {(a <= b)[0]} {(a > b)[0]} {(a >= b)[0]}");
    println(f"math={a.sqrt().reduce_sum()} {a.exp().reduce_sum()} {b.ln().reduce_sum()}");
    println(f"act={a.sigmoid().reduce_sum()} {a.tanh().reduce_sum()}");
    println(f"round={a.floor().reduce_sum()} {a.ceil().reduce_sum()} {a.round().reduce_sum()} {a.trunc().reduce_sum()}");
    println(f"red={a.reduce_min()} {a.reduce_max()} {a.reduce_product()} {a.dot(b)}");
    println(f"lanes={a.reverse().reduce_sum()} {a.rotate_lanes_left(1).reduce_sum()} {a.replace(0, 1.0bf16).reduce_sum()} {a[0]}");
}
"#,
    );
    // Same opcode-anchored matching as the scalar sibling, widened to the
    // `<N x bfloat>` spelling. Anchoring on `= <opcode> ` rather than a
    // bare substring matters for the same reason it does there: an LLVM
    // value NAME derived from an op (`%fneg = bitcast …`) trips a loose
    // match, and those bitcasts are exactly what the fix emits.
    const OPS: [&str; 13] = [
        "= fadd ",
        "= fsub ",
        "= fmul ",
        "= fdiv ",
        "= frem ",
        "= fneg ",
        "= fcmp ",
        "= fptrunc ",
        "= fpext ",
        "= fptosi ",
        "= fptoui ",
        "= sitofp ",
        "= uitofp ",
    ];
    let mut offenders: Vec<&str> = Vec::new();
    for line in ir.lines() {
        let t = line.trim();
        let touches_bf16 = t.contains(" bfloat") || t.contains("x bfloat>");
        let native_op = touches_bf16 && OPS.iter().any(|op| t.contains(op));
        // `llvm.sqrt.v4bf16` and friends: an intrinsic is a native bf16 op
        // wearing a call's clothing, and the vector transcendentals are
        // where that form actually bit.
        let native_intrinsic = t.contains("@llvm.") && t.contains("bf16");
        if native_op || native_intrinsic {
            offenders.push(line);
        }
    }
    assert!(
        offenders.is_empty(),
        "emitted native `bfloat` node(s) — unselectable on aarch64 and wasm32:\n{}",
        offenders.join("\n")
    );
    // Anti-vacuity, and it has to be the VECTOR shape: the scalar test's
    // `alloca bfloat` would be satisfied by a fixture whose vectors had
    // all been optimized into scalars, which is the one way this assertion
    // could hold while covering nothing.
    assert!(
        ir.contains("x bfloat>"),
        "fixture stopped producing `<N x bfloat>` values at all — the \
             no-native-node assertion above would then hold vacuously"
    );
}

/// B-2026-08-29-34, the behavioural half: a `Vector[bf16, N]` still
/// computes the right answers after every lane op is rerouted through
/// `<N x float>`.
///
/// The values are the ones the "each operation widens to f32 and rounds
/// back" rule gives, and they are what the INTERPRETER produces for the
/// same program — the A/B oracle. Its sibling above catches the
/// regression; this one catches a *wrong* widening, e.g. a round-to-
/// nearest-even step that differs from the scalar path's and so makes
/// `reduce_sum` (which folds lanes through the SCALAR fold) disagree with
/// itself.
#[test]
fn e2e_vector_bf16_operations_lower_without_a_native_bfloat_node() {
    assert_eq!(
        run_program(
            r#"
fn main() {
    let a: Vector[bf16, 4] = Vector[bf16, 4](1.5bf16, 2.5bf16, 1.5bf16, 2.5bf16);
    let b: Vector[bf16, 4] = Vector[bf16, 4].splat(0.5bf16);
    println(f"add={(a + b).reduce_sum()} sub={(a - b).reduce_sum()}");
    println(f"mul={(a * b).reduce_sum()} div={(a / b).reduce_sum()}");
    println(f"cmp={(a > b)[0]} {(a < b)[0]} {(a == b)[0]}");
    println(f"red={a.reduce_min()} {a.reduce_max()} {a.dot(b)}");
    println(f"round={a.floor().reduce_sum()} {a.ceil().reduce_sum()}");
    println(f"sqrt={a.sqrt().reduce_sum()}");
}
"#
        ),
        Some(
            "add=10 sub=6\n\
                 mul=4 div=16\n\
                 cmp=true false false\n\
                 red=1.5 2.5 4\n\
                 round=6 10\n\
                 sqrt=5.625\n"
                .to_string()
        )
    );
}

/// B-2026-08-29-34 — the SCALAR math methods, which B-2026-08-29-23 left
/// emitting `llvm.<op>.bf16`.
///
/// That row fixed the plain-instruction sites and `min`/`max` and stopped
/// there, so `sqrt` / `exp` / `ln` / `log2` / `log10` / `exp2` / `sin` /
/// `cos` / `floor` / `ceil` / `round` / `trunc` / `signum` / `recip` /
/// `to_degrees` / `fract` all still instantiated an overloaded intrinsic
/// at `bfloat`. Every one aborts ISel off x86 (measured: `bf16 = fsqrt`,
/// `bf16 = fexp`, `bf16 = ffloor`), and from the moment -23's guard
/// landed they were a hard compile error on EVERY host, because that
/// guard's first pass matches intrinsics by name and `llvm.sqrt.bf16`
/// contains "bf16".
///
/// The scalar fixture in `bf16_operations_emit_no_native_bfloat_node`
/// exercises arithmetic, comparison, `abs`, `min`/`max` and `clamp` — and
/// passed throughout, which is exactly why this list needs its own
/// fixture rather than a line appended to that one.
#[test]
fn scalar_bf16_math_methods_emit_no_native_bfloat_intrinsic() {
    let ir = ir_for(
        r#"
fn main() {
    let n = env.args().len() as i64;
    let a: bf16 = ((n + 1) as f32) as bf16;
    let b: bf16 = ((n + 2) as f32) as bf16;
    println(f"m1={a.sqrt()} {a.exp()} {a.ln()} {a.log2()} {a.log10()} {a.exp2()}");
    println(f"m2={a.sin()} {a.cos()} {a.tan()} {a.atan()} {a.asin()} {a.acos()}");
    println(f"m3={a.sinh()} {a.cosh()} {a.tanh()} {a.ln_1p()} {a.exp_m1()}");
    println(f"m4={a.floor()} {a.ceil()} {a.round()} {a.trunc()} {a.fract()}");
    println(f"m5={a.signum()} {a.recip()} {a.to_degrees()} {a.to_radians()}");
    println(f"m6={a.atan2(b)} {a.hypot(b)} {a.pow(b)}");
}
"#,
    );
    let offenders: Vec<&str> = ir
        .lines()
        .filter(|l| l.contains("@llvm.") && l.contains("bf16"))
        .collect();
    assert!(
        offenders.is_empty(),
        "instantiated an LLVM intrinsic at `bfloat` — unselectable on \
             aarch64 and wasm32:\n{}",
        offenders.join("\n")
    );
    assert!(
        ir.contains("bfloat"),
        "fixture stopped exercising bf16 at all"
    );
}

/// B-2026-09-01-21 — A DISCARDED STRUCT LITERAL MIXING A LIVE-LOCAL SOURCE
/// WITH A MINTED SIBLING lost the minted field's `Drop` body on both
/// compiled backends.
///
/// `discarded_owned_literal_tail`'s struct arm required EVERY field fresh,
/// and an `Identifier` naming a live Drop-bearing local is not, so one such
/// field declined the whole literal and no owner registered for ANY field:
/// `dR7` (the local's own scope-exit body) against `dR9 dR7` from the
/// interpreter and from the BOUND `let`, which is the oracle here.
///
/// The TUPLE arm has had the escape hatch since B-2026-08-01-8 — admit the
/// place, retract that source's UserDrop at the caller so the temp's walk
/// is the single owner. `discarded_movable_literal_tail` is that hatch for
/// struct literals, kept SEPARATE from the all-fresh predicate because the
/// latter also feeds `discarded_arm_owned_aggregate_tail`, which performs
/// no retraction and would double-free.
#[test]
fn e2e_a_discarded_literal_mixing_a_source_and_a_mint_runs_both_bodies() {
    const PRELUDE: &str = "struct R { id: i64 }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\"); } }\n\
             struct S { r: R, k: i64 }\n\
             struct S2 { r: R, s: R, k: i64 }\n";
    let cases: &[(&str, &str, &str)] = &[
        (
            "mixed literal, bare statement",
            "fn go() -> i64 { let t = R { id: 7 };\n\
                 S2 { r: t, s: R { id: 9 }, k: 1 };\n\
                 return 7; }\n\
                 fn take() -> i64 { return go(); }",
            "dR9\ndR7\nv=7\n",
        ),
        (
            "mixed literal, wildcard `let`",
            "fn go() -> i64 { let t = R { id: 7 };\n\
                 let _ = S2 { r: t, s: R { id: 9 }, k: 1 };\n\
                 return 7; }\n\
                 fn take() -> i64 { return go(); }",
            "dR9\ndR7\nv=7\n",
        ),
        (
            "mixed literal behind a block wrapper",
            "fn go() -> i64 { let t = R { id: 7 };\n\
                 { S2 { r: t, s: R { id: 9 }, k: 1 } };\n\
                 return 7; }\n\
                 fn take() -> i64 { return go(); }",
            "dR9\ndR7\nv=7\n",
        ),
        (
            "MINTED field first, source second — the order follows the literal",
            "fn go() -> i64 { let t = R { id: 7 };\n\
                 S2 { r: R { id: 9 }, s: t, k: 1 };\n\
                 return 7; }\n\
                 fn take() -> i64 { return go(); }",
            "dR7\ndR9\nv=7\n",
        ),
        (
            "ORACLE: the BOUND `let` of the same literal",
            "fn go() -> i64 { let t = R { id: 7 };\n\
                 let w = S2 { r: t, s: R { id: 9 }, k: 1 };\n\
                 return w.k + 6; }\n\
                 fn take() -> i64 { return go(); }",
            "dR9\ndR7\nv=7\n",
        ),
        (
            "control: TWO sources and no mint, unchanged by the widening",
            "fn go() -> i64 { let t = R { id: 7 }; let u = R { id: 8 };\n\
                 S2 { r: t, s: u, k: 1 };\n\
                 return 7; }\n\
                 fn take() -> i64 { return go(); }",
            "dR8\ndR7\nv=7\n",
        ),
        (
            "control: ONE source, single-field literal",
            "fn go() -> i64 { let t = R { id: 7 };\n\
                 S { r: t, k: 1 };\n\
                 return 7; }\n\
                 fn take() -> i64 { return go(); }",
            "dR7\nv=7\n",
        ),
        (
            "control: ALL-MINTED keeps the owner it already had",
            "fn go() -> i64 {\n\
                 S2 { r: R { id: 7 }, s: R { id: 9 }, k: 1 };\n\
                 return 7; }\n\
                 fn take() -> i64 { return go(); }",
            "dR9\ndR7\nv=7\n",
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
            assert_eq!(aot, *want, "{label}: every field's body, each once");
        }
    }
}

/// B-2026-08-30-26 — AN INTEGER <-> `bf16` CONVERSION WAS REFUSED BY BOTH
/// COMPILED BACKENDS, IN EVERY DIRECTION, WHILE THE INTERPRETER PERFORMED IT.
///
/// `compile_cast`'s float->float lane had gone through
/// `build_float_cast_bf16_safe` since B-2026-08-29-23, but its int->float
/// and float->int lanes had not: `sitofp`/`uitofp` INTO `bfloat` and
/// `llvm.fptosi.sat.iN.bf16` are native bf16 nodes LLVM 18 cannot select,
/// so the module-wide guard rejected the program outright. All 16 shapes
/// (8 integer widths x 2 directions) failed; `f16` was unaffected.
///
/// PRE-FIX THIS FAILS LOUDLY RATHER THAN VACUOUSLY, which is worth stating
/// because the reverse is the usual hazard with `if let Some(out)`.
/// `run_program` PANICS on a codegen error by design — only a link or exec
/// failure soft-skips to `None` — and the pre-fix symptom IS a codegen
/// error. Verified by reverting the fix: `codegen failed for test program:
/// internal error: codegen emitted a native bfloat llvm.fptosi.sat.i8.bf16`.
/// An `assert!(out.is_some())` would add nothing here and would break the
/// documented soft-skip on a machine with no runtime archive.
///
/// The values are chosen so a lowering that merely compiles cannot pass.
/// `257 -> 256` and `-12345 -> -12352` pin bf16's 8-bit significand
/// rounding; `1000.0 as i8 -> 127` and `-1000.0 as i8 -> -128` pin
/// saturation at both ends; `-2.5 as u8 -> 0` pins the unsigned clamp; and
/// `1.0e30`/`-1.0e30` into i32/u32/i64 pin it at the extremes.
///
/// GOING VIA f32 IS EXACT, NOT AN APPROXIMATION, IN BOTH DIRECTIONS.
/// bf16->f32 widens a truncated f32, so it is lossless. int->f32->bf16
/// double-rounds, but rounding to p1 bits then to p2 agrees with rounding
/// straight to p2 when p1 >= 2*p2 + 2, and 24 >= 2*8 + 2 — which is why
/// these outputs match the interpreter, whose own `i as bf16` takes a
/// different route (through f64).
///
/// Mirrored in `tests/interpreter.rs::int_bf16_conversions_round_and_saturate`.
#[test]
fn e2e_int_bf16_conversions_are_lowered_through_f32() {
    if let Some(out) = run_program(
        r#"
fn main() {
    let n = env.args().len() as i64;
    let zero: i64 = n - 1;
    let onef: f32 = (n as f32);
    let a1: i8 = (-127i8 + (zero as i8));
    let a2: i8 = (3i8 + (zero as i8));
    let a3: u8 = (255u8 + (zero as u8));
    let a4: i16 = (257i16 + (zero as i16));
    let a5: i16 = (-12345i16 + (zero as i16));
    let a6: u16 = (65535u16 + (zero as u16));
    let a7: i32 = (2147483647i32 + (zero as i32));
    let a8: u32 = (4294967295u32 + (zero as u32));
    let a9: i64 = (9223372036854775807i64 + zero);
    let a10: i64 = (-9223372036854775807i64 + zero);
    let a11: u64 = (6148914691236517205u64 + (zero as u64));
    println(f"i2b {(a1 as bf16)} {(a2 as bf16)} {(a3 as bf16)} {(a4 as bf16)}");
    println(f"i2b {(a5 as bf16)} {(a6 as bf16)} {(a7 as bf16)} {(a8 as bf16)}");
    println(f"i2b {(a9 as bf16)} {(a10 as bf16)} {(a11 as bf16)}");
    let b1: bf16 = ((2.5f32 * onef) as bf16);
    let b2: bf16 = ((-2.5f32 * onef) as bf16);
    let b3: bf16 = ((1000.0f32 * onef) as bf16);
    let b4: bf16 = ((-1000.0f32 * onef) as bf16);
    let b5: bf16 = ((1.0e30f32 * onef) as bf16);
    let b6: bf16 = ((-1.0e30f32 * onef) as bf16);
    let b7: bf16 = ((0.4f32 * onef) as bf16);
    println(f"b2i {(b1 as i8)} {(b2 as i8)} {(b3 as i8)} {(b4 as i8)}");
    println(f"b2i {(b1 as u8)} {(b2 as u8)} {(b3 as u8)} {(b7 as u8)}");
    println(f"b2i {(b5 as i32)} {(b6 as i32)} {(b5 as u32)} {(b6 as u32)}");
    println(f"b2i {(b5 as i64)} {(b6 as i64)} {(b3 as i16)} {(b4 as i16)}");
}
"#,
    ) {
        assert_eq!(out, "i2b -127 3 255 256\ni2b -12352 65536 2147483648 4294967296\ni2b 9223372036854776000 -9223372036854776000 6160924290242839000\nb2i 2 -2 127 -128\nb2i 2 0 255 0\nb2i 2147483647 -2147483648 4294967295 0\nb2i 9223372036854775807 -9223372036854775808 1000 -1000\n");
    }
}

/// B-2026-09-15-5 — EVERY lookup entry point on EVERY container runs its key
/// temporary's user `Drop` body, and this matrix exists because the first pass
/// missed three of the eleven.
///
/// Codegen funnels all eleven through ONE chokepoint
/// (`free_fresh_owned_struct_key_arg`), so its half was complete the moment that
/// dispatcher gained the bodies call. The interpreter has a SEPARATE arm per
/// container per method, so a per-site fix there is only as complete as the list
/// the author enumerated — and mine was short by `Set.remove`,
/// `SortedSet.remove` and `Vec.contains`. The asymmetry is the whole lesson: a
/// one-chokepoint backend and an eleven-site backend cannot be paired by fixing
/// "the obvious sites", and the gap it leaves is a RUN/BUILD DIVERGENCE (codegen
/// correct, interpreter silent), which is worse than the symmetric gap it
/// replaced.
///
/// Found by sweeping the matrix against the compiled backend rather than by
/// reading the interpreter, which is why every cell is here rather than only the
/// three that were broken: the eight that were already right are what make the
/// three a gap instead of a guess.
///
/// Each cell looks up a key the container does NOT hold, so the two bodies are
/// unambiguous: `dK2` is the discarded argument temporary (owed AT the lookup)
/// and `dK1` is the container's own stored element, which fires at the
/// container's live-range end — the lookup, since that is its last use.
#[test]
fn e2e_every_lookup_entry_point_runs_its_key_temporarys_body() {
    let hdr = "#[derive(Hash, Eq, PartialEq, Ord)]\n\
                   struct K { n: i64 }\n\
                   impl Drop for K { fn drop(mut ref self) { println(f\"dK{self.n}\"); } }\n";
    for (label, stmts, want) in [
            (
                "Map.get",
                "let mut c: Map[K, i64] = Map.new();\n\
                 c.insert(K { n: 1 }, 1);\n\
                 println(\"pre\");\n\
                 match c.get(K { n: 2 }) { Some(v) => { println(\"hit\"); } None => { println(\"miss\"); } }\n\
                 println(\"post\");\n",
                "pre\ndK2\nmiss\ndK1\npost\n",
            ),
            (
                "Map.contains_key",
                "let mut c: Map[K, i64] = Map.new();\n\
                 c.insert(K { n: 1 }, 1);\n\
                 println(\"pre\");\n\
                 if c.contains_key(K { n: 2 }) { println(\"hit\"); } else { println(\"miss\"); }\n\
                 println(\"post\");\n",
                "pre\ndK2\nmiss\ndK1\npost\n",
            ),
            (
                "Map.remove",
                "let mut c: Map[K, i64] = Map.new();\n\
                 c.insert(K { n: 1 }, 1);\n\
                 println(\"pre\");\n\
                 c.remove(K { n: 2 });\n\
                 println(\"post\");\n",
                "pre\ndK2\ndK1\npost\n",
            ),
            (
                "SortedMap.get",
                "let mut c: SortedMap[K, i64] = SortedMap.new();\n\
                 c.insert(K { n: 1 }, 1);\n\
                 println(\"pre\");\n\
                 match c.get(K { n: 2 }) { Some(v) => { println(\"hit\"); } None => { println(\"miss\"); } }\n\
                 println(\"post\");\n",
                "pre\ndK2\nmiss\ndK1\npost\n",
            ),
            (
                "SortedMap.contains_key",
                "let mut c: SortedMap[K, i64] = SortedMap.new();\n\
                 c.insert(K { n: 1 }, 1);\n\
                 println(\"pre\");\n\
                 if c.contains_key(K { n: 2 }) { println(\"hit\"); } else { println(\"miss\"); }\n\
                 println(\"post\");\n",
                "pre\ndK2\nmiss\ndK1\npost\n",
            ),
            (
                "SortedMap.remove",
                "let mut c: SortedMap[K, i64] = SortedMap.new();\n\
                 c.insert(K { n: 1 }, 1);\n\
                 println(\"pre\");\n\
                 c.remove(K { n: 2 });\n\
                 println(\"post\");\n",
                "pre\ndK2\ndK1\npost\n",
            ),
            (
                "Set.contains",
                "let mut c: Set[K] = Set.new();\n\
                 c.insert(K { n: 1 });\n\
                 println(\"pre\");\n\
                 if c.contains(K { n: 2 }) { println(\"hit\"); } else { println(\"miss\"); }\n\
                 println(\"post\");\n",
                "pre\ndK2\nmiss\ndK1\npost\n",
            ),
            (
                "Set.remove -- MISSED on the first pass",
                "let mut c: Set[K] = Set.new();\n\
                 c.insert(K { n: 1 });\n\
                 println(\"pre\");\n\
                 c.remove(K { n: 2 });\n\
                 println(\"post\");\n",
                "pre\ndK2\ndK1\npost\n",
            ),
            (
                "SortedSet.contains",
                "let mut c: SortedSet[K] = SortedSet.new();\n\
                 c.insert(K { n: 1 });\n\
                 println(\"pre\");\n\
                 if c.contains(K { n: 2 }) { println(\"hit\"); } else { println(\"miss\"); }\n\
                 println(\"post\");\n",
                "pre\ndK2\nmiss\ndK1\npost\n",
            ),
            (
                "SortedSet.remove -- MISSED on the first pass",
                "let mut c: SortedSet[K] = SortedSet.new();\n\
                 c.insert(K { n: 1 });\n\
                 println(\"pre\");\n\
                 c.remove(K { n: 2 });\n\
                 println(\"post\");\n",
                "pre\ndK2\ndK1\npost\n",
            ),
            (
                "Vec.contains -- MISSED on the first pass",
                "let mut c: Vec[K] = Vec.new();\n\
                 c.push(K { n: 1 });\n\
                 println(\"pre\");\n\
                 if c.contains(K { n: 2 }) { println(\"hit\"); } else { println(\"miss\"); }\n\
                 println(\"post\");\n",
                "pre\ndK2\nmiss\ndK1\npost\n",
            ),
        ] {
            let src = format!("{hdr}fn main() {{\n{stmts}\n}}\n");
            assert_eq!(run_program(&src).as_deref(), Some(want), "[{label}]");
        }
}
