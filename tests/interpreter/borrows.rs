//! ref and mut ref params, borrows, escape analysis, elision -- fixtures for `tests/interpreter.rs`.
//!
//! Split out of `tests/interpreter.rs` on 2026-09-21. The TEST TARGET is
//! unchanged: this file is a module of that target, so
//! `cargo test --features llvm --test interpreter` still runs everything
//! and CI needs no edit. Run this area alone with:
//!
//!     cargo test --features llvm --test interpreter borrows::
//!
//! New fixtures about ref and mut ref params, borrows, escape analysis, elision belong in this file.

use super::*;

/// B-2026-09-01-15 — the interpreter twin of `tests/codegen.rs`'s
/// `e2e_ref_binding_over_nested_chain_roots`, pinned to the same string.
///
/// This side handled every root already — each container evaluates to a
/// `Value::Array` its existing arm matches — so this half is the ORACLE the
/// compiled side was built against and passes before the fix too. It is here
/// so the two stay pinned to one string as roots are added.
#[test]
fn test_ref_binding_over_nested_chain_roots() {
    assert_eq!(
        run("struct Hh { grid: Vec[Vec[i64]] }\n\
             fn main() {\n\
             \x20   let av: Array[Vec[i64], 1] = Array[[1, 2]];\n\
             \x20   let g1 = ref av[0][1];\n\
             \x20   println(f\"arrayvec {g1}\");\n\
             \x20   let b: Vec[Vec[i64]] = [[3, 4]];\n\
             \x20   let sl: Slice[Vec[i64]] = b.as_slice();\n\
             \x20   let g2 = ref sl[0][1];\n\
             \x20   println(f\"slicevec {g2}\");\n\
             \x20   let va: Vec[Array[i64, 2]] = [Array[5, 6]];\n\
             \x20   let g3 = ref va[0][1];\n\
             \x20   println(f\"vecarray {g3}\");\n\
             \x20   let aa: Array[Array[i64, 2], 1] = Array[Array[7, 8]];\n\
             \x20   let g4 = ref aa[0][1];\n\
             \x20   println(f\"arrayarray {g4}\");\n\
             \x20   let h: Hh = Hh { grid: [[9, 10]] };\n\
             \x20   let g5 = ref h.grid[0][1];\n\
             \x20   println(f\"field {g5}\");\n\
             \x20   let mut m: Vec[Array[i64, 2]] = [Array[1, 2]];\n\
             \x20   let ga = ref m[0][1];\n\
             \x20   m[0][1] = 66;\n\
             \x20   println(f\"alias {ga}\");\n\
             }"),
        "arrayvec 2\nslicevec 4\nvecarray 6\narrayarray 8\nfield 10\nalias 66\n"
    );
}

