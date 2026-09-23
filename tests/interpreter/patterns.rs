//! match, arms, if/while let, destructuring -- fixtures for `tests/interpreter.rs`.
//!
//! Split out of `tests/interpreter.rs` on 2026-09-21. The TEST TARGET is
//! unchanged: this file is a module of that target, so
//! `cargo test --features llvm --test interpreter` still runs everything
//! and CI needs no edit. Run this area alone with:
//!
//!     cargo test --features llvm --test interpreter patterns::
//!
//! New fixtures about match, arms, if/while let, destructuring belong in this file.

use super::*;

/// `codegen_generic_destructured_payload_renders_at_its_instantiation`
/// (tests/codegen.rs), pinned to the same string.
///
/// The interpreter has no monomorphization, so the arm binding is simply the
/// value that was there and every line was correct throughout — which is what
/// made this an A/B divergence rather than a shared gap. Pinned here so the
/// compiled fix cannot later be "restored" by weakening this side to match a
/// regressed backend.
#[test]
fn test_generic_destructured_payload_renders_at_its_instantiation() {
    assert_eq!(
        run(r#"fn show[T: Display](x: Option[T]) { match x { Some(t) => { println(f"{t}") } None => { println("n") } } }
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
"#),
        "[1, 2]\n[7, 8, 9]\n[1, 2]\n[4, 5]\n[ab, cd]\n[ef]\n(5, 6)\nVector(1, 2, 3, 4)\n7\nhi\nm:[7, 8, 9]\nr:[1, 2]\nn\n"
    );
}

#[test]
fn test_faulting_match_scrutinee_propagates_not_ice() {
    // Regression: a faulting expression in `match` scrutinee position (here an
    // index-out-of-bounds) sets the control-flow fault and must propagate as a
    // runtime error, NOT reach `eval_match`'s non-exhaustive `unreachable!`
    // when the poison value matches no arm.
    let errors = runtime_errors(
        "fn main() {\n\
         let v: Vec[i64] = Vec.new();\n\
         match v[5] { 0 => println(1i64), 1 => println(2i64) }\n\
         }",
    );
    assert!(
        !errors.is_empty(),
        "expected an out-of-bounds runtime error from the faulting scrutinee"
    );
}

// ── while let / let else (phase-6 line 489, interpreter parity) ─

#[test]
fn test_while_let_drains_and_binds() {
    // Re-evaluates the scrutinee each iteration, binds the payload, and
    // exits when it stops matching (`None`). Mirrors the codegen E2E test
    // `test_e2e_while_let_drains_and_binds` (Vec.pop yields 30, 20, 10).
    assert_eq!(
        run("fn main() {\n\
                 let mut v: Vec[i64] = Vec.new();\n\
                 v.push(10_i64);\n\
                 v.push(20_i64);\n\
                 v.push(30_i64);\n\
                 let mut sum = 0_i64;\n\
                 while let Some(x) = v.pop() {\n\
                     sum = sum + x;\n\
                     println(x);\n\
                 }\n\
                 println(sum);\n\
             }"),
        "30\n20\n10\n60\n"
    );
}

#[test]
fn test_while_let_break() {
    // `break` exits the while-let loop (Vec.pop yields 4, 3, then x==2).
    assert_eq!(
        run("fn main() {\n\
                 let mut v: Vec[i64] = Vec.new();\n\
                 v.push(1_i64);\n\
                 v.push(2_i64);\n\
                 v.push(3_i64);\n\
                 v.push(4_i64);\n\
                 while let Some(x) = v.pop() {\n\
                     if x == 2_i64 { break; }\n\
                     println(x);\n\
                 }\n\
                 println(99_i64);\n\
             }"),
        "4\n3\n99\n"
    );
}

#[test]
fn test_while_let_continue() {
    // `continue` skips the rest of the iteration (3 is skipped).
    assert_eq!(
        run("fn main() {\n\
                 let mut v: Vec[i64] = Vec.new();\n\
                 v.push(1_i64);\n\
                 v.push(2_i64);\n\
                 v.push(3_i64);\n\
                 v.push(4_i64);\n\
                 while let Some(x) = v.pop() {\n\
                     if x == 3_i64 { continue; }\n\
                     println(x);\n\
                 }\n\
             }"),
        "4\n2\n1\n"
    );
}

#[test]
fn test_let_else_match_and_diverge() {
    // Match edge binds and continues; non-match edge runs the else block,
    // which diverges (`return`). Mirrors the codegen E2E test
    // `test_e2e_let_else_binds_then_else_diverges`.
    assert_eq!(
        run("fn make(empty: bool) -> Option[i64] {\n\
                 if empty { return Option.None; }\n\
                 return Option.Some(7_i64);\n\
             }\n\
             fn check(empty: bool) {\n\
                 let Some(x) = make(empty) else {\n\
                     println(0_i64);\n\
                     return\n\
                 }\n\
                 println(x);\n\
             }\n\
             fn main() {\n\
                 check(false);\n\
                 check(true);\n\
                 println(99_i64);\n\
             }"),
        "7\n0\n99\n"
    );
}

// ── Enums & Match ──────────────────────────────────────────────

#[test]
fn test_enum_match_unit_variants() {
    assert_eq!(
        run("enum Color { Red, Green, Blue }\n\
             fn name(c: Color) -> i64 {\n\
                 match c {\n\
                     Red => 1,\n\
                     Green => 2,\n\
                     Blue => 3,\n\
                 }\n\
             }\n\
             fn main() { println(name(Red)); }"),
        "1\n"
    );
}

#[test]
fn test_enum_match_dotted_unit_variants() {
    // Regression: a *dotted* unit-variant pattern (`Side.Left`) was matched
    // via an `env.get("Side.Left")` lookup that always failed (variants
    // aren't keyed by dotted name), so the arm fell through to the catch-all
    // "binds anything" and matched EVERY value — `Side.Right` silently took
    // the `Side.Left` arm. Both arms must now select correctly. (Bare unit
    // variants — `test_enum_match_unit_variants` — were unaffected and must
    // keep working.)
    assert_eq!(
        run("enum Side { Left, Right }\n\
             fn label(s: Side) -> i64 {\n\
                 match s {\n\
                     Side.Left => 1,\n\
                     Side.Right => 2,\n\
                 }\n\
             }\n\
             fn main() { println(label(Side.Left)); println(label(Side.Right)); }"),
        "1\n2\n"
    );
}

#[test]
fn test_enum_match_tuple_variant() {
    assert_eq!(
        run("enum Shape { Circle(i64), Rect(i64, i64) }\n\
             fn area(s: Shape) -> i64 {\n\
                 match s {\n\
                     Circle(r) => r * r,\n\
                     Rect(w, h) => w * h,\n\
                 }\n\
             }\n\
             fn main() { println(area(Rect(3, 4))); }"),
        "12\n"
    );
}

#[test]
fn test_match_bare_ordering_variant_from_cmp() {
    // `match x.cmp(y) { Less => .., Equal => .., Greater => .. }` with BARE
    // (unqualified) Ordering variant patterns. The interpreter registered the
    // Ordering variants only under their qualified names ("Ordering.Less"), so
    // `env.get("Less")` was None and the pattern matcher classified bare `Less`
    // as a catch-all binding — the FIRST arm always matched, silently returning
    // Less for every comparison (int AND String) under `karac run` while codegen
    // was correct. B-2026-06-30-14. Now the bare names are bound too, so the
    // right arm fires (peer to codegen's e2e_string_cmp / int cmp coverage).
    assert_eq!(
        run("fn tag(a: i64, b: i64) -> i64 {\n\
                 match a.cmp(b) { Less => 0, Equal => 1, Greater => 2 }\n\
             }\n\
             fn main() {\n\
                 println(tag(1, 2));\n\
                 println(tag(2, 1));\n\
                 println(tag(3, 3));\n\
             }"),
        "0\n2\n1\n"
    );
    // String receiver — the same bare-variant match over `String.cmp`.
    assert_eq!(
        run("fn tag(a: String, b: String) -> i64 {\n\
                 match a.cmp(b) { Less => 0, Equal => 1, Greater => 2 }\n\
             }\n\
             fn main() {\n\
                 println(tag(\"abc\", \"abd\"));\n\
                 println(tag(\"abd\", \"abc\"));\n\
                 println(tag(\"abc\", \"abc\"));\n\
             }"),
        "0\n2\n1\n"
    );
}

#[test]
fn test_match_wildcard() {
    assert_eq!(
        run("fn describe(x: i64) -> i64 {\n\
                 match x {\n\
                     0 => 100,\n\
                     _ => 200,\n\
                 }\n\
             }\n\
             fn main() { println(describe(5)); }"),
        "200\n"
    );
}

#[test]
fn test_match_with_guard() {
    assert_eq!(
        run("enum Opt { Some(i64), None }\n\
             fn check(o: Opt) -> i64 {\n\
                 match o {\n\
                     Some(x) if x > 10 => 1,\n\
                     Some(x) => 2,\n\
                     None => 3,\n\
                 }\n\
             }\n\
             fn main() { println(check(Some(5))); }"),
        "2\n"
    );
}

#[test]
fn test_nested_match_patterns() {
    assert_eq!(
        run("enum Outer { A(i64), B }\n\
             fn check(o: Outer) -> i64 {\n\
                 match o {\n\
                     A(0) => 100,\n\
                     A(n) => n,\n\
                     B => -1,\n\
                 }\n\
             }\n\
             fn main() {\n\
                 println(check(A(0)));\n\
                 println(check(A(42)));\n\
                 println(check(B));\n\
             }"),
        "100\n42\n-1\n"
    );
}

#[test]
fn test_into_at_struct_field_if_and_match_tails() {
    // `.into()` must thread the expected type through three positions the
    // let/return/call-arg tests above don't reach: a struct-literal field
    // value (checked against the declared field type), an if/else tail, and
    // a match-arm tail. All three already flow through `check_expr` with the
    // contextual type, so the user `impl From[Celsius] for Kelvin` fires at
    // each — this guards that the threading stays wired at every position.
    let output = run("struct Celsius { deg: i64 }\n\
         struct Kelvin { k: i64 }\n\
         impl From for Kelvin {\n\
             fn from(c: Celsius) -> Kelvin { Kelvin { k: c.deg + 273 } }\n\
         }\n\
         struct Reading { temp: Kelvin }\n\
         fn pick(hot: bool) -> Kelvin {\n\
             if hot { (Celsius { deg: 100 }).into() }\n\
             else { (Celsius { deg: 0 }).into() }\n\
         }\n\
         fn classify(n: i64) -> Kelvin {\n\
             match n {\n\
                 0 => (Celsius { deg: 0 }).into(),\n\
                 _ => (Celsius { deg: n }).into(),\n\
             }\n\
         }\n\
         fn main() {\n\
             let r: Reading = Reading { temp: (Celsius { deg: 27 }).into() };\n\
             println(r.temp.k);\n\
             println(pick(true).k);\n\
             println(pick(false).k);\n\
             println(classify(0).k);\n\
             println(classify(50).k);\n\
         }");
    assert_eq!(output, "300\n373\n273\n273\n323\n");
}

#[test]
fn test_match_freshtemp_result_struct_field_moved_to_free_fn() {
    // B-2026-07-23-4 interpreter parity: matching a fresh-temp `Result[W, _]`
    // whose `Ok(w)` struct payload has a heap `String` field and passing that
    // field by value to a free fn (`println(w.s)` / `use_s(w.s)`) is clean in
    // the tree-walk interpreter (the bug was codegen-only — a double-free under
    // `karac build`). This pins the reference behavior the codegen fix matches.
    let output = run_no_errors(
        "struct W { s: String }\n\
         fn use_s(x: String) -> i64 { x.len() }\n\
         fn f() -> Result[W, i64] { Ok(W { s: \"boom\".to_string() }) }\n\
         fn main() {\n\
             match f() { Ok(w) => println(w.s), Err(e) => println(e.to_string()) }\n\
             match f() { Ok(w) => println(use_s(w.s).to_string()), Err(e) => println(e.to_string()) }\n\
         }",
    );
    assert_eq!(output, "boom\n4\n");
}

#[test]
fn test_pool_error_variants_match_in_pattern() {
    // Sanity: the three PoolError variants are reachable from user
    // pattern matches without an explicit import — confirms scope-0
    // visibility for the enum + every variant.
    let output = run(r#"fn classify(e: PoolError) -> String {
             match e {
                 PoolError.Timeout => "timeout",
                 PoolError.PoolClosed => "closed",
                 PoolError.CreateFailed => "create_failed",
             }
         }
         fn main() {
             println(classify(PoolError.Timeout));
             println(classify(PoolError.PoolClosed));
             println(classify(PoolError.CreateFailed));
         }"#);
    assert_eq!(output, "timeout\nclosed\ncreate_failed\n");
}

#[test]
fn test_a_bound_optres_local_whose_arm_missed_runs_its_payload_drop_body() {
    // B-2026-09-02-14 — a BOUND `Option`/`Result` local whose arm MISSED lost
    // its payload's `Drop` body, on all four surfaces.
    //
    // `let r = mkerr(); if let Ok(w) = r { … }` ran nothing, while the same
    // local with no `if let` at all ran the body correctly — so the walk
    // EXISTS, and this was a retraction that should not have applied rather
    // than a missing registration. The retraction is a hand-off ("the arm's
    // binding owns the payload from here on"), true on the hit edge and
    // applied on every path out of the construct, including the one where the
    // pattern did not match and there is no binding to hand anything to.
    //
    // The interpreter's disarm moved INSIDE the match test — and, for `match`,
    // onto the TAKEN ARM alone rather than a scan of every arm. Both used to
    // be flow-insensitive to stay in step with codegen's compile-time
    // retraction; codegen now clears a per-path flag in the arm's own block,
    // so the two backends decide per path and still decide identically.
    //
    // On the DEFAULT leg because half the fix is the interpreter's, and the
    // cross-backend matrix
    // (`e2e_a_bound_optres_local_whose_arm_missed_runs_its_payload_drop_body`)
    // is gated on `--features llvm`.
    const PRELUDE: &str = "struct W { id: i64 }\n\
         impl Drop for W { fn drop(mut ref self) { println(f\"dW{self.id}\"); } }\n\
         struct H { tag: String }\n\
         impl Drop for H { fn drop(mut ref self) { println(f\"dH{self.tag}\"); } }\n\
         enum E { A(W), B(W) }\n\
         fn mkerr() -> Result[W, W] { return Err(W { id: 7 }); }\n\
         fn mkok() -> Result[W, W] { return Ok(W { id: 1 }); }\n\
         fn mksome() -> Option[W] { return Some(W { id: 5 }); }\n\
         fn mkerrh() -> Result[H, H] { return Err(H { tag: \"seven\" }); }\n\
         fn mkb() -> E { return E.B(W { id: 3 }); }\n";
    let wrap =
        |body: &str| format!("{PRELUDE}fn main() {{\n    {body}\n    println(\"after\");\n}}\n");

    // THE ROW.
    assert_eq!(
        run(&wrap(
            "let r: Result[W, W] = mkerr();\n\
             if let Ok(w) = r { println(f\"v{w.id}\"); }"
        )),
        "dW7\nafter\n"
    );

    // The row's first unmeasured shape: a `match` whose arm set binds only
    // `Ok` and wildcards the rest retracts through the same helper.
    assert_eq!(
        run(&wrap(
            "let r: Result[W, W] = mkerr();\n\
             match r { Ok(a) => { println(f\"ma{a.id}\"); } _ => { println(\"wild\"); } }"
        )),
        "wild\ndW7\nafter\n"
    );

    // Second unmeasured shape: a USER enum with payloads in both variants.
    // This one was an interp-vs-compiled DIVERGENCE, not an agreed silence —
    // the compiled backends printed the body and `--interp` did not.
    assert_eq!(
        run(&wrap(
            "let e: E = mkb();\n\
             if let E.A(w) = e { println(f\"v{w.id}\"); }"
        )),
        "dW3\nafter\n"
    );

    // …and its `let ... else` spelling, divergent the same way. A user enum
    // that declares its OWN `Drop` was already correct everywhere, so the hole
    // was specific to an enum whose only body is its payload's.
    assert_eq!(
        run(&wrap(
            "let e: E = mkb();\n\
             let E.A(w) = e else { println(\"elsearm\"); println(\"after\"); return };\n\
             println(f\"v{w.id}\");"
        )),
        "elsearm\nafter\ndW3\n"
    );

    // Third unmeasured shape: a heap-carrying payload. Bodies-only either way
    // — valgrind stayed at 9 allocs / 9 frees, 0 errors across the fix.
    assert_eq!(
        run(&wrap(
            "let r: Result[H, H] = mkerrh();\n\
             if let Ok(h) = r { println(f\"v{h.tag}\"); }"
        )),
        "dHseven\nafter\n"
    );

    // The `while let` spelling, whose first pass misses.
    assert_eq!(
        run(&wrap(
            "let r: Result[W, W] = mkerr();\n\
             while let Ok(w) = r { println(f\"v{w.id}\"); break; }"
        )),
        "dW7\nafter\n"
    );

    // The `let ... else` spelling, a RUN-VS-BUILD divergence before this fix:
    // `--interp` printed nothing where the compiled backends printed the body
    // at the divergent exit.
    assert_eq!(
        run(&wrap(
            "let r: Result[W, W] = mkerr();\n\
             let Ok(w) = r else { println(\"elsearm\"); println(\"after\"); return };\n\
             println(f\"v{w.id}\");"
        )),
        "elsearm\nafter\ndW7\n"
    );

    // THE CONSTRAINING SHAPE: `r`'s last use is the later `match`, so the body
    // is due there and exactly once. Emitting on the miss edge instead of
    // keeping the walk would print it twice.
    assert_eq!(
        run(&wrap(
            "let r: Result[W, W] = mkerr();\n\
             if let Ok(w) = r { println(f\"v{w.id}\"); }\n\
             println(\"mid\");\n\
             match r { Ok(a) => { println(f\"ma{a.id}\"); } Err(e) => { println(f\"me{e.id}\"); } }"
        )),
        "mid\nme7\ndW7\nafter\n"
    );

    // THE HIT-EDGE GATE: the arm's binding owns the payload and runs the body,
    // so the place's walk must stand down there.
    assert_eq!(
        run(&wrap(
            "let r: Result[W, W] = mkok();\n\
             if let Ok(w) = r { println(f\"v{w.id}\"); }"
        )),
        "v1\ndW1\nafter\n"
    );

    // CONTROL: the walk exists — this is what makes the row a retraction bug.
    assert_eq!(
        run(&wrap(
            "let r: Result[W, W] = mkerr();\n\
             println(\"x\");"
        )),
        "dW7\nx\nafter\n"
    );

    // CONTROL: a pattern that binds nothing never claimed the payload, so the
    // retraction never fired and this was correct before the fix too.
    assert_eq!(
        run(&wrap(
            "let r: Result[W, W] = mkerr();\n\
             if let Ok(_) = r { println(\"hit\"); }"
        )),
        "dW7\nafter\n"
    );

    // CONTROL: same reason, through `Option`/`None`.
    assert_eq!(
        run(&wrap(
            "let o: Option[W] = mksome();\n\
             if let None = o { println(\"none\"); } else { println(\"some\"); }"
        )),
        "some\ndW5\nafter\n"
    );

    // CONTROL: `match` binding both arms, correct throughout.
    assert_eq!(
        run(&wrap(
            "let r: Result[W, W] = mkerr();\n\
             match r { Ok(a) => { println(f\"ma{a.id}\"); } Err(e) => { println(f\"me{e.id}\"); } }"
        )),
        "me7\ndW7\nafter\n"
    );
}

#[test]
fn test_optres_partial_destructure_keeps_the_payload_drop_body() {
    // B-2026-09-02-8 — an `Option`/`Result` pattern that destructures the
    // payload but binds only a NON-`Drop` field lost the payload's body.
    //
    // Retracting a source's payload-bodies walk is a HAND-OFF, and the
    // Option/Result leg decided it SHAPE-only: `Some(H { n, .. })`, binding an
    // `i64` beside an untouched `Drop`-bearing `r: R`, counted as a consume.
    // The walk was retracted and nothing took over — the arm stash is filtered
    // by what the bindings actually own and came out empty — so the body ran
    // nowhere. Both backends now consult the payload's DECLARED field types.
    //
    // On the DEFAULT leg because half the fix is the interpreter's, and the
    // cross-backend matrix
    // (`e2e_an_optres_partial_destructure_keeps_the_payload_body`) is gated on
    // `--features llvm`.
    const PRELUDE: &str = "struct R { s: String }\n\
         impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.s}\"); } }\n\
         struct H { r: R, n: i64 }\n";
    let if_let = format!(
        "{PRELUDE}fn main() {{\n\
         let a: Option[H] = Some(H {{ r: R {{ s: \"a\" }}, n: 4 }});\n\
         if let Some(H {{ n, .. }}) = a {{ println(f\"A{{n}}\"); }}\n\
         println(\"end\");\n}}\n"
    );
    assert_eq!(run(&if_let), "A4\ndRa\nend\n");

    let matched = format!(
        "{PRELUDE}fn main() {{\n\
         let b: Option[H] = Some(H {{ r: R {{ s: \"b\" }}, n: 5 }});\n\
         match b {{ Some(H {{ n, .. }}) => println(f\"B{{n}}\"), None => {{}} }}\n\
         println(\"end\");\n}}\n"
    );
    assert_eq!(run(&matched), "B5\ndRb\nend\n");

    // The leg that was agreed-and-wrong across all four surfaces, so no A/B
    // gate could have caught it. The source dies at the destructure, so its
    // body fires ahead of the println.
    let let_else = format!(
        "{PRELUDE}fn main() {{\n\
         let e: Option[H] = Some(H {{ r: R {{ s: \"c\" }}, n: 6 }});\n\
         let Some(H {{ n, .. }}) = e else {{ return }};\n\
         println(f\"E{{n}}\");\n\
         println(\"end\");\n}}\n"
    );
    assert_eq!(run(&let_else), "dRc\nE6\nend\n");

    // THE GATE on the other side: binding the `Drop` field DOES hand the
    // payload over, so the retraction must still fire or the body runs twice.
    let binds_drop_field = format!(
        "{PRELUDE}fn main() {{\n\
         let o: Option[H] = Some(H {{ r: R {{ s: \"d\" }}, n: 7 }});\n\
         if let Some(H {{ r, .. }}) = o {{ println(f\"F{{r.s}}\"); }}\n\
         println(\"end\");\n}}\n"
    );
    assert_eq!(run(&binds_drop_field), "Fd\ndRd\nend\n");

    // A BARE binding keeps the shape answer — the payload type is a generic
    // parameter neither backend can read — so this proves the narrowing did
    // not reach it.
    let bare = format!(
        "{PRELUDE}fn main() {{\n\
         let o: Option[R] = Some(R {{ s: \"e\" }});\n\
         let Some(r) = o else {{ return }};\n\
         println(f\"W{{r.s}}\");\n\
         println(\"end\");\n}}\n"
    );
    assert_eq!(run(&bare), "We\ndRe\nend\n");

    // CONTROL: no envelope, correct throughout.
    let no_envelope = format!(
        "{PRELUDE}fn main() {{\n\
         let c: H = H {{ r: R {{ s: \"f\" }}, n: 8 }};\n\
         if let H {{ n, .. }} = c {{ println(f\"C{{n}}\"); }}\n\
         println(\"end\");\n}}\n"
    );
    assert_eq!(run(&no_envelope), "C8\ndRf\nend\n");
}

#[test]
fn test_while_let_binding_runs_the_payload_drop_body_once_per_pass() {
    // B-2026-09-02-7 — a `while let` arm that BINDS its payload ran the
    // payload's `Drop` body TWICE PER PASS on this backend, where both
    // compiled backends ran it once. `if let` retracts the source place's
    // payload-body walk when the pattern moves the payload out
    // (`disarm_moved_out_enum_payload_one`); `while let` never made that call,
    // so its arm stash fired beside a still-armed source walk.
    //
    // The surplus SCALED with the iteration count, which is what kept it out
    // of the low band: two passes ran four bodies against the two that were
    // due.
    //
    // This test lives on the DEFAULT leg deliberately. The fix is
    // interpreter-only, and the cross-backend matrix
    // (`e2e_while_let_binding_runs_the_payload_drop_body_once_per_pass`) is
    // gated on `--features llvm`, so without a copy here a regression would
    // pass CI's default leg unseen.
    const PRELUDE: &str = "struct R { s: String }\n\
         impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.s}\"); } }\n\
         struct H { r: R, n: i64 }\n";
    // ONE `R` per pass, so ONE body per pass.
    let one_pass = format!(
        "{PRELUDE}fn main() {{\n\
         let mut q: Option[H] = Some(H {{ r: R {{ s: \"a\" }}, n: 4 }});\n\
         while let Some(H {{ r, .. }}) = q {{ println(f\"{{r.s}}\"); q = None; }}\n\
         println(\"end\");\n}}\n"
    );
    assert_eq!(run(&one_pass), "a\ndRa\nend\n");

    // The whole-payload spelling of the same thing.
    let whole = format!(
        "{PRELUDE}fn main() {{\n\
         let mut p: Option[R] = Some(R {{ s: \"b\" }});\n\
         while let Some(r) = p {{ println(f\"{{r.s}}\"); p = None; }}\n\
         println(\"end\");\n}}\n"
    );
    assert_eq!(run(&whole), "b\ndRb\nend\n");

    // TWO passes, a fresh value stored each time: two bodies, not four. This
    // is the row that showed the surplus scaling.
    let two_pass = format!(
        "{PRELUDE}fn main() {{\n\
         let mut k: i64 = 0;\n\
         let mut z: Option[R] = Some(R {{ s: \"c\" }});\n\
         while let Some(r) = z {{\n\
         println(f\"{{r.s}}\");\n\
         k = k + 1;\n\
         if k < 2 {{ z = Some(R {{ s: \"d\" }}); }} else {{ z = None; }}\n\
         }}\n\
         println(\"end\");\n}}\n"
    );
    assert_eq!(run(&two_pass), "c\ndRc\nd\ndRd\nend\n");

    // THE GATE on the fresh-temp exclusion: `v.pop()` has no place whose walk
    // could be retracted, so its stash must keep firing. Standing it down here
    // would hand the payload to nobody and lose both bodies — which is the
    // reading B-2026-08-28-67 left this leg with, and why the fix gates on
    // `place_walk_is_retractable` rather than disarming unconditionally.
    let fresh_temp = format!(
        "{PRELUDE}fn main() {{\n\
         let mut v: Vec[R] = Vec.new();\n\
         v.push(R {{ s: \"e\" }});\n\
         v.push(R {{ s: \"f\" }});\n\
         while let Some(r) = v.pop() {{ println(f\"pop{{r.s}}\"); }}\n\
         println(\"end\");\n}}\n"
    );
    assert_eq!(run(&fresh_temp), "popf\ndRf\npope\ndRe\nend\n");

    // CONTROL: an arm binding nothing was correct throughout — the same loop
    // over the same value, differing only in whether it binds, which is what
    // isolated the trigger to the binding rather than to the loop.
    let binds_nothing = format!(
        "{PRELUDE}fn main() {{\n\
         let mut u: Option[R] = Some(R {{ s: \"g\" }});\n\
         while let Some(_) = u {{ println(\"hit\"); u = None; }}\n\
         println(\"end\");\n}}\n"
    );
    assert_eq!(run(&binds_nothing), "hit\ndRg\nend\n");
}

#[test]
fn test_enum_f64_payload_match_interpreter() {
    // Parallel to the codegen regression (e2e_enum_f64_payload_match_codegen):
    // enum float payloads bind as floats, not raw bits. Covers Option[f64] and
    // a tuple-payload enum (the lexer's Token::Float shape).
    let output = run(r#"enum Tok { Float(f64, i64), Nil }
        fn main() {
            match Some(3.14) { Some(x) => println(x), None => println(0.0), }
            let o: Option[f64] = Some(1.5);
            match o { Some(x) => println(x), None => println(0.0), }
            match Tok.Float(2.5, 7) { Float(x, y) => { println(x); println(y); } Nil => println(0.0), }
        }"#);
    assert_eq!(output, "3.14\n1.5\n2.5\n7\n");
}

#[test]
fn test_match_readonly_enum_payload_not_corrupted() {
    // Guard against over-firing: a read-only `ref V` match must leave the
    // scrutinee unchanged across repeated matches (both reads see size 1).
    let output = run("enum V { Table(Map[String, i64]) }\n\
         fn size(v: ref V) -> i64 { match v { Table(m) => m.len() as i64 } }\n\
         fn main() {\n\
             let mut mp: Map[String, i64] = Map.new();\n\
             mp.insert(\"a\", 1);\n\
             let t = V.Table(mp);\n\
             println(size(t));\n\
             println(size(t));\n\
         }");
    assert_eq!(output, "1\n1\n");
}

// ── Match-arm assignment to the scrutinee (B-2026-07-28-7) ────────────────────
// The write-through above reconstructed the scrutinee from its arm bindings and
// stored it back UNCONDITIONALLY, which silently REVERTED an assignment the arm
// body made to the scrutinee place itself. `match cur { Some(n) => { cur = ..
// } }` is the shape of every linked-structure walk and most state machines, so
// the revert turned them into infinite loops. `eval_match` now watches the
// scrutinee's slot across the arm body and skips the write-through when the
// body replaced it.
#[test]
fn test_match_arm_assignment_to_scrutinee_survives() {
    // Minimal shape: the arm assigns `None` to the very variable being matched.
    // Before the fix the second match still saw `Some(1)`.
    let output = run("fn main() {\n\
             let mut cur = Some(1);\n\
             match cur { None => {} Some(n) => { println(n); cur = None; } }\n\
             match cur { None => { println(\"none\") } Some(m) => { println(m) } }\n\
         }");
    assert_eq!(output, "1\nnone\n");
}

#[test]
fn test_match_arm_scrutinee_countdown_terminates() {
    // Assigning a fresh `Some` (not just `None`) must stick too — otherwise the
    // loop never makes progress. Ran forever before the fix.
    let output = run("fn main() {\n\
             let mut cur = Some(3);\n\
             let mut guard = 0;\n\
             loop {\n\
                 if guard >= 9 { println(\"CYCLE\"); break; }\n\
                 match cur {\n\
                     None => { println(\"end\"); break; }\n\
                     Some(n) => { println(n); if n <= 1 { cur = None; } else { cur = Some(n - 1); } }\n\
                 }\n\
                 guard = guard + 1;\n\
             }\n\
         }");
    assert_eq!(output, "3\n2\n1\nend\n");
}

#[test]
fn test_match_arm_shared_struct_link_walk_terminates() {
    // The `examples/tangle/src/doubly_linked.kara` shape that surfaced this:
    // walking `Option[<shared struct>]` links by reassigning the cursor inside
    // the arm. Before the fix `cur` never advanced past the head, so the walk
    // re-read node 1 forever.
    let output = run(
        "shared struct Node { mut val: i64, mut next: Option[Node] }\n\
         fn main() {\n\
             let a = Node { val: 1, next: None };\n\
             let b = Node { val: 2, next: None };\n\
             a.next = Some(b);\n\
             let mut cur = Some(a);\n\
             let mut guard = 0;\n\
             loop {\n\
                 if guard >= 5 { println(\"CYCLE\"); break; }\n\
                 match cur {\n\
                     None => { println(\"end\"); break; }\n\
                     Some(n) => { println(n.val); cur = n.next; }\n\
                 }\n\
                 guard = guard + 1;\n\
             }\n\
         }",
    );
    assert_eq!(output, "1\n2\nend\n");
}

#[test]
fn test_match_arm_assignment_does_not_suppress_sibling_write_through() {
    // The skip must be scoped to the scrutinee's OWN slot: an arm that mutates
    // a bound payload while assigning some OTHER variable still writes through
    // (B-2026-07-23-12 must keep working alongside B-2026-07-28-7).
    let output = run("enum V { List(Vec[i64]) }\n\
         fn add(v: mut ref V) {\n\
             let mut touched = 0;\n\
             match v { List(xs) => { xs.push(99); touched = 1; } }\n\
             println(touched);\n\
         }\n\
         fn size(v: ref V) -> i64 { match v { List(xs) => xs.len() as i64 } }\n\
         fn main() {\n\
             let mut xs: Vec[i64] = [1, 2];\n\
             let mut t = V.List(xs);\n\
             add(mut t);\n\
             println(size(t));\n\
         }");
    assert_eq!(output, "1\n3\n");
}

#[test]
fn test_match_arm_assignment_in_nested_match_reaches_outer_scrutinee() {
    // An assignment made from inside a NESTED match must still be seen by the
    // OUTER match's watch — otherwise the inner match hides the write and the
    // outer write-through reverts it. This is why the watch is a stack that
    // marks every frame naming the slot, not just the innermost.
    let output = run("fn main() {\n\
             let mut cur = Some(1);\n\
             let flag = Some(7);\n\
             match cur {\n\
                 None => {}\n\
                 Some(n) => {\n\
                     println(n);\n\
                     match flag { None => {} Some(f) => { cur = Some(n + f); } }\n\
                 }\n\
             }\n\
             match cur { None => { println(\"none\") } Some(m) => { println(m) } }\n\
         }");
    assert_eq!(output, "1\n8\n");
}

#[test]
fn test_match_arm_guard() {
    // B-2026-07-12-9 — a `match` arm guard (`pat if cond => ..`) falls through
    // to the next arm when the condition is false. The interpreter has always
    // been correct here; this is the oracle the codegen fix is verified against
    // (codegen previously ignored the guard entirely). Covers scalar-binding,
    // enum-pattern, and multi-guard cascades.
    let output = run_no_errors(
        r#"
fn classify(n: i64) -> String {
    match n { 0 => f"zero", x if x < 0i64 => f"neg", x if x < 10i64 => f"small", _ => f"big" }
}
fn g(o: Option[i64]) -> i64 { match o { Some(x) if x > 5i64 => 1i64, Some(_) => 2i64, None => 3i64 } }
fn main() {
    println(classify(0i64));
    println(classify(0i64 - 3i64));
    println(classify(5i64));
    println(classify(100i64));
    println(f"{g(Some(10i64))} {g(Some(2i64))} {g(None)}");
}
"#,
    );
    assert_eq!(output, "zero\nneg\nsmall\nbig\n1 2 3\n");
}

/// Interpreter oracle for B-2026-08-27-43 — the ARM-LOCAL half of the same
/// branch-leaf family, where the arm binds a local and hands the local out.
///
/// Same reason as the sibling oracle above: the interpreter has no refcount to
/// get wrong and was correct throughout, and the codegen twin
/// (`option_shared_from_an_arm_local_leaf_survives_repeated_consumption`) sits
/// behind `#[cfg(feature = "llvm")]` where a plain `cargo test` never reaches
/// it. Leg 4 additionally pins that the enclosing binding the arm-local aliases
/// still reads correctly after the branch's value has been consumed.
#[test]
fn option_shared_arm_local_branch_leaf_survives_repeated_consumption() {
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
    assert_eq!(
        run_no_errors(src),
        "1 1 1\n4 4 4\n5 5 5\n7 7 7 7\n8 8 8 8 8\n9 9 9\n10 10 10\n11 11 11\n\
         12 12 12\n13 13 13\n20 20 20\n21 21 21\n22 22 22\n2 2 2\n",
        "every read of an arm-local branch-leaf `Option[shared]` must see the same value"
    );
}

#[test]
fn test_match_tuple_pattern() {
    // B-2026-07-12-13 — a `match` on a tuple scrutinee discriminates on each
    // element. The interpreter has always been correct; this is the oracle for
    // the codegen fix (codegen previously always fired the first tuple arm).
    let output = run_no_errors(
        r#"
fn f(a: i64, b: i64) -> i64 { match (a, b) { (0i64, 0i64) => 10i64, (1i64, 2i64) => 20i64, _ => 30i64 } }
fn nested(a: i64, b: i64, c: i64) -> i64 {
    match (a, (b, c)) { (0i64, (1i64, 2i64)) => 1i64, (0i64, (_, _)) => 2i64, _ => 3i64 }
}
fn main() {
    println(f"{f(0i64, 0i64)} {f(1i64, 2i64)} {f(5i64, 5i64)}");
    println(f"{nested(0i64, 1i64, 2i64)} {nested(0i64, 4i64, 5i64)} {nested(8i64, 1i64, 2i64)}");
}
"#,
    );
    assert_eq!(output, "10 20 30\n1 2 3\n");
}

// ── `ref name @ PATTERN` — explicit-ref @ bindings (design.md § @
// Bindings): bindings borrow, scrutinee stays usable after ──────────

#[test]
fn test_ref_at_binding_match_borrows_and_scrutinee_stays_live() {
    let out = run_no_errors(
        "struct Foo { a: String, n: i64 }\n\
         fn main() {\n\
             let foo = Foo { a: \"hi\", n: 7 };\n\
             match foo {\n\
                 ref x @ Foo { a, n } => {\n\
                     println(a);\n\
                     println(n);\n\
                     println(x.n);\n\
                 }\n\
             }\n\
             println(foo.a);\n\
         }",
    );
    assert_eq!(out, "hi\n7\n7\nhi\n");
}

#[test]
fn enum_struct_variant_construction_match_and_equality() {
    // Source-level `Enum.Variant { field: value }` construction builds a
    // proper `Value::EnumVariant` (not a `Value::Struct`), so match-with-
    // field-binding, `==`, and mixed unit/struct-variant comparison all work.
    let output = run(r#"
#[derive(Eq)]
enum Shape { Circle { r: i64 }, Square { side: i64 }, Unknown }

fn area(s: Shape) -> i64 {
    match s {
        Shape.Circle { r } => 3 * r * r,
        Shape.Square { side } => side * side,
        Shape.Unknown => 0,
    }
}

fn main() {
    let c = Shape.Circle { r: 2 };
    let c2 = Shape.Circle { r: 2 };
    let sq = Shape.Square { side: 3 };
    let u = Shape.Unknown;
    println(f"{area(c)}");
    println(f"{area(sq)}");
    println(f"{c == c2}");
    println(f"{c == sq}");
    println(f"{c == u}");
}
"#);
    assert_eq!(output, "12\n9\ntrue\nfalse\nfalse\n");
}

/// B-2026-07-30-11 (match-arm leg) — interpreter twin of `tests/codegen.rs`'s
/// `e2e_match_arm_moved_payload_runs_drop_body`, same source and expected
/// string. Block arm bodies adopt the payload binding as a real Drop slot in
/// the block executor; bare-expression bodies fire at arm end when the only
/// uses are field projections.
#[test]
fn test_match_arm_moved_payload_runs_drop_body() {
    assert_eq!(
        run("struct Res { id: i64 }\n\
             impl Drop for Res {\n\
                 fn drop(mut ref self) {\n\
                     println(f\"drop {self.id}\")\n\
                 }\n\
             }\n\
             enum Box2 { Full(Res), Empty }\n\
             fn main() {\n\
                 let b = Box2.Full(Res { id: 4 });\n\
                 match b {\n\
                     Box2.Full(r) => { println(f\"arm sees {r.id}\"); }\n\
                     Box2.Empty => {}\n\
                 }\n\
                 println(\"between\");\n\
                 let c = Box2.Full(Res { id: 9 });\n\
                 match c {\n\
                     Box2.Full(r) => println(r.id),\n\
                     Box2.Empty => {}\n\
                 }\n\
                 println(\"end\");\n\
             }\n"),
        "arm sees 4\ndrop 4\nbetween\n9\ndrop 9\nend\n"
    );
}

/// B-2026-07-30-11 (discarded-temp leg) — interpreter twin of
/// `tests/codegen.rs`'s `e2e_wildcard_let_discard_runs_drop_bodies`, same
/// source and expected string. `let _ = <owned temp>;` fires the discarded
/// value's Drop work at the `;` via `run_discarded_value_user_drops`.
#[test]
fn test_wildcard_let_discard_runs_drop_bodies() {
    assert_eq!(
        run("struct Res { id: i64 }\n\
             impl Drop for Res {\n\
                 fn drop(mut ref self) {\n\
                     println(f\"drop {self.id}\")\n\
                 }\n\
             }\n\
             fn mk(n: i64) -> Res {\n\
                 Res { id: n }\n\
             }\n\
             fn mkopt(n: i64) -> Option[Res] {\n\
                 Option.Some(Res { id: n })\n\
             }\n\
             fn main() {\n\
                 println(\"s1\");\n\
                 let _ = Res { id: 1 };\n\
                 println(\"s2\");\n\
                 let _ = mk(2);\n\
                 println(\"s3\");\n\
                 mk(3);\n\
                 println(\"s4\");\n\
                 let _ = (Res { id: 4 }, 40);\n\
                 println(\"s5\");\n\
                 let _ = Option.Some(Res { id: 5 });\n\
                 println(\"s6\");\n\
                 let _ = mkopt(6);\n\
                 println(\"end\");\n\
             }\n"),
        "s1\ndrop 1\ns2\ndrop 2\ns3\ndrop 3\ns4\ndrop 4\ns5\ndrop 5\ns6\ndrop 6\nend\n"
    );
}

/// B-2026-07-30-11 (discarded-temp leg, place-shape pins) — interpreter twin
/// of `tests/codegen.rs`'s `e2e_wildcard_let_discard_place_shapes_single_fire`,
/// same source and expected string: moved-binding discard shapes fire the
/// body exactly once.
#[test]
fn test_wildcard_let_discard_place_shapes_single_fire() {
    assert_eq!(
        run("struct Res { id: i64 }\n\
             struct W { r: Res }\n\
             impl Drop for Res {\n\
                 fn drop(mut ref self) {\n\
                     println(f\"drop {self.id}\")\n\
                 }\n\
             }\n\
             fn main() {\n\
                 let r = Res { id: 31 };\n\
                 println(\"a\");\n\
                 let _ = (r, 1);\n\
                 println(\"b\");\n\
                 let s = Res { id: 32 };\n\
                 let _ = Option.Some(s);\n\
                 println(\"c\");\n\
                 let r0 = Res { id: 33 };\n\
                 let _ = W { r: r0 };\n\
                 println(\"d\");\n\
                 let t = Res { id: 34 };\n\
                 let _ = t;\n\
                 println(\"e\");\n\
                 let k = 5;\n\
                 let _ = Res { id: k };\n\
                 println(\"end\");\n\
             }\n"),
        "a\ndrop 31\nb\ndrop 32\nc\ndrop 33\nd\ndrop 34\ne\ndrop 5\nend\n"
    );
}

/// B-2026-07-30-11 (if-let leg) — interpreter twin of `tests/codegen.rs`'s
/// `e2e_if_let_moved_payload_runs_drop_body`, same source and expected
/// string.
#[test]
fn test_if_let_moved_payload_runs_drop_body() {
    assert_eq!(
        run("struct Res { id: i64 }\n\
             impl Drop for Res {\n\
                 fn drop(mut ref self) {\n\
                     println(f\"drop {self.id}\")\n\
                 }\n\
             }\n\
             enum Box2 { Full(Res), Empty }\n\
             fn mkbox(n: i64) -> Box2 {\n\
                 Box2.Full(Res { id: n })\n\
             }\n\
             fn take_res(r: Res) {\n\
                 println(f\"consumed {r.id}\")\n\
             }\n\
             fn main() {\n\
                 let b = Box2.Full(Res { id: 33 });\n\
                 println(\"a\");\n\
                 if let Box2.Full(r) = b {\n\
                     println(f\"arm sees {r.id}\");\n\
                 }\n\
                 println(\"b\");\n\
                 if let Box2.Full(r) = mkbox(34) {\n\
                     println(f\"arm sees {r.id}\");\n\
                 }\n\
                 println(\"c\");\n\
                 let e = Box2.Empty;\n\
                 if let Box2.Full(r) = e {\n\
                     println(f\"arm sees {r.id}\");\n\
                 } else {\n\
                     println(\"empty\");\n\
                 }\n\
                 println(\"d\");\n\
                 let c = Box2.Full(Res { id: 35 });\n\
                 if let Box2.Full(r) = c {\n\
                     take_res(r);\n\
                     println(\"after move\");\n\
                 }\n\
                 println(\"end\");\n\
             }\n"),
        "a\narm sees 33\ndrop 33\nb\narm sees 34\ndrop 34\nc\nempty\nd\n\
         consumed 35\nafter move\ndrop 35\nend\n"
    );
}

/// B-2026-07-31-38 (enum sibling) — interpreter twin of `tests/codegen.rs`'s
/// `e2e_enum_walker_rearm_after_move_reassign`, same source and expected
/// string. The interpreter already fired both exit bodies (its move records
/// are cleared on assign); the test pins that as the parity target the
/// codegen re-arm now meets.
#[test]
fn test_enum_walker_rearm_after_move_reassign() {
    assert_eq!(
        run("struct Res { id: i64, name: String }\n\
             impl Drop for Res {\n\
                 fn drop(mut ref self) {\n\
                     println(f\"drop {self.id} {self.name}\")\n\
                 }\n\
             }\n\
             enum SBox { Full(Res), Empty }\n\
             fn mk(n: i64) -> SBox {\n\
                 return SBox.Full(Res { id: n, name: f\"s{n}\" });\n\
             }\n\
             fn use_res(r: Res) {\n\
                 println(f\"took {r.id}\");\n\
             }\n\
             fn main() {\n\
                 println(\"a\");\n\
                 let mut b = mk(3);\n\
                 let c = b;\n\
                 b = mk(4);\n\
                 println(\"m\");\n\
                 let mut d = mk(5);\n\
                 match d {\n\
                     SBox.Full(r) => { use_res(r); }\n\
                     SBox.Empty => {}\n\
                 }\n\
                 d = mk(6);\n\
                 println(\"end\");\n\
             }\n"),
        "a\ndrop 3 s3\ndrop 4 s4\nm\ntook 5\ndrop 5 s5\ndrop 6 s6\nend\n"
    );
}

/// B-2026-09-02-15 (interpreter twin / ORACLE) — the `if let` family over an
/// indexed element. The interpreter never clones, so it always printed the
/// right answer here; the compiled backends ABORTED with a double free until
/// the three `control_flow.rs` sites started cloning the element the way
/// `compile_match` always has.
///
/// Pinned to the same program and the same string as `tests/codegen.rs`'s
/// `e2e_index_element_clone_reaches_the_if_let_family`, whose doc carries the
/// leg-by-leg rationale.
#[test]
fn test_if_let_over_an_indexed_element_runs_one_payload_body() {
    assert_eq!(
        run(r#"struct R { id: i64, v: Vec[i64] }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
enum E { A(R), B }
impl Drop for E { fn drop(mut ref self) { println("dE") } }
struct H { xs: Vec[E] }

struct N { id: i64, v: Vec[i64] }
enum F { A(N), B }

fn mk(n: i64) -> E { let mut v: Vec[i64] = Vec.new(); v.push(n); return E.A(R { id: n, v: v }) }
fn mkf(n: i64) -> F { let mut v: Vec[i64] = Vec.new(); v.push(n); return F.A(N { id: n, v: v }) }

fn leg_iflet() {
    println("iflet");
    let mut v: Vec[E] = Vec.new();
    v.push(mk(1));
    if let E.A(r) = v[0] { println(f"got {r.id + r.v.len()}"); }
    println("iflet end");
}

fn leg_field() {
    println("field");
    let mut v: Vec[E] = Vec.new();
    v.push(mk(2));
    let h = H { xs: v };
    if let E.A(r) = h.xs[0] { println(f"got {r.id + r.v.len()}"); }
    println("field end");
}

fn leg_whilelet() {
    println("whilelet");
    let mut v: Vec[E] = Vec.new();
    v.push(mk(3));
    while let E.A(r) = v[0] {
        println(f"got {r.id + r.v.len()}");
        break
    }
    println("whilelet end");
}

fn leg_unbound() {
    println("unbound");
    let mut v: Vec[E] = Vec.new();
    v.push(mk(4));
    if let E.A(_) = v[0] { println("got"); }
    println("unbound end");
}

fn leg_nodrop() {
    println("nodrop");
    let mut v: Vec[F] = Vec.new();
    v.push(mkf(5));
    if let F.A(n) = v[0] { println(f"got {n.id + n.v.len()}"); }
    println("nodrop end");
}

fn leg_fresh() {
    println("fresh");
    if let E.A(r) = mk(6) { println(f"got {r.id + r.v.len()}"); }
    println("fresh end");
}

fn main() {
    leg_iflet();
    leg_field();
    leg_whilelet();
    leg_unbound();
    leg_nodrop();
    leg_fresh();
    println("end");
}
"#),
        r#"iflet
got 2
dE
dR1
iflet end
field
got 3
dE
dR2
field end
whilelet
got 4
dE
dR3
whilelet end
unbound
got
dE
dR4
unbound end
nodrop
got 6
nodrop end
fresh
got 7
dR6
dE
fresh end
end
"#
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
/// The interpreter half of the row. `record_optres_payload_te` — the only thing
/// that puts a name into `optres_payload_bodies_tes`, which is what
/// `run_optres_payload_user_drops` consults at drop time — was called under
/// `PatternKind::Binding` alone, so a destructure's leaves never reached a
/// registration moment and their payload bodies ran nowhere.
///
/// `annres` NO LONGER PINS A GAP — B-2026-09-03-22 closed it. The cell used to
/// assert that a `Result` leaf ran no payload body on any surface, because a
/// boxed `Result` payload had no memory owner to hand it to; it has one now, so
/// both backends run the body and `dR77/t77` is the line that changed.
///
/// THIS BUG COULD HIDE ITSELF, which is why `stale` is a cell. The table is
/// keyed by variable NAME and never cleared per function, so an earlier
/// function that bound an `Option[R]` to a variable of the SAME NAME left an
/// entry a later destructure then read — and the body ran, correctly, by
/// accident. Two spellings of the identical shape therefore disagreed depending
/// on what else the program did first. `stale` pins the shape that used to
/// depend on that: with the fix the leaf registers on its own, so the cell is
/// correct whether or not anything warmed the name.
///
/// Twin of `tests/codegen.rs`'s
/// `e2e_tuple_destructure_optres_leaf_owns_its_payload_body`, pinned to the same
/// string.
#[test]
fn test_tuple_destructure_optres_leaf_owns_its_payload_body() {
    assert_eq!(
        run(r#"struct R { id: i64, tag: String }
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
fn stale() { let t = (mk(8), Option.Some(mk(88))); let (r, o) = t; println(f"  rd{r.id}") }

fn main() {
    println("loc1");   loc1()
    println("loc0");   loc0()
    println("ctl");    ctl()
    println("proj");   proj()
    println("used");   used()
    println("none");   none()
    println("annres"); annres()
    println("stale");  stale()
    println("done")
}
"#),
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
stale
dR88/t88
  rd8
dR8/t8
done
"#
    );
}

/// B-2026-09-03-24 — the interpreter half: a destructured STRUCT's `Option`
/// FIELD leaf owns its payload's `Drop` body.
///
/// `record_destructure_optres_payload_tes` returned on anything but a
/// `PatternKind::Tuple`, so a struct pattern's leaves reached no registration
/// moment at all and `let Ho2 { a, b } = h;` ran `dR1` and never `dR101`. The
/// two shapes resolve a leaf's type from DIFFERENT places — a tuple leaf is
/// positional and reads the SOURCE's element types, a struct field leaf is
/// named and reads the DECLARATION — which is why the arm is a second collector
/// feeding one shared `Option` filter rather than a widened pattern match.
///
/// `ren` is the cell that separates the two field spellings: `Ho2 { a, b }`
/// parses as a `FieldPattern` with NO sub-pattern (the leaf takes the field's
/// own name) while `Ho2 { a: p, b: q }` carries `Some(Binding(..))`, so a
/// collector that handled only one of them would fix half the shapes.
///
/// The `param` cell is the constraint, not decoration: the caller/entry copy
/// already runs those bodies, and registering the leaf as well doubles them —
/// the same `owned_param_names_stack` gate the tuple arm above uses, now shared
/// by both.
///
/// Twin of `tests/codegen.rs`'s
/// `e2e_struct_field_destructure_option_leaf_owns_its_payload_body`, pinned to
/// the same string.
/// B-2026-09-03-33 — the INTERPRETER side of the `Result` husk: this backend
/// was already right, and the test exists to keep it that way while the
/// compiled backends were brought to it.
///
/// `let HoRes { a, b } = h;` over `{ a: R, b: Result[R, String] }` printed
/// `dR0/` on jit/aot/AUTO_PAR=0 — `R`'s body over a ZEROED object — and nothing
/// here. The fix is entirely in codegen (mask the source's field-bodies walk
/// for the moved-out `Result` field, which its `Option` sibling gets for free
/// from a tag-zero `Result` has no equivalent of), so every line below is
/// unchanged by it. Pinning the string is what makes a future change to the
/// remaining `Result` deferral fail LOUDLY on the side that is correct, instead
/// of quietly re-opening the split from the other direction.
///
/// The `param` cell is the one that carries information rather than
/// confirmation: `dR107` runs here and on all three compiled surfaces, because
/// the owner's own walk runs it. That is why the missing `dR101` on the `loc`
/// cell cannot be fixed by registering the leaf unconditionally, and why this
/// fix removes the phantom without touching the absence.
///
/// Twin of `tests/codegen.rs`'s
/// `e2e_struct_field_destructure_result_leaf_leaves_no_husk_body`, pinned to
/// the same string.
///
/// B-2026-09-04-1 — the "missing body is agreed-and-absent on both and stays
/// that way" sentence above is now history: the leaf owns the body on every
/// source shape here (`loc`, `box`, `awild`, `nest`, `call`, `lit` each gain
/// their `dR1xx` line, at the destructure — the unused leaf's last use). The
/// phantom this fixture was written for stays gone, which is what `wild` and
/// `param` (unchanged) still pin. `awild` was pinned per backend (B-2026-09-04-23):
/// both ran `a`'s discard body and `b5`'s body at the same statement, in opposite
/// sequence. B-2026-09-03-32 settled the tie in this backend's favour — the
/// discard is the destructure's own destruction and precedes an unread leaf's
/// NLL death — so the codegen twin now carries this same string.
#[test]
fn test_struct_field_destructure_result_leaf_leaves_no_husk_body() {
    assert_eq!(
        run(r#"struct R  { id: i64, tag: String }
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
"#),
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

#[test]
fn test_struct_field_destructure_option_leaf_owns_its_payload_body() {
    assert_eq!(
        run(r#"struct R { id: i64, tag: String }
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
"#),
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
#[test]
fn test_tuple_destructure_result_payload_survives_every_depth() {
    assert_eq!(
        run(r#"struct R { id: i64, tag: String }
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
"#),
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

/// B-2026-09-04-8, `Option` half — the same depth defect on the other built-in
/// payload. Distinct leaf names per cell, for the reason the `Result` twin
/// spells out.
#[test]
fn test_tuple_destructure_option_payload_survives_every_depth() {
    assert_eq!(
        run(r#"struct R { id: i64, tag: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}/{self.tag}") } }
fn mk(n: i64) -> R { return R { id: n, tag: f"t{n}" }; }

struct WrapO { inner: (R, Option[R]) }
struct OuterO { h: WrapO }

fn c_named() { let t: (R, Option[R]) = (mk(1), Option.Some(mk(101))); let (a1, b1) = t; println(f"  rd{a1.id}") }
fn c_one()   { let w = WrapO { inner: (mk(2), Option.Some(mk(102))) }; let (a2, b2) = w.inner; println(f"  rd{a2.id}") }
fn c_two()   { let g = OuterO { h: WrapO { inner: (mk(3), Option.Some(mk(103))) } }; let (a3, b3) = g.h.inner; println(f"  rd{a3.id}") }

fn main() {
    println("named"); c_named(); println("named end")
    println("one");   c_one();   println("one end")
    println("two");   c_two();   println("two end")
    println("done")
}
"#),
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
done
"#
    );
}

/// B-2026-09-10-16 — the INTERPRETER twin of
/// `tests/codegen.rs`'s `e2e_arm_bound_tuple_payload_field_read_resolves`,
/// pinned to the same string.
///
/// This backend ALWAYS answered these cells — the defect was codegen refusing to
/// lower the field read at all ("cannot resolve field 'id' on this receiver"),
/// while `karac check` accepted the program and `--interp` ran it. So this twin is
/// not a regression guard for a fix that landed here; it is the ORACLE the compiled
/// side is now pinned against, and the thing that would catch a future "fix" to
/// codegen that made the two backends agree by changing this one.
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
fn test_arm_bound_tuple_payload_field_read_resolves() {
    assert_eq!(
        run(r#"struct R { id: i64, tag: String }
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
"#),
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

/// B-2026-09-10-21 — the interpreter twin of
/// `tests/codegen.rs`'s `e2e_arm_bound_nested_tuple_payload_field_read_resolves`,
/// and deliberately NOT pinned to the same string.
///
/// The field READS are what this row is about and they are identical on both
/// backends. What differs is the payload elements' `Drop` bodies: the compiled
/// backends run them (correct — the arm binding owns the payload, and the one-hop
/// `n8` agrees on both), and the interpreter runs NONE of them once the payload
/// element is itself a TUPLE. That is B-2026-09-17-20, filed, and it became
/// observable only when this row's fix let the compiled side build at all.
///
/// Pinning the interpreter's own transcript here rather than the agreed one is
/// what keeps the divergence VISIBLE in the pair: whoever fixes -19 makes this
/// string gain the `dW…` lines its twin already has, and the two fixtures then
/// hold the same text.
#[test]
fn test_arm_bound_nested_tuple_payload_field_read_resolves() {
    assert_eq!(
        run(r#"struct W { id: i64, tag: String }
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
"#),
        r#"n1
  a1/t1
n2
  b102
n3
  c3
n4
  d4
n5
  e5
n6
  f6/60/600
n7
  g7/5
n8
  h8
dW8/t8
end
"#
    );
}

#[test]
fn test_struct_pattern_destructure_of_owned_param_is_a_view() {
    assert_eq!(
        run(r#"struct R { id: i64 }
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
"#),
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

/// B-2026-08-29-6, interpreter leg — a passthrough call used directly as a
/// MATCH SCRUTINEE yields the payload exactly once.
///
/// The twin of `e2e_passthrough_match_scrutinee_leaves_the_source_sole_owner`
/// in `tests/codegen.rs`, carrying the same cases with the same expected
/// output. Keep the two in step verbatim.
///
/// This backend was CORRECT before the fix, so every case is a control: the
/// defect was a compiled-only double free (process abort) affecting free
/// functions and methods alike, and what these pin is that codegen moved onto
/// the interpreter's answer rather than both moving.
#[test]
fn test_passthrough_match_scrutinee_leaves_the_source_sole_owner() {
    const DECLS: &str = "struct Bx { n: i64 }\n\
         impl Bx {\n\
         fn take(ref self, o: Option[String]) -> Option[String] { o }\n\
         fn take_res(ref self, o: Result[String, i64]) -> Result[String, i64] { o }\n\
         fn fresh(ref self, o: Option[String]) -> Option[String] { Option.Some(f\"fresh{self.n}\") }\n\
         }\n\
         fn takef(o: Option[String]) -> Option[String] { o }\n";
    for (label, body, want) in [
        (
            "free-fn-scrutinee-passthrough",
            "fn main() { let n = 1; let s = Option.Some(f\"a{n}\"); \
             match takef(s) { Option.Some(v) => { println(f\"got {v}\") } \
             Option.None => { println(\"none\") } } }\n",
            "got a1\n",
        ),
        (
            "method-scrutinee-passthrough",
            "fn main() { let b = Bx { n: 1 }; let s = Option.Some(f\"a{b.n}\"); \
             match b.take(s) { Option.Some(v) => { println(f\"got {v}\") } \
             Option.None => { println(\"none\") } } }\n",
            "got a1\n",
        ),
        (
            "result-scrutinee-passthrough",
            "fn main() { let b = Bx { n: 1 }; \
             let s: Result[String, i64] = Result.Ok(f\"c{b.n}\"); \
             match b.take_res(s) { Result.Ok(v) => { println(f\"got {v}\") } \
             Result.Err(e) => { println(f\"err {e}\") } } }\n",
            "got c1\n",
        ),
        (
            "no-passthrough-scrutinee-keeps-owner",
            "fn main() { let b = Bx { n: 1 }; let s = Option.Some(f\"h{b.n}\"); \
             match b.fresh(s) { Option.Some(v) => { println(f\"got {v}\") } \
             Option.None => { println(\"none\") } } }\n",
            "got fresh1\n",
        ),
        (
            "let-bound-spelling-still-correct",
            "fn main() { let b = Bx { n: 1 }; let s = Option.Some(f\"k{b.n}\"); \
             let o = b.take(s); \
             match o { Option.Some(v) => { println(f\"got {v}\") } \
             Option.None => { println(\"none\") } } }\n",
            "got k1\n",
        ),
    ] {
        assert_eq!(run(&format!("{DECLS}{body}")), want, "{label}");
    }
}

/// B-2026-08-29-58 — a `match` / `if let` arm that ASSIGNS an owned-param
/// payload to an OUTER local runs that payload's `Drop` body exactly once.
///
/// The ASSIGNMENT spelling of the move `let m = r;` has handled since
/// B-2026-08-29-17. Pre-fix the interpreter ran the body TWICE — its own slot
/// for the assigned-into local on top of the caller's payload walk — against
/// one on all three compiled surfaces. Unlike its parent row B-2026-08-29-48,
/// whose defect was agreed-wrong across every backend, this one IS an A/B
/// divergence, so the compiled output is a usable oracle and every expectation
/// below is exactly what `karac build` produces.
///
/// The `let` twin is carried as a CONTROL rather than described in prose: it is
/// what makes one body the right answer here rather than a preference, so a
/// change that "fixed" the assignment spelling by moving the `let` one would
/// fail on it.
#[test]
fn test_arm_payload_assigned_to_outer_local_drop_body_runs_once() {
    const DROPPER: &str = "struct R { id: i64, tag: String }\n\
         impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\") } }\n\
         enum E { A(R), B }\n\
         impl Drop for E { fn drop(mut ref self) { println(\"dE\") } }\n";
    for (label, body, want) in [
        // The row's headline program. Pre-fix the interpreter printed an extra
        // `dR8` between `mid` and `dE` -- its own slot for `out` firing on top of
        // the caller's payload walk.
        (
            "assign-nonescaping",
            "fn dies(b: E) -> i64 {\n\
                 let mut out: R = R { id: 0, tag: f\"t0\" };\n\
                 match b { E.A(r) => { out = r; } E.B => { } }\n\
                 println(\"mid\");\n\
                 return out.id;\n\
             }\n\
             fn main() { let c1: E = E.A(R { id: 8, tag: f\"t8\" }); let v1: i64 = dies(c1); println(f\"v{v1}\"); }\n",
            "dR0\nmid\ndE\ndR8\nv8\n",
        ),
        // The `if let` spelling binds through a separate path in this backend and
        // needed confirming separately; it doubled identically pre-fix.
        (
            "assign-iflet",
            "fn dies(b: E) -> i64 {\n\
                 let mut out: R = R { id: 0, tag: f\"t0\" };\n\
                 if let E.A(r) = b { out = r; }\n\
                 println(\"m\");\n\
                 return out.id;\n\
             }\n\
             fn main() { let c: E = E.A(R { id: 8, tag: f\"t8\" }); let v: i64 = dies(c); println(f\"v{v}\"); }\n",
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
            "fn dies(b: E) -> i64 {\n\
                 let mut out: R = R { id: 0, tag: f\"t0\" };\n\
                 match b { E.A(r) => { out = r; } E.B => { } }\n\
                 println(\"mid\");\n\
                 let loc: R = R { id: 9, tag: f\"t9\" };\n\
                 let s: i64 = out.id + loc.id;\n\
                 println(\"pre-ret\");\n\
                 return s;\n\
             }\n\
             fn main() { let c1: E = E.A(R { id: 8, tag: f\"t8\" }); let v1: i64 = dies(c1); println(\"post-call\"); println(f\"v{v1}\"); }\n",
            "dR0\nmid\ndR9\npre-ret\ndE\ndR8\npost-call\nv17\n",
        ),
        // Assigned TWICE. The displacement body must fire for the value the target
        // genuinely owned (`R{0}`) and NOT for the view it later held (`R{8}`), which
        // is the caller's to run -- so silencing through the whole-binding set has to
        // cover the displacement fire too, not only the slot.
        (
            "assign-repeated",
            "fn dies(b1: E, b2: E) -> i64 {\n\
                 let mut out: R = R { id: 0, tag: f\"t0\" };\n\
                 match b1 { E.A(r) => { out = r; } E.B => { } }\n\
                 println(\"m1\");\n\
                 match b2 { E.A(r) => { out = r; } E.B => { } }\n\
                 println(\"m2\");\n\
                 return out.id;\n\
             }\n\
             fn main() { let v: i64 = dies(E.A(R { id: 8, tag: f\"t8\" }), E.A(R { id: 9, tag: f\"t9\" })); println(f\"v{v}\"); }\n",
            "dR0\nm1\nm2\ndE\ndR9\ndE\ndR8\nv9\n",
        ),
        // A FRESH-TEMP argument rather than a named binding: the caller's fire is
        // `run_fresh_temp_arg_drops` instead of a binding's own NLL drop, and the
        // stand-down is correct against both.
        (
            "assign-fresh-temp-arg",
            "fn dies(b: E) -> i64 {\n\
                 let mut out: R = R { id: 0, tag: f\"t0\" };\n\
                 match b { E.A(r) => { out = r; } E.B => { } }\n\
                 println(\"m\");\n\
                 return out.id;\n\
             }\n\
             fn main() { let v: i64 = dies(E.A(R { id: 8, tag: f\"t8\" })); println(f\"v{v}\"); }\n",
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
             fn main() { let c: E = E.A(R { id: 8, tag: f\"t8\" }); let v: i64 = f(c); println(f\"v{v}\"); }\n",
            "dR0\nm\ndR7\ndE\ndR8\nv7\n",
        ),
        // GUARD RAIL -- a LOCAL scrutinee has no caller behind it, so the assigned-into
        // local really is the only owner and MUST keep its body. Gating on the owned-
        // param view set is what buys this.
        (
            "guard-local-scrutinee",
            "fn main() {\n\
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
             fn main() {\n\
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
            "fn take(b: E, p: R) -> i64 {\n\
                 let mut out: R = R { id: 0, tag: f\"t0\" };\n\
                 match b { E.A(r) => { out = r; } E.B => { } }\n\
                 println(\"m\");\n\
                 return out.id + p.id;\n\
             }\n\
             fn main() { let c: E = E.A(R { id: 8, tag: f\"t8\" }); let v: i64 = take(c, R { id: 2, tag: f\"t2\" }); println(f\"v{v}\"); }\n",
            "dR0\nm\ndR2\ndE\ndR8\nv10\n",
        ),
        // GUARD RAIL -- B-2026-08-29-48's case, the opposite boundary: the assigned-into
        // local IS returned, so the value escapes and the caller's result binding owns
        // it. That row made the caller stand down here; this fix must not disturb it.
        (
            "guard-assigned-and-returned",
            "fn takes(b: E) -> R {\n\
                 let mut out: R = R { id: 0, tag: f\"t0\" };\n\
                 match b { E.A(r) => { out = r; } E.B => { } }\n\
                 println(\"mid\");\n\
                 return out;\n\
             }\n\
             fn main() { let c1: E = E.A(R { id: 8, tag: f\"t8\" }); let v1: R = takes(c1); println(\"post-call\"); println(f\"v{v1.id}\"); }\n",
            "dR0\nmid\ndE\npost-call\nv8\ndR8\n",
        ),
        // CONTROL -- the `let` spelling of the same move, correct since B-2026-08-29-17
        // and unchanged here. It is the oracle that made the assignment spelling the
        // anomaly rather than a matter of preference.
        (
            "control-let-twin",
            "fn dies(b: E) -> i64 {\n\
                 let mut k: i64 = 0;\n\
                 match b { E.A(r) => { let inner: R = r; k = inner.id; } E.B => { } }\n\
                 println(\"mid\");\n\
                 return k;\n\
             }\n\
             fn main() { let c1: E = E.A(R { id: 8, tag: f\"t8\" }); let v1: i64 = dies(c1); println(f\"v{v1}\"); }\n",
            "mid\ndE\ndR8\nv8\n",
        ),
        // INTERPRETER ONLY, and deliberately absent from the codegen twin. When the
        // arm does NOT run, `out` still holds its own initializer, which nobody else
        // owns -- so its body must fire, and here it does. Both compiled backends LOSE
        // it (`mid` / `dE` / `v0`), which is a separate defect filed on its own row;
        // sharing this case with the twin would mean writing that defect into an
        // expectation.
        (
            "guard-arm-not-taken",
            "fn dies(b: E) -> i64 {\n\
                 let mut out: R = R { id: 0, tag: f\"t0\" };\n\
                 match b { E.A(r) => { out = r; } E.B => { } }\n\
                 println(\"mid\");\n\
                 return out.id;\n\
             }\n\
             fn main() { let c1: E = E.B; let v1: i64 = dies(c1); println(f\"v{v1}\"); }\n",
            "mid\ndR0\ndE\nv0\n",
        ),
    ] {
        assert_eq!(run(&format!("{DROPPER}{body}")), want, "{label}");
    }
}

/// B-2026-08-28-12, interpreter leg — a WILDCARD leaf of a `let` destructure
/// runs the discarded value's user `Drop` body exactly once.
///
/// Pre-fix it ran ZERO times: a wildcard binds no name, so `push_drops_for_stmt`
/// registered no Drop slot for it and nothing else owned the value. The level
/// above (`let _ = R { .. }`) has been correct since B-2026-07-30-11 and a
/// `match` arm's wildcard was correct too, which is what shows this was a
/// `let`-destructure hole rather than a rule about wildcards.
///
/// `param-tuple-control` and `param-struct-control` carry the INTERPRETER's
/// order: it fires the caller-side argument walk AT the call, codegen defers it
/// to the caller's scope exit, so the same one body prints before the result
/// instead of after. That split is B-2026-08-28-19, predates this fix, and is
/// untouched by it — those two rows are exactly the ones the fix's gate
/// EXCLUDES, since a by-value param's body is the caller's to run. Every other
/// row here matches the compiled twin verbatim.
///
/// Twin: `tests/codegen.rs`'s
/// `e2e_wildcard_destructure_leaf_user_drop_body_runs_once`, whose doc records
/// the one family member still absent (a wildcard field over a struct LITERAL
/// source, blocked behind a separate struct-literal destructure defect).
#[test]
fn test_wildcard_destructure_leaf_user_drop_body_runs_once() {
    const DROPPER: &str = "struct R { id: i64 }\n\
         impl Drop for R { fn drop(mut ref self) { println(f\"drop {self.id}\") } }\n";
    for (label, body, want) in [
        (
            "tuple-place",
            "fn main() { let p = (R { id: 41 }, 1); let (_, n) = p; println(f\"{n}\") }\n",
            "drop 41\n1\n",
        ),
        (
            "tuple-fresh-literal",
            "fn main() { let (_, n) = (R { id: 41 }, 1); println(f\"{n}\") }\n",
            "drop 41\n1\n",
        ),
        (
            "tuple-fresh-call",
            "fn mk() -> (R, i64) { (R { id: 41 }, 1) }\n\
             fn main() { let (_, n) = mk(); println(f\"{n}\") }\n",
            "drop 41\n1\n",
        ),
        // A wildcard inside a NESTED tuple pattern. The tree-walker collected
        // only top-level wildcard positions, so this shape ran no body here
        // while the compiled backends' place-source walker recursed and ran
        // one — a run-vs-build divergence in the opposite direction from the
        // one this row fixed. Both sides recurse now.
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
        // An ENUM element's live-variant payload, the twin of the codegen
        // rows: the tree-walker was already right here, so these pin the side
        // that did not move while codegen caught up.
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
            "struct-fresh-call",
            "struct W { r: R, n: i64 }\n\
             fn mk() -> W { W { r: R { id: 41 }, n: 1 } }\n\
             fn main() { let W { r: _, n } = mk(); println(f\"{n}\") }\n",
            "drop 41\n1\n",
        ),
        // The one row of this family that was a run-vs-build split, and the
        // interpreter was the WRONG side of it — compiled already ran one.
        (
            "struct-place",
            "struct W { r: R, n: i64 }\n\
             fn main() { let w = W { r: R { id: 41 }, n: 1 };\n\
             \x20           let W { r: _, n } = w; println(f\"{n}\") }\n",
            "drop 41\n1\n",
        ),
        (
            "two-droppers-one-wildcard",
            "fn main() { let p = (R { id: 41 }, R { id: 42 });\n\
             \x20           let (_, b) = p; println(f\"{b.id}\") }\n",
            "drop 41\n42\ndrop 42\n",
        ),
        (
            "both-wildcards",
            "fn main() { let p = (R { id: 41 }, R { id: 42 });\n\
             \x20           let (_, _) = p; println(\"x\") }\n",
            "drop 41\ndrop 42\nx\n",
        ),
        // CONTROL — by-value param sources, already correct at one body. The
        // ORDER is this backend's own (B-2026-08-28-19).
        (
            "param-tuple-control",
            "fn take(p: (R, i64)) -> i64 { let (_, n) = p; n }\n\
             fn main() { println(f\"{take((R { id: 41 }, 1))}\") }\n",
            "drop 41\n1\n",
        ),
        (
            "param-struct-control",
            "struct W { r: R, n: i64 }\n\
             fn take(w: W) -> i64 { let W { r: _, n } = w; n }\n\
             fn main() { println(f\"{take(W { r: R { id: 41 }, n: 1 })}\") }\n",
            "drop 41\n1\n",
        ),
        (
            "match-arm-control",
            "fn main() { let p = (R { id: 41 }, 1);\n\
             \x20           match p { (_, n) => { println(f\"{n}\") } } }\n",
            "1\ndrop 41\n",
        ),
        (
            "wildcard-on-non-dropper-control",
            "fn main() { let p = (R { id: 41 }, 1); let (r, _) = p; println(f\"{r.id}\") }\n",
            "41\ndrop 41\n",
        ),
        (
            "whole-pattern-wildcard-control",
            "fn main() { let _ = R { id: 41 }; println(\"x\") }\n",
            "drop 41\nx\n",
        ),
        (
            "no-destructure-control",
            "fn main() { let p = (R { id: 41 }, 1); println(f\"{p.1}\") }\n",
            "1\ndrop 41\n",
        ),
    ] {
        assert_eq!(run(&format!("{DROPPER}{body}")), want, "{label}");
    }
}

/// B-2026-08-28-46 / -47, interpreter leg — the twin of
/// `e2e_own_drop_enum_member_runs_its_body_when_never_destructured`.
///
/// `-46` is the row this file is the RED half of: a struct field holding an
/// own-`Drop` enum ran the body on BOTH compiled backends and nothing here, so
/// it was a live run-vs-build divergence in the interpreter-silent direction —
/// the opposite direction from every neighbouring row in the family.
///
/// `-47` (the tuple element) was silent on all three backends instead, so its
/// rows are RED everywhere.
///
/// Every row is asserted identically to the codegen twin, because "the same
/// enum runs the same bodies wherever it is stored" is the whole property.
#[test]
fn test_own_drop_enum_member_runs_its_body_when_never_destructured() {
    const H: &str = "enum E { A(R), B }\n\
         impl Drop for E { fn drop(mut ref self) { println(\"drop E\") } }\n\
         struct R { id: i64 }\n\
         impl Drop for R { fn drop(mut ref self) { println(f\"drop R{self.id}\") } }\n";
    for (label, body, want) in [
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
        (
            "tuple-inside-struct-field",
            "struct W { p: (E, i64) }\n\
             fn main() { let w = W { p: (E.B, 1) }; println(\"hi\"); }\n",
            "drop E\nhi\n",
        ),
        // CONTROL — already correct here before the fix. It is also the row
        // that caught the first cut: making the field walk fire without also
        // making an enum field count as Drop-relevant CONTENT left `w`
        // unmarked by the destructure's move-out bookkeeping, and this row
        // printed `drop E` TWICE.
        (
            "destructured-control",
            "fn main() { let p = (E.B, 1); let (_, n) = p; println(f\"{n}\"); }\n",
            "drop E\n1\n",
        ),
        (
            "struct-destructured-control",
            "struct W { e: E, n: i64 }\n\
             fn main() { let w = W { e: E.B, n: 1 }; let W { e: _, n } = w;\n\
             \x20            println(f\"{n}\"); }\n",
            "drop E\n1\n",
        ),
        (
            "moved-on",
            "fn main() { let p = (E.B, 1); let q = p; println(f\"{q.1}\"); }\n",
            "1\ndrop E\n",
        ),
    ] {
        assert_eq!(run(&format!("{H}{body}")), want, "{label}");
    }
    // B-2026-08-28-55 — the `Vec` element, which this file's own guard pinned
    // as SILENCE when -46/-47 landed: the one element loop serves both tuple
    // and Vec bindings, and only the tuple walker had an enum leg in codegen,
    // so firing for a Vec would have manufactured a divergence. Widening the
    // Vec walker's SELECTOR closed that, and the interpreter's shape gate came
    // off in the same commit. Three spellings, because the element type is
    // derived differently in each and only the inferred one had a working
    // struct precedent to copy.
    for (label, prog) in [
        (
            "vec-inferred",
            format!(
                "{H}fn main() {{ let mut v = Vec.new(); v.push(E.B); println(f\"{{v.len()}}\"); }}\n"
            ),
        ),
        (
            "vec-annotated",
            format!(
                "{H}fn main() {{ let mut v: Vec[E] = Vec.new(); v.push(E.B);\n\
                 \x20            println(f\"{{v.len()}}\"); }}\n"
            ),
        ),
        (
            "vec-literal",
            format!("{H}fn main() {{ let v = [E.B]; println(f\"{{v.len()}}\"); }}\n"),
        ),
    ] {
        assert_eq!(run(&prog), "1\ndrop E\n", "{label}");
    }
    // A payload variant in a Vec runs BOTH bodies, like every other position.
    // `owns_body` alone would stop at `drop E`.
    assert_eq!(
        run(&format!(
            "{H}fn main() {{ let mut v = Vec.new(); v.push(E.A(R {{ id: 7 }}));\n\
             \x20            println(f\"{{v.len()}}\"); }}\n"
        )),
        "1\ndrop E\ndrop R7\n",
        "vec-payload"
    );
    // B-2026-08-28-54 — the payload-only enum, pinned AS SILENCE by this
    // assertion when -46/-47 landed and closed once codegen's
    // `type_runs_user_drop` learned to look inside variant payloads. All three
    // positions, since the predicate widening covered them together.
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
            run(&format!(
                "enum E2 {{ A(R), B }}\n\
                 struct R {{ id: i64 }}\n\
                 impl Drop for R {{ fn drop(mut ref self) {{ println(f\"drop R{{self.id}}\") }} }}\n\
                 {body}"
            )),
            "1\ndrop R5\n",
            "{label}"
        );
    }
    // BOUNDARY — the payloadless variant of the same enum runs nothing.
    assert_eq!(
        run("enum E2 { A(R), B }\n\
             struct R { id: i64 }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"drop R{self.id}\") } }\n\
             fn main() { let p = (E2.B, 1); println(f\"{p.1}\"); }\n"),
        "1\n",
        "payload-only-unit-variant"
    );
}

/// B-2026-08-28-63, interpreter leg — the twin of
/// `codegen::e2e_consuming_arm_runs_the_bound_enum_payloads_drop_body`.
///
/// The interpreter's gate was the same struct-only bind, in three copies —
/// the `match` arm stash in `pattern_match.rs` and the `if let` / `while let`
/// stashes in `eval_expr.rs` — so all three bound an enum payload without
/// giving it a Drop slot. They have to move together, or the same program
/// prints differently depending on which spelling consumed the payload, which
/// is why `if-let` is a row here beside `match`.
#[test]
fn consuming_arm_runs_the_bound_enum_payloads_drop_body() {
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
        (
            "payload-only-enum",
            "fn main() { let o: Option[H] = Some(H.A(R { id: 4 }));\n\
             \x20            match o { Some(h) => { println(\"got\") }\n\
             \x20                      None => { println(\"none\") } } }\n",
            "got\ndrop R4\n",
        ),
        (
            "heap-payload",
            "fn main() { let o: Option[G] = Some(G.A(f\"z{9}\"));\n\
             \x20            match o { Some(g) => { println(\"got\") }\n\
             \x20                      None => { println(\"none\") } } }\n",
            "got\ndrop G\n",
        ),
        (
            "struct-control",
            "fn main() { let o: Option[R] = Some(R { id: 2 });\n\
             \x20            match o { Some(r) => { println(\"got\") }\n\
             \x20                      None => { println(\"none\") } } }\n",
            "got\ndrop R2\n",
        ),
        (
            "escaping-enum-tail",
            "fn main() { let o: Option[E] = Some(E.B);\n\
             \x20            let k = match o { Some(e) => { e } None => { E.B } };\n\
             \x20            println(\"kept\"); }\n",
            "drop E\nkept\n",
        ),
        (
            "moved-to-sink",
            "fn main() { let o: Option[E] = Some(E.B);\n\
             \x20            match o { Some(e) => { sink(e) }\n\
             \x20                      None => { println(\"none\") } } }\n",
            "sank\ndrop E\n",
        ),
        (
            "no-bind-wildcard-arm",
            "fn main() { let o: Option[E] = Some(E.A(R { id: 1 }));\n\
             \x20            match o { Some(_) => { println(\"some\") }\n\
             \x20                      None => { println(\"none\") } } }\n",
            "some\ndrop E\ndrop R1\n",
        ),
        (
            "no-drop-enum-payload",
            "fn main() { let o: Option[J] = Some(J.A(2));\n\
             \x20            match o { Some(x) => { println(\"got\") }\n\
             \x20                      None => { println(\"none\") } } }\n",
            "got\n",
        ),
    ] {
        assert_eq!(run(&format!("{H}{body}")), want, "{label}");
    }
}

/// B-2026-08-28-67 — a `match` / `if let` arm that only READS THROUGH the
/// payload it binds runs the two `Drop` bodies in the COMPILED order: the
/// enum's own first, its payload's second.
///
/// Before the fix the interpreter printed them the other way round (`dR5 dE`
/// against the compiled `dE dR5`) — same count, so every count-based fixture in
/// the -46/-47/-54/-55/-57/-58/-63 family stayed green while the two backends
/// disagreed on sequence. Which order is right is settled by design.md § Part 8
/// ("drop each field in order" is what the compiler does AFTER the user's drop
/// body returns), and independently by the interpreter contradicting itself: a
/// direct `let x = E.A(R { .. });` already printed `dE` then `dR5` here.
///
/// The rows split into two groups that must NOT move together, which is the
/// whole point of the fixture:
///
///   * READ-THROUGH — the binding is only projected (`r.id`), used as a method
///     receiver (`r.get()` / `r.eat()`, either receiver mode), or not mentioned
///     at all. Nothing was moved out, so the scrutinee keeps its payload walk
///     and runs both bodies at its own death. These are the rows the fix moved.
///   * MATERIALIZED — the binding appears somewhere as a bare value (a call
///     argument, a `let` right-hand side, an aggregate element). The arm really
///     does own it, and these rows were ALREADY correct on all four surfaces;
///     they are here so a future edit cannot "fix" the first group by breaking
///     the second.
///
/// `interpolation-hole` is load-bearing in a way its neighbours are not: the
/// only mention of `r` is inside an `f"…"` hole. `consume_class`'s walker does
/// not descend into `InterpolatedStringLit`, so reusing it here would have
/// scored the row read-through and produced `h5 dE dR5` — the wrong half of the
/// split. It is why `binding_use`'s walk is exhaustive over `ExprKind`.
#[test]
fn readthrough_arm_leaves_the_payload_with_its_enum() {
    const H: &str = "struct R { id: i64 }\n\
         impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\") } }\n\
         impl R { fn get(ref self) -> i64 { self.id }\n\
         \x20         fn eat(self) -> i64 { self.id } }\n\
         enum E { A(R), B }\n\
         impl Drop for E { fn drop(mut ref self) { println(\"dE\") } }\n\
         enum H { A(R), B }\n\
         struct W { r: R }\n\
         fn keep(r: R) -> i64 { r.id }\n\
         fn mk() -> E { E.A(R { id: 7 }) }\n";
    for (label, body, want) in [
        // ── read-through: the scrutinee keeps the payload ──────────────────
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
            // An OWNED-`self` receiver is still a read-through here: every
            // compiled backend leaves the payload with the scrutinee for it,
            // measured, so the receiver's mode is not the question.
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
            // The spelling the divergence was first seen through: an inner
            // `match` over a payload that is itself an arm binding.
            "nested-in-option",
            "let o: Option[E] = Some(E.A(R { id: 3 }));\n\
             \x20 match o { Some(x) => { match x { E.A(r) => { println(\"inner\") }\n\
             \x20                                  E.B => {} } } None => {} }\n",
            "inner\ndE\ndR3\npost\n",
        ),
        // ── materialized: the arm owns the payload (unchanged controls) ────
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
        // ── boundary: a FRESH-TEMP scrutinee keeps the arm stash ───────────
        (
            // The gate stands the arm stash down only where the DISARM could
            // have retracted the scrutinee's walk — an identifier or `self`
            // place. `match mk() { .. }` has a stash but nothing named to
            // retract, so standing it down hands the payload to nobody: an
            // earlier cut of this fix printed `v7 dE`, with `dR7` gone
            // entirely. This row is that regression, pinned.
            //
            // Interpreter-only on purpose. The compiled side puts the
            // scrutinee's own `dE` after the enclosing statement here
            // (`v7 dR7 post dE`), which is a SEPARATE pre-existing divergence
            // filed as its own row — asserting it in the compiled twin would
            // pin a bug as if it were the contract.
            "fresh-temp-scrutinee",
            "match mk() { E.A(r) => { println(f\"v{r.id}\") } E.B => {} }\n",
            "v7\ndR7\ndE\npost\n",
        ),
        // ── boundary: no own `Drop`, so nothing to order against ───────────
        (
            // `H` has a Drop-bearing payload but no `Drop` of its own. There is
            // no destructor whose fields must all be present, so the gate stays
            // out and this row is untouched by the fix — it agreed before and
            // agrees after.
            "no-own-drop-enum",
            "let e = H.A(R { id: 5 });\n\
             \x20 match e { H.A(r) => { println(f\"v{r.id}\") } H.B => {} }\n",
            "v5\ndR5\npost\n",
        ),
    ] {
        let src = format!("{H}fn main() {{ {body} println(\"post\") }}\n");
        assert_eq!(run(&src), want, "{label}");
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
/// The parity twin of
/// `codegen::e2e_if_let_miss_drops_the_scrutinee_temp_before_the_else_arm`.
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
fn if_let_miss_drops_the_scrutinee_temp_before_the_else_arm() {
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
        let src = format!("{H}fn main() {{ {body} println(\"post\") }}\n");
        assert_eq!(run(&src), want, "{label}");
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
        let src = format!("{H}fn main() {{ {body} }}\n");
        assert_eq!(run(&src), want, "{label}");
    }
}

/// B-2026-08-30-14 — a `return` / `break` / `continue` / `?` inside a match
/// arm was SILENTLY DISCARDED by the interpreter when the scrutinee was a
/// FRESH TEMP whose type carries its own `Drop` body.
///
/// This is a CONTROL-FLOW defect, not the drop-ORDER kind the rest of this
/// family is made of: the statement after the `return` ran, the loop never
/// broke, and `return n` handed back UNIT. `karac run` is the default
/// executor, so it is what a user sees first.
///
/// ONE CAUSE, and it is not in `eval_match`. The fresh-temp scrutinee's own
/// body is run by `run_user_drop_body_only` AFTER the arm, while the arm's
/// signal is still sitting in `pending_cf`. The block evaluator drains
/// `pending_cf` into its own `Result` (`eval_stmt.rs`'s
/// `Ok(_) => self.pending_cf.take()`) and that call discards the `Result` —
/// so the body ATE the signal. The body was lost too, in the same motion:
/// the console builtins decline to emit while `pending_cf` is set, so it ran
/// into a silenced world and `dE` never appeared either. The fix brackets the
/// body with a save/restore of `pending_cf`, which is the invariant every
/// OTHER drop-body site already satisfies by construction (`run_cleanup` is
/// reached only after the signal has been taken out).
///
/// THE ROW'S GATE WAS TOO NARROW IN ONE DIRECTION AND THE SPELLINGS TOO FEW
/// IN ANOTHER, both found by sweeping rather than by reading:
///   * `struct-scrutinee` — the row's gate says the scrutinee's ENUM must
///     carry the `Drop`. A fresh-temp STRUCT scrutinee with its own `Drop`
///     lost the `return` identically; the trigger is the scrutinee having a
///     body of its own, not its being an enum.
///   * `question-mark` — `?` desugars to an early return and was discarded
///     the same way, which turned an `Err` propagation into `Ok(0)`. That is
///     a WRONG VALUE out of the idiomatic error-propagation operator, and the
///     row lists only `return` / `break` / `continue`.
///
/// NINE OF THE TWELVE ROWS ARE RED WITHOUT THE FIX, measured shape by shape
/// on a stashed-and-rebuilt tree rather than assumed: the interpreter printed
/// `v7 AFTER done` where `v7 dE done` is due, and `labeled-break` ran its 3x3
/// loop nest to completion — nine `v7`s. The test as a whole fails pre-fix on
/// its first row.
///
/// THE THREE THAT WERE ALREADY GREEN are kept deliberately, and only two of
/// them are boundaries in the designed sense. `named-scrutinee-control` and
/// `no-own-drop-control` are those two — a named scrutinee is owned elsewhere
/// and never reached the interrupted path, and a type with no `Drop` has no
/// body to run into the signal. `while-let` is the SURPRISE: the `match` and
/// `if let` spellings of this program both lost the `return` and the
/// `while let` spelling did not, so the three sibling constructs did NOT
/// behave alike here. It is pinned so that stays true — a later change that
/// unified these paths could regress the one that was already correct.
///
/// `struct-scrutinee` is INTERPRETER-ONLY on purpose. Both compiled backends
/// run no body at all for a fresh-temp struct scrutinee (B-2026-08-30-15), so
/// pinning this shape on all four surfaces would write that separate bug into
/// the contract. What is asserted here is only the half this fix owns — the
/// `return` happens — and the compiled twin deliberately omits the row.
///
/// `named-scrutinee-control` and `no-own-drop-control` are the unchanged
/// boundaries, both measured identical before and after: a named scrutinee is
/// owned elsewhere and was never routed through the interrupted path, and a
/// type with no `Drop` of its own has no body to run into the signal.
#[test]
fn diverging_arm_over_a_freshtemp_scrutinee_keeps_its_control_flow() {
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
        assert_eq!(run(&format!("{H}{prog}")), want, "{label}");
    }
}

/// B-2026-08-30-14, the STRUCT half — interpreter-only, and the doc comment on
/// `diverging_arm_over_a_freshtemp_scrutinee_keeps_its_control_flow` says why:
/// both compiled backends run no body at all here (B-2026-08-30-15), so the
/// four-surface twin would have to encode that separate defect to pass.
///
/// Kept as its own test rather than a row in the table above so the split is
/// visible at the point where it matters, and so closing -15 is a one-line
/// move of this shape INTO the shared table rather than an archaeology
/// exercise.
#[test]
fn diverging_arm_over_a_freshtemp_struct_scrutinee_keeps_its_control_flow() {
    let src = "struct S { id: i64 }\n\
         impl Drop for S { fn drop(mut ref self) { println(f\"dS{self.id}\") } }\n\
         fn mk(n: i64) -> S { return S { id: n } }\n\
         fn f() { match mk(7) { S { id } => { println(f\"v{id}\"); return } }\n\
         \x20 println(\"AFTER\") }\n\
         fn main() { f(); println(\"done\") }\n";
    // Pre-fix this printed `v7\nAFTER\ndone\n` — the `return` was discarded and
    // the body never ran, exactly as for an enum scrutinee.
    assert_eq!(run(src), "v7\ndS7\ndone\n");
}

/// B-2026-08-29-29, interpreter leg — the TUPLE-ELEMENT half, which is where
/// the interpreter had a defect of its own rather than merely the compiled
/// side's mirror.
///
/// `match t.0 { E.A(r) => println(r.id) }` printed `v8 dR8`: the enum's OWN
/// body, `dE`, was gone entirely. The tuple disarm
/// (`moved_out_tuple_elem_bodies`) retracts the whole ELEMENT's walk, not just
/// its payload's body, and it asked `pattern_consumes_user_drop_payload`
/// directly instead of the shared `match_disarms_payload_walk` — so a
/// read-through arm, which moves nothing out and can never take over the
/// enum's own body, still silenced it. That is exactly the drift
/// B-2026-08-28-67 routed the identifier arm through one function to prevent,
/// reached by the one arm it did not route.
///
/// The struct-FIELD rows were already correct here (the interpreter declines
/// both the disarm and the stash for a `FieldAccess` place) and are pinned
/// anyway: the compiled twin
/// (`codegen::e2e_readthrough_arm_over_a_projection_leaves_the_payload_with_its_owner`)
/// asserts the same strings, so a later edit cannot reconcile the two by moving
/// the interpreter instead.
#[test]
fn readthrough_arm_over_a_projection_leaves_the_payload_with_its_owner() {
    const H: &str = "struct R { id: i64 }\n\
         impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\") } }\n\
         enum E { A(R), B }\n\
         impl Drop for E { fn drop(mut ref self) { println(\"dE\") } }\n\
         enum H { A(R), B }\n\
         struct S { e: E }\n\
         struct Sh { h: H }\n\
         struct W { s: S }\n";
    for (label, body, want) in [
        // ── tuple element: RED rows, the interpreter's own defect ──────────
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
            // The `if let` spelling reaches a DIFFERENT site (`eval_expr`'s
            // hand-rolled place test, now routed through the same
            // `place_walk_is_retractable`), and it was wrong in the other
            // direction: `v8 dR8 dE dR8`, the stash firing beside the tuple's
            // element walk for two bodies where one is due.
            "if-let-tuple-element",
            "let t = (E.A(R { id: 8 }), 1i64);\n\
             \x20 if let E.A(r) = t.0 { println(f\"v{r.id}\") }\n",
            "v8\ndE\ndR8\npost\n",
        ),
        (
            "no-own-drop-tuple",
            "let t = (H.A(R { id: 8 }), 1i64);\n\
             \x20 match t.0 { H.A(r) => { println(f\"v{r.id}\") } H.B => {} }\n",
            "v8\ndR8\npost\n",
        ),
        // ── struct field: PARITY PINS, green before and after ──────────────
        (
            "struct-field",
            "let s = S { e: E.A(R { id: 8 }) };\n\
             \x20 match s.e { E.A(r) => { println(f\"v{r.id}\") } E.B => {} }\n",
            "v8\ndE\ndR8\npost\n",
        ),
        (
            "nested-field-chain",
            "let w = W { s: S { e: E.A(R { id: 8 }) } };\n\
             \x20 match w.s.e { E.A(r) => { println(f\"v{r.id}\") } E.B => {} }\n",
            "v8\ndE\ndR8\npost\n",
        ),
        (
            "if-let-struct-field",
            "let s = S { e: E.A(R { id: 8 }) };\n\
             \x20 if let E.A(r) = s.e { println(f\"v{r.id}\") }\n",
            "v8\ndE\ndR8\npost\n",
        ),
        (
            "no-own-drop-field",
            "let s = Sh { h: H.A(R { id: 8 }) };\n\
             \x20 match s.h { H.A(r) => { println(f\"v{r.id}\") } H.B => {} }\n",
            "v8\ndR8\npost\n",
        ),
    ] {
        let src = format!("{H}fn main() {{ {body} println(\"post\") }}\n");
        assert_eq!(run(&src), want, "{label}");
    }
}

/// B-2026-08-29-33, interpreter leg — a MATERIALIZING arm over a
/// PROJECTION-PLACE enum scrutinee owns the payload exactly once here too.
///
/// The interpreter had two defects of its own, in opposite directions. A
/// STRUCT-FIELD place had no disarm at all, so the arm's binding ran the body
/// AND the owner's field walk ran it again (`m8 dR8 dE dR8`). A TUPLE element
/// had one, but it retracted the whole element, so the enum's own body went
/// with it (`m8 dR8`, `dE` gone) — the same whole-vs-payload granularity
/// problem codegen had, reached from the other side.
///
/// `optres-field-local-match` is the carve-out, and it is load-bearing: an
/// `Option`/`Result` field is walked through `run_discarded_value_user_drops`
/// BEFORE the user-enum arm the payload mask guards, so routing it through the
/// finer mask half-masks it and the body fires twice. Those places are
/// deliberately left on the old path, and the arm stash is locked to whether
/// the disarm actually recorded — stash without retract is the other half of
/// the same double.
#[test]
fn materializing_arm_over_a_projection_owns_the_payload_once() {
    const H: &str = "struct R { id: i64 }\n\
         impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\") } }\n\
         enum E { A(R), B }\n\
         impl Drop for E { fn drop(mut ref self) { println(\"dE\") } }\n\
         enum N { A(R), B }\n\
         struct S { e: E }\n\
         struct Sn { n: N }\n\
         struct Sr { r: Result[R, i64] }\n\
         fn keep(x: R) -> R { x }\n";
    for (label, body, want) in [
        (
            "field-let-rebind",
            "let s = S { e: E.A(R { id: 8 }) };\n\
             \x20 match s.e { E.A(r) => { let m = r; println(f\"m{m.id}\") } E.B => {} }\n",
            "m8\ndR8\ndE\npost\n",
        ),
        (
            // ONE body since B-2026-08-29-15, matching `field-let-rebind`
            // above and the codegen twin. The former `dR8` twice rested on an
            // entry-copy model that measurement refutes; see the twin's note.
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
            "no-own-drop-field",
            "let s = Sn { n: N.A(R { id: 8 }) };\n\
             \x20 match s.n { N.A(r) => { let m = r; println(f\"m{m.id}\") } N.B => {} }\n",
            "m8\ndR8\npost\n",
        ),
        (
            // THE CARVE-OUT — see the doc comment. Pinned here beside the rows
            // it must not be folded into.
            "optres-field-local-match",
            "let s = Sr { r: Result.Ok(R { id: 8 }) };\n\
             \x20 let v = match s.r { Result.Ok(x) => x.id, Result.Err(e) => e };\n\
             \x20 println(f\"v{v}\");\n",
            "dR8\nv8\npost\n",
        ),
        (
            "ident-oracle-let-rebind",
            "let e = E.A(R { id: 8 });\n\
             \x20 match e { E.A(r) => { let m = r; println(f\"m{m.id}\") } E.B => {} }\n",
            "m8\ndR8\ndE\npost\n",
        ),
    ] {
        let src = format!("{H}fn main() {{ {body} println(\"post\") }}\n");
        assert_eq!(run(&src), want, "{label}");
    }
}

/// Interpreter mirror of `codegen.rs`'s
/// `e2e_returned_option_local_borrowed_by_a_readonly_arm_is_freed_once`.
///
/// The interpreter was never wrong here — B-2026-08-30-8 is a compiled-only
/// double free, and `--interp` printed the right answer on every shape below
/// while the JIT and both AOT legs aborted — so these are PARITY PINS, not a
/// reproduction. They fix the strings the compiled backends must produce, so a
/// later interpreter change cannot drift away from the backend this fix just
/// corrected.
#[test]
fn returned_option_local_borrowed_by_a_readonly_arm_is_freed_once() {
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
        (
            "readonly-arm-returned",
            "match r_plain() { Some(s) => println(f\"[{s}]\"), None => println(\"none\"), }\n",
            "saw a\n[a]\npost\n",
        ),
        (
            "arm-then-reassign",
            "match r_reassign() { Some(s) => println(f\"[{s}]\"), None => println(\"none\"), }\n",
            "saw a\n[z]\npost\n",
        ),
        (
            "reassign-after-match",
            "match r_after() { Some(s) => println(f\"[{s}]\"), None => println(\"none\"), }\n",
            "saw a\n[z]\npost\n",
        ),
        (
            "arm-escapes-payload",
            "match r_escapes() { Some(s) => println(f\"[{s}]\"), None => println(\"none\"), }\n",
            "[a!]\npost\n",
        ),
        (
            "explicit-return-in-if",
            "match r_ret_if(true) { Some(s) => println(f\"[{s}]\"), None => println(\"none\"), }\n",
            "saw a\n[a]\npost\n",
        ),
        (
            "tail-when-return-not-taken",
            "match r_ret_if(false) { Some(s) => println(f\"[{s}]\"), None => println(\"none\"), }\n",
            "saw a\n[z]\npost\n",
        ),
        (
            "result-twin",
            "match r_result() { Ok(s) => println(f\"[{s}]\"), Err(e) => println(f\"e{e}\"), }\n",
            "saw a\n[a]\npost\n",
        ),
        (
            "vec-payload",
            "match r_vec() { Some(v) => println(f\"[{v[0]}]\"), None => println(\"none\"), }\n",
            "len 1\n[7]\npost\n",
        ),
        (
            "selfhost-doc-comment-shape",
            "match r_loop(3i64) { Some(s) => println(f\"[{s}]\"), None => println(\"none\"), }\n",
            "[L0-L1-L2]\npost\n",
        ),
        (
            "caller-discards",
            "let _ = r_reassign();\n",
            "saw a\npost\n",
        ),
        (
            "wildcard-arm-control",
            "match r_wild() { Some(s) => println(f\"[{s}]\"), None => println(\"none\"), }\n",
            "[z]\npost\n",
        ),
        (
            "consumed-locally-control",
            "println(f\"{r_local()}\");\n",
            "saw a\n[z]\n1\npost\n",
        ),
        (
            "map-chain-return-control",
            "println(f\"{r_chain(true)}\"); println(f\"{r_chain(false)}\");\n",
            "saw hi\n2\nsaw hi\n2\npost\n",
        ),
    ] {
        let src = format!("{H}fn main() {{ {body} println(\"post\") }}\n");
        assert_eq!(run(&src), want, "{label}");
    }
}

/// B-2026-08-29-48, interpreter leg — the twin of
/// `codegen::e2e_arm_assigned_payload_that_is_returned_runs_one_drop_body`,
/// asserting the same strings so a later one-sided edit cannot re-negotiate
/// them.
///
/// An arm that assigns the param's payload to an outer binding the callee then
/// returns hands the value out exactly as `return r` does, so one `Drop` body
/// is due at the caller's result binding. Both surfaces ran it twice, agreeing,
/// which is why only an absolute expectation catches it.
///
/// The `control-assigned-but-not-returned` row of the compiled fixture has no
/// peer here on purpose: the interpreter runs an extra body on that shape, a
/// pre-existing divergence outside this fix and filed on its own.
#[test]
fn arm_assigned_payload_that_is_returned_runs_one_drop_body() {
    const H: &str = "struct R { id: i64, tag: String }\n\
         impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\") } }\n\
         enum E { A(R), B }\n\
         impl Drop for E { fn drop(mut ref self) { println(\"dE\") } }\n\
         struct Holder { inner: R }\n\
         struct T { n: i64 }\n\
         impl T {\n\
         \x20   fn assign_enum(ref self, b: E) -> R {\n\
         \x20       let mut out: R = R { id: 0, tag: f\"t0\" };\n\
         \x20       match b { E.A(r) => { out = r; } E.B => { } }\n\
         \x20       let loc = R { id: 9, tag: f\"t9\" }; println(f\"mid{loc.id}\"); return out; }\n\
         \x20   fn assign_opt(ref self, b: Option[R]) -> R {\n\
         \x20       let mut out: R = R { id: 0, tag: f\"t0\" };\n\
         \x20       match b { Some(r) => { out = r; } None => { } }\n\
         \x20       let loc = R { id: 9, tag: f\"t9\" }; println(f\"mid{loc.id}\"); return out; }\n\
         \x20   fn direct_enum(ref self, b: E) -> R {\n\
         \x20       match b { E.A(r) => { return r } E.B => { return R { id: 0, tag: f\"t0\" } } } }\n\
         \x20   fn field_enum(ref self, b: E) -> Holder {\n\
         \x20       let mut h: Holder = Holder { inner: R { id: 0, tag: f\"t0\" } };\n\
         \x20       match b { E.A(r) => { h.inner = r; } E.B => { } }\n\
         \x20       let loc = R { id: 9, tag: f\"t9\" }; println(f\"mid{loc.id}\"); return h; }\n\
         \x20   fn iflet_opt(ref self, b: Option[R]) -> R {\n\
         \x20       let mut out: R = R { id: 0, tag: f\"t0\" };\n\
         \x20       if let Some(r) = b { out = r; }\n\
         \x20       let loc = R { id: 9, tag: f\"t9\" }; println(f\"mid{loc.id}\"); return out; } }\n\
         fn f_assign_enum(b: E) -> R {\n\
         \x20   let mut out: R = R { id: 0, tag: f\"t0\" };\n\
         \x20   match b { E.A(r) => { out = r; } E.B => { } }\n\
         \x20   let loc = R { id: 9, tag: f\"t9\" }; println(f\"mid{loc.id}\"); return out; }\n\
         fn f_assign_opt(b: Option[R]) -> R {\n\
         \x20   let mut out: R = R { id: 0, tag: f\"t0\" };\n\
         \x20   match b { Some(r) => { out = r; } None => { } }\n\
         \x20   let loc = R { id: 9, tag: f\"t9\" }; println(f\"mid{loc.id}\"); return out; }\n\
         fn f_direct_enum(b: E) -> R {\n\
         \x20   match b { E.A(r) => { return r } E.B => { return R { id: 0, tag: f\"t0\" } } } }\n";
    for (label, body, want) in [
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
        (
            "free-enum-assign-shadowed-name",
            "let b: E = E.A(R { id: 8, tag: f\"t8\" });\n\
             \x20 let v: R = f_assign_enum(b); println(f\"v{v.id}\");\n",
            "dR0\nmid9\ndR9\ndE\nv8\ndR8\npost\n",
        ),
        // Two calls in one frame: the second must not be silenced by whatever
        // the first recorded.
        (
            "two-calls-second-must-still-fire",
            "let c1: E = E.A(R { id: 1, tag: f\"t1\" }); let c2: E = E.A(R { id: 2, tag: f\"t2\" });\n\
             \x20 let a: R = f_assign_enum(c1); println(f\"a{a.id}\");\n\
             \x20 let d: R = f_assign_enum(c2); println(f\"b{d.id}\");\n",
            "dR0\nmid9\ndR9\ndE\na1\ndR1\ndR0\nmid9\ndR9\ndE\nb2\ndR2\npost\n",
        ),
    ] {
        let src = format!("{H}fn main() {{ {body} println(\"post\") }}\n");
        assert_eq!(run(&src), want, "{label}");
    }
}

/// B-2026-08-28-40, interpreter leg — an own-`impl Drop` enum discarded by a
/// wildcard destructure leaf runs its LIVE PAYLOAD's body too.
///
/// This is the SOUNDNESS half of the row rather than a parity half: pre-fix
/// every backend printed the enum's own body alone, so the two agreed on a
/// number that neither the A/B gate nor `karac check` could object to. The
/// bound spelling of the same value printed two, and that mismatch is the only
/// thing that showed it.
///
/// The walk is site-local, inside `run_wildcard_destructure_leaf_user_drops`
/// and NOT inside the 31-caller `run_discarded_value_user_drops` — the same
/// discipline B-2026-08-28-39 arrived at by measurement one site over, and for
/// the same reason: that walker's other callers include the CALL-source discard,
/// which compiled runs at one body. `call-source-control` here is that guard.
#[test]
fn test_wildcard_discard_of_own_drop_enum_runs_its_payload_body() {
    const H: &str = "enum E { A(R), B }\n\
         impl Drop for E { fn drop(mut ref self) { println(\"drop E\") } }\n\
         struct R { id: i64 }\n\
         impl Drop for R { fn drop(mut ref self) { println(f\"drop R{self.id}\") } }\n";
    for (label, body, want) in [
        (
            "tuple-leaf",
            "fn main() { let p = (E.A(R { id: 41 }), 1); let (_, n) = p; println(f\"{n}\"); }\n",
            "drop E\ndrop R41\n1\n",
        ),
        (
            "struct-field",
            "struct W { e: E, n: i64 }\n\
             fn main() { let w = W { e: E.A(R { id: 41 }), n: 1 };\n\
             \x20            let W { e: _, n } = w; println(f\"{n}\"); }\n",
            "drop E\ndrop R41\n1\n",
        ),
        // The BOUND spelling of the same value — what the two rows above are
        // measured against, and what they failed to match pre-fix.
        (
            "bound-control",
            "fn main() { let e = E.A(R { id: 41 }); println(\"mid\"); }\n",
            "drop E\ndrop R41\nmid\n",
        ),
        // BOUNDARY — a payloadless variant stays at one body.
        (
            "no-payload-variant",
            "fn main() { let p = (E.B, 1); let (_, n) = p; println(f\"{n}\"); }\n",
            "drop E\n1\n",
        ),
        // GUARD — a CALL source, which compiled runs at one body. It goes
        // through the shared walker; this row fails the moment the payload walk
        // is moved there.
        (
            "call-source-control",
            "fn mk() -> E { E.A(R { id: 41 }) }\n\
             fn main() { let _ = mk(); println(\"end\") }\n",
            "drop E\ndrop R41\nend\n",
        ),
    ] {
        assert_eq!(run(&format!("{H}{body}")), want, "{label}");
    }
    // The walk follows the LIVE variant: both variants carry an `R` and only
    // the constructed one prints.
    assert_eq!(
        run("enum E { A(R), B(R) }\n\
             impl Drop for E { fn drop(mut ref self) { println(\"drop E\") } }\n\
             struct R { id: i64 }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"drop R{self.id}\") } }\n\
             fn main() { let p = (E.B(R { id: 7 }), 1); let (_, n) = p; println(f\"{n}\"); }\n"),
        "drop E\ndrop R7\n1\n",
        "second-variant"
    );
}

/// B-2026-08-01-6 — a `match self { Full(r) => .. }` under a `ref self`
/// method binds a BORROWED view: the arm stash must stay silent (the
/// receiver's real owner fires the body). Pre-fix the interpreter treated
/// the `self` scrutinee as consuming and fired the payload body inside the
/// method — `karac run` printed `drop 5 e5` where `karac build` printed
/// nothing (fresh receiver, shape a) and double-fired the named-binding
/// shape (b). `scrutinee_expr_is_consuming` now reads the receiver mode
/// from `self_param_stack`: `self` is consuming only under an OWNED
/// receiver. Twin of `tests/codegen.rs`'s
/// `e2e_ref_self_match_borrowed_payload_silent`, same source and expected
/// string (the codegen side was already correct — its twin is the parity
/// pin). The fresh-receiver shape (a) was silent on every surface when this
/// landed — the borrowed temp's body had no owner at all; since
/// B-2026-09-06-38 the caller's receiver-temp registrar runs it once at the
/// statement's end (`drop 5 e5` before `t=1`), still outside the arm.
#[test]
fn test_ref_self_match_borrowed_payload_silent() {
    assert_eq!(
        run("struct Res { id: i64, name: String }\n\
             impl Drop for Res {\n\
                 fn drop(mut ref self) {\n\
                     println(f\"drop {self.id} {self.name}\")\n\
                 }\n\
             }\n\
             enum Box2 { Full(Res), Empty }\n\
             impl Box2 {\n\
                 fn tag(ref self) -> i64 {\n\
                     match self {\n\
                         Box2.Full(r) => { return 1; }\n\
                         Box2.Empty => { return 0; }\n\
                     }\n\
                 }\n\
             }\n\
             fn mk_e(n: i64) -> Box2 {\n\
                 return Box2.Full(Res { id: n, name: f\"e{n}\" });\n\
             }\n\
             fn main() {\n\
                 println(\"a: ref-self match on fresh receiver\");\n\
                 let t = mk_e(5).tag();\n\
                 println(f\"t={t}\");\n\
                 println(\"b: ref-self match on named binding\");\n\
                 let bx = mk_e(8);\n\
                 let u = bx.tag();\n\
                 println(f\"u={u}\");\n\
                 println(\"end\");\n\
             }\n"),
        "a: ref-self match on fresh receiver\ndrop 5 e5\nt=1\n\
         b: ref-self match on named binding\ndrop 8 e8\nu=1\nend\n"
    );
}

#[test]
fn test_tuple_elem_match_scrutinee_body_fires_at_arm_end() {
    // B-2026-08-03-6 — `match t.0 { Ok(r) => .. }` on a tuple-element scrutinee.
    // The interpreter did not treat the match as consuming the element (it
    // matches a copied value), so the tuple's own element walk still owned the
    // body and ran it at the BINDING's death — after `println(t.1)` — while
    // codegen retracts that walk at the arm and fires there. The retraction is
    // load-bearing on codegen's side (without it the arm and the tuple's element
    // drop both FREE the payload), so the interpreter had to follow: the element
    // move is now recorded per `(binding, index)` and the arm binding takes the
    // body over, which is why `drop` prints BEFORE `4`.
    assert_eq!(
        run("struct Res { id: i64, name: String }\n\
             impl Drop for Res {\n\
                 fn drop(mut ref self) { println(f\"drop {self.id} {self.name}\") }\n\
             }\n\
             fn main() {\n\
                 println(\"a\");\n\
                 {\n\
                     let t: (Result[Res, i64], i64) =\n\
                         (Result.Ok(Res { id: 3, name: f\"cccc{1}\" }), 4);\n\
                     match t.0 {\n\
                         Result.Ok(r) => { println(r.id) }\n\
                         Result.Err(e) => { println(e) }\n\
                     }\n\
                     println(t.1);\n\
                 }\n\
                 println(\"end\");\n\
             }\n"),
        "a\n3\ndrop 3 cccc1\n4\nend\n"
    );
}

#[test]
fn test_freshtemp_result_arm_binding_a_struct_payload_owns_it_once() {
    // B-2026-08-04-11 leg (b) — the ORACLE half. The interpreter has always
    // given the arm's struct-payload binding sole ownership of the buffer;
    // codegen ALSO left the source's inline-payload drop armed whenever the
    // arm merely bound or read the payload instead of consuming it, so the
    // overlay free and the binding's `__karac_drop_<S>` both fired and the
    // program aborted with `free(): double free detected`.
    //
    // Keep in step with the codegen twin
    // `e2e_freshtemp_result_arm_binding_a_struct_payload_owns_it_once`. The
    // one deliberate difference is the seed: the codegen fixture takes it from
    // `env.args().len()` so the payloads survive `-O2` const-folding, and 1 is
    // what that yields there (the harness execs with no extra argv). Here it
    // is spelled as the literal 1 — there is no optimizer to defeat, and
    // `env.args()` inside an in-process interpreter test would report the TEST
    // binary's argv, which varies with the filter. Every printed value is
    // therefore byte-identical to the codegen twin's.
    assert_eq!(
        run("struct One { msg: String }\n\
             struct Two { code: i64, msg: String }\n\
             fn s_of(tag: String, i: i64) -> String {\n\
             let mut s: String = String.new();\n\
             s.push_str(tag);\n\
             s.push_str(f\"-payload-{i}\");\n\
             return s;\n\
             }\n\
             fn digits(i: i64) -> String {\n\
             let mut d: String = String.new();\n\
             d.push_str(f\"{i}\");\n\
             return d;\n\
             }\n\
             fn g1(i: i64) -> Result[i64, One] { return Result.Err(One { msg: s_of(\"one\", i) }); }\n\
             fn g2(i: i64) -> Result[i64, Two] { return Result.Err(Two { code: i, msg: s_of(\"two\", i) }); }\n\
             fn main() {\n\
             let n: i64 = 1i64;\n\
             match g1(n) {\n\
             Result.Ok(v) => { println(f\"ok{v}\"); }\n\
             Result.Err(e) => { println(\"a:unused\"); }\n\
             }\n\
             match g1(n + 10i64) {\n\
             Result.Ok(v) => { println(f\"ok{v}\"); }\n\
             Result.Err(e) => {\n\
             if e.msg.contains(digits(n + 10i64)) { println(f\"b:{e.msg.len()}\"); }\n\
             else { println(\"b:BAD\"); }\n\
             }\n\
             }\n\
             match g2(n + 100i64) {\n\
             Result.Ok(v) => { println(f\"ok{v}\"); }\n\
             Result.Err(e) => {\n\
             if e.msg.contains(digits(n + 100i64)) { println(f\"c:{e.code}:{e.msg.len()}\"); }\n\
             else { println(\"c:BAD\"); }\n\
             }\n\
             }\n\
             match g1(n + 1000i64) {\n\
             Result.Ok(v) => { println(f\"ok{v}\"); }\n\
             Result.Err(_) => { println(\"d:wild\"); }\n\
             }\n\
             let r: Result[i64, One] = g1(n + 10000i64);\n\
             match r {\n\
             Result.Ok(v) => { println(f\"ok{v}\"); }\n\
             Result.Err(e) => {\n\
             if e.msg.contains(digits(n + 10000i64)) { println(f\"e:{e.msg.len()}\"); }\n\
             else { println(\"e:BAD\"); }\n\
             }\n\
             }\n\
             match g1(n + 100000i64) {\n\
             Result.Ok(v) => { println(f\"ok{v}\"); }\n\
             Result.Err(e) => {\n\
             let m: String = e.msg;\n\
             if m.contains(digits(n + 100000i64)) { println(f\"f:{m.len()}\"); }\n\
             else { println(\"f:BAD\"); }\n\
             }\n\
             }\n\
             println(\"end\");\n\
             }\n"),
        "a:unused\nb:14\nc:101:15\nd:wild\ne:17\nf:18\nend\n"
    );
}

#[test]
fn test_boxed_optres_payload_struct_destructure_deboxes() {
    // B-2026-08-04-5 — the ORACLE half. The interpreter has always destructured
    // a heap-BOXED `Option`/`Result` payload with a STRUCT sub-pattern
    // correctly; codegen CRASHED on it (`ExtractOutOfRange`), because a bare
    // `Full { .. }` path resolved to the prelude's `enum ChannelError { Full }`
    // and both payload-sizing arms fell to their 1-word defaults. Pinning the
    // oracle keeps the codegen twin honest.
    //
    // The struct name `Full` is load-bearing: renaming it to anything the
    // prelude does not use as a variant name is the one-line bisect, so a
    // rename here would silently retire the test.
    //
    // Keep byte-identical to the codegen twin
    // `e2e_boxed_optres_payload_struct_destructure_deboxes`.
    assert_eq!(
        run("struct Full { name: String, buf: Vec[i64] }\n\
             struct Narrow { name: String }\n\
             fn mk(i: i64) -> Full {\n\
             let mut v: Vec[i64] = Vec.new();\n\
             v.push(i);\n\
             return Full { name: f\"pay-{i}\", buf: v };\n\
             }\n\
             fn opt(i: i64) -> Option[Full] { return Option.Some(mk(i)); }\n\
             fn res(i: i64) -> Result[i64, Full] { return Result.Err(mk(i)); }\n\
             fn main() {\n\
             let o: Option[Full] = Option.Some(mk(1i64));\n\
             match o {\n\
             Option.Some(Full { name, buf }) => { println(f\"a:{name}:{buf.len()}\"); }\n\
             Option.None => { println(\"a:none\"); }\n\
             }\n\
             match opt(2i64) {\n\
             Option.Some(Full { name, buf }) => { println(f\"b:{name}:{buf.len()}\"); }\n\
             Option.None => { println(\"b:none\"); }\n\
             }\n\
             match res(3i64) {\n\
             Result.Ok(v) => { println(f\"c:ok{v}\"); }\n\
             Result.Err(Full { name, buf }) => { println(f\"c:{name}:{buf.len()}\"); }\n\
             }\n\
             let o4: Option[Full] = Option.Some(mk(4i64));\n\
             match o4 {\n\
             Option.Some(Full { name, buf: _ }) => { println(f\"d:{name}\"); }\n\
             Option.None => { println(\"d:none\"); }\n\
             }\n\
             let o5: Option[Full] = Option.Some(mk(5i64));\n\
             if let Option.Some(Full { name, buf }) = o5 {\n\
             println(f\"e:{name}:{buf.len()}\");\n\
             }\n\
             let mut v6: Vec[Full] = Vec.new();\n\
             v6.push(mk(6i64));\n\
             while let Option.Some(Full { name, buf }) = v6.pop() {\n\
             println(f\"f:{name}:{buf.len()}\");\n\
             }\n\
             let o7: Option[Narrow] = Option.Some(Narrow { name: f\"nar-{7i64}\" });\n\
             match o7 {\n\
             Option.Some(Narrow { name }) => { println(f\"g:{name}\"); }\n\
             Option.None => { println(\"g:none\"); }\n\
             }\n\
             println(\"end\");\n\
             }\n"),
        "a:pay-1:1\nb:pay-2:1\nc:pay-3:1\nd:pay-4\n\
         e:pay-5:1\nf:pay-6:1\ng:nar-7\nend\n"
    );
}

#[test]
fn test_struct_pattern_wins_over_a_same_named_enum_variant() {
    // B-2026-08-04-5, the general hazard behind the ICE. All three resolution
    // tiers: a plain struct scrutinee whose type shares a name with an enum
    // variant, the same bare name over a scrutinee that IS that enum (the
    // scrutinee hint must still win), and the qualified spelling.
    //
    // Keep byte-identical to the codegen twin
    // `e2e_struct_pattern_wins_over_a_same_named_enum_variant`.
    assert_eq!(
        run("struct Full { a: i64 }\n\
             enum Holder { Full { a: i64 }, Nothing }\n\
             fn main() {\n\
             let s: Full = Full { a: 11i64 };\n\
             match s {\n\
             Full { a } => { println(f\"s:{a}\"); }\n\
             }\n\
             let h: Holder = Holder.Full { a: 22i64 };\n\
             match h {\n\
             Full { a } => { println(f\"v:{a}\"); }\n\
             Nothing => { println(\"v:none\"); }\n\
             }\n\
             let h2: Holder = Holder.Nothing;\n\
             match h2 {\n\
             Holder.Full { a } => { println(f\"q:{a}\"); }\n\
             Holder.Nothing => { println(\"q:none\"); }\n\
             }\n\
             println(\"end\");\n\
             }\n"),
        "s:11\nv:22\nq:none\nend\n"
    );
}

#[test]
fn test_named_optres_boxed_payload_arm_runs_user_drop_body() {
    // B-2026-08-02-25 (match-arm leg) — the ORACLE half. The interpreter has
    // always fired the payload's Drop body at the arm binding's death here; AOT
    // was silent for a heap-BOXED payload, which made this a run-vs-build split
    // on the SHIPPING side. Pinning the oracle keeps the codegen twin honest:
    // it is judged by matching this byte for byte.
    //
    // `Wide` is 4 words, so it is BOXED at Option's 3-word payload area and
    // INLINE at Result's 5-word one; `Wider` is 7 and boxed in both. Widths are
    // load-bearing — a boxing threshold is what separated the working spelling
    // from the broken one — so do not "simplify" these structs.
    //
    // Keep byte-identical to the codegen twin
    // `e2e_named_optres_boxed_payload_arm_runs_user_drop_body`.
    assert_eq!(
        run("struct Wide { tag: i64, name: String }\n\
             impl Drop for Wide { fn drop(mut ref self) { println(f\"D{self.tag}\") } }\n\
             struct Wider { tag: i64, a: String, b: String }\n\
             impl Drop for Wider { fn drop(mut ref self) { println(f\"W{self.tag}\") } }\n\
             struct Narrow { name: String }\n\
             impl Drop for Narrow { fn drop(mut ref self) { println(f\"N{self.name}\") } }\n\
             fn mkw(t: i64) -> Wide { return Wide { tag: t, name: \"payload-string-data\" }; }\n\
             fn mkr(t: i64) -> Wider {\n\
             return Wider { tag: t, a: \"payload-string-data\", b: \"second-payload-str\" };\n\
             }\n\
             fn main() {\n\
             {\n\
             let o: Option[Wide] = Some(mkw(1i64));\n\
             match o {\n\
             Some(r) => { println(f\"a{r.tag}\"); }\n\
             None => { println(\"a-none\"); }\n\
             }\n\
             println(\"a-end\");\n\
             }\n\
             {\n\
             let o: Option[Wide] = Some(mkw(2i64));\n\
             if let Some(r) = o { println(f\"b{r.tag}\"); }\n\
             println(\"b-end\");\n\
             }\n\
             {\n\
             let o: Option[Wide] = Some(mkw(3i64));\n\
             let Some(r) = o else { return; }\n\
             println(f\"c{r.tag}\");\n\
             println(\"c-end\");\n\
             }\n\
             {\n\
             let o: Result[Wider, i64] = Ok(mkr(4i64));\n\
             match o {\n\
             Ok(r) => { println(f\"d{r.tag}\"); }\n\
             Err(e) => { println(f\"d-err{e}\"); }\n\
             }\n\
             println(\"d-end\");\n\
             }\n\
             {\n\
             let o: Option[Wide] = Some(mkw(5i64));\n\
             match o {\n\
             Some(_) => { println(\"e\"); }\n\
             None => { println(\"e-none\"); }\n\
             }\n\
             println(\"e-end\");\n\
             }\n\
             {\n\
             let o: Option[Narrow] = Some(Narrow { name: \"n6\" });\n\
             match o {\n\
             Some(r) => { println(f\"f{r.name}\"); }\n\
             None => { println(\"f-none\"); }\n\
             }\n\
             println(\"f-end\");\n\
             }\n\
             println(\"end\");\n\
             }\n"),
        "a1\nD1\na-end\n\
         b2\nD2\nb-end\n\
         c3\nD3\nc-end\n\
         d4\nW4\nd-end\n\
         e\nD5\ne-end\n\
         fn6\nNn6\nf-end\n\
         end\n"
    );
}

/// B-2026-08-04-4 — the interpreter's move-out records are keyed by BINDING
/// NAME with no scope component, and a match arm did not re-arm the names it
/// binds. So a container-push move recorded for one block's `r` outlived its
/// block and silenced an unrelated later `r`'s `impl Drop` body.
///
/// The name was the entire trigger: renaming the second binding made the body
/// fire. That is why this test runs BOTH spellings and asserts they agree —
/// pinning only the reused-name case would pass against a fix that broke the
/// distinct-name one, and pinning only one output would not show that the
/// difference used to be the bug.
///
/// AOT was correct throughout, so this was a run-vs-build split on the `karac
/// run` side.
#[test]
fn match_arm_rebinding_a_moved_name_still_runs_its_drop_body() {
    let program = |second: &str| {
        format!(
            "struct Res {{ id: i64, name: String }}\n\
             impl Drop for Res {{ fn drop(mut ref self) {{ println(\"D\" + self.id.to_string()); }} }}\n\
             fn main() {{\n\
                 let o: Option[Res] = Option.Some(Res {{ id: 4, name: \"pay4\" }});\n\
                 let mut v: Vec[Res] = Vec.new();\n\
                 match o {{\n\
                     Option.Some(r) => {{ v.push(r); }}\n\
                     Option.None => {{}}\n\
                 }}\n\
                 println(v[0].name);\n\
                 let o2: Option[Res] = Option.Some(Res {{ id: 6, name: \"pay6\" }});\n\
                 match o2 {{\n\
                     Option.Some({second}) => {{ println({second}.name); }}\n\
                     Option.None => {{}}\n\
                 }}\n\
                 println(\"end\");\n\
             }}"
        )
    };
    // `q` never collided, so this spelling was always correct — it is the
    // control that a too-broad fix would break.
    let distinct = run_no_errors(&program("q"));
    // `r` reuses the name the first arm moved into the Vec. This printed
    // without `D6` before the fix.
    let reused = run_no_errors(&program("r"));
    assert!(
        reused.contains("D6"),
        "reusing a moved binding's NAME in a later match arm must not silence \
         that arm's Drop body; got: {reused:?}"
    );
    assert_eq!(
        distinct, reused,
        "the binding's NAME must not change observable drop behaviour"
    );
}

#[test]
fn test_qualified_unit_variant_match_oracle() {
    // Oracle twin of `tests/codegen.rs`'s
    // `test_e2e_qualified_unit_variant_match` (B-2026-08-17-41). The bug was
    // in the exhaustiveness ANALYSIS, not in lowering — both backends always
    // selected the right arm — so this pins that the arm selection stayed
    // correct while the analysis learned to strip the qualifier.
    let out = run("\n\
         enum Dir { North, South, East, West }\n\
         fn f(d: Dir) -> i64 {\n\
             match d {\n\
                 Dir.North => 0,\n\
                 Dir.South => 1,\n\
                 Dir.East  => 2,\n\
                 Dir.West  => 3,\n\
             }\n\
         }\n\
         fn main() {\n\
             println(f(Dir.North));\n\
             println(f(Dir.South));\n\
             println(f(Dir.East));\n\
             println(f(Dir.West));\n\
         }\n");
    assert_eq!(out, "0\n1\n2\n3\n");
}

/// The interpreter twin of `e2e_128bit_literal_patterns_match_their_own_value`
/// (tests/codegen.rs), B-2026-08-20-4. Both backends read the pattern's payload
/// off the same widened AST node, so the pair pins that they agree — including
/// on the wrapped encoding a `u128` past `i128::MAX` rides.
#[test]
fn a_128bit_literal_pattern_matches_its_own_value() {
    assert_eq!(
        run_no_errors(
            "fn main() {\n\
             let a: i128 = 170141183460469231731687303715884105727i128;\n\
             match a { 170141183460469231731687303715884105727i128 => println(\"imax\"), _ => println(\"no\") }\n\
             let b: i128 = 1267650600228229401496703205376i128;\n\
             match b { 170141183460469231731687303715884105727i128 => println(\"imax\"), _ => println(\"no\") }\n\
             let c: u128 = 340282366920938463463374607431768211455u128;\n\
             match c { 340282366920938463463374607431768211455u128 => println(\"umax\"), _ => println(\"no\") }\n\
             let d: u128 = 170141183460469231731687303715884105728u128;\n\
             match d { 340282366920938463463374607431768211455u128 => println(\"umax\"), _ => println(\"no\") }\n\
             let e: i128 = 1500000000000000000000000000000i128;\n\
             match e { 1000000000000000000000000000000i128..=2000000000000000000000000000000i128 => println(\"band\"), _ => println(\"no\") }\n\
             }"
        ),
        "imax\nno\numax\nno\nband\n"
    );
}

/// The interpreter twin of `e2e_negative_literal_patterns_match`
/// (tests/codegen.rs), B-2026-08-20-7. Both backends read the folded sign off
/// the same AST node, so the pair pins that they agree — including on the two
/// MIN magnitudes, which exist only as an already-negated literal.
#[test]
fn a_negative_literal_pattern_matches() {
    assert_eq!(
        run_no_errors(
            "fn main() {\n\
             let a: i64 = 0i64 - 5i64;\n\
             match a { -5 => println(\"neg5\"), _ => println(\"no\") }\n\
             match a { -10..=-1 => println(\"band\"), _ => println(\"no\") }\n\
             let b: i64 = 7i64;\n\
             match b { -5 => println(\"neg5\"), _ => println(\"no\") }\n\
             let m: i64 = 0i64 - 9223372036854775807i64 - 1i64;\n\
             match m { -9223372036854775808 => println(\"min\"), _ => println(\"no\") }\n\
             let w: i128 = 0i128 - 170141183460469231731687303715884105727i128 - 1i128;\n\
             match w { -170141183460469231731687303715884105728i128 => println(\"i128min\"), _ => println(\"no\") }\n\
             let f: f64 = 0.0 - 1.5;\n\
             match f { -1.5 => println(\"float\"), _ => println(\"no\") }\n\
             let t = (0i64 - 1i64, 2i64);\n\
             match t { (-1, 2) => println(\"tuple\"), _ => println(\"no\") }\n\
             }"
        ),
        "neg5\nband\nno\nmin\ni128min\nfloat\ntuple\n"
    );
}

#[test]
fn upper_half_unsigned_patterns_match_the_right_arm() {
    // B-2026-08-20-1 — behaviour, not just parsing: the pattern rides the same
    // wrapped bit pattern the scrutinee does, so `u64::MAX` selects its own arm
    // and an upper-half inclusive RANGE selects its own too.
    assert_eq!(
        run("fn classify(n: u64) -> i64 {
                 match n {
                     18446744073709551615u64 => 1,
                     18446744073709551610u64..=18446744073709551614u64 => 2,
                     0u64 => 3,
                     _ => 4,
                 }
             }
             fn main() {
                 println(classify(18446744073709551615u64));
                 println(classify(18446744073709551612u64));
                 println(classify(0u64));
                 println(classify(7u64));
             }"),
        "1\n2\n3\n4\n"
    );
}

#[test]
fn test_bare_and_qualified_stdlib_variant_patterns_agree() {
    // The qualified spelling was always correct, which is what isolated the
    // trigger to the bare form rather than to the type. Both spellings of one
    // program, so a fix that only moved the bug would show up here.
    let out = run("fn main() {\n\
            let a = Stdio.Inherit;\n\
            let c = Stdio.Piped;\n\
            match a { Inherit => println(\"bare:a=Inherit\"), Null => println(\"bare:a=Null\"), Piped => println(\"bare:a=Piped\") }\n\
            match c { Inherit => println(\"bare:c=Inherit\"), Null => println(\"bare:c=Null\"), Piped => println(\"bare:c=Piped\") }\n\
            match a { Stdio.Inherit => println(\"qual:a=Inherit\"), Stdio.Null => println(\"qual:a=Null\"), Stdio.Piped => println(\"qual:a=Piped\") }\n\
            match c { Stdio.Inherit => println(\"qual:c=Inherit\"), Stdio.Null => println(\"qual:c=Null\"), Stdio.Piped => println(\"qual:c=Piped\") }\n\
        }");
    assert_eq!(
        out,
        "bare:a=Inherit\nbare:c=Piped\nqual:a=Inherit\nqual:c=Piped\n"
    );
}

#[test]
fn test_user_enum_bare_variant_patterns_were_never_affected() {
    // The control that scopes the bug to BAKED-STDLIB enums: a user enum's
    // variants are registered unqualified as well, so the env lookup always
    // found them and bare patterns were correct throughout.
    let out = run("enum Color { Red, Green, Blue }\n\
        fn main() {\n\
            let c = Color.Blue;\n\
            match c { Red => println(\"Red\"), Green => println(\"Green\"), Blue => println(\"Blue\") }\n\
        }");
    assert_eq!(out, "Blue\n");
}

/// B-2026-08-28-69 — a DISCARDED `match` whose arm value is an owned
/// Drop-bearing temp must run that body exactly once, on every backend.
///
/// Exactly one `R` is constructed and nothing takes it, so it dies at the
/// statement. Before the fix the interpreter ran NO body for an arm handing on
/// a bound payload (the compiled backends ran one), and BOTH sides ran none
/// when the arm minted a fresh value — an agreed-but-wrong cell that only an
/// A/B comparison can see, since the memory is balanced either way.
///
/// The two halves had to land together: an earlier interpreter-only attempt at
/// this row was measured and REVERTED because it moved the divergence instead
/// of removing it.
#[test]
fn test_discarded_match_arm_value_runs_its_drop_body_once() {
    let hdr = "struct R { id: i64 }\n\
               impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\") } }\n";
    let rows: [(&str, &str, &str); 6] = [
        // The arm hands on a BOUND payload — braced and bare.
        (
            "let o: Option[R] = Some(R { id: 1 });\n\
             match o { Some(r) => { r } None => { R { id: 0 } } };\n\
             println(\"dropped\");",
            "dR1\ndropped",
            "braced arm yields the bound payload",
        ),
        (
            "let o: Option[R] = Some(R { id: 1 });\n\
             match o { Some(r) => r, None => R { id: 0 } };\n\
             println(\"dropped\");",
            "dR1\ndropped",
            "bare arm yields the bound payload",
        ),
        // The arm mints a FRESH value — a struct literal, braced and bare.
        (
            "let n = 1;\n\
             match n { 1 => { R { id: 7 } } _ => { R { id: 0 } } };\n\
             println(\"dropped\");",
            "dR7\ndropped",
            "braced arm yields a fresh literal",
        ),
        (
            "let n = 1;\n\
             match n { 1 => R { id: 7 }, _ => R { id: 0 } };\n\
             println(\"dropped\");",
            "dR7\ndropped",
            "bare arm yields a fresh literal",
        ),
        // …and via a CALL, which reaches the tracker's other type-resolution arm.
        (
            "let n = 1;\n\
             match n { 1 => mk(7), _ => mk(0) };\n\
             println(\"dropped\");",
            "dR7\ndropped",
            "arm yields a call result",
        ),
        (
            "let r = R { id: 41 };\n\
             let n = 0;\n\
             match n { 0 => r, _ => R { id: 9 } };\n\
             println(\"end\");",
            "dR41\nend",
            "BOUNDARY: arm hands out a LIVE enclosing local",
        ),
    ];
    for (body, expected, label) in rows {
        let src = format!(
            "{hdr}fn mk(i: i64) -> R {{ return R {{ id: i }}; }}\nfn main() {{\n{body}\n}}\n"
        );
        assert_eq!(run(&src).trim(), expected, "[{label}]");
    }
}

/// B-2026-08-29-20 — the `let _ = <match>` SIBLING of
/// `test_discarded_match_arm_value_runs_its_drop_body_once`, row for row.
///
/// `discard_rhs_produces_owned_value` had no `Match` arm, so this spelling ran
/// NO `Drop` body here while the bare-statement spelling ran one; codegen's
/// wildcard-let gate was missing the same leg, so the two backends agreed on
/// the silence and only an absolute expectation could see it.
///
/// ROWS 3-5 ARE THE ONES THIS MOVED (0 bodies -> 1, on all three backends).
/// Rows 1, 2 and 6 are pinned AS MEASURED and are all still wrong: each is an
/// arm that HANDS OUT A BINDING rather than minting a value. B-2026-08-29-5
/// fixed the LEAK for that population; the missing BODY, which is what these
/// rows measure, is B-2026-08-29-31. Row 2 is
/// additionally a live run-vs-build divergence — compiled prints `dR1` through
/// the general match lowering, this backend prints nothing.
///
/// Row 2 is also exactly why the `Match` arm EXCLUDES a bare `Identifier` tail
/// instead of recursing into `discard_rhs_produces_owned_value` whole: that
/// predicate's `ExprKind::Identifier(_) => true` is far more permissive than
/// codegen's freshness gate, so inheriting it would fire here on row 1 too,
/// where compiled is silent — moving the divergence rather than removing it.
/// An earlier interpreter-only attempt at the PARENT row was reverted for the
/// same measurement, which is why the exclusion is spelled out rather than
/// assumed.
#[test]
fn test_discarded_let_wildcard_match_runs_its_drop_body_once() {
    let hdr = "struct R { id: i64 }\n\
               impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\") } }\n";
    let rows: [(&str, &str, &str); 6] = [
        (
            "let o: Option[R] = Some(R { id: 1 });\n\
             let _ = match o { Some(r) => { r } None => { R { id: 0 } } };\n\
             println(\"dropped\");",
            "dR1\ndropped",
            "FIXED (B-2026-08-29-31): braced arm hands out the bound payload",
        ),
        (
            "let o: Option[R] = Some(R { id: 1 });\n\
             let _ = match o { Some(r) => r, None => R { id: 0 } };\n\
             println(\"dropped\");",
            "dR1\ndropped",
            "FIXED (B-2026-08-29-31): bare arm hands out the bound payload — this \
             backend was the silent half of a live divergence, compiled having \
             fired here all along",
        ),
        (
            "let n = 1;\n\
             let _ = match n { 1 => { R { id: 7 } } _ => { R { id: 0 } } };\n\
             println(\"dropped\");",
            "dR7\ndropped",
            "FIXED: braced arm yields a fresh literal",
        ),
        (
            "let n = 1;\n\
             let _ = match n { 1 => R { id: 7 }, _ => R { id: 0 } };\n\
             println(\"dropped\");",
            "dR7\ndropped",
            "FIXED: bare arm yields a fresh literal",
        ),
        (
            "let n = 1;\n\
             let _ = match n { 1 => mk(7), _ => mk(0) };\n\
             println(\"dropped\");",
            "dR7\ndropped",
            "FIXED: arm yields a call result",
        ),
        (
            "let r = R { id: 41 };\n\
             let n = 0;\n\
             let _ = match n { 0 => r, _ => R { id: 9 } };\n\
             println(\"end\");",
            "dR41\nend",
            "FIXED (B-2026-08-29-31): arm hands out an enclosing local",
        ),
    ];
    for (body, expected, label) in rows {
        let src = format!(
            "{hdr}fn mk(i: i64) -> R {{ return R {{ id: i }}; }}\nfn main() {{\n{body}\n}}\n"
        );
        assert_eq!(run(&src).trim(), expected, "[{label}]");
    }
    // The control that localized the defect: the same discard site with a
    // non-match RHS was correct throughout.
    let call = format!(
        "{hdr}fn mk() -> R {{ return R {{ id: 1 }}; }}\n\
         fn main() {{ let _ = mk(); println(\"dropped\"); }}\n"
    );
    assert_eq!(run(&call).trim(), "dR1\ndropped", "[control: let _ = call]");
    // A read-only arm yields unit, so the widened gate must stay a no-op there.
    let read = format!(
        "{hdr}fn main() {{\n\
         let o: Option[R] = Some(R {{ id: 1 }});\n\
         let _ = match o {{ Some(r) => {{ println(f\"saw{{r.id}}\") }} None => {{ println(\"none\") }} }};\n\
         println(\"dropped\");\n}}\n"
    );
    assert_eq!(
        run(&read).trim(),
        "saw1\ndR1\ndropped",
        "[control: read-only arm]"
    );
}

/// B-2026-08-29-30 (remaining half) — the INTERPRETER twin of
/// `e2e_no_else_if_arm_owns_the_value_it_mints`, landed in the same commit.
/// Both tables are the SAME shapes in the same order, so the two backends
/// cannot be fixed to different answers.
///
/// Four spellings of one program — `let _ =` and bare, each over a struct
/// literal and a call — were silent on all three backends and leaked one
/// allocation per evaluation. Agreed silence is invisible to every A/B parity
/// gate, which is why this needed a row rather than showing up as a failure.
///
/// The CONTROLS carry the fix's whole safety argument, and each one is a shape
/// where an over-eager owner is a DOUBLE body rather than a missing one:
///
///   * a tail that NAMES a live local keeps its own scope-exit body and must
///     stay declined (`place-tail-*`);
///   * an `if` WITH an `else` is owned by the STATEMENT site and must not gain
///     a second owner in its arms (`else-*`);
///   * a struct literal whose FIELD names a live local is declined by the
///     all-fresh gate — the binding still owns that field's heap
///     (`field-is-a-place`);
///   * the branch NOT taken mints nothing, so nothing may fire
///     (`branch-not-taken`).
#[test]
fn test_no_else_if_arm_owns_the_value_it_mints() {
    let hdr = "struct R { id: i64, name: String }\n\
               impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\") } }\n\
               struct H { s: String }\n\
               enum E { A(R), B }\n\
               impl Drop for E { fn drop(mut ref self) { println(\"dE\") } }\n\
               fn mk(i: i64) -> R { return R { id: i, name: f\"heap-{i}\" }; }\n";
    for (label, body, want) in [
        // ── the row's four spellings ──────────────────────────────────
        (
            "wildcard-let-struct-literal",
            "let _ = if n == 1 { R { id: 7, name: f\"h\" } };",
            "dR7\nend",
        ),
        (
            "wildcard-let-call",
            "let _ = if n == 1 { mk(7) };",
            "dR7\nend",
        ),
        (
            "bare-statement-struct-literal",
            "if n == 1 { R { id: 7, name: f\"h\" } };",
            "dR7\nend",
        ),
        ("bare-statement-call", "if n == 1 { mk(7) };", "dR7\nend"),
        // ── shapes the widened admission gate brings with it ──────────
        (
            "block-wrapped-rhs",
            "let _ = { if n == 1 { mk(19) } };",
            "dR19\nend",
        ),
        (
            "nested-branch-tail",
            "let d = 1;\nlet _ = if n == 1 { if d == 1 { mk(6) } else { mk(7) } };",
            "dR6\nend",
        ),
        (
            "nested-match-tail",
            "let d = 1;\nlet _ = if n == 1 { match d { 1 => mk(6), _ => mk(7) } };",
            "dR6\nend",
        ),
        (
            "tuple-literal-tail",
            "let _ = if n == 1 { (mk(12), 20) };",
            "dR12\nend",
        ),
        // An own-`Drop` enum runs its OWN body and its PAYLOAD's, which is
        // what the direct `let _ = E.A(..)` spelling already did on every
        // backend — the peel that makes the two agree is
        // `discard_producer_expr`.
        (
            "inline-enum-ctor-tail",
            "let _ = if n == 1 { E.A(mk(8)) };",
            "dE\ndR8\nend",
        ),
        ("unit-variant-tail", "let _ = if n == 1 { E.B };", "dE\nend"),
        (
            "loop-body-fires-per-iteration",
            "for i in 0..3 { if n == 1 { mk(i) }; }",
            "dR0\ndR1\ndR2\nend",
        ),
        // ── controls: an over-eager owner here is a DOUBLE body ───────
        (
            "control: place-tail-bare",
            "let r = mk(1);\nif n == 1 { r };",
            "dR1\nend",
        ),
        (
            // FIXED by B-2026-08-29-31, which stopped a wildcard `let` marking
            // its RHS as an escaping position. This pinned `end` when it was
            // written: the local was recorded moved-out and nothing ran its
            // body. It now keeps its own scope-exit body, at ONE — still a
            // control, for the opposite direction.
            "control: place-tail-wildcard-let",
            "let r = mk(1);\nlet _ = if n == 1 { r };",
            "dR1\nend",
        ),
        (
            "control: else-struct-literal",
            "let _ = if n == 1 { R { id: 2, name: f\"a\" } } else { R { id: 3, name: f\"b\" } };",
            "dR2\nend",
        ),
        (
            "control: else-call",
            "let _ = if n == 1 { mk(4) } else { mk(5) };",
            "dR4\nend",
        ),
        (
            "control: else-if-chain",
            "let _ = if n == 0 { mk(20) } else if n == 1 { mk(21) } else { mk(22) };",
            "dR21\nend",
        ),
        (
            "control: field-is-a-place",
            "let s = f\"live\";\nlet _ = if n == 1 { H { s: s } };\nprintln(\"kept\");",
            "kept\nend",
        ),
        (
            "control: branch-not-taken",
            "let _ = if n == 2 { mk(9) };",
            "end",
        ),
        (
            "control: branch-not-taken-place-tail",
            "let r = mk(18);\nlet _ = if n == 2 { r };\nprintln(\"still\");",
            "dR18\nstill\nend",
        ),
    ] {
        let src = format!("{hdr}fn main() {{\nlet n = 1;\n{body}\nprintln(\"end\");\n}}\n");
        assert_eq!(run(&src).trim(), want, "[{label}]");
    }
}

/// B-2026-09-06-25 — see the codegen twin for the two shapes; this is the
/// backend that was wrong. Both fixes are interpreter-side: the caller-side
/// escape masks stand down for a `ref` / `mut ref` parameter
/// (`callee_param_is_borrow`), and `self.e` under a borrowed receiver gets the
/// disarm-and-stash lockstep a named `h.e` root always had.
///
/// Twin of `tests/codegen.rs`'s `e2e_borrow_projection_view_materialized_by_arm_value_or_call_arg_copies`, pinned to the same string.
#[test]
fn test_borrow_projection_view_materialized_by_arm_value_or_call_arg_copies() {
    assert_eq!(
        run(r#"struct R { id: i64, tag: String, xs: Vec[i64] }
impl Drop for R { fn drop(mut ref self) { println(f"  dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, tag: f"t{i}", xs: [i] } }
enum E { A(R), B }
impl Drop for E { fn drop(mut ref self) { println("  dE") } }
struct H { e: E }
struct W { r: R }
fn consume(x: R) -> i64 { return x.id }

fn p_out(h: ref H) -> R { let r2 = match h.e { E.A(r) => r, E.B => mk(0) }; return r2; }
fn p_out_mut(h: mut ref H) -> R { match h.e { E.A(r) => { return r; } E.B => { return mk(0); } } }
fn p_field(w: ref W) -> R { return w.r; }
fn p_consume(h: ref H) -> i64 { match h.e { E.A(r) => { return consume(r); } E.B => { return 0; } } }
fn p_read(h: ref H) -> i64 { match h.e { E.A(r) => { return r.id; } E.B => { return 0; } } }
impl H {
    fn m_consume(ref self) -> i64 { match self.e { E.A(r) => { return consume(r); } E.B => { return 0; } } }
    fn m_consume_mut(mut ref self) -> i64 { match self.e { E.A(r) => { return consume(r); } E.B => { return 0; } } }
    fn m_consume_iflet(ref self) -> i64 { if let E.A(r) = self.e { return consume(r); } return 0; }
    fn m_let(ref self) -> i64 { match self.e { E.A(r) => { let m = r; return consume(m); } E.B => { return 0; } } }
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
"#),
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

/// B-2026-09-06-24 — the interpreter was the reference here (a read-only
/// `if let` / `while let` over a borrow projection binds a view); pinned on
/// this side too so a one-sided regression fails loudly.
///
/// Twin of `tests/codegen.rs`'s `e2e_readonly_if_let_over_borrow_projection_binds_a_view`, pinned to the same string.
#[test]
fn test_readonly_if_let_over_borrow_projection_binds_a_view() {
    assert_eq!(
        run(r#"struct R { id: i64, tag: String, xs: Vec[i64] }
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
fn p_iflet_move(h: ref H) -> i64 { if let E.A(r) = h.e { let m = r; return m.id; } else { return 0; } }
fn p_iflet_consume(h: ref H) -> i64 { if let E.A(r) = h.e { return consume(r); } return 0; }
impl H {
    fn m_iflet(ref self) -> i64 { if let E.A(r) = self.e { return r.id; } else { return 0; } }
    fn m_iflet_mut(mut ref self) -> i64 { if let E.A(r) = self.e { return r.id; } else { return 0; } }
    fn m_whilelet(ref self) -> i64 { while let E.A(r) = self.e { return r.id; } return 0; }
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
"#),
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

/// B-2026-09-07-33 — the interpreter half of the boxed-payload hand-out.
///
/// The interpreter was CORRECT throughout this row: the defect was a compiled
/// backend writing into an envelope it had already freed, on a program every
/// backend printed correctly. So this twin fails on neither side of the fix,
/// and it is here for the reason every twin is — to make a future regression on
/// either backend show up as a DIVERGENCE rather than as two backends quietly
/// agreeing on a wrong answer.
///
/// Twin of `tests/codegen.rs`'s
/// `e2e_boxed_payload_handed_out_of_a_match_arm_keeps_its_value`, pinned to the
/// same string.
#[test]
fn test_boxed_payload_handed_out_of_a_match_arm_keeps_its_value() {
    assert_eq!(
        run(r#"struct X1 { a: Option[i64], s: String }
struct Ctl { s: String, n: i64 }
enum W { T(X1), U(i64) }
enum C { T(Ctl), U(i64) }
fn mkx(i: i64) -> X1 { return X1 { a: Option.Some(i), s: f"s{i}" }; }
fn mkc(i: i64) -> Ctl { return Ctl { s: f"c{i}", n: i }; }
fn payout(w: W) -> X1 { return match w { W.T(x) => x, W.U(n) => mkx(n) }; }
fn payoutc(c: C) -> Ctl { return match c { C.T(x) => x, C.U(n) => mkc(n) }; }
fn rebind(w: W) -> i64 { let v = w; return match v { W.T(x) => x.a.unwrap_or(0), W.U(n) => n }; }
fn store(w: W, out: mut ref Vec[W]) { out.push(w); }
fn main() {
    let p = payout(W.T(mkx(23))); println(f"a={p.a.unwrap_or(0)}/{p.s}")
    let q = payout(W.U(24)); println(f"b={q.a.unwrap_or(0)}/{q.s}")
    let r = payoutc(C.T(mkc(25))); println(f"c={r.n}/{r.s}")
    println(f"d={rebind(W.T(mkx(26)))}")
    let mut v: Vec[W] = Vec.new(); store(W.T(mkx(27)), mut v); println(f"e={v.len()}")
    println("end")
}
"#),
        "a=23/s23\nb=24/s24\nc=25/c25\nd=26\ne=1\nend\n"
    );
}

/// B-2026-09-07-7 — the interpreter never disarms a discarded arm's source,
/// so it was correct on every cell; the twin holds the compiled string to it.
///
/// Twin of `tests/codegen.rs`'s `e2e_discarded_arm_literal_over_a_named_local`, pinned to the same string.
#[test]
fn test_discarded_arm_literal_over_a_named_local() {
    assert_eq!(
        run(r#"struct D { s: String }
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
"#),
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
/// Twin of `tests/codegen.rs`'s `e2e_enum_leaf_handed_back_out_of_a_destructure`, pinned to the same string.
#[test]
fn test_enum_leaf_handed_back_out_of_a_destructure() {
    assert_eq!(
        run(r#"struct R { id: i64, tag: String, xs: Vec[i64] }
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
"#),
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
/// Twin of `tests/codegen.rs`'s `e2e_read_only_arm_on_owned_enum_receiver_runs_payload_body`, pinned to the same string.
#[test]
fn test_read_only_arm_on_owned_enum_receiver_runs_payload_body() {
    assert_eq!(
        run(r#"struct R { id: i64, tag: String, xs: Vec[i64] }
impl Drop for R { fn drop(mut ref self) { println(f"  dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, tag: f"t{i}", xs: [i] } }
enum E { A(R), B }
impl Drop for E { fn drop(mut ref self) { println("  dE") } }
enum P { A(R), B }
impl E {
    fn m_read(self) -> i64 { match self { E.A(r) => { return r.id; } E.B => { return 0; } } }
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
"#),
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
/// Twin of `tests/codegen.rs`'s `e2e_two_level_destructure_hand_back_runs_one_payload_body`, pinned to the same string.
#[test]
fn test_two_level_destructure_hand_back_runs_one_payload_body() {
    assert_eq!(
        run(r#"struct R { id: i64, tag: String, xs: Vec[i64] }
impl Drop for R { fn drop(mut ref self) { println(f"  dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, tag: f"t{i}", xs: [i] } }
enum E { A(R), B }
impl Drop for E { fn drop(mut ref self) { println("  dE") } }
struct H1 { e: E }
struct S { e: E }
struct H2 { s: S }
struct Hb { e: E, b: E }
fn p_r(e: E) -> R { match e { E.A(r) => { return r; } E.B => { return mk(0); } } }
fn p_h(h: H1) -> R { match h { H1 { e } => { match e { E.A(r) => { return r; } E.B => { return mk(0); } } } } }
fn p_let(h: H1) -> R { let H1 { e } = h; match e { E.A(r) => { return r; } E.B => { return mk(0); } } }
fn p_iflet(h: H1) -> R { match h { H1 { e } => { if let E.A(r) = e { return r; } else { return mk(0); } } } }
fn p_proj(h: H1) -> R { match h.e { E.A(r) => { return r; } E.B => { return mk(0); } } }
fn p_deep(h: H2) -> R { match h { H2 { s } => { match s { S { e } => { match e { E.A(r) => { return r; } E.B => { return mk(0); } } } } } } }
fn p_two(h: Hb) -> R { match h { Hb { e, b } => { match e { E.A(r) => { return r; } E.B => { return mk(0); } } } } }
impl H1 { fn hand(self) -> R { match self { H1 { e } => { match e { E.A(r) => { return r; } E.B => { return mk(0); } } } } } }
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
"#),
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
/// THE TWO BACKENDS AGREED ON THE SET AND DIFFERED ON TWO CELLS' ORDER —
/// `second_bound` and `both_bound` — and this comment predicted that "when
/// -06-21 lands, one of these two pinned strings changes and the other does
/// not". -06-21 landed (with B-2026-09-16-17) and BOTH changed, converging on
/// one string, which is why the twin in `tests/codegen.rs` now pins the same
/// bytes this does. Two independent corrections met here:
///
///   * B-2026-09-16-17 — an enum VARIANT's payload fields ran their bodies in
///     DECLARATION order on every surface, against design.md § `Drop` Field
///     drop order ("within a single struct or enum variant ... the reverse of
///     the order they are declared"). That flipped every husk-run pair on both
///     backends: `first_bound`, `iflet`, `both_wild`, `each_variant_takes`.
///   * B-2026-09-06-21 — the interpreter stashed a READ-ONLY arm binding as an
///     arm-scoped `Drop` slot, so it ran the body at the arm's end rather than
///     leaving it to the husk. design.md § Match Arm Binding Modes: "bindings
///     that are only read BORROW from the already-owned value". That is what
///     `second_bound` and `both_bound` were pinning, and it is why the two
///     strings could differ at all.
///
/// The set is unchanged by both, on `--interp`, jit, `KARAC_AUTO_PAR=0` and the
/// default auto-par build.
///
/// Twin of `tests/codegen.rs`'s
/// `e2e_mixed_bind_and_wildcard_arm_keeps_the_unbound_payload_body`.
#[test]
fn test_mixed_bind_and_wildcard_arm_keeps_the_unbound_payload_body() {
    assert_eq!(
        run(r#"struct R { id: i64, tag: String }
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
"#),
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
/// Twin of `tests/codegen.rs`'s
/// `e2e_partial_struct_match_pattern_drops_the_bound_leaf_first`, pinned to the
/// same string.
#[test]
fn test_partial_struct_match_pattern_drops_the_bound_leaf_first() {
    assert_eq!(
        run(r#"struct R { id: i64, name: String }
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
"#),
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
/// Twin of `tests/codegen.rs`'s
/// `e2e_reordered_struct_let_pattern_drops_in_pattern_order`, pinned to the
/// same string.
#[test]
fn test_reordered_struct_let_pattern_drops_in_pattern_order() {
    assert_eq!(
        run(r#"struct R { id: i64, name: String }
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
"#),
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
/// Twin of `tests/codegen.rs`'s `e2e_wildcard_arm_over_owned_enum_receiver_runs_payload_body`, pinned to the same string.
#[test]
fn test_wildcard_arm_over_owned_enum_receiver_runs_payload_body() {
    assert_eq!(
        run(r#"struct R { id: i64, tag: String, xs: Vec[i64] }
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
"#),
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
fn test_wildcard_let_discard_owns_what_its_arm_hands_out() {
    let hdr = "struct R { id: i64, name: String }\n\
               impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\") } }\n\
               fn mk(i: i64) -> R { return R { id: i, name: f\"heap-{i}\" }; }\n";
    for (label, body, want) in [
        // ── the tail names an ENCLOSING LOCAL ─────────────────────────────
        (
            "if-hands-out-a-local",
            "let r = mk(41);\nlet _ = if n == 0 { r } else { mk(9) };",
            "dR41\nend",
        ),
        (
            "match-hands-out-a-local",
            "let r = mk(41);\nlet _ = match n { 0 => r, _ => mk(9) };",
            "dR41\nend",
        ),
        (
            "block-hands-out-a-local",
            "let r = mk(41);\nlet _ = { r };",
            "dR41\nend",
        ),
        // ── the tail names the arm's own PATTERN BINDING ──────────────────
        (
            "payload-braced-arm",
            "let o: Option[R] = Some(mk(1));\n\
             let _ = match o { Some(r) => { r } None => { mk(9) } };",
            "dR1\nend",
        ),
        // ── double-own guards: ONE body, not two ──────────────────────────
        (
            "guard: block-wrapped match is owned by the statement site",
            "let _ = { match n { 0 => { mk(7) } _ => { mk(3) } } };",
            "dR7\nend",
        ),
        (
            "guard: block-wrapped call is owned by the statement site",
            "let _ = { mk(7) };",
            "dR7\nend",
        ),
        // ── controls ──────────────────────────────────────────────────────
        (
            "control: bare-statement form (B-2026-08-29-5)",
            "let r = mk(41);\nif n == 0 { r } else { mk(9) };",
            "dR41\nend",
        ),
        (
            "control: all-mint if stays statement-owned",
            "let _ = if n == 0 { mk(2) } else { mk(3) };",
            "dR2\nend",
        ),
        (
            "control: all-mint match stays statement-owned",
            "let _ = match n { 0 => mk(2), _ => mk(3) };",
            "dR2\nend",
        ),
        (
            "control: else-if chain",
            "let _ = if n == 1 { mk(20) } else if n == 0 { mk(21) } else { mk(22) };",
            "dR21\nend",
        ),
        (
            "control: mixed branch — minted arm and live local both fire once",
            "let r = mk(18);\nlet _ = if n == 9 { r } else { mk(4) };\nprintln(\"still\");",
            "dR4\ndR18\nstill\nend",
        ),
    ] {
        let src = format!("{hdr}fn main() {{\nlet n = 0;\n{body}\nprintln(\"end\");\n}}\n");
        assert_eq!(run(&src).trim(), want, "[{label}]");
    }
    // B-2026-08-31-28 — this was PINNED DIVERGENT here, on the side that
    // fires: a bare arm handing out a HEAP-carrying `Option` payload ran the
    // body under `--interp` and on neither compiled backend. The compiled
    // half is fixed, so it is asserted as agreement now rather than pinned as
    // a divergence. The interpreter's answer never changed — it was the
    // correct column throughout, which is why this assertion reads the same
    // as it did while it was a pin.
    let boxed = format!(
        "{hdr}fn main() {{\n\
         let o: Option[R] = Some(mk(1));\n\
         let _ = match o {{ Some(r) => r, None => mk(9) }};\n\
         println(\"end\");\n}}\n"
    );
    assert_eq!(
        run(&boxed).trim(),
        "dR1\nend",
        "[bare arm handing out a heap-carrying Option payload, B-2026-08-31-28]"
    );
    // The bare-STATEMENT spelling, which diverged the same way and is closed
    // by the same guard.
    let boxed_stmt = format!(
        "{hdr}fn main() {{\n\
         let o: Option[R] = Some(mk(1));\n\
         match o {{ Some(r) => r, None => mk(9) }};\n\
         println(\"end\");\n}}\n"
    );
    assert_eq!(
        run(&boxed_stmt).trim(),
        "dR1\nend",
        "[bare STATEMENT spelling, B-2026-08-31-28]"
    );
}

/// The two controls that localize the fix above: a discarded CALL result and a
/// read-only arm both already ran exactly one body on every backend, and must
/// keep doing so — they are what prove the change did not widen into
/// discarded temporaries generally or into arm bindings generally.
#[test]
fn test_discarded_match_controls_keep_their_single_body() {
    let hdr = "struct R { id: i64 }\n\
               impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\") } }\n";
    let call = format!("{hdr}fn mk() -> R {{ return R {{ id: 1 }}; }}\nfn main() {{ mk(); println(\"dropped\"); }}\n");
    assert_eq!(run(&call).trim(), "dR1\ndropped", "[discarded call result]");
    let read = format!(
        "{hdr}fn main() {{\n\
         let o: Option[R] = Some(R {{ id: 1 }});\n\
         match o {{ Some(r) => {{ println(f\"saw{{r.id}}\") }} None => {{ println(\"none\") }} }};\n\
         println(\"dropped\");\n}}\n"
    );
    assert_eq!(run(&read).trim(), "saw1\ndR1\ndropped", "[read-only arm]");
}

/// B-2026-08-31-27 — the DEFAULT-LEG half of
/// `tests/codegen.rs`'s `e2e_destructuring_arm_runs_each_drop_body_exactly_once`.
/// That twin is the real gate (it pins both backends against each other), but it
/// is `--features llvm` only; this one runs on the leg CI executes on every push,
/// so an interpreter regression cannot wait for the codegen job.
///
/// The interpreter ran a payload's `Drop` body ZERO times whenever the pattern
/// reached through a container: `arm_moved_user_drop_payload_bindings` collected
/// a binding only from a bare-name sub-pattern, and `is_drop_binding`'s struct
/// arm asked only whether the bound type declares a `Drop` of its OWN while the
/// enum arm beside it asked the transitive question.
///
/// Every expected value is what `karac build` prints — the compiled backends
/// were correct at all of these and the interpreter was not, so it is not its
/// own oracle here.
#[test]
fn interp_a_destructuring_arm_runs_each_payload_drop_body_once() {
    const PRELUDE: &str = "struct R { s: String }\n\
         impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.s}\"); } }\n\
         struct P { r: R, n: i64 }\n\
         enum E { A { r: R }, B }\n\
         enum T { A(R), B }\n\
         enum W { C(P), D }\n";
    let cases: &[(&str, &str, &str)] = &[
        (
            "`Option` + struct payload destructure",
            "let o: Option[P] = Some(P { r: R { s: \"a\" }, n: 4 });\n\
             match o { Some(P { r, .. }) => println(r.s), None => {} }",
            "a\ndRa\nend\n",
        ),
        (
            "`Option` + enum STRUCT-variant destructure",
            "let o: Option[E] = Some(E.A { r: R { s: \"a\" } });\n\
             match o { Some(E.A { r }) => println(r.s), Some(E.B) => {}, None => {} }",
            "a\ndRa\nend\n",
        ),
        (
            "`Option` + enum TUPLE-variant destructure",
            "let o: Option[T] = Some(T.A(R { s: \"a\" }));\n\
             match o { Some(T.A(r)) => println(r.s), Some(T.B) => {}, None => {} }",
            "a\ndRa\nend\n",
        ),
        (
            "`Result` + struct payload destructure",
            "let o: Result[P, i64] = Ok(P { r: R { s: \"a\" }, n: 4 });\n\
             match o { Ok(P { r, .. }) => println(r.s), Err(_) => {} }",
            "a\ndRa\nend\n",
        ),
        (
            "USER enum + struct payload destructure",
            "let w: W = W.C(P { r: R { s: \"a\" }, n: 4 });\n\
             match w { W.C(P { r, .. }) => println(r.s), W.D => {} }",
            "a\ndRa\nend\n",
        ),
        (
            "whole-bound payload whose type has only a Drop-bearing FIELD",
            "let o: Option[P] = Some(P { r: R { s: \"a\" }, n: 4 });\n\
             match o { Some(p) => println(p.n), None => {} }",
            "4\ndRa\nend\n",
        ),
        // Controls — each already agreed with the compiled backends before the
        // fix, and each is a path the widening must not disturb.
        (
            "control: bare enum scrutinee",
            "let e: E = E.A { r: R { s: \"a\" } };\n\
             match e { E.A { r } => println(r.s), E.B => {} }",
            "a\ndRa\nend\n",
        ),
        (
            "control: NESTED match over a whole-bound payload",
            "let o: Option[E] = Some(E.A { r: R { s: \"a\" } });\n\
             match o { Some(p) => { match p { E.A { r } => println(r.s), E.B => {} } }, None => {} }",
            "a\ndRa\nend\n",
        ),
        (
            "control: the payload IS the Drop type",
            "let o: Option[R] = Some(R { s: \"a\" });\n\
             match o { Some(r) => println(r.s), None => {} }",
            "a\ndRa\nend\n",
        ),
        (
            "control: binds only the NON-Drop field",
            "let h: P = P { r: R { s: \"a\" }, n: 4 };\n\
             match h { P { n, .. } => println(n) }",
            "4\ndRa\nend\n",
        ),
    ];
    for (label, stmts, want) in cases {
        let src = format!("{PRELUDE}fn main() {{\n    {stmts}\n    println(\"end\");\n}}\n");
        assert_eq!(run(&src), *want, "{label}");
    }
}

/// B-2026-08-31-47, shape 1 — a fresh-temp enum argument whose arm BINDS the
/// payload still runs its body.
///
/// Disarming the scrutinee's payload walk is a HAND-OFF: the arm binds the
/// payload out and its new owner runs the body. In a method frame reached with
/// a fresh temp there is no such owner — the arm defers to the caller
/// (`scrutinee_expr_is_consuming`'s caller-retains carve-out) and the caller
/// has no binding to defer to — so the body ran nowhere.
///
/// The three rows are the isolation, and they are what shows this is about the
/// BINDING arm rather than about enum arguments or method frames generally:
/// `nomatch` and the wildcard `Full(_)` were correct throughout, and only
/// `Full(r)` lost the body.
///
/// `handed-back` is the guard on the other edge. There the arm's payload
/// ESCAPES through the return, so the caller's RESULT binding owns it; arming
/// the walk for that shape too runs two bodies against one compiled, which an
/// earlier version of this fix did.
///
/// Twin: `tests/codegen.rs`'s
/// `test_e2e_method_fresh_temp_enum_arg_arm_binds_payload`, same programs and
/// expectations — the property is that the two backends agree.
#[test]
fn method_fresh_temp_enum_arg_arm_binds_payload() {
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
        assert_eq!(run(&src), want, "{label}");
    }
}

/// B-2026-08-31-32 — a fresh-temp STRUCT scrutinee's arm bindings own a `Drop`
/// slot.
///
/// The arm-stash gate read `matches!(scrutinee, Value::EnumVariant { .. })`, so
/// a struct scrutinee registered nothing and `match P { .. } { P { r, .. } => }`
/// ran NO body against one on both compiled backends. The exact mirror of
/// B-2026-08-30-55, whose gate was `Value::Struct`-only and missed enums.
///
/// Five rows, and the two that must NOT change are the reason the fix is a
/// gate rather than an unconditional widening:
///
/// * `fresh-literal` / `fresh-call` — the reported hole. A value built or
///   returned AT the scrutinee has no other owner.
/// * `bound-local` — the control that a naive widening breaks. `p` owns its own
///   field walk, and stashing beside it printed the body TWICE (measured);
///   a struct has no `moved_out_enum_payload_bindings` equivalent to retract
///   that walk with, which is why the enum path can admit a named place here
///   and this one cannot.
/// * `no-binding` — a `..`-only arm binds nothing. THIS CELL WAS PINNED AT THE
///   WRONG ANSWER (`"nb\n"`, zero bodies) from 2026-08-31 until B-2026-09-16-18:
///   the two backends did agree, and they agreed on losing the husk's field
///   bodies outright and leaking the heap under them. The doc here read "nothing
///   changes … pinned so a later widening cannot quietly make this backend the
///   odd one out", which is exactly how a shared gap survives a fixture — the
///   pin was watching for a DIVERGENCE and the defect was an AGREEMENT.
///   B-2026-09-16-18 flips it to `"nb\ndR[d]\n"` on both backends together.
/// * `own-drop-struct` — the scrutinee declares its own `Drop`. Included
///   because it is the shape where a partial move out of a `Drop`-bearing
///   struct meets this gate, and both backends agree on the field body.
///
/// Twin: `tests/codegen.rs`'s
/// `test_e2e_fresh_temp_struct_scrutinee_arm_binding_runs_its_body`.
#[test]
fn fresh_temp_struct_scrutinee_arm_binding_runs_its_body() {
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
            // B-2026-09-16-18 — the husk of a fresh temp whose arm binds nothing
            // is still owned by the match: its fields' `Drop` bodies run, in
            // reverse declaration order, after the arm. Flipped from `"nb\n"`.
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
        assert_eq!(
            run(&format!("{H}fn main() {{\n{body}\n}}\n")),
            want,
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
fn iflet_letelse_fresh_temp_husk_fields_run_their_drop_bodies() {
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
        assert_eq!(run(&src), want, "{label}");
    }
}

/// B-2026-09-21-6 — a fresh-temp struct scrutinee whose type has its OWN
/// `impl Drop` ran NOTHING on this backend, where all three compiled surfaces
/// ran the drop body and the field walk after it.
///
/// The husk walker B-2026-09-21-1 gave these constructs returned outright on an
/// own-`Drop` struct, on the reasoning that codegen's
/// `materialize_freshtemp_struct_scrutinee` `has_user_drop` leg "is already
/// correct for a fresh temp". It is — on codegen. The interpreter's other
/// channel for that body (`freshtemp_scrutinee_user_drop_type`) accepts a call
/// or method-call scrutinee and NOTHING else, so a struct LITERAL temp fell
/// between the two and `match Od { a: mk(50), b: mk(51) } { Od { .. } => … }`
/// printed nothing at all against `dOd dR51 dR50` compiled. Correctness on one
/// backend was read as correctness.
///
/// The two channels OVERLAP on exactly the temps the helper does accept, which
/// is why the fix is a flag on the stash rather than a second unconditional
/// body: arming the walker alone made `match mkod(80)` run `dOd dR81 dR80`
/// TWICE (measured). The set site answers "do I owe the body?" from the
/// scrutinee EXPRESSION, because the walker only ever sees a `Value` and by
/// then the expression that produced it is gone.
///
/// Ten cells: every construct that can take a fresh-temp struct scrutinee
/// (`match`, `if let`, `while let`, `let ... else`) at BOTH spellings, plus the
/// row's guarded shape and a named control. Four move and six do not, and the
/// six that do not are as much the point as the four that do:
///
/// * `match-literal` / `iflet-literal` / `letelse-literal` /
///   `match-guarded-literal` — the reported hole. Each printed NOTHING before
///   the fix (measured on a control arm), and each now matches the compiled
///   oracle.
/// * the four `*-call` / `whilelet-*` cells — a CALL scrutinee is the shape the
///   other channel already owned, and `while let` was correct at BOTH
///   spellings. These were right before the fix and must stay right: they are
///   what a double-fire regression trips over, and arming the walker without
///   the flag made `match-call` print its whole transcript TWICE.
/// * `named-control` — a bound local owns its own body and walk; the husk
///   channel must stay out of it.
///
/// Both spellings of each construct are listed because they reach the flag by
/// OPPOSITE routes (literal: the walker owes the body; call: the helper does),
/// and a spelling-dependent split is this family's recurring defect.
///
/// Twin: `tests/codegen.rs`'s
/// `test_e2e_freshtemp_own_drop_struct_scrutinee_runs_its_body`.
#[test]
fn freshtemp_own_drop_struct_scrutinee_runs_its_body() {
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
            // The row's FOURTH spelling. It is the only GUARDED shape this type
            // can legally take: `partial_move_of_drop_struct` rejects an arm
            // that binds a field out of an own-`Drop` struct, so a guarded
            // match over one can only bind nothing.
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
        assert_eq!(run(&src), want, "{label}");
    }
}

/// B-2026-09-21-3 — `while let` over a STRUCT scrutinee ran no `Drop` body at
/// all on this backend, while every compiled surface ran the bound field's.
///
/// A run-vs-build DIVERGENCE rather than this family's usual agreed gap, which
/// is why it was split out of B-2026-09-21-1 instead of listed in it. The gate
/// in `eval_expr.rs`'s `ExprKind::WhileLet` admitted `Value::EnumVariant` only,
/// so a struct scrutinee produced an empty stash; the `match` leg
/// (`pattern_match.rs`) and the `if let` leg (B-2026-09-06-35, in the same
/// file) had both already been widened. Fourth copy of one test, third
/// widening, same symptom each time: one spelling printing differently from
/// its siblings on an identical program.
///
/// FOUR CELLS CHANGE AND SIX DO NOT, measured against `origin/main`:
/// `whilelet-one-bound`, `-all-bound`, `-nested-pattern` and `-two-iterations`
/// each ran NO body there (`z=76` against the compiled `dR76 z=76`, and so on),
/// and the other six were already correct. The six are the point of the
/// fixture as much as the four — `whilelet-named-scrutinee` in particular was
/// ALREADY `dR86` before this fix, from the scrutinee's own walk, so it is the
/// double-fire guard for the stash-plus-mask path this change newly admits.
/// `whilelet-enum-control` and `-user-enum-control` pin the arm that was
/// already there; `iflet-sibling-control` and `match-sibling-control` pin the
/// two spellings this one now matches.
///
/// `whilelet-none-bound-agreed-gap` PINS A DELIBERATELY WRONG ANSWER. A fresh
/// temp's UNBOUND fields are owned by nobody here either, so `S3 { .. }` runs
/// nothing and leaks both buffers — but that gap is AGREED on all four
/// surfaces and is B-2026-09-21-1's to close on both backends at once. Fixing
/// it here alone would turn an agreed gap into a second divergence, which is
/// strictly worse. `whilelet-one-bound` carries the same deliberate omission:
/// `dR77` is absent and owed.
///
/// No ASAN twin is owed: this fix is interpreter-only and moves no compiled
/// allocation. The leak belongs to B-2026-09-21-1.
///
/// Twin: `tests/codegen.rs`'s
/// `test_e2e_while_let_struct_scrutinee_binding_runs_its_drop_body`.
#[test]
fn while_let_struct_scrutinee_binding_runs_its_drop_body() {
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
        let src = format!("{H}{cell}\nfn main() {{ let z: i64 = c(); println(f\"z={{z}}\"); }}\n");
        assert_eq!(run(&src), want, "{label}");
    }
}

/// B-2026-08-31-32 — the `if let` spelling moves with `match`.
///
/// Both reach one shared place test rather than a second copy, because this
/// family has had to close a spelling-dependent split twice already
/// (B-2026-08-28-63, B-2026-08-29-17). The `bound-local` row is the same
/// double-fire guard as in the `match` fixture.
#[test]
fn fresh_temp_struct_scrutinee_if_let_matches_the_match_spelling() {
    const H: &str = "struct R { s: String }\n\
         impl Drop for R { fn drop(mut ref self) { println(f\"dR[{self.s}]\") } }\n\
         struct P { r: R, n: i64 }\n";
    for (label, body, want) in [
        (
            "fresh-literal",
            "if let P { r, .. } = P { r: R { s: \"a\" }, n: 1 } { println(r.s) }",
            "a\ndR[a]\n",
        ),
        (
            "bound-local",
            "let p = P { r: R { s: \"b\" }, n: 1 };\n\
             if let P { r, .. } = p { println(r.s) }",
            "b\ndR[b]\n",
        ),
    ] {
        assert_eq!(
            run(&format!("{H}fn main() {{\n{body}\n}}\n")),
            want,
            "{label}"
        );
    }
}

/// B-2026-09-01-28 — `match`, `if let` and `while let` agree on a payload
/// struct whose `Drop` is only in a FIELD.
///
/// The drop-slot filter existed as FOUR copies. B-2026-08-31-27 widened the
/// struct arm from "declares its own `Drop`" to the transitive
/// `value_runs_user_drop` at the two `match` copies and left the two `let`-form
/// copies on the narrow test, so `struct P { r: R }` — no `Drop` of its own, a
/// Drop-bearing field — registered no slot in the `let` forms and ran no body.
///
/// All four now call one `pattern_binding_owes_drop_body`. This fixture is the
/// guard on that: it fails if any site is ever re-specialized, which is how the
/// previous three splits in this family arose.
///
/// `own-drop-payload` is the control that shows what the narrow test DID catch,
/// and must keep catching — a payload declaring its own `Drop` was correct in
/// every spelling throughout.
#[test]
fn pattern_spellings_agree_on_a_field_only_drop_payload() {
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
            run(&format!("{H}fn main() {{\n{body}\n}}\n")),
            want,
            "{label}"
        );
    }

    // `while let` drains a driver, so it pins that EVERY iteration's body runs
    // — the shape lost both of them before the fix, which a single-iteration
    // fixture could not have distinguished from losing one.
    assert_eq!(
        run("struct R { s: String }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"dR[{self.s}]\") } }\n\
             struct P { r: R, n: i64 }\n\
             struct C { mut i: i64 }\n\
             impl C { fn next(mut ref self) -> Option[P] {\n\
             \x20   self.i = self.i + 1;\n\
             \x20   if self.i > 2 { return Option.None; }\n\
             \x20   return Option.Some(P { r: R { s: f\"w{self.i}\" }, n: self.i });\n\
             } }\n\
             fn main() {\n\
             \x20   let mut c = C { i: 0 };\n\
             \x20   while let Some(p) = c.next() { println(p.n) }\n\
             \x20   println(\"end\");\n\
             }\n"),
        "1\ndR[w1]\n2\ndR[w2]\nend\n",
        "while-let: one body per iteration"
    );
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
/// Twin of `tests/codegen.rs`'s `e2e_iflet_bare_tuple_elem_runs_one_body`, pinned to the same string.
#[test]
fn test_iflet_bare_tuple_elem_runs_one_body() {
    assert_eq!(
        run(r#"struct R { id: i64, tag: String, xs: Vec[i64] }
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
"#),
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

/// B-2026-09-05-9 — interpreter twin of `tests/codegen.rs`'s
/// `e2e_agg_leaf_boxed_payload_moving_arm_frees_envelope`, same program and
/// string. The interpreter was the correct reference throughout.
#[test]
fn test_agg_leaf_boxed_payload_moving_arm_frees_envelope() {
    assert_eq!(
        run(r#"struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
struct W { id: i64, x: String, y: String }
impl Drop for W { fn drop(mut ref self) { println(f"dW{self.id}/{self.x}{self.y}") } }
struct HoW { a: R, b: Result[W, String] }
fn mkw(k: i64) -> W { return W { id: k, x: f"x{k}", y: f"y{k}" } }
fn pickr(k: i64) -> W { let t: (R, Result[W, String]) = (R { id: k }, Result.Ok(mkw(k * 11))); let (a, b) = t; match b { Result.Ok(w) => w, Result.Err(e) => mkw(0) } }
fn picko(k: i64) -> W { let t: (R, Option[W]) = (R { id: k }, Option.Some(mkw(k * 11))); let (a, b) = t; match b { Option.Some(w) => w, Option.None => mkw(0) } }
fn pickboth(ok: bool) -> W { let t: (R, Result[W, W]) = (R { id: 13 }, if ok { Result.Ok(mkw(1313)) } else { Result.Err(mkw(1331)) }); let (a, b) = t; match b { Result.Ok(w) => w, Result.Err(w) => w } }
fn main() {
    { let t: (R, Result[W, String]) = (R { id: 1 }, Result.Ok(mkw(11))); let (a, b) = t; match b { Result.Ok(w) => { let g: W = w; println(f"g{g.id}") }, Result.Err(e) => println("err") } println("one") }
    { let w: W = pickr(2); println(f"got{w.id}"); println("two") }
    { let t: (R, Result[W, String]) = (R { id: 3 }, Result.Ok(mkw(33))); let (a, b) = t; if let Result.Ok(w) = b { let g: W = w; println(f"g{g.id}") } println("three") }
    { let t: (R, Option[W]) = (R { id: 4 }, Option.Some(mkw(44))); let (a, b) = t; match b { Option.Some(w) => { let g: W = w; println(f"g{g.id}") }, Option.None => println("none") } println("four") }
    { let w: W = picko(5); println(f"got{w.id}"); println("five") }
    { let t: (R, Option[W]) = (R { id: 6 }, Option.Some(mkw(66))); let (a, b) = t; if let Option.Some(w) = b { let g: W = w; println(f"g{g.id}") } println("six") }
    { let t: (R, Result[String, W]) = (R { id: 7 }, Result.Err(mkw(77))); let (a, b) = t; match b { Result.Ok(s) => println(f"ok{s}"), Result.Err(w) => { let g: W = w; println(f"g{g.id}") } } println("seven") }
    { let h: HoW = HoW { a: R { id: 8 }, b: Result.Ok(mkw(88)) }; let HoW { a, b } = h; match b { Result.Ok(w) => { let g: W = w; println(f"g{g.id}") }, Result.Err(e) => println("err") } println("eight") }
    { let t: (R, Result[W, String]) = (R { id: 9 }, Result.Err("e9")); let (a, b) = t; match b { Result.Ok(w) => { let g: W = w; println(f"g{g.id}") }, Result.Err(e) => println(f"err{e}") } println("nine") }
    { let t: (R, Option[W]) = (R { id: 10 }, Option.None); let (a, b) = t; match b { Option.Some(w) => { let g: W = w; println(f"g{g.id}") }, Option.None => println("none") } println("ten") }
    { let t: (R, Result[R, String]) = (R { id: 12 }, Result.Ok(R { id: 1212 })); let (a, b) = t; match b { Result.Ok(r) => { let g: R = r; println(f"g{g.id}") }, Result.Err(e) => println("err") } println("twelve") }
    { let w: W = pickboth(true); let v: W = pickboth(false); println(f"got{w.id}{v.id}"); println("thirteen") }
    println("end")
}
"#),
        "dR1\ng11\ndW11/x11y11\none\ndR2\ngot22\ndW22/x22y22\ntwo\ndR3\ng33\ndW33/x33y33\nthree\ndR4\ng44\ndW44/x44y44\nfour\ndR5\ngot55\ndW55/x55y55\nfive\ndR6\ng66\ndW66/x66y66\nsix\ndR7\ng77\ndW77/x77y77\nseven\ndR8\ng88\ndW88/x88y88\neight\ndR9\nerre9\nnine\ndR10\nnone\nten\ndR12\ng1212\ndR1212\ntwelve\ndR13\ndR13\ngot13131331\ndW1331/x1331y1331\ndW1313/x1313y1313\nthirteen\nend\n"
    );
}

/// B-2026-09-03-34 — interpreter twin of `tests/codegen.rs`'s
/// `e2e_match_arm_struct_payload_binding_runs_its_field_bodies`, same program
/// and string. The interpreter was the correct reference throughout.
#[test]
fn test_match_arm_struct_payload_binding_runs_its_field_bodies() {
    assert_eq!(
        run(r#"struct R { id: i64, tag: String, xs: Vec[i64] }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
struct Ho2 { a: R, b: Option[R] }
struct Two { a: R, b: R }
struct Nest { inner: Two, z: i64 }
enum Wrap { W(Ho2), T(Two), Nn(Nest), N }
fn mk(k: i64) -> R { return R { id: k, tag: f"t{k}", xs: [k] } }
fn main() {
    { let w: Wrap = Wrap.W(Ho2 { a: mk(52), b: Option.Some(mk(152)) }); match w { Wrap.W(h) => { let Ho2 { a, b } = h; println("in") }, _ => println("n") } println("one") }
    { let w: Wrap = Wrap.W(Ho2 { a: mk(55), b: Option.Some(mk(155)) }); match w { Wrap.W(h) => { println("in") }, _ => println("n") } println("two") }
    { let w: Wrap = Wrap.W(Ho2 { a: mk(56), b: Option.Some(mk(156)) }); if let Wrap.W(h) = w { let Ho2 { a, b } = h; println("in") } println("three") }
    { let w: Wrap = Wrap.T(Two { a: mk(57), b: mk(157) }); match w { Wrap.T(h) => { let Two { a, b } = h; println("in") }, _ => println("n") } println("four") }
    { let w: Wrap = Wrap.T(Two { a: mk(58), b: mk(158) }); match w { Wrap.T(h) => { let Two { a, b: _ } = h; println("in") }, _ => println("n") } println("five") }
    { let w: Wrap = Wrap.Nn(Nest { inner: Two { a: mk(59), b: mk(159) }, z: 1 }); match w { Wrap.Nn(h) => { let Nest { inner, z } = h; println("in") }, _ => println("n") } println("six") }
    { let w: Wrap = Wrap.T(Two { a: mk(60), b: mk(160) }); match w { Wrap.T(h) => { println(f"use{h.a.id}") }, _ => println("n") } println("seven") }
    { let w: Wrap = Wrap.T(Two { a: mk(61), b: mk(161) }); match w { Wrap.T(h) => { let g: Two = h; println("in") }, _ => println("n") } println("eight") }
    { let w: Wrap = Wrap.T(Two { a: mk(62), b: mk(162) }); match w { Wrap.T(h) => { println("in") }, _ => println("n") } println("nine") }
    { let w: Wrap = Wrap.T(Two { a: mk(63), b: mk(163) }); match w { _ => println("n") } println("ten") }
    println("end")
}
"#),
        "dR152\ndR52\nin\none\nin\ndR155\ndR55\ntwo\ndR156\ndR56\nin\nthree\ndR157\ndR57\nin\nfour\ndR158\ndR58\nin\nfive\ndR159\ndR59\nin\nsix\nuse60\ndR160\ndR60\nseven\ndR161\ndR61\nin\neight\nin\ndR162\ndR62\nnine\nn\ndR163\ndR63\nten\nend\n"
    );
}

/// B-2026-09-05-27 — interpreter twin of `tests/codegen.rs`'s
/// `e2e_match_arm_handing_out_a_tuple_element_frees_it_once`, same program and
/// string. The interpreter was the correct reference throughout.
#[test]
fn test_match_arm_handing_out_a_tuple_element_frees_it_once() {
    assert_eq!(
        run(r#"struct R { id: i64, tag: String, xs: Vec[i64] }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
struct S1 { id: i64, tag: String }
impl Drop for S1 { fn drop(mut ref self) { println(f"dS{self.id}") } }
enum E { A(R), B }
fn mk(i: i64) -> R { return R { id: i, tag: f"t{i}", xs: [i] } }
fn mk1(i: i64) -> S1 { return S1 { id: i, tag: f"t{i}" } }
fn p4(t: (R, i64)) -> R { match t { (r, k) => { r } } }
fn p4r(t: (R, i64)) -> R { match t { (r, k) => { return r } } }
fn p1(t: (S1, i64)) -> S1 { match t { (r, k) => { r } } }
fn pe(e: E) -> R { match e { E.A(r) => { r }, E.B => mk(0) } }
fn pd(t: (R, i64)) -> R { let x: R = match t { (r, k) => r }; return x }
fn pl() -> R { let t: (R, i64) = (mk(13), 0); match t { (r, k) => { r } } }
fn pc(t: (R, i64)) -> R { match t { (r, k) => { let g: R = r; g } } }
fn main() {
    { let a: R = p4((mk(3), 0)); println(f"got{a.id}"); println("one") }
    { let a: R = p4((mk(3), 0)); let b: R = p4((mk(6), 0)); println(f"got{a.id}{b.id}"); println("two") }
    { let a: R = p4r((mk(4), 0)); let b: R = p4r((mk(7), 0)); println(f"got{a.id}{b.id}"); println("three") }
    { let a: S1 = p1((mk1(5), 0)); let b: S1 = p1((mk1(8), 0)); println(f"got{a.id}{b.id}"); println("four") }
    { let a: R = pe(E.A(mk(9))); let b: R = pe(E.A(mk(10))); println(f"got{a.id}{b.id}"); println("five") }
    { let t: (R, i64) = (mk(11), 0); let a: R = p4(t); let u: (R, i64) = (mk(12), 0); let b: R = p4(u); println(f"got{a.id}{b.id}"); println("six") }
    { let a: R = pd((mk(14), 0)); println(f"got{a.id}"); println("seven") }
    { let a: R = pl(); println(f"got{a.id}"); println("eight") }
    { let a: R = pc((mk(15), 0)); println(f"got{a.id}"); println("nine") }
    println("end")
}
"#),
        "got3\ndR3\none\ngot36\ndR6\ndR3\ntwo\ngot47\ndR7\ndR4\nthree\ngot58\ndS8\ndS5\nfour\ngot910\ndR10\ndR9\nfive\ngot1112\ndR12\ndR11\nsix\ngot14\ndR14\nseven\ngot13\ndR13\neight\ngot15\ndR15\nnine\nend\n"
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
/// B-2026-09-03-39 — THE FRESH TUPLE SOURCE, the WILDCARD half of the pair
/// this fixture recorded as knowingly absent. The nine `fr*` cells are pinned;
/// six of them were red on the compiled backends and none ever moved on this
/// side. The BINDING leaf of the same literal is still split and still absent
/// — the twins share ONE string, so it cannot live here until it agrees.
///
/// THIS HALF IS THE INVARIANT, and that is why it is worth its own file. The
/// defect was codegen-only at a different site from the `w*` cells' —
/// `infer_arg_elem_te` erased an enum-constructor element's generic argument,
/// so the leaf walkers got a bare `Option` where they needed `Option[R]` —
/// and the interpreter, which walks VALUES rather than reconstructed types,
/// printed the same transcript before and after. Pinning it here is what
/// makes the codegen twin's expected string an assertion about the compiled
/// backends rather than about the program.
///
/// Twin of `tests/codegen.rs`'s
/// `e2e_wildcard_tuple_leaf_over_optres_owns_its_payload_body`, pinned to the
/// same string.
#[test]
fn test_wildcard_tuple_leaf_over_optres_owns_its_payload_body() {
    let out = run(r#"struct R { id: i64, tag: String }
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
"#);
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

/// B-2026-09-04-10 — the INTERPRETER half of
/// `e2e_option_agg_destructure_leaf_move_disarms_its_source`, pinned to the
/// same string.
///
/// This side never moved: the defect was a compiled-backend double free — a
/// tuple destructure leaf over `Option[<struct>]`, made the payload's sole
/// owner by B-2026-09-03-15, whose source was not disarmed when the leaf was
/// moved whole. The interpreter walks values rather than a static roster of
/// armed slots, so it printed this transcript before and after. Pinning it is
/// what makes the codegen twin's expected string an assertion about the
/// compiled backends rather than about the program.
#[test]
fn test_option_agg_destructure_leaf_move_disarms_its_source() {
    let out = run(r#"struct R { id: i64, s: String, xs: Vec[i64] }
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
"#);
    assert_eq!(
        out,
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
///
/// Interpreter twin of `e2e_result_agg_leaf_moving_arm_zeroes_the_whole_payload` (tests/codegen.rs) — same program string, same
/// pin.
#[test]
fn test_result_agg_leaf_moving_arm_zeroes_the_whole_payload() {
    let out = run(r#"struct R { id: i64, tag: String }
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
"#);
    assert_eq!(
        out,
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
    );
}

/// B-2026-09-03-22 — the INTERPRETER half of
/// `e2e_result_agg_destructure_leaf_owns_its_payload_body`, pinned to the same
/// string.
///
/// NOT A NO-OP HERE. Eight of these lines were missing on this side too: the
/// defect was agreed-wrong rather than a split, because
/// `record_destructure_optres_payload_tes` filtered its leaves to `Option` for
/// exactly as long as the compiled side declined `Result` — deliberately, since
/// running the body here alone would have been the divergence the family's
/// rules forbid. Both filters moved together, so this file is half the
/// regression rather than the invariant it usually is.
///
/// The two cells this side was ALREADY right about — `consok` and `conserr`,
/// where an arm binds the payload out — are the ones the compiled backends lost
/// separately.
#[test]
fn test_result_agg_destructure_leaf_owns_its_payload_body() {
    let out = run(r#"struct R { id: i64, s: String, xs: Vec[i64] }
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
"#);
    assert_eq!(
        out,
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
    );
}

/// B-2026-09-05-28 / B-2026-09-05-30 — a match arm over an OWNED by-value
/// tuple parameter runs the body of every element that dies in the call
/// exactly once, whichever the arm does with it: moves it into a by-value
/// callee (`consume(r)`, -28), never reads it (`(r, k) => k`, -30), wildcards
/// it, rebinds then consumes it. The caller retains an owned tuple argument
/// and runs its elements' bodies after the call, skipping the elements the
/// callee hands out; that skip list came from `fn_returns_param_payload`,
/// which stood the WHOLE argument down as soon as any arm binding left the
/// frame — and counted `k` (an `i64`) leaving, or `r` passed to ANY call, as
/// leaving. Seventeen of the twenty-one cells here printed no `dR` under
/// `--interp` against one on every compiled backend. The tuple pattern now
/// answers per ELEMENT (`fn_returns_param_tuple_arm_elems`, program-aware for
/// the forwarded-call case), and the whole-param predicate no longer answers
/// for it — the same split codegen always had, which is why the compiled
/// column was correct throughout.
///
/// `one`/`two` are the rows' cells; `three`/`four` the RETURN and field-READ
/// spellings that were already correct; `six`/`seven`/`nine` rebind, wildcard
/// and `let`-destructure; `ten`/`eleven`/`twentysix` the named-local argument
/// spelling (the binding's own element walk, masked per element); `twelve` and
/// `fourteen` a two-`Drop` tuple, where the unread sibling's body must survive
/// the other's escape; `sixteen` a nested tuple; `seventeen`/`eighteen` the
/// METHOD path; the rest conditional, nested-`match` and two-arm bodies, each
/// on both paths. Codegen twin: `tests/codegen.rs`'s
/// `e2e_match_arm_element_moved_into_a_callee_or_unread_runs_one_body`, same
/// program and string.
#[test]
fn test_match_arm_element_moved_into_a_callee_or_unread_runs_one_body() {
    assert_eq!(
        run(r#"struct R { id: i64, tag: String, xs: Vec[i64] }
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
"#),
        "dR1\nr1\none\ndR2\nr0\ntwo\nr3\ndR3\nthree\ndR4\nr4\nfour\ndR6\nr6\nsix\ndR7\nr0\nseven\ndR9\nr9\nnine\ndR10\nr10\nten\ndR11\nr0\neleven\ndR13\nr12\ndR12\ntwelve\ndR14\ndR15\nr14\nfourteen\ndR16\nr0\nsixteen\ndR17\nr117\nseventeen\ndR18\nr100\neighteen\ndR19\nr19\nnineteen\ndR20\nr0\ntwenty\ndR21\nr21\ntwentyone\ndR22\nr5\ntwentytwo\ndR23\nr23\ntwentythree\ndR24\nr3\ntwentyfour\ndR25\ndR26\nr25\ntwentysix\nend\n"
    );
}

/// B-2026-09-05-28's program-aware half, on the interpreter alone: an
/// element FORWARDED through a call that returns it (`wrap(r)`) or handed to
/// a callee that stores it under a `mut ref` parameter (`stash(r, v)`) is
/// owned by the result / the container, so the caller's walk must stand down
/// for that element and no other. The coarse predicate got these right by
/// accident (any call counted); the per-element one asks the callee. Not a
/// four-surface twin because every compiled backend runs a SECOND body on
/// exactly these cells (B-2026-09-05-33) — this pins the interpreter's answer
/// so the codegen fix has a reference to meet.
#[test]
fn test_match_arm_element_forwarded_or_stashed_keeps_one_owner_interp() {
    assert_eq!(
        run(r#"struct R { id: i64, tag: String, xs: Vec[i64] }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, tag: f"t{i}", xs: [i] } }
fn wrap(x: R) -> R { return x }
fn stash(x: R, v: mut ref Vec[R]) { v.push(x) }
fn t_fwd(t: (R, i64)) -> R { match t { (r, k) => { wrap(r) } } }
fn t_stash(t: (R, i64), v: mut ref Vec[R]) -> i64 { match t { (r, k) => { stash(r, v); k } } }
fn main() {
    { let a: R = t_fwd((mk(4), 0)); println(f"r{a.id}"); println("four") }
    { let mut v: Vec[R] = []; let d: i64 = t_stash((mk(6), 0), mut v); println(f"r{d} n{v.len()}"); println("six") }
    { let t: (R, i64) = (mk(14), 0); let a: R = t_fwd(t); println(f"r{a.id}"); println("fourteen") }
    println("end")
}
"#),
        "r4\ndR4\nfour\nr0 n1\ndR6\nsix\nr14\ndR14\nfourteen\nend\n"
    );
}

/// B-2026-09-05-33 — interpreter twin of `tests/codegen.rs`'s
/// `e2e_match_arm_element_escaping_by_call_store_or_assignment_has_one_owner`,
/// same program and string. The interpreter was the reference on the five
/// free-function cells; the two METHOD cells (`six`, `twelve`) were its own:
/// `callee_owns_arg_beyond_call`'s method leg stood a Drop-returning method's
/// whole tuple argument down by RETURN TYPE, which lost the unread sibling's
/// body (`m_two`) and, through the `continue` it triggers, skipped the
/// named-local element mask (`h.m_ret(t)` ran the handed-out element's body
/// twice). A tuple parameter is now exempt from that leg — its elements are
/// classified one at a time by `callee_escaping_tuple_elems`, which is at
/// least as conservative (a constructor wrap of an element counts there).
#[test]
fn test_match_arm_element_escaping_by_call_store_or_assignment_has_one_owner() {
    assert_eq!(
        run(r#"struct R { id: i64, tag: String, xs: Vec[i64] }
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
"#),
        "r1\ndR1\none\nr0 n1\ndR2\ntwo\nr0 n1\ndR3\nthree\ndR50\nr4\ndR4\nfour\nr5\ndR5\nfive\ndR6\nr7\ndR7\nsix\nr9 n1\ndR8\neight\nr10\ndR10\nten\nr0 n1\ndR11\neleven\nr12\ndR12\ntwelve\nr1314\ndR14\ndR13\nthirteen\nend\n"
    );
}

/// B-2026-09-05-35 — interpreter twin of `tests/codegen.rs`'s
/// `e2e_enum_payload_consumed_or_unread_in_the_arm_runs_one_body`, same
/// program and string. Both backends consult the same predicate for an enum
/// argument, so every cell was agreed-and-wrong here exactly as compiled and
/// the two move together; the interpreter asks with the argument's RUNTIME
/// variant (`callee_owns_arg_beyond_call`'s new `variant` parameter, and the
/// binding's value at the two identifier-argument gates).
#[test]
fn test_enum_payload_consumed_or_unread_in_the_arm_runs_one_body() {
    assert_eq!(
        run(r#"struct R { id: i64, tag: String, xs: Vec[i64] }
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
"#),
        "dR1\nr1\none\ndR2\nr5\ntwo\ndR3\nr3\nthree\nr4\ndR4\nfour\nr5\ndR5\nfive\nr1 n1\ndR6\nsix\nr1 n1\ndR7\nseven\ndR8\nr9\neight\ndR9\nr9\nnine\ndR10\nr5\nten\ndR11\nr5\neleven\ndR12\nr12\ntwelve\ndR13\nr5\nthirteen\ndR14\nr114\nfourteen\ndR15\nr100\nfifteen\ndR16\nr16\nsixteen\ndR17\nr5\nseventeen\nr18\ndR18\neighteen\nr19\nnineteen\nend\n"
    );
}

/// B-2026-09-06-20 — a `match` (or `if let`) that destructures a mixed
/// wrap's VIEW slot runs the view's `Drop` body once. `let w = W2.Two(r,
/// mk(2)); match w { W2.Two(a, b) => .. }` bound `a` out of the slot the
/// scrutinee's own mask (`moved_out_enum_payload_slots`) marks as the
/// caller's and gave it a Drop slot of its own — `dR2 dR1 dR1` here against
/// `dR2 dR1` on every compiled surface. Such a binding is now a view
/// (`masked_payload_view_names`, shared by the `match`, `if let` and
/// `while let` legs): into the view set, out of the stash, per slot. Twin of
/// `tests/codegen.rs`'s `e2e_match_over_a_masked_wrap_slot_binds_a_view`,
/// same program and string.
///
/// `one`..`four` direct / rebound / `a` read / unread, `five` the `if let`
/// spelling, `six` `let m = a` inside the arm (the compiled side needed the
/// view mark for this one), `seven` the view in the other slot, `eight` the
/// tuple sibling (always right).
#[test]
fn test_match_over_a_masked_wrap_slot_binds_a_view() {
    assert_eq!(
        run(r#"struct R { id: i64, name: String }
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
"#),
        "dR2\ndR1\nv=2\none\ndR4\ndR3\nv=4\ntwo\ndR6\ndR5\nv=5\nthree\ndR8\ndR7\nv=1\nfour\ndR10\ndR9\nv=10\nfive\ndR12\ndR11\nv=11\nsix\ndR14\ndR13\nv=14\nseven\ndR20\nv=21\nten\nend\n"
    );
}

/// B-2026-09-06-22 — a `match` (or `if let`) that destructures a mixed
/// STRUCT literal's view field binds a view. The row is the compiled side's
/// (`dR2 dR1 dR1` against this backend's `dR2 dR1`); this backend was right
/// on the direct read — a named local's masked struct walk is its own single
/// owner — but gave `let m = a;` inside the arm a body of its own, so the
/// struct arm of `masked_payload_view_names` (reading
/// `param_view_struct_fields`) puts the field binding into the view set.
/// Twin of `tests/codegen.rs`'s
/// `e2e_match_over_a_masked_struct_field_binds_a_view`, same program and
/// string.
///
/// `one`..`three` direct / `a` read / unread, `four` rebound, `five` the
/// `if let` spelling, `six` `let m = a` inside the arm, `seven` the view in
/// the other field, `eight` a fresh literal (no view), `nine` a partial
/// pattern (always right).
#[test]
fn test_match_over_a_masked_struct_field_binds_a_view() {
    assert_eq!(
        run(r#"struct R { id: i64, name: String }
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
"#),
        "dR2\ndR1\nv=2\none\ndR4\ndR3\nv=3\ntwo\ndR6\ndR5\nv=1\nthree\ndR8\ndR7\nv=8\nfour\ndR10\ndR9\nv=10\nfive\ndR12\ndR11\nv=11\nsix\ndR14\ndR13\nv=14\nseven\ndR17\ndR16\ndR15\nv=16\neight\ndR21\ndR20\nv=21\nten\nend\n"
    );
}

/// B-2026-09-06-33 — twin of `tests/codegen.rs`'s
/// `e2e_partial_or_reordered_struct_let_pattern_binds_by_name`, same
/// program and string. The interpreter was already right here (it binds
/// struct-pattern fields by name); the twin pins the agreed output so the
/// compiled backends' by-position extraction cannot silently come back.
///
/// `one` a partial pattern over a Drop-bearing struct with a view in the
/// other field, `two`..`five` scalar partial / reordered / middle-field /
/// renamed-with-rest patterns.
#[test]
fn test_partial_or_reordered_struct_let_pattern_binds_by_name() {
    assert_eq!(
        run(r#"struct R { id: i64, name: String }
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
"#),
        "dR5\ndR4\nv=5\none\nv=3\ntwo\nv=321\nthree\nv=2\nfour\nv=31\nfive\nend\n"
    );
}

/// B-2026-09-16-17 + B-2026-09-06-21 — an ENUM VARIANT's payload fields drop in
/// REVERSE declaration order, and a READ-ONLY match arm binding does not take
/// ownership of the payload.
///
/// design.md § `Drop` Field drop order: "Within a single struct **or enum
/// variant**, fields are dropped in the reverse of the order they are
/// declared". The struct walker did that; the enum one ran DECLARATION order,
/// so `struct P { a: R, b: R }` printed `dR2 dR1` while `enum E { T(R, R) }`
/// printed `dR1 dR2`. BOTH BACKENDS AGREED on the wrong answer, which is why no
/// A/B check ever reported it — the compiled twin is
/// `e2e_enum_payload_fields_drop_in_reverse_declaration_order`.
///
/// design.md § Match Arm Binding Modes settles the second half: "If the
/// scrutinee is an owned value ... bindings that are only read BORROW from the
/// already-owned value". So a read-only arm binding owes no `Drop` of its own —
/// the husk keeps the payload and it dies in the ENCLOSING scope's LIFO, after
/// a local declared later. The interpreter instead stashed such bindings as
/// real arm-scoped `Drop` slots and ran the bodies at the ARM's end, i.e.
/// BEFORE that later local. B-2026-09-06-21 read this the other way round —
/// "the compiled arm walker is the side to move" — which is backwards, and is
/// the one thing worth carrying forward from it.
///
/// `two_locals` is the cell that pins the position: `w` declared first, `z`
/// second, both live into the arm, so `z` dies first. `single`, `wildcard` and
/// `guard` are the same shape through the other three spellings. `consuming`
/// and `fresh_temp` are the controls that were ALWAYS agreed — an arm that
/// genuinely consumes a binding does take the payload, per the same spec
/// sentence — and they must not move.
#[test]
fn test_enum_payload_drops_reverse_and_readonly_arm_bindings_are_borrows() {
    let out = run(r#"
struct R { id: i64, name: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, name: f"nnnnnnnn{i}" } }
struct P2 { a: R, b: R }
enum E2 { T(R, R), N }
enum E1 { O(R), N1 }
fn take(r: R) -> i64 { return r.id }
fn structs() { let p: P2 = P2 { a: mk(1), b: mk(2) }; println("-structs") }
fn enums() { let w: E2 = E2.T(mk(1), mk(2)); println("-enums") }
fn two_locals() -> i64 {
    let w: E2 = E2.T(mk(1), mk(2));
    let z: R = mk(3);
    match w { E2.T(a, b) => { return a.id + z.id; } E2.N => { return 0; } }
}
fn single() -> i64 {
    let w: E1 = E1.O(mk(4));
    let z: R = mk(5);
    match w { E1.O(a) => { return a.id + z.id; } E1.N1 => { return 0; } }
}
fn wildcard() -> i64 {
    let w: E2 = E2.T(mk(6), mk(7));
    let z: R = mk(8);
    match w { E2.T(a, _) => { return a.id + z.id; } E2.N => { return 0; } }
}
fn consuming() -> i64 {
    let w: E2 = E2.T(mk(9), mk(10));
    let z: R = mk(11);
    match w { E2.T(a, b) => { return take(a) + b.id + z.id; } E2.N => { return 0; } }
}
fn fresh_temp() -> i64 {
    let z: R = mk(12);
    match E2.T(mk(13), mk(14)) { E2.T(a, b) => { return a.id + z.id; } E2.N => { return 0; } }
}
fn main() {
    structs(); enums();
    println(f"a={two_locals()}");
    println(f"b={single()}");
    println(f"c={wildcard()}");
    println(f"d={consuming()}");
    println(f"e={fresh_temp()}");
}
"#);
    assert_eq!(
        out,
        // structs: reverse, as it always was. enums: reverse, the fix.
        "dR2\ndR1\n-structs\n\
         dR2\ndR1\n-enums\n\
         dR3\ndR2\ndR1\na=4\n\
         dR5\ndR4\nb=9\n\
         dR8\ndR7\ndR6\nc=14\n\
         dR10\ndR9\ndR11\nd=30\n\
         dR14\ndR13\ndR12\ne=25\n",
        "got:\n{out}"
    );
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
/// Twin of `tests/codegen.rs`'s
/// `e2e_owned_enum_receiver_runs_its_payload_body_when_no_arm_claims_it`, byte-identical source and expectation — the only fixture
/// shape that can hold an agreed gap closed.
#[test]
fn test_owned_enum_receiver_runs_its_payload_body_when_no_arm_claims_it() {
    let out = run(r#"struct R { id: i64, tag: String, xs: Vec[i64] }
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
"#);
    assert_eq!(out, "none\n  x5\n  dE\n  dR1\ntemp\n  dE\n  dR2\n  x5\nnodrop\n  x5\n  dR3\ngeneric\n  x5\n  dR4\nmatches\n  x5\n  dE\n  dR5\nret_self\n  dE\n  dR6\n  dE\n  got\nwrap\n  dE\n  dR7\n  dE\n  got\nrefm\n  x3\n  dE\n  dR8\nplain\n  dE\n  dR9\n  x9\nend\n", "got:\n{out}");
}

/// B-2026-09-10-14 — the INTERPRETER twin of `tests/codegen.rs`'s
/// `e2e_whole_payload_arm_binding_over_a_tuple_payload_runs_element_bodies`,
/// byte-identical source and expectation.
///
/// This side was wrong in exactly the same way and had to move in the same
/// commit: the gap was AGREED, so fixing one backend alone would have turned a
/// missing body into an A/B divergence. The two disarms now ask one shared
/// predicate (`binding_use::optres_arm_takes_whole_payload`) about the same
/// AST, which is what keeps them from drifting apart again.
#[test]
fn test_whole_payload_arm_binding_over_a_tuple_payload_runs_element_bodies() {
    let out = run(r#"struct R { id: i64, name: String }
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
"#);
    assert_eq!(out, "bind\n  x\n  hit\n  dR1\n  dR2\nread\n  x\n  hit3\n  dR3\n  dR4\niflet\n  x\n  hit\n  dR5\n  dR6\nwhilet\n  x\n  hit\n  dR7\n  dR8\n  end\nresult\n  x\n  hit\n  dR9\n  dR10\ntwice\n  x\n  a\n  b\n  dR11\n  dR12\nafter\n  x\n  hit\n  dR13\n  dR14\n  end\nwild\n  x\n  hit\n  dR15\n  dR16\nstruct\n  x\n  hit\n  dW18\n  dR17\nsingle\n  x\n  hit\n  dR19\nret\n  x\n  dR30\n  dR31\n  hit\neat\n  x\n  eat\n  hit7\nnomatch\n  dR22\n  dR23\n  x\nnone\n  x\n  n\ntlocal\n  dR24\n  dR25\n  x\nend\n", "got:\n{out}");
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
/// Twin of `tests/codegen.rs`'s `e2e_read_only_arm_on_owned_enum_receiver_orders_payload_after_shell`, pinned to the same string.
#[test]
fn test_read_only_arm_on_owned_enum_receiver_orders_payload_after_shell() {
    assert_eq!(
        run(r#"struct R { id: i64, tag: String, xs: Vec[i64] }
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
"#),
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
/// The CODEGEN twin is `tests/codegen.rs`'s
/// `e2e_destructured_payload_keeps_the_unmoved_parts_body`, byte-identical
/// source and expectation — and byte-identical is the assertion here, since an
/// agreed defect can only be closed by moving both backends together.
#[test]
fn test_destructured_payload_keeps_the_unmoved_parts_body() {
    let out = run(r#"struct R { id: i64 }
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
"#);
    assert_eq!(out, "scalarout\n  dR5\n  got:9\ndropout\n  dR7\n  got:6\n  dR6\nresultout\n  dR8\n  got:9\nwildkept\n  dR10\n  got:9\nmiddleout\n  dR11\n  dR13\n  got:12\n  dR12\nbothout\n  got:14\n  dR14\n  dR15\nreadonly\n  dR16\n  dR17\n  got:33\nwholeout\n  got:9\n  dR18\ncondfalse\n  got:20\n  dR20\ngenout\n  dR21\n  got:9\nmethod\n  dR22\n  got:9\nunused\n  dR24\n  dR23\n  x\nend\n", "got:\n{out}");
}

/// B-2026-09-19-30 — a WILDCARD leaf in a heap-BOXED destructured payload made
/// its NAMED siblings read from the wrong offset, returning a silently wrong
/// value on every compiled backend.
///
/// The row that filed this read the symptom as "the arm returns the `None`
/// arm's value", because the cell's `None` arm returned literal 0 and the bug
/// returned 0. It is not: with the `None` arm changed to return 77 the wildcard
/// spelling still returns 0, and in the FIRST position (`Some((b, _))` over
/// `Option[(i64, H)]`) it returns a pointer-shaped integer. The arm is selected
/// correctly — the payload's `Drop` body runs — and only the binding is wrong.
///
/// The cause is that the payload's width was computed from the PATTERN.
/// `pattern_payload_word_count` sizes each leaf from the type the typechecker
/// recorded for it, and a `_` binds nothing so nothing was recorded: it fell to
/// the 1-word default. That sum is what the debox predicate in
/// `reconstruct_payload_value` tests (`want > field_words.len()`), so one
/// under-counted leaf made a boxed payload look inline, and the arm rebuilt the
/// tuple out of the ENVELOPE words instead of loading through the box. The
/// envelope's word 0 is the box POINTER, which is where the pointer-shaped
/// integer came from; word 1 is past the end of the area, which is the zero.
///
/// The fix records the wildcard's type in `check_pattern_against`, which is
/// handed it and used to walk past. The IR diff is the whole defect in two
/// lines: the named spelling emits `inttoptr` + `load` of the real tuple, the
/// wildcard spelling emitted `insertvalue { i64, i64 }` straight from the
/// envelope words.
///
/// `named` is the control that was always correct, `allwild` the one that binds
/// nothing and so never read an offset, and `narrow` the payload that rides
/// INLINE (`struct R { id: i64 }`, under the three-word area) and therefore
/// never took the boxed channel at all. `strwild` is a `String` leaf with no
/// user struct in the payload, `twoof3` and `twowild` are the arities where the
/// wildcard is not a lone prefix, and `okside` / `errside` are the two `Result`
/// channels — all measured wrong before the fix, all agreeing after.
///
/// The `if let` spelling is deliberately ABSENT: it loses the payload's `Drop`
/// body on the compiled backends, which reproduces with this fix reverted and
/// is a separate defect with its own row. Its VALUE is fixed here like the
/// rest; including the cell would pin that missing body instead.
///
/// Byte-identical to the codegen twin, which is the assertion, and identical on
/// `--interp`, the JIT, `-O0` and `-O2`; valgrind reports no errors and no
/// leaks on the compiled program.
#[test]
fn test_wildcard_leaf_in_a_boxed_payload_binds_its_siblings_at_the_right_offset() {
    let out = run(r#"struct H { id: i64, s: String }
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
"#);
    assert_eq!(out, "second\n  dH1\n  n9\nsecond2\n  dH2\n  n4242\nfirst\n  dH3\n  n9\nnamed\n  dH4\n  n9\ntwoof3\n  dH5\n  n109\nallwild\n  dH6\n  n55\ntwowild\n  dH7\n  dH8\n  n66\nstrwild\n  n88\nnarrow\n  n44\nokside\n  dH10\n  n11\nerrside\n  dH11\n  n22\nnonearm\n  n77\nend\n", "got:\n{out}");
}

/// B-2026-09-14-18 (BOXED leg) — the same defect on the OTHER channel, where
/// the payload is too wide to sit inline and the bodies belong to the CALLEE.
///
/// A payload of at most three words rides inside the envelope and the CALLER
/// keeps its `Drop` bodies, which is the channel the sibling fixture
/// (`test_destructured_payload_keeps_the_unmoved_parts_body`) pins. A wider one
/// — `(H, i64)` with `H { id: i64, s: String }` — heap-boxes, and the box's
/// interior walk becomes the ONLY holder of the bodies. Both channels had the
/// same one-bit answer and so the same defect, and fixing only the one that
/// happened to be measured first would have left the two backends disagreeing
/// on the other.
///
/// Measured before the fix, identical on `--interp`, the JIT, `-O0` and `-O2`
/// auto-par: `scalarOut` printed `got:9` where `dH1 got:9` is due, and the
/// `(H, H)` cells lost whichever element the arm did not return. Agreed and
/// wrong, so no A/B gate saw it.
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
/// `test_wildcard_leaf_in_a_boxed_payload_binds_its_siblings_at_the_right_offset`
/// (B-2026-09-19-30). Keeping the two apart is what lets each keep measuring
/// one thing.
///
/// The CODEGEN twin is `tests/codegen.rs`'s
/// `e2e_boxed_destructured_payload_keeps_the_unmoved_parts_body`,
/// byte-identical source and expectation — and byte-identical is the assertion
/// here, since an agreed defect can only be closed by moving both backends
/// together.
#[test]
fn test_boxed_destructured_payload_keeps_the_unmoved_parts_body() {
    let out = run(r#"struct H { id: i64, s: String }
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
"#);
    assert_eq!(out, "scalar\n  dH1\n  got:9\nreadscalar\n  in2\n  dH2\n  got:8\nfirst\n  dH4\n  got:3\n  dH3\nsecond\n  dH5\n  got:6\n  dH6\nmiddle\n  dH7\n  dH9\n  got:8\n  dH8\nbothtouched\n  keep11\n  dH11\n  got:10\n  dH10\nnoneout\n  dH12\n  dH13\n  got:25\nresultout\n  dH14\n  got:7\nend\n", "got:\n{out}");
}

/// Interpreter twin of `e2e_tuple_pattern_destructures_through_a_borrow`
/// (B-2026-09-23-28). The `let (name, k) = ref ps[i]` line is the half this
/// backend got wrong on its own: the borrow evaluates to an element reference
/// rather than a tuple, and the tuple arm of `bind_pattern` bound nothing, so
/// the first read of `name` died with "resolved but has no binding at run
/// time".
#[test]
fn test_tuple_pattern_destructures_through_a_borrow() {
    let out = run(r#"
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
    println(f"{total(vec![(1, 2), (3, 4)])} {nested(vec![(1, (2, 3)), (4, (5, 6))])}");
    let mut ps = vec![("ab".to_string(), 5)];
    println(f"{muts(mut ps)} {ps.len()}");
    let mut m: Map[String, i64] = Map.new();
    m.insert("ab".to_string(), 3);
    m.insert("c".to_string(), 4);
    let u = ("abc".to_string(), 2);
    println(f"{map_total(m)} {pair(u)} {by_index(ps)}");
}
"#);
    assert_eq!(out, "46 41\n7 2\n10 5 306\n");
}

/// Interpreter twin of `e2e_iterator_terminals_take_destructuring_params_and_enumerate`
/// and `e2e_for_over_a_call_that_returns_a_slice` (B-2026-09-23-29,
/// B-2026-09-23-30). The interpreter always ran these; the twin pins the values
/// the compiled fixtures are compared against.
#[test]
fn test_iterator_terminals_destructure_and_slice_calls_iterate() {
    let out = run(r#"
struct G { nbr: Vec[i64] }
impl G {
    fn part(ref self, a: i64, b: i64) -> Slice[i64] { return self.nbr[a..b]; }
}
fn main() {
    let label = vec![0, 0, 2, 1, 4];
    let v = vec![(1, 2), (5, 3), (4, 6)];
    println(f"{label.iter().enumerate().filter(|(i, l)| i == l).count()} {label.iter().enumerate().map(|(i, l)| i * l).sum()}");
    println(f"{v.iter().map(|(a, b)| a * b).sum()} {v.iter().filter(|(a, b)| a < b).count()} {v.iter().position(|(a, b)| a > b)}");
    let g = G { nbr: vec![5, 6, 7, 8] };
    let mut s = 0;
    for w in g.part(1, 3) { s += w; }
    for (i, w) in g.part(0, 4).iter().enumerate() { s += i * w * 100; }
    println(f"{s}");
}
"#);
    assert_eq!(out, "3 23\n41 2 Some(1)\n4413\n");
}