/// B-2026-08-26-36 — `let r = ref v[i]` is a LIVE BORROW, not a snapshot.
///
/// The aliasing is the contract, not an optimisation. Codegen compiles the
/// binding to a pointer into the container's buffer; the interpreter binds
/// `Value::ElemRef { arr, idx }` over the same `Arc`. If either side took a
/// copy, the two backends would disagree the moment the container changed
/// while the borrow was live — the run-vs-build divergence class
/// B-2026-08-26-21 exists to close. So the swap between the two reads is the
/// whole point of the fixture: it must be observed through `r`.
#[test]
fn ref_binding_is_a_live_alias_not_a_snapshot() {
    let out = run(r#"
fn main() {
    let mut v: Vec[i64] = Vec.new();
    v.push(4);
    v.push(5);
    let r = ref v[1];
    println(f"{r}");
    v.swap(0, 1);
    println(f"{r}");
}
"#);
    assert_eq!(out, "5\n4\n", "ref must observe the swap; got {out:?}");
}

#[test]
fn a_conditional_store_into_a_mut_ref_param_runs_one_body_on_both_paths() {
    // B-2026-08-30-28. The interpreter twin of
    // `e2e_conditional_store_that_misses_still_runs_one_body`, and the reason
    // it exists as a separate test: this row's fix had to move BOTH backends,
    // and a codegen-only fix would have left the interpreter losing the body —
    // a run-vs-build divergence rather than a shared gap.
    //
    // `take` stores its by-value parameter into a `mut ref` container on ONE
    // path. Before the fix nobody owned the body on the other: the caller stood
    // down because `fn_moves_param_into_outliving_place` saw a store, and the
    // callee registered nothing for the same reason, so `drop 7` simply never
    // printed on any of the four surfaces.
    //
    // The STORING path is asserted in the same program, because the two paths
    // fail in opposite directions — losing a body and doubling one — and a test
    // that pins only the miss would pass a fix that doubled the hit.
    let src = "struct Res { id: i64 }
        impl Drop for Res { fn drop(mut ref self) { println(f\"drop {self.id}\"); } }
        fn take(sink: mut ref Vec[Res], r: Res) { if r.id > 100 { sink.push(r); } }
        fn main() {
            let mut sink: Vec[Res] = Vec.new();
            take(mut sink, Res { id: 7 });
            println(\"--missed--\");
            let named: Res = Res { id: 9 };
            take(mut sink, named);
            println(\"--missed-named--\");
            take(mut sink, Res { id: 200 });
            println(f\"pushed {sink.len()}\");
        }";
    assert_eq!(
        run_no_errors(src),
        "drop 7\n--missed--\ndrop 9\n--missed-named--\npushed 1\ndrop 200\n",
        "each object runs its body exactly once: the two that were not stored die \
         in the callee, the one that was stored dies at the container's drain"
    );
}

#[test]
fn test_scalar_mut_ref_writeback_through_forwarded_call() {
    // A `mut ref i64` scalar accumulator FORWARDED into a nested call must
    // still propagate the callee's mutation back to the caller. The CICO
    // write-back keys on the callee's `mut ref` param mode, not the call-site
    // `mut` marker — a forwarded in-scope borrow carries no marker (design.md
    // § Call-site mutation markers), so a marker-only gate would drop the
    // chain and the accumulator would read 0. Regression for B-2026-07-03-13
    // (LeetCode #52 N-Queens II marker-array counter, all-zeros under `run`).
    assert_eq!(
        run("fn inc(x: mut ref i64) { x = x + 1i64; }\n\
             fn wrap(x: mut ref i64) { inc(x); }\n\
             fn main() {\n\
                 let mut t: i64 = 0;\n\
                 wrap(mut t);\n\
                 println(t);\n\
             }"),
        "1\n"
    );
}

#[test]
fn test_scalar_mut_ref_accumulator_through_recursion() {
    // The N-Queens-II shape: a `mut ref i64` total threaded down a recursion,
    // bumped only at the leaves. Each recursive call forwards the borrow
    // unmarked; every level must chain the write-back. bump(3) visits 2^3 = 8
    // leaves, so `total` must read 8.
    assert_eq!(
        run("fn bump(d: i64, t: mut ref i64) {\n\
                 if d == 0i64 { t = t + 1i64; return; }\n\
                 bump(d - 1i64, t);\n\
                 bump(d - 1i64, t);\n\
             }\n\
             fn main() {\n\
                 let mut t: i64 = 0;\n\
                 bump(3i64, mut t);\n\
                 println(t);\n\
             }"),
        "8\n"
    );
}

#[test]
fn test_with_provider_mut_ref_self_mutation_visible_across_calls() {
    // B-2026-07-31-4 — a `mut ref self` provider method's mutation must be
    // visible to later calls in the same `with_provider` scope
    // (design.md § Provider-Rooted Resources: mutation-visible-after-pop). The
    // provider is dispatched against a by-value clone bound to `self`, so
    // without a write-back into the provider frame the mutation was discarded:
    // two `bump()`s read back 0. Codegen already got this right (`2`); this is
    // the interpreter twin of that behaviour.
    let output = run(
        "trait Counter { fn bump(mut ref self); fn get(ref self) -> i64; }
         effect resource Ctr: Counter;
         struct InMem { n: i64 }
         impl Counter for InMem {
             fn bump(mut ref self) { self.n = self.n + 1; }
             fn get(ref self) -> i64 { self.n }
         }
         fn do_bump() with writes(Ctr) { Ctr.bump(); }
         fn read() -> i64 with reads(Ctr) { Ctr.get() }
         fn main() {
             let c = InMem { n: 0 };
             with_provider[Ctr](c, || { do_bump(); do_bump(); println(f\"{read()}\"); });
         }",
    );
    assert_eq!(output, "2\n");
}

#[test]
fn test_with_provider_ref_self_does_not_rebind_provider() {
    // A `ref self` reader must NOT trigger a write-back (it can't mutate), so
    // repeated reads see the original value unchanged. Guards against an
    // over-eager write-back that rebinds on every call.
    let output = run(
        "trait Counter { fn bump(mut ref self); fn get(ref self) -> i64; }
         effect resource Ctr: Counter;
         struct InMem { n: i64 }
         impl Counter for InMem {
             fn bump(mut ref self) { self.n = self.n + 1; }
             fn get(ref self) -> i64 { self.n }
         }
         fn peek() -> i64 with reads(Ctr) { let a = Ctr.get(); let b = Ctr.get(); return a + b; }
         fn main() {
             with_provider[Ctr](InMem { n: 5 }, || { println(f\"{peek()}\"); });
         }",
    );
    assert_eq!(output, "10\n");
}

#[test]
fn test_lowercase_alias_rand_advances_state() {
    // Lowercase `rand.next_u64()` dispatches to the same ambient
    // `RandomSource` as the capitalized form (via the interpreter's
    // lowercase→capitalized alias map). Two draws differ = state advanced.
    let output = run("fn main() {\n\
                          let a = rand.next_u64();\n\
                          let b = rand.next_u64();\n\
                          println(a != b);\n\
                      }");
    assert_eq!(output, "true\n");
}

#[test]
fn test_lowercase_alias_clock_with_provider_overrides_default() {
    // `with_provider[Clock]` must intercept a lowercase `clock.now()` call —
    // the alias routes through `eval_resource_method`, which consults the
    // provider stack exactly as the capitalized `Clock.now()` does.
    let output = run("struct FakeClock {}\n\
                      impl FakeClock { fn now(self) -> i64 { 999 } }\n\
                      fn main() {\n\
                          with_provider[Clock](FakeClock {}, || {\n\
                              println(clock.now());\n\
                          });\n\
                      }");
    assert_eq!(output, "999\n");
}

#[test]
fn test_local_var_shadows_lowercase_ambient_alias() {
    // A same-name local binding shadows the module alias: `let clock = Timer
    // { .. }; clock.now()` dispatches to the user's `Timer::now`, not the
    // ambient `Clock`. The interpreter alias map guards on `env.get(name)`
    // so the local wins — parity with codegen and the typechecker, which
    // both apply the same shadow guard. (Regression guard: before the guard
    // the interpreter ignored the local and called the ambient default.)
    let output = run("struct Timer { ticks: i64 }\n\
                      impl Timer { fn now(ref self) -> i64 { self.ticks } }\n\
                      fn main() {\n\
                          let clock = Timer { ticks: 42 };\n\
                          println(clock.now());\n\
                      }");
    assert_eq!(output, "42\n");
}

// ── Prefix dereference operator ───────────────────────────────────────────────

#[test]
fn test_deref_read_ref_param() {
    let output = run("fn read_val(r: ref i64) -> i64 { *r }\nfn main() { let x = 42_i64; println(read_val(x)); }");
    assert_eq!(output, "42\n");
}

#[test]
fn test_deref_read_mut_ref_param() {
    let output = run("fn read_val(r: mut ref i64) -> i64 { *r }\nfn main() { let x = 7_i64; println(read_val(x)); }");
    assert_eq!(output, "7\n");
}

#[test]
fn test_deref_write_through_mut_ref() {
    let output = run("fn set_val(r: mut ref i64) { *r = 99; }\nfn main() { let mut x = 1_i64; set_val(mut x); println(x); }");
    assert_eq!(output, "99\n");
}

#[test]
fn test_deref_double() {
    let output = run("fn double_in_place(r: mut ref i64) { *r = *r * 2; }\nfn main() { let mut n = 5_i64; double_in_place(mut n); println(n); }");
    assert_eq!(output, "10\n");
}

// ── Implicit assign-through on a `mut ref` scalar (B-2026-06-30-9/10) ──
// design.md § "Compound assignment on `mut ref` lvalues" (:5306): `a = a + b`
// on a `mut ref T` lvalue desugars to `*a = *a + b` — no explicit deref
// needed. The interpreter already applied this; these lock in run/build parity
// (the codegen counterparts live in tests/codegen.rs).
#[test]
fn test_mut_ref_scalar_implicit_assign_through() {
    // `x = x + 1i64` (no `*`) reads through the borrow and writes back.
    let output = run("fn inc(x: mut ref i64) { x = x + 1i64; }\n\
         fn main() { let mut n: i64 = 10i64; inc(mut n); println(n); }");
    assert_eq!(output, "11\n");
}

#[test]
fn test_mut_ref_scalar_compound_assign_through() {
    // `x += 1i64` on a `mut ref i64` — the compound-assign sibling.
    let output = run("fn inc(x: mut ref i64) { x += 1i64; }\n\
         fn main() { let mut n: i64 = 10i64; inc(mut n); println(n); }");
    assert_eq!(output, "11\n");
}

#[test]
fn test_interp_generic_refinement_alias_iterates_and_reads_fields() {
    // B-2026-07-29-26 end-to-end on the tree-walk lane: a generic
    // refinement alias narrowed with `as`, then iterated, with a field
    // read and a cast-out of a second refinement inside the loop — the
    // three projection rules together, which is exactly the shape
    // `examples/weave`'s `aggregate` uses. The typechecker rejected all
    // three before the fix, so this never got to run.
    let output = run_no_errors(
        r#"
type PositiveQty = i64 where self > 0;
type NonEmpty[T] = Vec[T] where self.len() > 0;
struct Item { price: f64, qty: PositiveQty }

fn total(rows: NonEmpty[Item]) -> f64 {
    let mut t = 0.0;
    for r in rows { t = t + r.price * (r.qty as f64); }
    t
}

fn main() {
    let v = vec![Item { price: 1.5, qty: 2 }, Item { price: 0.25, qty: 4 }];
    println(total(v as NonEmpty[Item]));
}
"#,
    );
    assert_eq!(output.trim(), "4");
}

// ── Returned borrows (`-> ref T`) — interpreter parity ──────────────
// Mirrors the codegen E2E shapes in tests/codegen.rs (B-2026-06-07-5) so
// `karac run` and `karac build` agree on every accepted borrow-return form:
// let-bound caller, conditional (`if`/`match`) selectors, method accessors,
// chained free-fn calls, and direct (unbound) use. The static-acceptance of
// these is pinned in tests/ownership.rs / tests/safety_design.rs; here we
// pin runtime output.

#[test]
fn test_borrow_return_interp_let_bound_caller() {
    let out = run("fn name_of(u: ref String) -> ref String { u }\n\
         fn main() {\n\
             let s = String.from(\"hello\");\n\
             let n = name_of(s);\n\
             println(n);\n\
         }");
    assert_eq!(out, "hello\n");
}

#[test]
fn test_borrow_return_interp_longer_if() {
    let out = run("fn longer(a: ref String, b: ref String) -> ref String {\n\
             if a.len() > b.len() { a } else { b }\n\
         }\n\
         fn main() {\n\
             let x = String.from(\"short\");\n\
             let y = String.from(\"a longer string\");\n\
             let z = longer(x, y);\n\
             println(z);\n\
         }");
    assert_eq!(out, "a longer string\n");
}

#[test]
fn test_borrow_return_interp_method_accessor() {
    let out = run("struct User { name: String, age: i64 }\n\
         impl User { fn name(ref self) -> ref String { self.name } }\n\
         fn main() {\n\
             let u = User { name: String.from(\"ada\"), age: 36 };\n\
             let n = u.name();\n\
             println(n);\n\
         }");
    assert_eq!(out, "ada\n");
}

#[test]
fn test_borrow_return_interp_chained_call() {
    let out = run("fn echo(s: ref String) -> ref String { s }\n\
         fn echo_twice(s: ref String) -> ref String {\n\
             let t = echo(s);\n\
             echo(t)\n\
         }\n\
         fn main() {\n\
             let s = String.from(\"world\");\n\
             let r = echo_twice(s);\n\
             println(r);\n\
         }");
    assert_eq!(out, "world\n");
}

#[test]
fn test_borrow_return_interp_direct_use() {
    let out = run("fn name_of(u: ref String) -> ref String { u }\n\
         fn shout(x: ref String) { println(x); }\n\
         fn main() {\n\
             let s = String.from(\"hello\");\n\
             println(name_of(s));\n\
             shout(name_of(s));\n\
             println(name_of(s).len());\n\
         }");
    assert_eq!(out, "hello\nhello\n5\n");
}

#[test]
fn test_borrow_return_interp_borrowed_struct() {
    // Borrowed-struct return parity (design.md Feature 4 Part 3): the
    // interpreter constructs `Parser` with a borrow of `s`, returns it, and
    // reads both the owned and borrowed fields. Mirrors the codegen E2E.
    let out = run("struct Parser { source: ref String, position: i64 }\n\
         fn make_parser(s: ref String) -> ref Parser {\n\
             Parser { source: s, position: 7 }\n\
         }\n\
         fn main() {\n\
             let s = String.from(\"input data\");\n\
             let p = make_parser(s);\n\
             println(p.position);\n\
             println(p.source);\n\
         }");
    assert_eq!(out, "7\ninput data\n");
}

// ── `mut ref self` receiver write-back (CICO) ──────────────────
//
// Regression for phase-12 self-hosting blocker #2: a `mut ref self`
// method's mutations to `self` were dropped on return in the tree-walk
// interpreter (the receiver was passed by value and never written back to
// the call-site place), making `karac run` unsound for any self-mutating
// method. Codegen was already correct; these pin the interpreter to the
// same semantics. The fix mirrors the free-function `mut ref T` CICO
// write-back: capture the post-body `self` and copy it back to the
// receiver place, gated strictly on `SelfParam::MutRef`.

#[test]
fn test_mut_ref_self_method_mutation_persists() {
    // The minimal repro from phase-12 §blocker #2.
    let out = run_no_errors(
        r#"
struct C { n: i64 }
impl C {
    fn inc(mut ref self) { self.n = self.n + 1; }
}
fn main() {
    let mut c = C { n: 0 };
    c.inc();
    c.inc();
    println(c.n);
}
"#,
    );
    assert_eq!(out, "2\n");
}

#[test]
fn test_mut_ref_self_nested_method_calls_propagate() {
    // A `mut ref self` method that mutates `self` *through another
    // self-method* (`self.inc()` inside `bump_twice`) — the inner call's
    // write-back targets the `SelfValue` place so the mutation propagates up
    // the receiver chain (the lexer's `skip_ws` → `self.adv()` shape).
    let out = run_no_errors(
        r#"
struct Counter { n: i64 }
impl Counter {
    fn inc(mut ref self) { self.n = self.n + 1; }
    fn bump_twice(mut ref self) { self.inc(); self.inc(); }
    fn get(ref self) -> i64 { self.n }
}
fn main() {
    let mut c = Counter { n: 0 };
    c.bump_twice();
    c.inc();
    println(c.get());
}
"#,
    );
    assert_eq!(out, "3\n");
}

/// B-2026-08-13-16 — the MUTATION half of the pair above. Its sibling pinned
/// that a field READ leaves the source intact; this pins that a WRITE through
/// the new binding is not visible through the old one.
///
/// The interpreter used to alias: `Value::Struct` holds its fields by value, so
/// cloning a struct copies the field map and Arc-BUMPS a `Vec` field — `let mut
/// t = a; t.lines.push("y")` was then visible through `a`. B-2026-08-01-27 had
/// already fixed exactly this for a bare `Vec` rebinding, but its guard was
/// pinned to `Value::Array` and an identifier RHS, which were the coordinates of
/// the shape reported at the time rather than the boundary of the behaviour.
///
/// design.md classes `let y = v` as a CONSUME, so the reuse must read an
/// independent copy — and the compiled backends were already on that answer,
/// which is the unusual part: the run-vs-build divergence here had the
/// INTERPRETER wrong. Every line below is 2 then 1; pre-fix each printed 2
/// then 2.
///
/// Seven positions, because the widening moved on two axes at once (value kind
/// and RHS shape) and one probe per axis would not have covered the corners.
/// The last two are pinned HERE only: `t.0`-rooted sources and a whole-tuple
/// rebind have their own, separate codegen defects, so they have no compiled
/// twin to compare against yet.
#[test]
fn test_let_rebinding_is_a_copy_not_an_alias() {
    assert_eq!(
        run("struct A { mut lines: Vec[String] }\n\
             struct B { mut a: A }\n\
             fn main() {\n\
                 let mut a1 = A { lines: Vec.new() };\n\
                 a1.lines.push(f\"x\");\n\
                 let mut r1 = a1;\n\
                 r1.lines.push(f\"y\");\n\
                 println(f\"{r1.lines.len()} {a1.lines.len()}\");\n\
                 let mut a2 = A { lines: Vec.new() };\n\
                 a2.lines.push(f\"x\");\n\
                 let mut r2 = a2.lines;\n\
                 r2.push(f\"y\");\n\
                 println(f\"{r2.len()} {a2.lines.len()}\");\n\
                 let mut inner = A { lines: Vec.new() };\n\
                 inner.lines.push(f\"x\");\n\
                 let mut b3 = B { a: inner };\n\
                 let mut r3 = b3.a;\n\
                 r3.lines.push(f\"y\");\n\
                 let chk3 = b3.a;\n\
                 println(f\"{r3.lines.len()} {chk3.lines.len()}\");\n\
                 let mut v4: Vec[A] = Vec.new();\n\
                 v4.push(A { lines: Vec.new() });\n\
                 v4[0].lines.push(f\"x\");\n\
                 let mut r4 = v4[0];\n\
                 r4.lines.push(f\"y\");\n\
                 println(f\"{r4.lines.len()} {v4[0].lines.len()}\");\n\
                 let mut m6: Map[i64, Vec[i64]] = Map.new();\n\
                 let _ = m6.insert(1, Vec.new());\n\
                 m6[1].push(1);\n\
                 let mut r6 = m6;\n\
                 r6[1].push(2);\n\
                 println(f\"{r6[1].len()} {m6[1].len()}\");\n\
                 let mut seed = A { lines: Vec.new() };\n\
                 seed.lines.push(f\"x\");\n\
                 let t5: (A, i64) = (seed, 1);\n\
                 let mut r5 = t5.0;\n\
                 r5.lines.push(f\"y\");\n\
                 let chk5 = t5.0;\n\
                 println(f\"{r5.lines.len()} {chk5.lines.len()}\");\n\
                 let mut v7: Vec[i64] = Vec.new();\n\
                 v7.push(1);\n\
                 let t7: (Vec[i64], i64) = (v7, 0);\n\
                 let mut r7 = t7;\n\
                 r7.0.push(2);\n\
                 println(f\"{r7.0.len()} {t7.0.len()}\");\n\
             }"),
        "2 1\n2 1\n2 1\n2 1\n2 1\n2 1\n2 1\n"
    );
}

/// B-2026-09-07-52 — interpreter twin of `tests/codegen.rs`'s
/// `e2e_borrowed_base_field_assign_displaced_bodies`, same source and expected
/// string. The interpreter's Assign leg flattens the target's base with the
/// same `Identifier`/`FieldAccess` walk codegen uses, so `self.f = <new>`
/// (parsed as `SelfValue`) declined the shape and the displaced value's body
/// was lost here too — this is not a run-vs-build divergence but one spelling
/// losing a body on every surface.
#[test]
fn test_borrowed_base_field_assign_displaced_bodies() {
    assert_eq!(
        run("struct Rs { id: i64, name: String }\n\
             impl Drop for Rs {\n\
                 fn drop(mut ref self) {\n\
                     println(f\"dS{self.id}\")\n\
                 }\n\
             }\n\
             fn mks(i: i64) -> Rs { return Rs { id: i, name: f\"h{i}\" }; }\n\
             struct Bs { mut one: Rs }\n\
             struct Outer { mut inner: Bs }\n\
             impl Bs {\n\
                 fn set(mut ref self, r: Rs) { self.one = r; }\n\
                 fn set_lit(mut ref self) { self.one = mks(9); }\n\
             }\n\
             impl Outer {\n\
                 fn set_deep(mut ref self, r: Rs) { self.inner.one = r; }\n\
             }\n\
             fn setf(h: mut ref Bs, r: Rs) { h.one = r; }\n\
             fn main() {\n\
                 println(\"c1\");\n\
                 let mut a = Bs { one: mks(1) };\n\
                 a.set(mks(7));\n\
                 println(f\"o{a.one.id}\");\n\
                 println(\"c2\");\n\
                 let mut b = Bs { one: mks(1) };\n\
                 b.one = mks(7);\n\
                 println(f\"o{b.one.id}\");\n\
                 println(\"c3\");\n\
                 let mut c = Bs { one: mks(1) };\n\
                 setf(mut c, mks(7));\n\
                 println(f\"o{c.one.id}\");\n\
                 println(\"c4\");\n\
                 let mut d = Bs { one: mks(1) };\n\
                 d.set_lit();\n\
                 println(f\"o{d.one.id}\");\n\
                 println(\"c5\");\n\
                 let mut e = Outer { inner: Bs { one: mks(1) } };\n\
                 e.set_deep(mks(7));\n\
                 println(f\"o{e.inner.one.id}\");\n\
                 println(\"end\");\n\
             }\n"),
        "c1\ndS1\no7\ndS7\nc2\ndS1\no7\ndS7\nc3\ndS1\no7\ndS7\n\
         c4\ndS1\no9\ndS9\nc5\ndS1\no7\ndS7\nend\n"
    );
}

/// B-2026-08-05-39 — the ORACLE half. The interpreter has always replaced the
/// pointee of a `mut ref` aggregate parameter on reassignment; codegen stored
/// the aggregate into the 8-byte alloca holding the borrow POINTER, so the
/// caller's value never changed and the store ran past the slot.
///
/// This test passed both before and after the fix, which is exactly its job:
/// it is the reference answer the codegen twin
/// `e2e_mut_ref_aggregate_param_reassignment_writes_through` is measured
/// against, and it pins that the interpreter does not drift onto codegen's old
/// behaviour. Seed is the literal 1 here rather than `env.args().len()` for the
/// usual reason — an in-process interpreter test would see the TEST binary's
/// argv.
#[test]
fn test_mut_ref_aggregate_param_reassignment_replaces_pointee() {
    assert_eq!(
        run(r#"struct Q { s: String, v: Vec[i64] }

fn mkv(k: i64) -> Vec[i64] {
    let mut v: Vec[i64] = Vec.new();
    v.push(k);
    v.push(k + 1i64);
    return v;
}

fn reps(x: mut ref String, k: i64) { x = f"fresh-{k}-payload"; }
fn repsc(x: mut ref String) { x = x + "tail"; }
fn repbind(x: mut ref String, k: i64) { let t: String = f"bind-{k}-payload"; x = t; }
fn repv(v: mut ref Vec[i64], k: i64) { v = mkv(k); }
fn repq(q: mut ref Q, k: i64) { q = Q { s: f"q-{k}-payload", v: mkv(k) }; }

fn main() {
    let n: i64 = 1i64;
    let mut acc: i64 = 0;
    let mut i: i64 = 0;
    while i < n + 199i64 {
        let mut s: String = f"seed-{i}-payload";
        reps(mut s, i);
        if s.contains("fresh") { acc = acc + 1i64; }
        repsc(mut s);
        if s.contains("tail") { acc = acc + 1i64; }
        repbind(mut s, i);
        if s.contains("bind") { acc = acc + 1i64; }
        let mut v: Vec[i64] = mkv(i);
        repv(mut v, i + 1i64);
        acc = acc + v[0i64] + v.len();
        let mut q: Q = Q { s: f"orig-{i}-payload", v: mkv(i) };
        repq(mut q, i);
        if q.s.contains("payload") { acc = acc + 1i64; }
        acc = acc + q.v[1i64];
        i = i + 1i64;
    }
    println(acc);
}
"#),
        "41400\n"
    );
}

#[test]
fn test_mut_ref_place_argument_writes_back() {
    // B-2026-08-05-37 — the interpreter half. A `mut ref` parameter given a
    // PLACE argument must write back into that place.
    //
    // The interpreter's CICO write-back only accepted a bare IDENTIFIER
    // argument; every projection `continue`d, so the callee's write was
    // silently discarded and the program printed the PRE-call value. Codegen
    // had the same bug through a different mechanism (a pointer to a shallow
    // copy), so the two AGREED on the wrong answer for most shapes — which is
    // why the usual run-vs-build oracle did not catch it. Only `bump(mut v[0])`
    // diverged, and only because codegen had an unrelated `vec[i]` arm.
    //
    // Keep in step with the codegen twin `e2e_mut_ref_place_argument_writes_back`
    // (same arms, same expected output). The seed is the literal 1 here rather
    // than `env.args().len()` for the reason given in the tuple-element twin
    // above: the codegen fixture needs an opaque seed to survive -O2, and 1 is
    // what that yields under its harness.
    assert_eq!(
        run("struct Q { v: i64 }\n\
             struct P { q: Q }\n\
             struct F { v: f64 }\n\
             struct B { v: bool }\n\
             struct S { v: i64 }\n\
             struct V { a: Vec[i64] }\n\
             fn bump(x: mut ref i64) { x = x + 1i64; }\n\
             fn bumpf(x: mut ref f64) { x = x + 1.0; }\n\
             fn setb(x: mut ref bool) { x = true; }\n\
             fn setq(x: mut ref Q) { x.v = 9i64; }\n\
             fn setg[T](x: mut ref T, d: T) { x = d; }\n\
             fn fwd(p: mut ref S) { bump(p.v); }\n\
             fn push2(v: mut ref Vec[i64]) { v.push(42i64); }\n\
             fn main() {\n\
                let n: i64 = 1i64;\n\
                let mut a: S = S { v: n + 6i64 };\n\
                bump(mut a.v);\n\
                println(f\"a:{a.v}\");\n\
                let mut b: P = P { q: Q { v: n + 6i64 } };\n\
                bump(mut b.q.v);\n\
                println(f\"b:{b.q.v}\");\n\
                let mut c: (i64, i64) = (n + 6i64, 0i64);\n\
                bump(mut c.0);\n\
                println(f\"c:{c.0}\");\n\
                let mut d: Vec[i64] = Vec.new();\n\
                d.push(n + 6i64);\n\
                bump(mut d[0i64]);\n\
                println(f\"d:{d[0i64]}\");\n\
                let mut e: Vec[S] = Vec.new();\n\
                e.push(S { v: n + 6i64 });\n\
                bump(mut e[0i64].v);\n\
                println(f\"e:{e[0i64].v}\");\n\
                let mut f: P = P { q: Q { v: n + 6i64 } };\n\
                setq(mut f.q);\n\
                println(f\"f:{f.q.v}\");\n\
                let mut g: F = F { v: 7.0 };\n\
                bumpf(mut g.v);\n\
                println(f\"g:{g.v as i64}\");\n\
                let mut h: B = B { v: false };\n\
                setb(mut h.v);\n\
                println(f\"h:{h.v}\");\n\
                let mut i2: S = S { v: n + 6i64 };\n\
                fwd(mut i2);\n\
                println(f\"i:{i2.v}\");\n\
                let mut j: S = S { v: n + 6i64 };\n\
                setg(mut j.v, n + 7i64);\n\
                println(f\"j:{j.v}\");\n\
                let mut k: V = V { a: Vec.new() };\n\
                k.a.push(n);\n\
                push2(mut k.a);\n\
                println(f\"k:{k.a.len()}:{k.a[1i64]}\");\n\
                let mut l: (Vec[i64], i64) = (Vec.new(), 0i64);\n\
                l.0.push(n);\n\
                push2(mut l.0);\n\
                println(f\"l:{l.0.len()}:{l.0[1i64]}\");\n\
             }\n"),
        "a:8\nb:8\nc:8\nd:8\ne:8\nf:9\ng:8\nh:true\ni:8\nj:8\nk:2:42\nl:2:42\n"
    );
}

/// B-2026-08-05-37, the conservative half: the write-back RE-EVALUATES the
/// place after the callee's scope is popped, so a subscript that is not
/// obviously pure is skipped rather than evaluated twice. `next(mut c)` must
/// run exactly once — evaluating it again would both double its side effect
/// and pick a different slot than the one the callee borrowed.
///
/// The skipped write is the pre-fix behaviour (lost), not a new wrong answer,
/// and this pins that choice so a later widening is deliberate. Codegen has no
/// such restriction (it passes a pointer computed once), so this shape is a
/// known interp/AOT divergence recorded on the row.
#[test]
fn test_mut_ref_place_writeback_skips_impure_subscript() {
    assert_eq!(
        run(
            "fn next(c: mut ref i64) -> i64 { c = c + 1i64; return 0i64; }\n\
             fn bump(x: mut ref i64) { x = x + 1i64; }\n\
             fn main() {\n\
                let mut c: i64 = 0i64;\n\
                let mut v: Vec[i64] = Vec.new();\n\
                v.push(7i64);\n\
                bump(mut v[next(mut c)]);\n\
                println(f\"{v[0i64]}:{c}\");\n\
             }\n"
        ),
        "7:1\n"
    );
}

#[test]
fn test_tuple_element_borrowed_in_place_for_ref_params() {
    // B-2026-08-05-1 and -2 — the ORACLE half. The interpreter has always
    // borrowed a tuple element in place for a `ref` / `mut ref` parameter;
    // codegen let it fall through to the rvalue path, which copies the
    // {ptr,len,cap} header into a temp. That temp's scope-exit free
    // double-freed a buffer the tuple still owned (arm a), and for `mut ref`
    // the callee mutated the COPY, so the write never reached the tuple and
    // the following indexed read panicked (arm b). Arms (e)/(f) are the
    // struct-field spelling, correct on both backends since B-2026-07-12-1 —
    // that asymmetry is what located the missing arm.
    //
    // Keep in step with the codegen twin
    // `e2e_tuple_element_borrowed_in_place_for_ref_params`. The seed is the
    // literal 1 here rather than `env.args().len()`: the codegen fixture needs
    // an opaque seed to survive -O2 folding and 1 is what that yields under its
    // harness, while `env.args()` in an in-process interpreter test would
    // report the TEST binary's argv.
    assert_eq!(
        run("struct H { a: Vec[i64], b: i64 }\n\
             fn mkv(k: i64) -> Vec[i64] { let mut v: Vec[i64] = Vec.new(); v.push(k); v.push(k + 1i64); return v; }\n\
             fn mks(k: i64) -> String { let mut s: String = String.new(); s.push_str(f\"pay-{k}\"); return s; }\n\
             fn dig(i: i64) -> String { let mut d: String = String.new(); d.push_str(f\"{i}\"); return d; }\n\
             fn peek(v: ref Vec[i64]) -> i64 { return v[0i64] + v.len(); }\n\
             fn bump(v: mut ref Vec[i64]) { v.push(42i64); }\n\
             fn slen(s: ref String) -> i64 { return s.len(); }\n\
             fn main() {\n\
                let n: i64 = 1i64;\n\
                // (a) ref param, TUPLE element — was a double free\n\
                let t1: (Vec[i64], i64) = (mkv(n), 5i64);\n\
                println(f\"a:{peek(t1.0)}:{t1.0[1i64]}\");\n\
                // (b) mut ref param, TUPLE element — the mutation was lost, then panicked\n\
                let mut t2: (Vec[i64], i64) = (mkv(n), 5i64);\n\
                bump(mut t2.0);\n\
                println(f\"b:{t2.0.len()}:{t2.0[2i64]}\");\n\
                // (c) ref param, tuple element in SECOND position\n\
                let t3: (i64, Vec[i64]) = (5i64, mkv(n));\n\
                println(f\"c:{peek(t3.1)}\");\n\
                // (d) ref param, STRING tuple element\n\
                let t4: (String, i64) = (mks(n), 5i64);\n\
                if t4.0.contains(dig(n)) { println(f\"d:{slen(t4.0)}\"); } else { println(\"d:BAD\"); }\n\
                // CONTROL: the struct-FIELD spelling, correct since B-2026-07-12-1\n\
                let h1: H = H { a: mkv(n), b: 5i64 };\n\
                println(f\"e:{peek(h1.a)}:{h1.a[1i64]}\");\n\
                let mut h2: H = H { a: mkv(n), b: 5i64 };\n\
                bump(mut h2.a);\n\
                println(f\"f:{h2.a.len()}:{h2.a[2i64]}\");\n\
                println(\"end\");\n\
             }\n"),
        "a:3:2\nb:3:42\nc:3\nd:5\ne:3:2\nf:3:42\nend\n"
    );
}

/// B-2026-08-10-4 — the interpreter twin of `split_at_mut`, pinned separately
/// from the codegen E2E because its failure mode is different and silent.
///
/// The interpreter normalizes a `Value::Slice` receiver into a fresh snapshot
/// `Array` before the seq method surface sees it — correct for the read-only
/// methods it was built for, fatal here. Without exempting `split_at_mut` from
/// that normalization the two halves window a DETACHED copy: their lengths are
/// right, reads through them are right, and every write is silently lost.
///
/// So the assertion has to write through a half and read back through the
/// ORIGINAL collection. Both receivers are covered because only the `mut
/// Slice` one goes through the normalization — measured before the fix, the
/// `Vec` case propagated and the `Slice` case did not, on otherwise identical
/// programs.
#[test]
fn split_at_mut_halves_alias_the_source_for_both_receivers() {
    // Vec receiver.
    assert_eq!(
        run_no_errors(
            "fn main() {\n\
                 let mut v: Vec[i64] = [1i64, 2i64, 3i64, 4i64];\n\
                 let mut p: (mut Slice[i64], mut Slice[i64]) = v.split_at_mut(2i64);\n\
                 p.0[0] = 10i64;\n\
                 p.1[0] = 30i64;\n\
                 println(f\"{v[0]} {v[1]} {v[2]} {v[3]}\");\n\
             }"
        )
        .trim(),
        "10 2 30 4"
    );
    // `mut Slice` receiver — the normalization path.
    assert_eq!(
        run_no_errors(
            "fn main() {\n\
                 let mut a: Array[i64, 4] = [1i64, 2i64, 3i64, 4i64];\n\
                 let mut s: mut Slice[i64] = a.as_slice_mut();\n\
                 let mut p: (mut Slice[i64], mut Slice[i64]) = s.split_at_mut(1i64);\n\
                 p.1[0] = 55i64;\n\
                 println(f\"{a[0]} {a[1]}\");\n\
             }"
        )
        .trim(),
        "1 55"
    );
}

/// B-2026-08-14-21 — the interpreter oracle for compound assignment through a
/// `mut ref` parameter. This backend was right on all five lines already; what
/// makes it the oracle is that codegen silently disagreed on exactly one of
/// them (`s += x` on the aggregate `mut ref`), with no diagnostic and no crash.
#[test]
fn test_compound_assign_through_mut_ref_param() {
    assert_eq!(
        run("fn append_plus_eq(s: mut ref String) { s += \"abc\"; }\n\
             fn append_push(s: mut ref String) { s.push_str(\"abc\"); }\n\
             fn append_assign(s: mut ref String) { s = s + \"abc\"; }\n\
             fn append_loop(s: mut ref String) {\n\
                 let mut i = 0i64;\n\
                 while i < 3i64 { s += \"z\"; i = i + 1i64; }\n\
             }\n\
             fn bump(n: mut ref i64) { n += 5i64; }\n\
             fn main() {\n\
                 let mut a = \"X\"; append_plus_eq(mut a); println(a);\n\
                 let mut b = \"X\"; append_push(mut b); println(b);\n\
                 let mut c = \"X\"; append_assign(mut c); println(c);\n\
                 let mut d = \"X\"; append_loop(mut d); println(d);\n\
                 let mut n = 1i64; bump(mut n); println(n);\n\
             }\n"),
        "Xabc\nXabc\nXabc\nXzzz\n6\n"
    );
}

#[test]
fn test_method_mut_ref_scalar_arg_writes_back() {
    // B-2026-08-21-38: a `mut ref T` METHOD parameter must publish the
    // callee's final value back to the caller's place, exactly as the
    // identical free function does. The interpreter binds every parameter
    // to a by-value copy, so without an explicit write-back the mutation
    // died with the callee's scope — measured as `6 5` here against `6 6`
    // for the free-function spelling and `6 6` on both compiled backends.
    let out = run("struct H { acc: i64 }\n\
        impl H { fn bump(ref self, x: mut ref i64) -> i64 { x = x + 1; x } }\n\
        fn free_bump(x: mut ref i64) -> i64 { x = x + 1; x }\n\
        fn main() {\n\
            let h = H { acc: 0 };\n\
            let mut n = 5;\n\
            println(h.bump(mut n));\n\
            println(n);\n\
            let mut m = 5;\n\
            println(free_bump(mut m));\n\
            println(m);\n\
        }");
    assert_eq!(out, "6\n6\n6\n6\n");
}

#[test]
fn test_method_mut_ref_arg_writes_back_alongside_mut_ref_self() {
    // The receiver write-back predates B-2026-08-21-38 and must survive it:
    // a `mut ref self` method that also takes a `mut ref` argument has to
    // publish BOTH, to two different call-site places.
    let out = run("struct H { acc: i64 }\n\
        impl H {\n\
            fn bump(mut ref self, x: mut ref i64) -> i64 {\n\
                self.acc = self.acc + 1;\n\
                x = x + 10;\n\
                x\n\
            }\n\
        }\n\
        fn main() {\n\
            let mut h = H { acc: 0 };\n\
            let mut q = 1;\n\
            println(h.bump(mut q));\n\
            println(q);\n\
            println(h.acc);\n\
        }");
    assert_eq!(out, "11\n11\n1\n");
}

#[test]
fn test_method_mut_ref_arg_writes_back_when_forwarded_unmarked() {
    // design.md § Call-site mutation markers: an argument already rooted at
    // a `mut ref` binding forwards WITHOUT a `mut` marker. Keying the
    // write-back on the marker alone would drop the inner hop of
    // `forward` → `self.bump(x)` and leave `f` at 100. Keying it on the
    // callee's declared parameter mode — what `method_param_mut_ref_flags`
    // reports — carries the chain.
    let out = run("struct H { acc: i64 }\n\
        impl H {\n\
            fn bump(ref self, x: mut ref i64) -> i64 { x = x + 1; x }\n\
            fn forward(ref self, x: mut ref i64) -> i64 { self.bump(x) }\n\
        }\n\
        fn main() {\n\
            let h = H { acc: 0 };\n\
            let mut f = 100;\n\
            println(h.forward(mut f));\n\
            println(f);\n\
        }");
    assert_eq!(out, "101\n101\n");
}

#[test]
fn test_method_mut_ref_arg_writes_back_to_every_place_shape() {
    // The write-back lands through `assign_to_place`, so it must reach a
    // field, a nested field, a tuple index and an index place — every shape
    // `place_is_writeback_safe` admits — not just a bare identifier.
    let out = run("struct Inner { w: i64 }\n\
        struct Box { v: i64, inner: Inner }\n\
        struct H { acc: i64 }\n\
        impl H { fn bump(ref self, x: mut ref i64) -> i64 { x = x + 1; x } }\n\
        fn main() {\n\
            let h = H { acc: 0 };\n\
            let mut b = Box { v: 1, inner: Inner { w: 1 } };\n\
            h.bump(mut b.v);\n\
            h.bump(mut b.inner.w);\n\
            println(b.v);\n\
            println(b.inner.w);\n\
            let mut t = (1, 2);\n\
            h.bump(mut t.0);\n\
            println(t.0);\n\
            let mut a = [10, 20];\n\
            h.bump(mut a[1]);\n\
            println(a[1]);\n\
        }");
    assert_eq!(out, "2\n2\n2\n21\n");
}

#[test]
fn test_method_two_mut_ref_args_write_back_independently() {
    // Two `mut ref` parameters are two distinct caller places; the
    // write-back must be per-argument, not "the first one wins".
    let out = run("struct H { acc: i64 }\n\
        impl H {\n\
            fn twice(ref self, a: mut ref i64, b: mut ref i64) {\n\
                a = a + 1;\n\
                b = b + 2;\n\
            }\n\
        }\n\
        fn main() {\n\
            let h = H { acc: 0 };\n\
            let mut a = 0;\n\
            let mut b = 0;\n\
            h.twice(mut a, mut b);\n\
            println(a);\n\
            println(b);\n\
        }");
    assert_eq!(out, "1\n2\n");
}

/// B-2026-08-28-13 oracle. Moving a heap field OUT of a BORROWED binding is
/// legal and yields a COPY — that is what the `mutate` case pins, and it is
/// what settled the ownership question the ledger row left open (reject the
/// move, or copy for the callee?). The row's "likely shape of the fix"
/// suggested the front end arguably should not accept the move at all; the
/// tree walk and the NON-GENERIC compiled path had both already agreed for as
/// long as they existed that it is a copy, so the answer was to make the
/// generic compiled path match, not to add a diagnostic.
///
/// Every case here was ALREADY CORRECT under the interpreter — that is the
/// point of the file. These are the reference values the codegen twin
/// (`e2e_generic_borrowed_field_move_out_is_a_copy_not_an_alias`) is asserted
/// against, so a future change that "fixes" a divergence by moving the
/// interpreter fails here first.
#[test]
fn generic_borrowed_field_move_out_yields_a_copy() {
    // The local is a copy: pushing to it must not reach the caller's field.
    assert_eq!(
        run("struct Bag[=T] { xs: Vec[T] }\n\
             impl[T] Bag[T] { fn n(ref self) -> i64 { let mut v = self.xs; v.push(99); return v.len(); } }\n\
             fn main() { let mut a: Bag[i64] = Bag { xs: Vec.new() };\n\
             a.xs.push(1); a.xs.push(2); println(f\"{a.n()}\"); println(f\"{a.xs.len()}\"); }"),
        "3\n2\n"
    );
    // The caller's field survives the call intact.
    assert_eq!(
        run("struct Bag[=T] { xs: Vec[T] }\n\
             impl[T] Bag[T] { fn n(ref self) -> i64 { let v = self.xs; return v.len(); } }\n\
             fn main() { let mut a: Bag[i64] = Bag { xs: Vec.new() };\n\
             a.xs.push(1); a.xs.push(2); println(f\"{a.n()}\"); println(f\"{a.xs[0]}\"); }"),
        "2\n1\n"
    );
    // A generic FREE fn with a `ref` param — not about the `self` receiver.
    assert_eq!(
        run("struct Bag[=T] { xs: Vec[T] }\n\
             fn n[T](b: ref Bag[T]) -> i64 { let v = b.xs; return v.len(); }\n\
             fn main() { let mut a: Bag[i64] = Bag { xs: Vec.new() };\n\
             a.xs.push(1); a.xs.push(2); println(f\"{n(a)}\"); }"),
        "2\n"
    );
    // `T` bound to a type that itself carries generic args.
    assert_eq!(
        run("struct Bag[=T] { xs: Vec[T] }\n\
             impl[T] Bag[T] { fn n(ref self) -> i64 { let v = self.xs; return v.len(); } }\n\
             fn main() { let mut a: Bag[Vec[i64]] = Bag { xs: Vec.new() };\n\
             let mut inner: Vec[i64] = Vec.new(); inner.push(7); a.xs.push(inner);\n\
             println(f\"{a.n()}\"); println(f\"{a.xs[0][0]}\"); }"),
        "1\n7\n"
    );
    // Non-generic twin: the same answer, which is why it was the control that
    // isolated the defect to type-param erasure.
    assert_eq!(
        run("struct Bag { xs: Vec[i64] }\n\
             impl Bag { fn n(ref self) -> i64 { let mut v = self.xs; v.push(99); return v.len(); } }\n\
             fn main() { let mut a: Bag = Bag { xs: Vec.new() };\n\
             a.xs.push(1); a.xs.push(2); println(f\"{a.n()}\"); println(f\"{a.xs.len()}\"); }"),
        "3\n2\n"
    );
}
