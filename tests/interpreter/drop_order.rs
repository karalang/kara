//! drop bodies, destructors, drop ordering -- fixtures for `tests/interpreter.rs`.
//!
//! Split out of `tests/interpreter.rs` on 2026-09-21. The TEST TARGET is
//! unchanged: this file is a module of that target, so
//! `cargo test --features llvm --test interpreter` still runs everything
//! and CI needs no edit. Run this area alone with:
//!
//!     cargo test --features llvm --test interpreter drop_order::
//!
//! New fixtures about drop bodies, destructors, drop ordering belong in this file.

use super::*;

#[test]
fn test_conditional_return_param_drop_matrix() {
    let out = run(
        "struct R { id: i64, s: String }\n\
         impl Drop for R { fn drop(mut ref self) { println(f\"d{self.id}\") } }\n\
         struct H { n: i64 }\n\
         fn mk(i: i64) -> String { return f\"pay-{i}-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\"; }\n\
         fn fpick(a: R, k: bool) -> R { if k { return R { id: 98, s: mk(98) }; } return a; }\n\
         impl H {\n\
             fn apick(a: R, k: bool) -> R { if k { return R { id: 98, s: mk(98) }; } return a; }\n\
             fn mpick(ref self, a: R, k: bool) -> R { if k { return R { id: 98, s: mk(98) }; } return a; }\n\
         }\n\
         fn ff_t() { let x = fpick(R { id: 11, s: mk(11) }, true); println(f\"A{x.id}\"); }\n\
         fn ff_f() { let x = fpick(R { id: 12, s: mk(12) }, false); println(f\"B{x.id}\"); }\n\
         fn fn_t() { let r = R { id: 13, s: mk(13) }; let x = fpick(r, true); println(f\"C{x.id}\"); }\n\
         fn fn_f() { let r = R { id: 14, s: mk(14) }; let x = fpick(r, false); println(f\"D{x.id}\"); }\n\
         fn af_t() { let x = H.apick(R { id: 21, s: mk(21) }, true); println(f\"E{x.id}\"); }\n\
         fn af_f() { let x = H.apick(R { id: 22, s: mk(22) }, false); println(f\"F{x.id}\"); }\n\
         fn an_t() { let r = R { id: 23, s: mk(23) }; let x = H.apick(r, true); println(f\"G{x.id}\"); }\n\
         fn an_f() { let r = R { id: 24, s: mk(24) }; let x = H.apick(r, false); println(f\"I{x.id}\"); }\n\
         fn mf_t() { let h = H { n: 0 }; let x = h.mpick(R { id: 31, s: mk(31) }, true); println(f\"J{x.id}\"); }\n\
         fn mf_f() { let h = H { n: 0 }; let x = h.mpick(R { id: 32, s: mk(32) }, false); println(f\"K{x.id}\"); }\n\
         fn mn_t() { let h = H { n: 0 }; let r = R { id: 33, s: mk(33) }; let x = h.mpick(r, true); println(f\"L{x.id}\"); }\n\
         fn mn_f() { let h = H { n: 0 }; let r = R { id: 34, s: mk(34) }; let x = h.mpick(r, false); println(f\"M{x.id}\"); }\n\
         fn main() {\n\
             ff_t(); ff_f(); fn_t(); fn_f();\n\
             af_t(); af_f(); an_t(); an_f();\n\
             mf_t(); mf_f(); mn_t(); mn_f();\n\
             println(\"end\");\n\
         }\n",
    );
    assert_eq!(
        out,
        "d11\nA98\nd98\n\
         B12\nd12\n\
         d13\nC98\nd98\n\
         D14\nd14\n\
         d21\nE98\nd98\n\
         F22\nd22\n\
         d23\nG98\nd98\n\
         I24\nd24\n\
         d31\nJ98\nd98\n\
         K32\nd32\n\
         d33\nL98\nd98\n\
         M34\nd34\n\
         end\n"
    );
}

/// B-2026-09-01-44 — an ASSOCIATED function with TWO by-value params, exactly
/// one of which is handed back on each path.
///
/// The matrix above varies the CALL spelling against one single-param callee;
/// this varies the CALLEE instead, and it is the shape that says the fix is a
/// name resolution rather than a per-parameter special case: whichever of `a`
/// and `b` dies inside must run its body once, and the other must run it once
/// at the caller's result binding. Before the fix the dying one ran no body in
/// either direction (`T42 d42`, `T43 d43`), while the compiled lanes were
/// already correct — so this is also the interpreter half of an A/B pair.
///
/// Twin of `codegen_assoc_fn_two_by_value_params_drop_matrix` (tests/codegen.rs),
/// pinned to the same string.
#[test]
fn test_assoc_fn_two_by_value_params_drop_matrix() {
    assert_eq!(
        run(r#"struct R { id: i64, s: String }
impl Drop for R { fn drop(mut ref self) { println(f"d{self.id}") } }
struct H { n: i64 }
fn mk(i: i64) -> String { return f"pay-{i}-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"; }

impl H {
    fn two(a: R, b: R, k: bool) -> R { if k { return b; } return a; }
}

fn t_t() { let x = H.two(R { id: 41, s: mk(41) }, R { id: 42, s: mk(42) }, true); println(f"T{x.id}"); }
fn t_f() { let x = H.two(R { id: 43, s: mk(43) }, R { id: 44, s: mk(44) }, false); println(f"T{x.id}"); }

fn main() { t_t(); t_f(); println("end"); }
"#),
        "d41\nT42\nd42\nd44\nT43\nd43\nend\n"
    );
}

/// B-2026-08-30-22 — an ASSOCIATED callee that hands its argument straight
/// back runs ONE `Drop` body, not two.
///
/// `run_fresh_temp_arg_drops` fires a fresh-temp argument's body as the call
/// returns, and suppresses that when the callee can RETURN the parameter — the
/// value flows out and the result binding owns it. Both of its guards searched
/// `Item::Function`, i.e. FREE functions only, and `Value::Function` carries
/// just the bare name, so `H.id(...)` arrived as `id`, matched nothing, and the
/// temp's body ran at the call AND the binding's at scope exit.
///
/// All three dispatch routes are asserted together because the asymmetry is the
/// evidence: the free-function and METHOD routes were already correct (the
/// method path claims arguments through the callee frame instead), so the
/// associated arm was a genuine third case rather than a variant of either —
/// which is what B-2026-08-29-46's fix note had already recorded about this
/// three-way split.
///
/// The compiled backends are the oracle here, unusually for this family: they
/// agreed with the free spelling throughout and valgrind was clean, so the
/// duplicate was this backend's alone.
#[test]
fn test_assoc_fn_passthrough_arg_runs_one_drop_body() {
    let hdr = "struct R { id: i64, s: String }\n\
               impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\") } }\n\
               fn mk(i: i64) -> String { return f\"pay-{i}\" }\n";
    let assoc = run(&format!(
        "{hdr}struct H {{ n: i64 }}\n\
         impl H {{ fn id(a: R) -> R {{ return a }} }}\n\
         fn main() {{ let x = H.id(R {{ id: 1, s: mk(1) }}); println(f\"x={{x.id}}\") }}"
    ));
    let free = run(&format!(
        "{hdr}fn idf(a: R) -> R {{ return a }}\n\
         fn main() {{ let x = idf(R {{ id: 1, s: mk(1) }}); println(f\"x={{x.id}}\") }}"
    ));
    let method = run(&format!(
        "{hdr}struct H {{ n: i64 }}\n\
         impl H {{ fn idm(ref self, a: R) -> R {{ return a }} }}\n\
         fn main() {{ let h = H {{ n: 0 }}; let x = h.idm(R {{ id: 1, s: mk(1) }}); \
         println(f\"x={{x.id}}\") }}"
    ));
    assert_eq!(free, "x=1\ndR1\n", "the free-function oracle itself moved");
    assert_eq!(method, "x=1\ndR1\n", "the method route itself moved");
    assert_eq!(
        assoc, free,
        "an associated passthrough must run exactly one body, as the free spelling does"
    );
}

#[test]
fn container_element_drop_order_is_the_containers_own_iteration_order() {
    // B-2026-08-27-7. The drop walk read STORAGE (`MapData::iter`), which is
    // insertion-ordered, while every other read path goes through the seeded
    // permutation `iter_observable` (B-2026-08-21-6). One run therefore printed
    // `iter 1 3 5 6 4 2` and then `drop 1 2 3 4 5 6` over the SAME map: two
    // walks of one container disagreeing with each other.
    //
    // The stability was the damage, not the disagreement. design.md § Map says
    // element destruction order is unspecified and per-process, and the
    // compiled backends deliver that (bucket order). The interpreter handed out
    // a STABLE insertion order instead, so a program that came to depend on it
    // looked correct under `karac run` and reordered under `karac build` — the
    // dependency only became visible at the backend boundary, which is the
    // worst place to find it.
    //
    // THE ASSERTION IS SELF-RELATIVE ON PURPOSE, and that is what makes it
    // runnable at all: it compares the program's own two walks rather than
    // pinning either to a literal, so it holds on every seed and every run
    // without asserting hash order — the thing CLAUDE.md § `Map`/`Set`
    // iteration order forbids. Eight elements, so a regression survives only on
    // the 1-in-40320 of seeds whose hash order happens to equal insertion order.
    let src = "struct V { id: i64 }
        impl Drop for V { fn drop(mut ref self) { println(f\"drop {self.id}\"); } }
        fn main() {
            let mut a: Map[i64, V] = Map.new();
            let mut i = 1;
            while i <= 8 { a.insert(i, V { id: i }); i = i + 1; }
            for (k, v) in a { println(f\"iter {k}\"); }
        }";
    let out = run_no_errors(src);
    let seq = |tag: &str| -> Vec<String> {
        out.lines()
            .filter_map(|l| l.strip_prefix(tag))
            .map(|s| s.trim().to_string())
            .collect()
    };
    let iterated = seq("iter ");
    let dropped = seq("drop ");
    assert_eq!(iterated.len(), 8, "all eight entries iterated: {out}");
    assert_eq!(
        dropped.len(),
        8,
        "each element's body fires exactly once: {out}"
    );
    assert_eq!(
        dropped, iterated,
        "elements must be destroyed in the container's own iteration order: {out}"
    );
}

// ── Prereq.4 user-`impl Drop` dispatch — interpreter parity ──
//
// Mirrors codegen's Prereq.3 wiring: when a binding's `CleanupAction::Drop`
// drains at NLL endpoint OR scope exit, the user-defined `<Type>.drop`
// body fires before the trace records the name. Codegen invokes
// `karac_drop_<Type>` (which calls `<Type>.drop` then field cleanup);
// the interpreter directly invokes `<Type>.drop` from the drain.

#[test]
fn test_user_drop_body_fires_at_nll_endpoint() {
    // Foo's last use is `let n = f.x` (stmt 2). The user drop fires
    // at NLL endpoint — immediately after that statement, before the
    // subsequent println — so the output sequence is:
    //   stmt 1: println(0)          → "0\n"
    //   stmt 2: let n = f.x         → no output
    //           [NLL drop fires]    → "42\n"
    //   stmt 3: println(99)         → "99\n"
    let (output, drops) = run_program_with_drops(
        "struct Foo { x: i64 }\n\
         impl Drop for Foo {\n\
             fn drop(mut ref self) {\n\
                 println(self.x);\n\
             }\n\
         }\n\
         fn main() {\n\
             let f = Foo { x: 42 };\n\
             println(0);\n\
             let n = f.x;\n\
             println(99);\n\
         }",
    );
    assert_eq!(
        output,
        vec!["0\n".to_string(), "42\n".to_string(), "99\n".to_string()],
        "expected NLL drop ordering (println(0), drop body 42, println(99)); got {:?}",
        output
    );
    // drop_trace still records both bindings — the mechanism that
    // codegen / interpreter use to know WHEN a drop fires is
    // unchanged; Prereq.4 only adds the side effect of running the
    // user body at the same point. `n` drops too (also a binding
    // and `let _ = ...` would suppress; here we keep the binding so
    // both drops fire). Both are due at the same statement (`let n =
    // f.x` is f's last use AND n's declaration-with-no-later-use), so
    // they fire LIFO — `n` (introduced later) before `f` — per the
    // design.md § 867 single-stack rule (B-2026-07-21-1).
    assert_eq!(drops, vec!["n".to_string(), "f".to_string()]);
}

#[test]
fn test_user_drop_body_does_not_fire_when_no_impl_drop() {
    let (output, drops) = run_program_with_drops(
        "struct Foo { x: i64 }\n\
         fn main() {\n\
             let f = Foo { x: 42 };\n\
             println(0);\n\
         }",
    );
    // No `impl Drop` for Foo → no body to fire. Output contains only
    // main's explicit println. drop_trace still records "f" — the
    // NLL placement mechanism is independent of whether a user body
    // exists.
    assert_eq!(output, vec!["0\n".to_string()]);
    assert_eq!(drops, vec!["f".to_string()]);
}

#[test]
fn test_user_drop_body_can_read_struct_fields() {
    // Sanity-check that `self.field` access works inside the drop
    // body. `Foo.drop` reads two fields and prints their sum.
    let (output, _drops) = run_program_with_drops(
        "struct Foo { a: i64, b: i64 }\n\
         impl Drop for Foo {\n\
             fn drop(mut ref self) {\n\
                 println(self.a + self.b);\n\
             }\n\
         }\n\
         fn main() {\n\
             let f = Foo { a: 10, b: 32 };\n\
         }",
    );
    // f's NLL endpoint is right after `let f = ...` (no later use),
    // so the drop body fires immediately, printing 42 (=10+32).
    assert_eq!(output, vec!["42\n".to_string()]);
}

#[test]
fn test_user_drop_fires_for_fresh_temp_struct_arg() {
    // `consume(Guard { id: 7 })` — no caller binding, so no
    // CleanupAction::Drop existed; the user body never fired under the
    // interpreter (codegen fixed the same shape as B-2026-07-01-6). The
    // temp's drop runs right after the call returns.
    let (output, _drops) = run_program_with_drops(
        "struct Guard { id: i64 }\n\
         impl Drop for Guard {\n\
             fn drop(mut ref self) {\n\
                 println(self.id);\n\
             }\n\
         }\n\
         fn consume(g: Guard) {\n\
             println(0);\n\
         }\n\
         fn main() {\n\
             consume(Guard { id: 7 });\n\
             println(99);\n\
         }",
    );
    assert_eq!(
        output,
        vec!["0\n".to_string(), "7\n".to_string(), "99\n".to_string()],
        "expected the fresh struct temp arg's drop after the call; got {:?}",
        output
    );
}

// ── B-2026-07-01-7 fn-returned Drop temps + passthrough guard ──

#[test]
fn test_user_drop_fires_for_fn_returned_temp_arg_and_discard() {
    // `consume(make())` (fn-returned temp arg) and a bare `make();`
    // (discarded statement position) each fire the user body exactly
    // once; a `mint(ctor)` whose callee returns a FRESH value keeps BOTH
    // drops (arg temp + result binding) — the passthrough guard is
    // per-callee syntactic analysis, not type-based.
    let (output, _drops) = run_program_with_drops(
        "struct Guard { id: i64 }\n\
         impl Drop for Guard {\n\
             fn drop(mut ref self) {\n\
                 println(self.id);\n\
             }\n\
         }\n\
         fn make() -> Guard { Guard { id: 7 } }\n\
         fn consume(g: Guard) { println(100); }\n\
         fn mint(g: Guard) -> Guard { Guard { id: g.id + 1 } }\n\
         fn main() {\n\
             consume(make());\n\
             make();\n\
             let x = mint(Guard { id: 20 });\n\
             println(x.id);\n\
         }",
    );
    assert_eq!(
        output,
        vec![
            "100\n".to_string(),
            "7\n".to_string(),
            "7\n".to_string(),
            "20\n".to_string(),
            "21\n".to_string(),
            "21\n".to_string()
        ],
        "expected consume-arg fire, discard fire, mint arg+result fires; got {:?}",
        output
    );
}

#[test]
fn test_user_drop_passthrough_arg_fires_once() {
    // `let x = pass(Guard { id: 7 })` where `pass` returns its param —
    // the arg-temp registration is SKIPPED (fn_returns_param guard) and
    // only x's binding drop fires. Pre-guard this double-fired.
    let (output, _drops) = run_program_with_drops(
        "struct Guard { id: i64 }\n\
         impl Drop for Guard {\n\
             fn drop(mut ref self) {\n\
                 println(self.id);\n\
             }\n\
         }\n\
         fn pass(g: Guard) -> Guard { g }\n\
         fn main() {\n\
             let x = pass(Guard { id: 7 });\n\
             println(x.id);\n\
         }",
    );
    let fires = output.iter().filter(|l| l.as_str() == "7\n").count();
    // x.id prints one "7"; exactly one MORE from the single drop.
    assert_eq!(
        fires, 2,
        "expected exactly one drop firing for the passed-through value; got {:?}",
        output
    );
}

#[test]
fn test_user_drop_shared_struct_fires_once() {
    let (output, _drops) = run_program_with_drops(
        "shared struct Res { id: i64 }\n\
         impl Drop for Res {\n\
             fn drop(mut ref self) {\n\
                 println(self.id);\n\
             }\n\
         }\n\
         fn main() {\n\
             let r = Res { id: 7 };\n\
         }",
    );
    // Sole reference, no later use → fires once, reading self.id.
    assert_eq!(output, vec!["7\n".to_string()]);
}

#[test]
fn test_user_drop_shared_struct_alias_fires_once_at_last_ref() {
    // phase-7 L940: `let r2 = r` is an Arc clone — two holders of one
    // inner. The body must fire EXACTLY once, when the last holder's slot
    // drains (env-release-on-drain decrements the strong-count as each
    // holder drains, so the final one reaches 1). Output is the read
    // `println(r2.id)` ("7") plus exactly one drop-body "7"; a third "7"
    // would be a double-drop, zero extra would be the pre-L940 under-fire.
    let (output, _drops) = run_program_with_drops(
        "shared struct Res { id: i64 }\n\
         impl Drop for Res {\n\
             fn drop(mut ref self) {\n\
                 println(self.id);\n\
             }\n\
         }\n\
         fn main() {\n\
             let r = Res { id: 7 };\n\
             let r2 = r;\n\
             println(r2.id);\n\
         }",
    );
    let fires = output.iter().filter(|l| l.trim() == "7").count();
    assert_eq!(
        fires, 2,
        "expected the read (one `7`) + exactly one drop-body `7`; got {output:?}"
    );
}

/// B-2026-09-03-9 — two holders of ONE shared value fire the body exactly
/// once, when the last of them releases it.
///
/// The guard against the obvious wrong fix: releasing on each holder's death
/// would print `dS1` twice, which is a double-drop of a live value rather
/// than the under-fire this row was about.
#[test]
fn test_user_drop_field_held_shared_aliased_by_two_holders_fires_once() {
    let (output, _drops) = run_program_with_drops(
        "shared struct S { id: i64 }\n\
         impl Drop for S { fn drop(mut ref self) { println(f\"dS{self.id}\"); } }\n\
         struct Sw { s: S }\n\
         fn main() {\n\
             let s: S = S { id: 1 };\n\
             let w1: Sw = Sw { s: s };\n\
             let w2: Sw = Sw { s: s };\n\
             println(f\"v{w1.s.id}{w2.s.id}\");\n\
             println(\"post\");\n\
         }",
    );
    assert_eq!(output.concat(), "v11\npost\ndS1\n");
}

/// B-2026-09-03-9, nested-shared leg — a `shared struct` held in ANOTHER
/// `shared struct`'s field, which is the shape the note above used to record
/// as permanently open.
///
/// It needs its own path: the shared arm releases the holder's slot as soon as
/// it drains, so by scope exit there is no binding left to re-read and the
/// values have to be captured while the holder is still alive. Two levels
/// deep, so the release also has to CASCADE within one block exit.
#[test]
fn test_user_drop_shared_struct_held_in_shared_field_fires_once() {
    let (output, _drops) = run_program_with_drops(
        "shared struct Leaf { id: i64 }\n\
         impl Drop for Leaf { fn drop(mut ref self) { println(f\"dL{self.id}\"); } }\n\
         shared struct Mid { l: Leaf }\n\
         shared struct Top { m: Mid }\n\
         fn main() {\n\
             let t: Top = Top { m: Mid { l: Leaf { id: 5 } } };\n\
             println(f\"v{t.m.l.id}\");\n\
             println(\"post\");\n\
         }",
    );
    assert_eq!(output.concat(), "v5\npost\ndL5\n");
}

/// B-2026-09-04-31 — a `shared struct` in a TUPLE ELEMENT runs its body ONCE,
/// at the tuple binding's scope exit, and every read before that returns the
/// live value.
///
/// Two reads, deliberately. On the compiled side this row was a
/// use-after-free whose FIRST read printed correctly (the field was loaded
/// before the box was released) and whose SECOND printed garbage; the
/// interpreter ran no body at all, because `run_array_element_user_drops`
/// answered for the tuple before the refcount path could see it. `post`
/// before the body pins the placement both backends now share.
#[test]
fn test_user_drop_tuple_held_shared_struct_fires_once_at_scope_exit() {
    let (output, _drops) = run_program_with_drops(
        "shared struct S { id: i64, tag: String }\n\
         impl Drop for S { fn drop(mut ref self) { println(f\"dS{self.id}/{self.tag}\"); } }\n\
         fn main() {\n\
             let a: (S, i64) = (S { id: 14, tag: \"alpha\" }, 3);\n\
             println(f\"r1={a.0.id}\");\n\
             println(f\"r2={a.0.id}\");\n\
             println(\"post\");\n\
         }",
    );
    assert_eq!(output.concat(), "r1=14\nr2=14\npost\ndS14/alpha\n");
}

/// B-2026-09-04-31 — the tuple's element moved out three ways, each of which
/// must still fire the body exactly once: an alias `let s = a.0` (two owners,
/// one release), a destructure `let (s, n) = a` (which marks the tuple
/// moved-out and used to lose the release entirely), and a tuple returned
/// out of a function and bound at the caller.
#[test]
fn test_user_drop_tuple_held_shared_struct_move_outs_fire_once() {
    let (output, _drops) = run_program_with_drops(
        "shared struct S { id: i64 }\n\
         impl Drop for S { fn drop(mut ref self) { println(f\"dS{self.id}\"); } }\n\
         fn build(k: i64) -> (S, i64) { let s: S = S { id: k }; return (s, 3) }\n\
         fn main() {\n\
             { let a: (S, i64) = (S { id: 1 }, 3); let s: S = a.0; println(f\"v{s.id}\"); println(\"one\"); }\n\
             { let a: (S, i64) = (S { id: 2 }, 3); let (s, n) = a; println(f\"v{s.id}{n}\"); println(\"two\"); }\n\
             { let a: (S, i64) = build(4); println(f\"v{a.0.id}\"); println(\"three\"); }\n\
             println(\"end\");\n\
         }",
    );
    assert_eq!(
        output.concat(),
        "v1\none\ndS1\nv23\ntwo\ndS2\nv4\nthree\ndS4\nend\n"
    );
}

// ── Move-suppression for user-Drop bindings (let-rebind) ──
//
// `let g = f;` where `f` has a user `impl Drop` moves the value
// into `g`. Without move-suppression both bindings' Drop actions
// would fire at scope exit, calling the user body twice on what is
// logically the same value (double-close fds, etc.). The interpreter
// helper `suppress_let_rebind_user_drop` removes the source's
// CleanupAction::Drop from the current cleanup frame before
// `push_drops_for_stmt` pushes the destination's. Non-user-Drop
// bindings still get their drop_trace records (the suppression is
// gated on `program.drop_method_keys`).

#[test]
fn test_user_drop_move_suppression_let_rebind() {
    let (output, drops) = run_program_with_drops(
        "struct R { tag: i64 }\n\
         impl Drop for R {\n\
             fn drop(mut ref self) { println(self.tag); }\n\
         }\n\
         fn main() {\n\
             let f = R { tag: 7 };\n\
             let g = f;\n\
             println(g.tag);\n\
         }",
    );
    // The user body should fire exactly ONCE — for `g`, not also for
    // `f`. Output sequence:
    //   stmt 0: let f = R { tag: 7 }  → no output, but the `value`
    //           expression is a struct literal so the user body
    //           would normally fire at f's NLL endpoint (which is
    //           THIS statement under interpreter NLL semantics)
    //           UNLESS move-suppression catches the next-stmt
    //           move-out first. Today the suppression runs at the
    //           NEXT statement's let-binding, AFTER `f`'s push +
    //           fire — so `f`'s drop body actually fires here.
    //           Documenting the observed behavior: this works for
    //           let-rebind only when `f` is used AFTER let-binding
    //           OR when the source binding's NLL endpoint is the
    //           statement that moves it.
    //   stmt 1: let g = f → before pushing g's Drop, suppress
    //           f's. But f's was already pushed AND fired at NLL
    //           endpoint of stmt 0. So suppression here is a no-op
    //           when the source already fired.
    //
    // To get the move-suppression to actually suppress, `f` must
    // still have its Drop slot in the cleanup vec when stmt 1 runs.
    // That requires `f` to be live past stmt 0 — which it IS,
    // because stmt 1 USES `f`. So `f`'s NLL endpoint is stmt 1, not
    // stmt 0. Drop slot survives stmt 0's fire_due_drops; at the
    // start of stmt 1's let-binding processing, suppress runs and
    // removes f's slot; then g's slot is pushed; then
    // fire_due_drops fires anything whose NLL endpoint is stmt 1
    // — but f's slot is gone, and g's last-use is later (stmt 2),
    // so neither fires here. At stmt 2's NLL endpoint, g fires.
    //
    // Expected: println(g.tag) → "7\n" then g's drop → "7\n".
    // f's drop body is suppressed — never appears.
    assert_eq!(
        output,
        vec!["7\n".to_string(), "7\n".to_string()],
        "expected one println(g.tag) + one g.drop (NOT also f.drop); got {:?}",
        output
    );
    // drop_trace records only `g` — `f`'s Drop slot was removed by
    // suppression before fire_due_drops could push the trace.
    assert_eq!(
        drops,
        vec!["g".to_string()],
        "expected drop_trace to contain only `g` (source `f` move-suppressed); got {:?}",
        drops
    );
}

// B-2026-07-29-38: an AGGREGATE-LITERAL field initializer is a move position
// too — `ownership_oracle`'s `Role::Move` set lists "aggregate-literal field"
// alongside the direct rebind, and the ownership checker already reports
// `value 'r' moved here` for it. Only the bare-rebind arm was suppressed, so
// `let h = Holder { r: r };` left `r`'s Drop slot in place and the NLL last-use
// placement fired it AT THE MOVE.
//
// That is a soundness bug, not a cosmetic one: `TcpListener` / `TcpStream` /
// `WebSocket` carry a synthesized `impl Drop` calling
// `karac_runtime_tcp_close(self.fd)`, so moving a listener into a struct field
// CLOSED ITS FD before the listener was used — the cause of the
// `coro_e2e::coroutine_method_handler_services_connection` hang (the port was
// printed, then nothing was listening).
#[test]
fn test_user_drop_no_premature_fire_when_moved_into_struct_literal() {
    let (output, _drops) = run_program_with_drops(
        "struct R { tag: i64 }\n\
         impl Drop for R {\n\
             fn drop(mut ref self) { println(99); }\n\
         }\n\
         struct Holder { r: R }\n\
         fn main() {\n\
             let r = R { tag: 7 };\n\
             let h = Holder { r: r };\n\
             println(h.r.tag);\n\
             println(40);\n\
         }",
    );
    // The move must NOT drop `r`. Before the fix the first line was "99\n" —
    // the drop firing at the move, ahead of any field read.
    assert!(
        !output.is_empty() && output[0] == "7\n",
        "the struct-literal move must not fire `r`'s drop before the field is \
         read; got {output:?}"
    );
    assert!(
        !output.contains(&"99\n".to_string())
            || output.iter().position(|l| l == "99\n").unwrap() > 0,
        "`r`'s drop must not precede the field read; got {output:?}"
    );
}

#[test]
fn test_user_drop_move_suppression_does_not_affect_non_drop_types() {
    // Plain `let y = x;` for a non-struct value (no user Drop) keeps
    // the existing drop_trace behaviour — both `x` and `y` get
    // recorded. Move-suppression is gated on `drop_method_keys`
    // containing the source's type, which is empty for primitives.
    let (_output, drops) = run_program_with_drops(
        "fn main() {\n\
             let x = 1;\n\
             let y = x;\n\
             println(y);\n\
         }",
    );
    // Both x and y appear in drop_trace per the existing NLL placement
    // (x's last use is `let y = x`, so it fires after stmt 1; y's
    // last use is println, fires after stmt 2).
    assert_eq!(
        drops,
        vec!["x".to_string(), "y".to_string()],
        "non-Drop bindings should still get drop_trace records; got {:?}",
        drops
    );
}

// ── Move-suppression for return-by-value of user-Drop bindings ──
//
// `fn make() -> T { let l = T::new(); l }` — `l`'s value moves out
// as the function's return value. The interpreter's
// `suppress_tail_expr_user_drop` (called before evaluating
// `block.final_expr`) removes `l`'s Drop slot from cleanup so its
// user-body doesn't fire when the function's `run_cleanup` runs;
// the caller fires its drop on the same logical value at its own
// scope exit. The companion `suppress_return_stmt_user_drop`
// handles explicit `return expr;` statements.

#[test]
fn test_user_drop_return_by_value_trailing_expression() {
    let (output, drops) = run_program_with_drops(
        "struct R { tag: i64 }\n\
         impl Drop for R {\n\
             fn drop(mut ref self) { println(self.tag); }\n\
         }\n\
         fn make() -> R {\n\
             let l = R { tag: 7 };\n\
             l\n\
         }\n\
         fn main() {\n\
             let r = make();\n\
             println(r.tag);\n\
         }",
    );
    // The user body fires EXACTLY ONCE — when `r` drops in `main`'s
    // scope exit. Not also in `make` (where `l` would otherwise fire
    // before returning, dropping the value the caller is about to
    // receive). Expected output:
    //   println(r.tag) → "7\n"
    //   r's user drop body → "7\n"
    // If the suppression were broken, we'd see "7\n", "7\n", "7\n"
    // (the suppressed-but-still-firing `l.drop` in `make` would add
    // an extra 7).
    assert_eq!(
        output,
        vec!["7\n".to_string(), "7\n".to_string()],
        "expected exactly two `7\\n` lines (println + one drop); got {:?}",
        output
    );
    // drop_trace shows only `r` — `l` was suppressed in `make`'s
    // cleanup (so its trace push never happened).
    assert_eq!(
        drops,
        vec!["r".to_string()],
        "expected drop_trace to contain only `r` (l move-suppressed in make); got {:?}",
        drops
    );
}

#[test]
fn test_user_drop_return_by_value_explicit_return() {
    let (output, drops) = run_program_with_drops(
        "struct R { tag: i64 }\n\
         impl Drop for R {\n\
             fn drop(mut ref self) { println(self.tag); }\n\
         }\n\
         fn make() -> R {\n\
             let l = R { tag: 7 };\n\
             return l;\n\
         }\n\
         fn main() {\n\
             let r = make();\n\
             println(r.tag);\n\
         }",
    );
    // Same expectation as the trailing-expression case — explicit
    // `return l;` is handled by `suppress_return_stmt_user_drop`.
    assert_eq!(
        output,
        vec!["7\n".to_string(), "7\n".to_string()],
        "explicit return: expected exactly two `7\\n` lines; got {:?}",
        output
    );
    assert_eq!(
        drops,
        vec!["r".to_string()],
        "explicit return: expected drop_trace [\"r\"]; got {:?}",
        drops
    );
}

// ── Prereq.5 user-`impl Drop` dispatch — edge cases ──
//
// Multiple-binding ordering, Drop / defer interleave, and the documented
// gaps that remain (move-suppression, RC integration, recursive drop-glue
// — see phase-7-codegen.md for follow-on tracker entries).

#[test]
fn test_user_drop_lifo_at_scope_exit_when_both_used_to_last_stmt() {
    // Both bindings are used at the last statement, so neither has
    // an NLL endpoint before scope exit; both drain at scope exit
    // via run_cleanup, which iterates cleanup-stack actions LIFO
    // (`cleanup.iter().rev()` at eval_stmt.rs). Last-declared
    // (B) drops first, then A.
    let (output, drops) = run_program_with_drops(
        "struct A { tag: i64 }\n\
         struct B { tag: i64 }\n\
         impl Drop for A {\n\
             fn drop(mut ref self) { println(self.tag); }\n\
         }\n\
         impl Drop for B {\n\
             fn drop(mut ref self) { println(self.tag); }\n\
         }\n\
         fn main() {\n\
             let a = A { tag: 1 };\n\
             let b = B { tag: 2 };\n\
             println(a.tag + b.tag);\n\
         }",
    );
    // Both a and b have last-use at stmt 2 (the println), so both fire
    // at that NLL endpoint — in LIFO (reverse-introduction) order per
    // the design.md § 867 single-stack rule: b (declared second) drops
    // first, then a. This is the B-2026-07-21-1 reconciliation: the
    // interpreter's fire_due_drops now walks back-to-front, and
    // codegen's `fire_due_user_drops` emits the identical order (the
    // old front-to-back FIFO here, and codegen's old scope-exit-only
    // drain, were the two halves of the divergence).
    assert_eq!(
        output,
        vec!["3\n".to_string(), "2\n".to_string(), "1\n".to_string()],
        "expected sum-print then both user-drop bodies (b then a, same-statement LIFO); got {:?}",
        output
    );
    // drop_trace records both bindings in the order they fired.
    assert_eq!(
        drops,
        vec!["b".to_string(), "a".to_string()],
        "drop_trace should record both bindings in LIFO fire order"
    );
}

#[test]
fn test_user_drop_interleaves_with_defer_at_scope_exit() {
    // Defer block and user-Drop binding share the same cleanup
    // stack; LIFO drain at scope exit interleaves them by
    // declaration order. The defer (declared after the binding)
    // runs before the binding's drop.
    let (output, _drops) = run_program_with_drops(
        "struct R { tag: i64 }\n\
         impl Drop for R {\n\
             fn drop(mut ref self) { println(self.tag); }\n\
         }\n\
         fn main() {\n\
             let r = R { tag: 7 };\n\
             defer { println(99); }\n\
             println(r.tag);\n\
         }",
    );
    // Sequence:
    //   stmt 0: bind r
    //   stmt 1: defer { println(99) }
    //   stmt 2: println(r.tag) → \"7\\n\". r's last use is stmt 2.
    //   NLL drop of r fires after stmt 2 → \"7\\n\" (from drop body).
    //   Scope exit: defer fires → \"99\\n\".
    //
    // The user-Drop runs at NLL endpoint (before defer at scope
    // exit) because of NLL placement. To get LIFO defer-then-drop
    // ordering, r would need to be live through scope exit. The
    // observed order pins NLL semantics' effect on user-Drop.
    assert_eq!(
        output,
        vec!["7\n".to_string(), "7\n".to_string(), "99\n".to_string()],
        "expected println(r.tag), drop body 7, defer 99 in that order; got {:?}",
        output
    );
}

#[test]
fn test_nll_drop_fires_after_last_use_not_at_scope_exit() {
    // Per design.md § Drop ordering within a branch: NLL drops fire
    // at the binding's last-use program point, not at scope exit.
    // `x` is read on line 3; subsequent statements never reference it.
    // Drop(x) must fire after stmt 3, before the later prints — and
    // before Drop(y) which dies later.
    let drops = drops_in(
        "fn main() {\n\
             let x = 1;\n\
             let y = 2;\n\
             println(x);\n\
             println(y);\n\
             println(99);\n\
         }",
    );
    assert_eq!(drops, vec!["x", "y"]);
}

#[test]
fn test_nll_drop_orders_by_last_use_not_declaration_order() {
    // Declaration order: a, b, c. Last-use order: c (idx 1), a (idx 2),
    // b (idx 3). Drops fire in last-use order, NOT declaration LIFO.
    let drops = drops_in(
        "fn main() {\n\
             let a = 1;\n\
             let b = 2;\n\
             let c = 3;\n\
             println(c);\n\
             println(a);\n\
             println(b);\n\
         }",
    );
    assert_eq!(drops, vec!["c", "a", "b"]);
}

#[test]
fn test_nll_unread_binding_drops_at_its_let() {
    // A binding never read after its declaration drops immediately
    // (last_use == its own let-stmt index). Subsequent let-and-drop
    // chains the same way.
    let drops = drops_in(
        "fn main() {\n\
             let _a = 1;\n\
             let _b = 2;\n\
             let _c = 3;\n\
         }",
    );
    assert_eq!(drops, vec!["_a", "_b", "_c"]);
}

#[test]
fn test_nll_binding_used_in_final_expr_drops_at_scope_exit() {
    // A binding referenced by `final_expr` stays live until the
    // expression evaluates; its Drop drains via the unified LIFO at
    // scope exit (sentinel: `last_use == stmts.len()`).
    let drops = drops_in(
        "fn main() {\n\
             let result = {\n\
                 let x = 7;\n\
                 x + 1\n\
             };\n\
             println(result);\n\
         }",
    );
    // `x` drops at the inner block's scope exit; `result` is the
    // outer-block binding and drops at outer scope exit. Both reach
    // the LIFO drain because they're referenced after the let.
    assert_eq!(drops, vec!["x", "result"]);
}

#[test]
fn test_nll_drop_ordering_with_defer_interleave() {
    // Bindings whose direct last-use is mid-block fire NLL early,
    // *before* any defer (registered later) drains at scope exit.
    // The unified LIFO ordering only kicks in for slots still in
    // `cleanup` at scope exit.
    let drops = drops_in(
        "fn main() {\n\
             let a = 1;\n\
             println(a);\n\
             defer { println(\"d\"); }\n\
             let b = 2;\n\
             println(b);\n\
         }",
    );
    // a's last use is stmt 1 → fires NLL after println(a).
    // b's last use is stmt 4 → fires NLL after println(b).
    // The defer body still drains at scope exit; it carries no
    // binding-name that would land on drop_trace.
    assert_eq!(drops, vec!["a", "b"]);
}

#[test]
fn test_weak_field_after_strong_drop_yields_none() {
    // After every strong holder of the referent is dropped, the
    // referent's allocation is freed and the weak field upgrade
    // returns `None`. `make_orphan` returns a Child whose only handle
    // to `Parent` is a weak reference; when the function frame exits,
    // the local strong `p` is dropped and the Arc count hits zero.
    assert_eq!(
        run("shared struct Parent { id: i64 }\n\
             shared struct Child { id: i64, mut weak parent: Parent }\n\
             fn make_orphan() -> Child {\n\
                 let p = Parent { id: 7 };\n\
                 Child { id: 2, parent: p }\n\
             }\n\
             fn main() {\n\
                 let c = make_orphan();\n\
                 match c.parent {\n\
                     Some(parent_ref) => println(parent_ref.id),\n\
                     None => println(\"dangling\"),\n\
                 }\n\
             }"),
        "dangling\n"
    );
}

#[test]
fn test_weak_field_multiple_aliases_all_see_drop() {
    // Two children weakly-reference the same parent. When the parent
    // is dropped, both children's weak fields upgrade to None.
    assert_eq!(
        run("shared struct Parent { id: i64 }\n\
             shared struct Child { id: i64, mut weak parent: Parent }\n\
             fn make_pair() -> (Child, Child) {\n\
                 let p = Parent { id: 1 };\n\
                 (Child { id: 10, parent: p }, Child { id: 11, parent: p })\n\
             }\n\
             fn main() {\n\
                 let pair = make_pair();\n\
                 let (a, b) = pair;\n\
                 match a.parent { Some(_) => println(\"a:alive\"), None => println(\"a:dangling\") }\n\
                 match b.parent { Some(_) => println(\"b:alive\"), None => println(\"b:dangling\") }\n\
             }"),
        "a:dangling\nb:dangling\n"
    );
}

#[test]
fn test_pool_pooled_connection_auto_releases_on_drop() {
    // Phase-8 line 200: drop-releases-automatically. A `PooledConnection`
    // bound with `let` returns its slot to the source pool when it leaves
    // scope — no explicit `pool.release(conn)`. `max_connections` is 1, so
    // the second acquire can only succeed (reusing the recycled slot) if
    // the first connection's scope-exit `Drop` handed its slot back;
    // without auto-release it would hit the cap and time out.
    let output = run(r#"fn make_int() -> i64 { 55 }
         fn main() {
             let pool: Pool[i64] = Pool.new(make_int, 1, 4);
             {
                 let c1 = pool.acquire(0).unwrap();
                 println(c1.val);
             }
             match pool.acquire(0) {
                 Ok(c2) => println(c2.val),
                 Err(_) => println("TIMEOUT"),
             }
         }"#);
    assert_eq!(output, "55\n55\n");
}

#[test]
fn test_pool_release_then_drop_returns_slot_once() {
    // Idempotent return: an explicit `release` followed by the binding's
    // scope-exit auto-`Drop` hands the slot back exactly once (keyed on the
    // checkout's `conn_id`). With `max_connections` = 1, a double-return
    // would let two simultaneous acquires both succeed; asserting the
    // second times out pins the single-return invariant. (A/B: with the
    // `checked_out` idempotency removed this prints "DOUBLE".)
    let output = run(r#"fn make_int() -> i64 { 7 }
         fn main() {
             let pool: Pool[i64] = Pool.new(make_int, 1, 4);
             {
                 let c1 = pool.acquire(0).unwrap();
                 pool.release(c1);
             }
             match pool.acquire(0) {
                 Ok(_) => {
                     match pool.acquire(0) {
                         Ok(_) => println("DOUBLE"),
                         Err(PoolError.Timeout) => println("single"),
                         Err(_) => println("other"),
                     }
                 }
                 Err(_) => println("acq_err"),
             }
         }"#);
    assert_eq!(output, "single\n");
}

#[test]
fn test_an_associated_fn_runs_an_owned_params_drop_body_once() {
    // B-2026-09-12-15 (interpreter half) — an ASSOCIATED function's by-value
    // params were never seeded into `owned_param_names_stack`, so anything bound
    // out of the entry copy inside the body registered a Drop slot of its own
    // and ran the body the CALLER was already going to run.
    //
    // `owned_param_names_of_fn` scanned `Item::Function` for a matching name.
    // An associated function lives in an `Item::ImplBlock`, so the lookup
    // returned nothing and the seed set was EMPTY. Two shapes doubled:
    //
    //   * a payload bound out of an `Option` argument in a `match` arm, and
    //   * a `let` destructure of an owned struct param — the very shape
    //     B-2026-08-01-12 added the stack for.
    //
    // Traced rather than guessed: panicking on the Nth body and reading the two
    // stacks showed body 1 from the caller's `run_fresh_temp_arg_drops` and
    // body 2 from the arm block's own `run_cleanup`.
    //
    // The fix routes the lookup through `callee_fn_for_param_ownership`, which
    // resolves a free function FIRST (so the already-correct spelling is
    // untouched) and whose `assoc_only` flag DROPS instance methods. That
    // exclusion is load-bearing: an instance method's caller path does not stand
    // down, so seeding its params moves `s.take(Some(..))` from one body to
    // zero. Both directions were measured.
    const PRELUDE: &str = "struct Rq { id: i64 }\n\
         impl Drop for Rq { fn drop(mut ref self) { println(f\"dRq{self.id}\"); } }\n";

    // The `Option`-payload shape: one body, not two.
    assert_eq!(
        run(&format!(
            "{PRELUDE}struct Sq {{ n: i64 }}\n\
             impl Sq {{\n\
             \x20   fn eat(x: Option[Rq]) {{\n\
             \x20       match x {{ Some(r) => {{ println(f\"a:{{r.id}}\") }} None => {{ println(\"n\") }} }}\n\
             \x20   }}\n\
             }}\n\
             fn main() {{\n\
             \x20   Sq.eat(Some(Rq {{ id: 1 }}));\n\
             \x20   println(\"end\");\n}}\n"
        )),
        "a:1\ndRq1\nend\n"
    );

    // The `let`-destructure shape, the stack's original purpose.
    assert_eq!(
        run(&format!(
            "{PRELUDE}struct Wq {{ r: Rq }}\n\
             struct Sq {{ n: i64 }}\n\
             impl Sq {{ fn eat(w: Wq) {{ let m = w.r; println(f\"m:{{m.id}}\") }} }}\n\
             fn main() {{\n\
             \x20   Sq.eat(Wq {{ r: Rq {{ id: 1 }} }});\n\
             \x20   println(\"end\");\n}}\n"
        )),
        "m:1\ndRq1\nend\n"
    );

    // CONTROL — an INSTANCE method must keep its body. This is the direction the
    // `assoc_only` flag protects: admitting instance methods here silences it.
    assert_eq!(
        run(&format!(
            "{PRELUDE}struct Sq {{ n: i64 }}\n\
             impl Sq {{\n\
             \x20   fn take(mut ref self, x: Option[Rq]) {{\n\
             \x20       match x {{ Some(r) => {{ println(f\"m:{{r.id}}\") }} None => {{ println(\"n\") }} }}\n\
             \x20   }}\n\
             }}\n\
             fn main() {{\n\
             \x20   let mut s = Sq {{ n: 0 }};\n\
             \x20   s.take(Some(Rq {{ id: 1 }}));\n\
             \x20   println(\"end\");\n}}\n"
        )),
        "m:1\ndRq1\nend\n"
    );

    // CONTROL — the FREE-function twin of the destructure cell, correct before
    // this change and resolved by the same lookup's first arm.
    assert_eq!(
        run(&format!(
            "{PRELUDE}struct Wq {{ r: Rq }}\n\
             fn eat(w: Wq) {{ let m = w.r; println(f\"m:{{m.id}}\") }}\n\
             fn main() {{\n\
             \x20   eat(Wq {{ r: Rq {{ id: 1 }} }});\n\
             \x20   println(\"end\");\n}}\n"
        )),
        "m:1\ndRq1\nend\n"
    );

    // CONTROL — a consuming associated callee: the payload goes into an
    // accumulator that outlives the call, so the accumulator runs the body and
    // this must still be exactly one.
    assert_eq!(
        run(&format!(
            "{PRELUDE}struct Sq {{ n: i64 }}\n\
             impl Sq {{\n\
             \x20   fn keep(x: Option[Rq], acc: mut ref Vec[Rq]) {{\n\
             \x20       match x {{ Some(r) => {{ acc.push(r) }} None => {{ println(\"n\") }} }}\n\
             \x20   }}\n\
             }}\n\
             fn main() {{\n\
             \x20   let mut acc: Vec[Rq] = [];\n\
             \x20   Sq.keep(Some(Rq {{ id: 1 }}), mut acc);\n\
             \x20   println(f\"len:{{acc.len()}}\");\n}}\n"
        )),
        "len:1\ndRq1\n"
    );
}

/// B-2026-08-27-50 / -52 — the interpreter ORACLE for a container argument
/// spelled as a STRUCT FIELD. Every case here was already correct under the
/// tree walk; they are the reference the compiled backends were measured
/// against, and they hold the A/B rule in place for the shapes that used to
/// diverge (a silent 8-byte swap over a 16-byte tuple element, a segfault at a
/// String element, a module-verification failure on the returning callee, and
/// a `free(): double free` on the read-only `ref` spelling).
/// B-2026-08-28-1 — the interpreter ORACLE for a user `Drop` body on a
/// destructure leaf. Every case here was already correct under the tree walk;
/// they are pinned because the compiled backends ran NONE of them, and the
/// interpreter is the reference the codegen twin in `tests/codegen.rs` is
/// asserted against.
///
/// Each case asserts the body fires exactly ONCE, and where it fires relative
/// to the surrounding prints — the placement is the NLL live-range end of the
/// LEAF, not scope exit, which is the property that decides which cleanup
/// channel codegen has to use.
#[test]
fn user_drop_body_fires_once_for_a_let_destructure_leaf() {
    const DROPPER: &str = "struct R { id: i64 }\n\
         impl Drop for R { fn drop(mut ref self) { println(f\"drop {self.id}\"); } }\n";
    for (label, body, want) in [
        (
            "tuple-local",
            "fn main() { let p = (R { id: 41 }, 1); let (r, n) = p; println(f\"{r.id + n}\"); }\n",
            "42\ndrop 41\n",
        ),
        (
            "call-result",
            "fn make() -> (R, i64) { return (R { id: 41 }, 1); }\n\
             fn main() { let (r, n) = make(); println(f\"{r.id + n}\"); }\n",
            "42\ndrop 41\n",
        ),
        (
            "tuple-literal",
            "fn main() { let (r, n) = (R { id: 41 }, 1); println(f\"{r.id + n}\"); }\n",
            "42\ndrop 41\n",
        ),
        (
            "field-bearing-leaf",
            "struct W { r: R }\n\
             fn main() { let p = (W { r: R { id: 41 } }, 1); let (w, n) = p; println(f\"{w.r.id + n}\"); }\n",
            "42\ndrop 41\n",
        ),
        (
            "two-droppers",
            "fn main() { let p = (R { id: 41 }, R { id: 7 }); let (a, b) = p; println(f\"{a.id + b.id}\"); }\n",
            "48\ndrop 7\ndrop 41\n",
        ),
        (
            "tuple-param",
            "fn take(p: (R, i64)) { let (r, n) = p; println(f\"{r.id + n}\"); }\n\
             fn main() { take((R { id: 41 }, 1)); }\n",
            "42\ndrop 41\n",
        ),
        (
            "inner-scope",
            "fn main() { { let p = (R { id: 41 }, 1); let (r, n) = p; println(f\"{r.id + n}\"); } println(\"after\"); }\n",
            "42\ndrop 41\nafter\n",
        ),
    ] {
        assert_eq!(run(&format!("{DROPPER}{body}")), want, "{label}");
    }
}

/// B-2026-07-29-39 — the interpreter half of aggregate field drop glue: a
/// struct holding a Drop-implementing value must run that value's body when it
/// dies. Pre-fix nothing walked an aggregate's fields, so `Holder { r: Res }`
/// dropped nothing and any resource in a field was held for the program's
/// lifetime.
///
/// Pins the same three properties as the codegen twin (`tests/codegen.rs`'s
/// `e2e_aggregate_runs_field_user_drop`), because the pair IS the parity
/// contract: the drop fires, nested aggregates recurse, fields die in reverse
/// declaration order, and a field moved out is dropped once by its destination.
#[test]
fn test_aggregate_runs_field_user_drop() {
    assert_eq!(
        run("struct A { t: i64 }\n\
             impl Drop for A { fn drop(mut ref self) { println(f\"dA{self.t}\"); } }\n\
             struct Mid { a: A }\n\
             struct Outer { m: Mid, a2: A }\n\
             struct Two { x: A, y: A }\n\
             struct Moved { a: A }\n\
             fn main() {\n\
                 { let o = Outer { m: Mid { a: A { t: 1 } }, a2: A { t: 2 } }; println(f\"{o.a2.t}\"); }\n\
                 { let t = Two { x: A { t: 4 }, y: A { t: 5 } }; println(f\"{t.x.t}\"); }\n\
                 { let h = Moved { a: A { t: 7 } }; let x = h.a; println(f\"{x.t}\"); }\n\
                 println(\"end\");\n\
             }\n"),
        "2\ndA2\ndA1\n4\ndA5\ndA4\n7\ndA7\nend\n"
    );
}

/// B-2026-07-30-11 SHAPE 2 — the interpreter half: an owned aggregate TEMP runs
/// its fields' user `impl Drop` in every position a temp can occupy (inline
/// struct-literal argument, fn-call argument, discarded fn result).
///
/// -39 taught the `let`-path this walk; every temp registrar still gated on the
/// type's OWN `drop_method_keys` entry, so `consume(H { .. })` / `consume(mk())`
/// / `mk();` each held the field's resource for the program's lifetime while
/// `let h = H { .. }; consume(h)` worked.
///
/// The passthrough case pins the disarm: `pass` returns its param, so the value
/// flows out to `p` and only `p`'s drop may run the body — `dA4` exactly once.
///
/// Same source and same expected string as `tests/codegen.rs`'s
/// `e2e_owned_aggregate_temp_runs_field_user_drop`; the pair IS the parity
/// contract, which is the whole reason a leak fix on one backend cannot land
/// without its twin.
#[test]
fn test_owned_aggregate_temp_runs_field_user_drop() {
    assert_eq!(
        run("struct A { t: i64 }\n\
             impl Drop for A { fn drop(mut ref self) { println(f\"dA{self.t}\"); } }\n\
             struct H { a: A }\n\
             fn consume(h: H) { println(\"c\"); }\n\
             fn mk(t: i64) -> H { H { a: A { t: t } } }\n\
             fn pass(h: H) -> H { h }\n\
             fn main() {\n\
                 consume(H { a: A { t: 1 } });\n\
                 println(\"-\");\n\
                 consume(mk(2));\n\
                 println(\"-\");\n\
                 mk(3);\n\
                 println(\"-\");\n\
                 let p = pass(H { a: A { t: 4 } });\n\
                 println(f\"{p.a.t}\");\n\
                 println(\"end\");\n\
             }\n"),
        "c\ndA1\n-\nc\ndA2\n-\ndA3\n-\n4\ndA4\nend\n"
    );
}

/// B-2026-07-30-11 (displaced-value leg) — interpreter twin of
/// `tests/codegen.rs`'s `e2e_struct_reassign_displaced_drop_semantics`, same
/// source and expected string. The displaced old value's body fires at the
/// assignment (reading the OLD id — the pre-fix interpreter ran one body
/// AFTER the store and printed the new id), a moved identifier source fires
/// exactly once, and a consumed-by-the-RHS old value fires nothing at the
/// assignment.
#[test]
fn test_struct_reassign_displaced_drop_semantics() {
    assert_eq!(
        run("struct G { id: i64, s: String }\n\
             impl Drop for G {\n\
                 fn drop(mut ref self) {\n\
                     println(f\"drop {self.id}\")\n\
                 }\n\
             }\n\
             fn consume(g: G) -> G {\n\
                 G { id: g.id + 10, s: g.s }\n\
             }\n\
             fn main() {\n\
                 let mut a = G { id: 1, s: \"first\".to_string() };\n\
                 a = G { id: 2, s: \"second\".to_string() };\n\
                 println(\"after overwrite\");\n\
                 let mut x = G { id: 3, s: \"xxx\".to_string() };\n\
                 let y = G { id: 4, s: \"yyy\".to_string() };\n\
                 x = y;\n\
                 println(\"after move\");\n\
                 let mut c = G { id: 5, s: \"ccc\".to_string() };\n\
                 c = consume(c);\n\
                 println(\"after consume\");\n\
             }\n"),
        "drop 1\ndrop 2\nafter overwrite\ndrop 3\ndrop 4\nafter move\ndrop 15\nafter consume\n"
    );
}

/// B-2026-07-30-11 (param-tuple leg, the A shape) — interpreter twin of
/// `tests/codegen.rs`'s `e2e_param_tuple_elements_run_drop_bodies`, same
/// source and expected string.
#[test]
fn test_param_tuple_elements_run_drop_bodies() {
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
             fn take_tuple(t: (Res, i64)) {\n\
                 println(f\"callee sees {t.1}\")\n\
             }\n\
             fn take_res(r: Res) {\n\
                 println(f\"callee sees res {r.id}\")\n\
             }\n\
             fn main() {\n\
                 println(\"a\");\n\
                 take_tuple((Res { id: 41 }, 10));\n\
                 println(\"b\");\n\
                 take_res(Res { id: 42 });\n\
                 println(\"c\");\n\
                 let g = Res { id: 43 };\n\
                 take_res(g);\n\
                 println(\"d\");\n\
                 let h = Res { id: 44 };\n\
                 take_tuple((h, 20));\n\
                 println(\"e\");\n\
                 take_tuple((mk(45), 30));\n\
                 println(\"end\");\n\
             }\n"),
        "a\ncallee sees 10\ndrop 41\nb\ncallee sees res 42\ndrop 42\nc\n\
         callee sees res 43\ndrop 43\nd\ncallee sees 20\ndrop 44\ne\n\
         callee sees 30\ndrop 45\nend\n"
    );
}

/// B-2026-07-30-11 (user-method discard) — interpreter twin of
/// `tests/codegen.rs`'s `e2e_discarded_user_method_return_runs_drop`, same
/// source and expected string. Pre-fix a USER impl method's owned Drop
/// return discarded in statement position (`f.make();`) or via wildcard-let
/// (`let _ = f.make();`) fired no body in either backend — the discard
/// gates admitted only free-fn calls and the owning container methods.
#[test]
fn test_discarded_user_method_return_runs_drop() {
    assert_eq!(
        run("struct Res { id: i64 }\n\
             impl Drop for Res {\n\
                 fn drop(mut ref self) {\n\
                     println(f\"drop {self.id}\")\n\
                 }\n\
             }\n\
             struct Fac { n: i64 }\n\
             impl Fac {\n\
                 fn make(ref self) -> Res {\n\
                     return Res { id: self.n };\n\
                 }\n\
             }\n\
             fn main() {\n\
                 let f = Fac { n: 13 };\n\
                 println(\"a\");\n\
                 f.make();\n\
                 println(\"b\");\n\
                 let _ = f.make();\n\
                 println(\"c\");\n\
                 let r = f.make();\n\
                 println(\"end\");\n\
             }\n"),
        // The bound `r` is unused after its let, so its NLL fire lands
        // before `end` — same position as the probe on both backends.
        "a\ndrop 13\nb\ndrop 13\nc\ndrop 13\nend\n"
    );
}

/// B-2026-08-01-2 — interpreter twin of `tests/codegen.rs`'s
/// `e2e_enum_return_discard_runs_payload_body`, same source and expected
/// string. Pre-fix the interpreter fired the wildcard-let shape but not the
/// bare statement (its own gap), fired an own-`impl Drop` enum's payload
/// walk codegen never runs (`drop 5 l5` after `loud drop`), and fired an
/// erased-generic payload codegen structurally cannot see (`drop 7 g7`) —
/// all three now follow the declared-type-driven walk codegen's
/// `__karac_dropelems_enum_<E>` admits, so the backends agree on every
/// quadrant.
/// B-2026-09-09-21 — a discarded TUPLE return ran no element `Drop` body on
/// ANY backend: `f(mk(20));` over `fn f(r: R) -> (R, i64)` printed only its
/// trailing statement. The value is built, handed to `f`, returned inside a
/// tuple and dropped on the floor at the `;`, so it owes exactly one body.
///
/// Because all four surfaces agreed, the A/B rule (`run` == `build`) passed and
/// every parity gate in the tree was silent — only reading the expected output
/// showed it. That is why the fix moved BOTH backends in one commit: the row
/// deliberately left the compiled side bodyless when it fixed the matching LEAK
/// (`asan_discarded_tuple_temp_frees_its_interior` pinned the bodyless stdout
/// as an interlock), because a compiled-only body would have turned a silent
/// agreed-wrong into a run-vs-build divergence — strictly worse.
///
/// Cells 3-5 are the controls that matter. A discarded BARE struct already
/// fired before this fix, so the gap was the aggregate wrapper rather than the
/// discard position; a tuple with no `Drop`-bearing element must stay silent;
/// and a BOUND tuple must still run exactly ONE body at its binding's end, not
/// a second from the discard walk.
#[test]
fn test_discarded_tuple_return_runs_its_element_drop_body() {
    let prelude = "struct R { id: i64, name: String }\n\
                   impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\") } }\n\
                   fn mk(i: i64) -> R { return R { id: i, name: f\"h{i}\" }; }\n\
                   fn f(r: R) -> (R, i64) { return (r, 9); }\n";

    // 1 -- the row's shape: a bare discard statement.
    assert_eq!(
        run(&format!(
            "{prelude}fn main() {{ f(mk(20)); println(\"ok\"); }}\n"
        )),
        "dR20\nok\n"
    );

    // 2 -- the `let _ =` spelling. The row recorded this path as a different
    //      one (`discarded_movable_literal_tail`) and NOT measured; it was
    //      broken the same way and is fixed by the same pair.
    assert_eq!(
        run(&format!(
            "{prelude}fn main() {{ let _ = f(mk(22)); println(\"ok\"); }}\n"
        )),
        "dR22\nok\n"
    );

    // 3 -- CONTROL: the discarded BARE struct, correct before this fix.
    assert_eq!(
        run(&format!(
            "{prelude}fn main() {{ mk(21); println(\"ok\"); }}\n"
        )),
        "dR21\nok\n"
    );

    // 3b -- B-2026-09-16-37, A REGRESSION THIS FIX INTRODUCED AND A FOLLOW-UP
    //       REMOVED. A GENERIC callee is declined, and that is PARITY rather
    //       than conservatism: codegen resolves this shape from the DECLARED
    //       element types, so an erased `T` yields no walker, while the
    //       interpreter reads the runtime value and knows it is an `R`. Gating
    //       on the value alone printed `dR31` here against nothing compiled.
    //       The erased-generic tuple discard stays an AGREED gap.
    assert_eq!(
        run(&format!(
            "{prelude}fn fgen[T](t: T) -> (T, i64) {{ return (t, 5); }}\n\
             fn main() {{ fgen(mk(31)); println(\"ok\"); }}\n"
        )),
        "ok\n"
    );

    // 3c -- B-2026-09-16-37, the other half. A METHOD callee is declined for
    //       the same parity reason, and on the compiled side it was worse than
    //       a divergence: the receiver's own walk already owns the element the
    //       method moved into the tuple, so registering bodies on the memory
    //       walk's verdict printed `dR dR`. Exactly one `dR32`, from the
    //       receiver, and the method spelling stays an agreed gap.
    assert_eq!(
        run(&format!(
            "{prelude}struct S {{ r: R }}\n\
             impl S {{ fn m(self) -> (R, i64) {{ return (self.r, 3); }} }}\n\
             fn main() {{ let s: S = S {{ r: mk(32) }}; s.m(); println(\"ok\"); }}\n"
        )),
        "dR32\nok\n"
    );

    // 4 -- CONTROL: a tuple whose elements have no user `Drop` must stay
    //      silent. The emitter's own type gate is what keeps it so.
    assert_eq!(
        run("fn g(i: i64) -> (i64, i64) { return (i, 9); }\n\
             fn main() { g(5); println(\"ok\"); }\n"),
        "ok\n"
    );

    // 5 -- CONTROL, and the one that would catch a double fire: a BOUND tuple
    //      owns its destructured element's body, so the discard walk must not
    //      add a second. Exactly one `dR24`.
    //
    //      It lands BEFORE `ok`, not after: `a` is never read again, so its
    //      live range ends at the `let` and design.md § Part 8 puts the body at
    //      the live-range end rather than at scope exit. Pinned in the order
    //      the implementation actually produces — the first draft of this cell
    //      asserted `ok` first and failed on that, which is worth recording
    //      because the COUNT (one) is what this control is for and the count
    //      was right all along.
    assert_eq!(
        run(&format!(
            "{prelude}fn main() {{ let (a, b) = f(mk(24)); println(\"ok\"); }}\n"
        )),
        "dR24\nok\n"
    );
}

/// B-2026-09-04-7, interpreter half — twin of `tests/codegen.rs`'s
/// `e2e_scalar_read_through_drop_field_keeps_every_body`, pinned to the SAME
/// string, because a scalar read through a `Drop`-bearing field is a copy on
/// every backend and the two must not drift apart again.
///
/// The interpreter's failure here was the loud one: `let z = h.a.id;` recorded
/// `("h", ["a", "id"])` in `moved_out_nested_field_bodies`, and that mask is
/// applied by DELETING the leaf field out of the intermediate value — so `R`'s
/// own drop body then read `self.id` off an `R` that no longer had one and hit
/// the `unreachable!` in `read_field`. An ICE, on a program the typechecker is
/// right to accept: it exempts a `Copy` field from `partial_move_of_drop_struct`
/// in as many words. The `shared` cell is the same crash through that rule's
/// other exemption (`copy_is_only_an_rc_retain`, "a `shared` read RETAINS"),
/// and the compiled backends were broken on it too — so it pins both halves.
#[test]
fn test_scalar_read_through_drop_field_keeps_every_body() {
    assert_eq!(
        run(r#"struct R { id: i64, tag: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}/{self.tag}") } }
fn mk(n: i64) -> R { return R { id: n, tag: f"t{n}" }; }
impl R { fn idm(ref self) -> i64 { return self.id; } }

struct H2 { a: R, b: R }
struct H3 { a: R, b: R, c: R }
struct Hn { a: R, n: i64 }
struct HOwn { a: R, b: R }
impl Drop for HOwn { fn drop(mut ref self) { println("dHOwn") } }
struct Inner { r: R }
struct Outer { h: Inner }
shared struct Box1 { v: i64 }
struct R2 { id: i64, sh: Box1 }
impl Drop for R2 { fn drop(mut ref self) { println(f"dR2{self.id}/{self.sh.v}") } }
struct Hb { a: R2, b: R2 }
struct Mid { r: R, q: R }
struct Top { m: Mid, s: R }

fn cell_scalar() { let h = H2 { a: mk(1), b: mk(101) }; let z = h.a.id; println(f"  z{z}") }
fn cell_method() { let h = H2 { a: mk(2), b: mk(102) }; let z = h.a.idm(); println(f"  z{z}") }
fn cell_nodest() { let h = H2 { a: mk(3), b: mk(103) }; println("  no") }
fn cell_three()  { let h = H3 { a: mk(4), b: mk(104), c: mk(204) }; let z = h.a.id; println(f"  z{z}") }
fn cell_own()    { let h = HOwn { a: mk(5), b: mk(105) }; let z = h.a.id; println(f"  z{z}") }
fn cell_whole()  { let h = H2 { a: mk(6), b: mk(106) }; let r = h.a; println(f"  z{r.id}") }
fn cell_plain()  { let h = Hn { a: mk(7), n: 4 }; let z = h.n; println(f"  z{z}") }
fn cell_move()   { let o = Outer { h: Inner { r: mk(8) } }; let x = o.h.r; println(f"  z{x.id}") }
fn cell_three_hop() { let t = Top { m: Mid { r: mk(9), q: mk(109) }, s: mk(209) }; let z = t.m.r.id; println(f"  z{z}") }
fn cell_shared() { let h = Hb { a: R2 { id: 11, sh: Box1 { v: 111 } }, b: R2 { id: 12, sh: Box1 { v: 112 } } }; let sv = h.a.sh; println(f"  s{sv.v}") }
fn cell_live()   { let h = H2 { a: mk(10), b: mk(110) }; let z = h.a.id; println(f"  z{z}"); println(f"  b{h.b.id}") }

fn main() {
    println("scalar");   cell_scalar()
    println("method");   cell_method()
    println("nodest");   cell_nodest()
    println("three");    cell_three()
    println("own");      cell_own()
    println("whole");    cell_whole()
    println("plain");    cell_plain()
    println("move");     cell_move()
    println("threehop"); cell_three_hop()
    println("shared");   cell_shared()
    println("live");     cell_live()
    println("done")
}
"#),
        r#"scalar
dR101/t101
dR1/t1
  z1
method
dR102/t102
dR2/t2
  z2
nodest
dR103/t103
dR3/t3
  no
three
dR204/t204
dR104/t104
dR4/t4
  z4
own
dHOwn
dR105/t105
dR5/t5
  z5
whole
dR106/t106
  z6
dR6/t6
plain
dR7/t7
  z4
move
  z8
dR8/t8
threehop
dR209/t209
dR109/t109
dR9/t9
  z9
shared
dR212/112
dR211/111
  s111
live
  z10
  b110
dR110/t110
dR10/t10
done
"#
    );
}

/// B-2026-09-02-38 — the STRUCT-PATTERN spelling of B-2026-09-02-25: a
/// `let S { r, k } = s;` over an owned struct param binds VIEWS of the callee's
/// entry copy, so a later `let m = r;` must MOVE the body rather than mint a
/// second owner. Was `b9 dR9 dR9` on all four surfaces where one body is due.
///
/// THE `end` MARKER IS LOAD-BEARING, and it is what refutes the reason this shape
/// was held out of -25. That row expected `param_view_locals` to be the wrong
/// instrument here, because `finish_owned_struct_destructure` TRANSFERS the
/// field's body to the leaf instead of leaving it with the source — and after a
/// transfer "someone else runs it" is false. Measured: the transfer happens for a
/// LOCAL source and NOT for a param, being gated on `var_owns_struct_field_bodies`
/// — on the source carrying a `StructFieldBodies` action, which a by-value param
/// has none of. So the `local` cell fires its body BEFORE `end` (the leaf's
/// live-range end) while every param cell fires AFTER it (the source's owner),
/// which is also where the tuple spelling puts it. Both orderings in one pinned
/// string is what would catch a regression that MOVES a fire rather than losing
/// one.
///
/// `heapstr` is the `owned_struct_params` cell. `Hs { r: R, name: String }` has a
/// direct `String` field, so the param IS in that set — the set whose presence
/// makes `finish_place_source_tuple_destructure` bail outright. The struct path
/// has no such bail and converges at one body here, the same refutation
/// `e2e_projection_source_tuple_destructure_is_a_view`'s `ownstr` cell records for
/// the tuple side.
///
/// `nested` binds a whole nested STRUCT FIELD (`let Ou { inner, k } = o;`), not a
/// nested PATTERN: codegen registers only dispatch for a nested pattern's leaves
/// and leaves their cleanup a tracked narrow leak, so the interpreter's
/// `collect_destructure_binding_names` deliberately does not recurse into one
/// either.
///
/// `norebind` and `refparam` are the over-reach controls, failing in opposite
/// directions: withholding a body too eagerly shows up as `norebind` running NONE,
/// and `refparam` (a borrowed receiver the caller still owns) must never gain one.
///
/// NO METHOD CELL, deliberately. A by-value param destructure inside a METHOD
/// places this body at the callee's scope exit on the compiled backends and at the
/// leaf's NLL death interpreted — one body either way, different point — and that
/// split is PRE-EXISTING and family-wide: the TUPLE spelling has it on `main`
/// today, from -25/-40. This fix takes the struct spelling's method case from two
/// bodies to one, i.e. onto exactly the tuple spelling's behaviour, rather than
/// inventing a new answer for it. Filed separately.
///
/// Twin of `tests/codegen.rs`'s
/// `e2e_struct_pattern_destructure_of_owned_param_is_a_view`, pinned to the same
/// string.
/// B-2026-09-03-7 — WHERE a method's fresh-temp argument runs its `Drop` body.
///
/// The row's fixture note says its own cells "deliberately carry NO method
/// cell, since they are pinned to one string across all four surfaces and this
/// shape cannot be". It can now, which is the whole content of the fix: the
/// interpreter fired a method's fresh-temp argument at the leaf's NLL DEATH
/// while every compiled backend fired it after the call returned. Six cells,
/// one string, all four surfaces.
///
/// `m1`-`m3`, `m5`, `m6` place the body AFTER the callee's `end` — design.md's
/// temporary-lifetime table, "Function/method call argument | After the call
/// returns". `m4` is the one shape that keeps it inside: the parameter is
/// handed to a LOCAL aggregate, which owns it from there
/// (`fn_moves_param_into_local_aggregate`). The count is one everywhere, and it
/// is PLACEMENT that this pin exists to hold — a body count cannot see this
/// bug at all, which is how it survived B-2026-09-02-25/-40/-38.
///
/// `m3`/`m5` (a move into another call) and `m6` (a destructure leaf that just
/// dies) are the cells that separate this from the neighbouring rows: all
/// three fire caller-side, so a fix that made the callee own its parameter
/// outright would move them and is not what this row asked for.
#[test]
fn test_method_fresh_temp_arg_drop_body_placement() {
    assert_eq!(
        run(r#"struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"  dR{self.id}") } }
struct S1 { r: R }
struct H  { n: i64 }
fn mk(n: i64) -> R { return R { id: n }; }
fn read(x: R) -> i64 { return x.id; }

impl H {
    fn m1(ref self, r: R)        { println("  end") }
    fn m2(ref self, r: R)        { let m = r; println("  end") }
    fn m3(ref self, r: R)        { let m = r; read(m); println("  end") }
    fn m4(ref self, r: R) -> i64 { let s = S1 { r: r }; let x = s.r; return 7; }
    fn m5(ref self, t: (R, i64)) { let (a, b) = t; let m = a; read(m); println("  end") }
    fn m6(ref self, t: (R, i64)) { let (a, b) = t; println("  end") }
}

fn main() {
    let h = H { n: 0 };
    println("m1 bare-die");       h.m1(mk(1))
    println("m2 rebind-die");     h.m2(mk(2))
    println("m3 into-call");      h.m3(mk(3))
    println("m4 wrap-moveout");   let v = h.m4(mk(4)); println(f"  v={v}")
    println("m5 destr-intocall"); h.m5((mk(5), 9))
    println("m6 destr-die");      h.m6((mk(6), 9))
    println("done")
}
"#),
        "m1 bare-die\n  end\n  dR1\nm2 rebind-die\n  end\n  dR2\nm3 into-call\n  end\n  dR3\nm4 wrap-moveout\n  dR4\n  v=7\nm5 destr-intocall\n  end\n  dR5\nm6 destr-die\n  end\n  dR6\ndone\n",
        "a method's fresh-temp arg body belongs after the call returns"
    );
}

/// B-2026-08-01-4 — interpreter twin of `tests/codegen.rs`'s
/// `e2e_fresh_arg_temp_drop_fires_at_statement_end`, same source and
/// expected string. The interpreter is the semantics oracle here (fresh
/// temps die at statement end, design.md § Drop ordering) and already
/// passed pre-fix — this pins the target the codegen statement-end drain
/// now meets.
#[test]
fn test_fresh_arg_temp_drop_fires_at_statement_end() {
    assert_eq!(
        run("struct Res { id: i64, name: String }\n\
             impl Drop for Res {\n\
                 fn drop(mut ref self) {\n\
                     println(f\"drop {self.id} {self.name}\")\n\
                 }\n\
             }\n\
             fn mk(n: i64) -> Res {\n\
                 return Res { id: n, name: f\"r{n}\" };\n\
             }\n\
             fn consume(r: Res) -> i64 {\n\
                 return r.id + 100;\n\
             }\n\
             fn peek(r: ref Res) -> i64 {\n\
                 return r.id;\n\
             }\n\
             fn main() {\n\
                 println(\"a\");\n\
                 let x = consume(mk(1));\n\
                 println(f\"x={x}\");\n\
                 println(\"b\");\n\
                 let y = peek(mk(2));\n\
                 println(f\"y={y}\");\n\
                 println(\"c\");\n\
                 consume(mk(3));\n\
                 println(\"end\");\n\
             }\n"),
        "a\ndrop 1 r1\nx=101\nb\ndrop 2 r2\ny=2\nc\ndrop 3 r3\nend\n"
    );
}

/// B-2026-08-01-4 (struct-literal residual, closed) — interpreter twin of
/// `tests/codegen.rs`'s `e2e_struct_literal_ref_param_arg_drop`, same
/// source and expected string. The interpreter's arg hook already fired
/// this shape — the pin is the parity target the widened codegen gate now
/// meets.
#[test]
fn test_struct_literal_ref_param_arg_drop() {
    assert_eq!(
        run("struct Res { id: i64, name: String }\n\
             impl Drop for Res {\n\
                 fn drop(mut ref self) {\n\
                     println(f\"drop {self.id} {self.name}\")\n\
                 }\n\
             }\n\
             fn peek(r: ref Res) -> i64 {\n\
                 return r.id;\n\
             }\n\
             fn main() {\n\
                 println(\"a\");\n\
                 let z = peek(Res { id: 4, name: f\"r4\" });\n\
                 println(f\"z={z}\");\n\
                 println(\"b\");\n\
                 peek(Res { id: 5, name: f\"r5\" });\n\
                 println(\"end\");\n\
             }\n"),
        "a\ndrop 4 r4\nz=4\nb\ndrop 5 r5\nend\n"
    );
}

/// B-2026-08-28-2, interpreter leg — one user `Drop` body per object when the
/// callee returns an element pulled out of an owned tuple param.
///
/// Pre-fix this printed `drop 41` twice for a single `R`: once from
/// `run_fresh_temp_arg_drops`' tuple arm, which fires every element of a
/// tuple-LITERAL argument, and once at the result binding's end. All three
/// backends did it, so the defect was invisible to every run-vs-build gate —
/// they agreed, at the wrong number.
///
/// The ORDER differs from the compiled twin on the two rows where an element
/// dies inside the call (`two-droppers`, `other-element-control`): this
/// backend fires the caller-side walk AT the call, codegen defers it to the
/// caller's scope exit. That split is older than this fix and untouched by it
/// — neither of those elements is one the fix removes from the walk — so the
/// expectations here are the interpreter's own, not a relaxation. Twin:
/// `tests/codegen.rs`'s
/// `e2e_user_drop_body_of_a_returned_tuple_param_element_runs_once`.
#[test]
fn test_returned_tuple_param_element_user_drop_body_runs_once() {
    const DROPPER: &str = "struct R { id: i64 }\n\
         impl Drop for R { fn drop(mut ref self) { println(f\"drop {self.id}\") } }\n";
    for (label, body, want) in [
        (
            "destructure-return",
            "fn take(p: (R, i64)) -> R { let (r, n) = p; r }\n\
             fn main() { let x = take((R { id: 41 }, 1)); println(f\"{x.id}\") }\n",
            "41\ndrop 41\n",
        ),
        (
            "explicit-return",
            "fn take(p: (R, i64)) -> R { let (r, n) = p; return r; }\n\
             fn main() { let x = take((R { id: 41 }, 1)); println(f\"{x.id}\") }\n",
            "41\ndrop 41\n",
        ),
        // No destructure anywhere — the same defect through a direct
        // projection, which is what showed the trigger is a PART of the param
        // escaping rather than the `let` the row was filed against.
        (
            "tuple-index-return",
            "fn take(p: (R, i64)) -> R { p.0 }\n\
             fn main() { let x = take((R { id: 41 }, 1)); println(f\"{x.id}\") }\n",
            "41\ndrop 41\n",
        ),
        (
            "result-discarded",
            "fn take(p: (R, i64)) -> R { let (r, n) = p; r }\n\
             fn main() { take((R { id: 41 }, 1)); println(\"end\") }\n",
            "drop 41\nend\n",
        ),
        // Both elements drop, only element 0 escapes. This is the row that
        // forces the guard to be per-element: suppressing the whole argument
        // fixes 41 and silences 42.
        (
            "two-droppers",
            "fn take(p: (R, R)) -> R { let (a, b) = p; a }\n\
             fn main() { let x = take((R { id: 41 }, R { id: 42 })); println(f\"{x.id}\") }\n",
            "drop 42\n41\ndrop 41\n",
        ),
        // B-2026-08-28-16 — the same shapes with a LOCAL as the argument. Every
        // row above passes a fresh tuple TEMP, which the caller-side parts
        // filter reaches; a bare identifier is a PLACE and never enters that
        // walk, so the second body came from the local's OWN element walk
        // firing at its live-range end on a value the callee had handed back.
        (
            "local-arg-destructure-return",
            "fn take(p: (R, i64)) -> R { let (r, n) = p; r }\n\
             fn main() { let q = (R { id: 41 }, 1); let x = take(q); println(f\"{x.id}\") }\n",
            "41\ndrop 41\n",
        ),
        (
            "local-arg-tuple-index-return",
            "fn take(p: (R, i64)) -> R { p.0 }\n\
             fn main() { let q = (R { id: 41 }, 1); let x = take(q); println(f\"{x.id}\") }\n",
            "41\ndrop 41\n",
        ),
        // Carries the same load as `two-droppers` above, for the local: only
        // element 0 escapes, so masking the local's whole walk loses `drop 42`.
        (
            "local-arg-two-droppers",
            "fn take(p: (R, R)) -> R { let (a, b) = p; a }\n\
             fn main() { let q = (R { id: 41 }, R { id: 42 }); let x = take(q);\n\
             \x20            println(f\"{x.id}\") }\n",
            "drop 42\n41\ndrop 41\n",
        ),
        // CONTROL — nothing escapes, so the local's walk stays fully armed.
        // Correct before this fix; it is what a too-eager mask would break.
        (
            "local-arg-nothing-escapes",
            "fn take(p: (R, i64)) -> i64 { let (r, n) = p; n }\n\
             fn main() { let q = (R { id: 41 }, 1); let x = take(q); println(f\"{x}\") }\n",
            "drop 41\n1\n",
        ),
        // CONTROL — the OTHER element is returned, so the dropper dies in the
        // call and the caller-side walk is its only body.
        (
            "other-element-control",
            "fn take(p: (R, i64)) -> i64 { let (r, n) = p; n }\n\
             fn main() { let x = take((R { id: 41 }, 1)); println(f\"{x}\") }\n",
            "drop 41\n1\n",
        ),
        // CONTROL — param returned BARE, the pre-existing whole-param guard.
        (
            "bare-param-control",
            "fn take(r: R) -> R { r }\n\
             fn main() { let x = take(R { id: 41 }); println(f\"{x.id}\") }\n",
            "41\ndrop 41\n",
        ),
        // CONTROL — nothing escapes; the whole temp dies in the call.
        (
            "nothing-escapes-control",
            "fn take(p: (R, i64)) { let (r, n) = p; println(f\"{r.id + n}\") }\n\
             fn main() { take((R { id: 41 }, 1)); println(\"end\") }\n",
            "42\ndrop 41\nend\n",
        ),
    ] {
        assert_eq!(run(&format!("{DROPPER}{body}")), want, "{label}");
    }
}

/// B-2026-08-28-53, interpreter leg — the ORACLE order for a DISCARDED
/// own-`Drop` parent temp whose returned field outlives it.
///
/// The interpreter was correct here throughout; this pins that so the compiled
/// twin cannot drift back. `take(W { r: R { id: 47 }, n: 5 });` with the result
/// thrown away printed `drop 47` / `drop W5` under LLJIT and AOT against this
/// leg's `drop W5` / `drop 47` — counts right on every backend, ordering alone.
///
/// The rule is design.md § Drop ordering's LIVE-RANGE end, not parent-before-
/// fields (the callee moves `r` OUT of `w`, so no parent/child relation
/// survives into the caller) and not LIFO (which would put the later-created
/// result first). The argument temporary's last use is the call, so it dies
/// there, before the result exists.
///
/// Twin: `tests/codegen.rs`'s
/// `e2e_discarded_own_drop_parent_temp_orders_its_body_first`, whose
/// expectations are these verbatim.
#[test]
fn test_discarded_own_drop_parent_temp_orders_its_body_first() {
    const D: &str = "struct R { id: i64 }\n\
         impl Drop for R { fn drop(mut ref self) { println(f\"drop {self.id}\") } }\n\
         struct W { r: R, n: i64 }\n\
         impl Drop for W { fn drop(mut ref self) { println(f\"drop W{self.n}\") } }\n\
         fn take(w: W) -> R { let W { r, n } = w; r }\n";
    for (label, body, want) in [
        (
            "discarded",
            format!("{D}fn main() {{ take(W {{ r: R {{ id: 47 }}, n: 5 }}); println(\"end\") }}\n"),
            "drop W5\ndrop 47\nend\n",
        ),
        (
            "discarded-wildcard",
            format!(
                "{D}fn main() {{ let _ = take(W {{ r: R {{ id: 47 }}, n: 5 }});\n\
                 \x20            println(\"end\") }}\n"
            ),
            "drop W5\ndrop 47\nend\n",
        ),
        (
            "bound",
            format!(
                "{D}fn main() {{ let got = take(W {{ r: R {{ id: 47 }}, n: 5 }});\n\
                 \x20            println(f\"got {{got.id}}\"); println(\"end\") }}\n"
            ),
            "drop W5\ngot 47\ndrop 47\nend\n",
        ),
    ] {
        assert_eq!(run(&body), want, "{label}");
    }
}

/// B-2026-08-28-65 — the UNDER-FIRE horn of B-2026-08-28-51's mechanism: a
/// `return <local>` NESTED IN A BRANCH lost the local's `Drop` body on the
/// path that never takes the `return`, on all three COMPILED backends while
/// the interpreter was right.
///
/// `fn take(k) -> R { let r = R { id: 41 }; if k { return r; } R { id: 99 } }`
/// with `k = false` printed `99` / `drop 99` under LLJIT, AOT and AOT with
/// `KARAC_AUTO_PAR=0`, against `--interp`'s correct `drop 41` / `99` /
/// `drop 99`. `r` dies in the callee on that path and must run its body
/// there.
///
/// The removal was `suppress_user_drop_for_var` in the `ExprKind::Return`
/// arm: a compile-time frame retraction, so it disarmed EVERY path, not the
/// one that returns. The interpreter's twin retracts from the CURRENT
/// block's cleanup vector, which for a nested `return` does not hold the
/// binding, so its retraction silently no-ops and it stayed correct by an
/// accident of scoping — which is why this was run-vs-build rather than a
/// shared wrong answer.
///
/// The fix keeps the action armed and clears B-2026-08-28-51's `i1` flag at
/// the `return`, so the guarded fire skips the body on the returning path
/// and runs it on the others. It is the same trade the MEMORY-side siblings
/// in that same `return` arm already make — `neutralize_moved_soa_groups_slot`
/// uses a runtime sentinel "not the tail path's compile-time frame removal"
/// for exactly this reason, since "the early-return cleanup frame is shared
/// with the fall-through path". Bodies simply had no sentinel until -51
/// built one.
///
/// `unconditional-return` is the row that CONSTRAINS the fix rather than
/// reproducing the bug. Its `return r;` is the body's own tail, where the
/// static removal is correct and today's behaviour must survive; the guard
/// is therefore gated on the action living in an ENCLOSING frame, which is
/// exactly the test for "this `return` is nested". Any version that guards
/// every `return` takes this row from one body to two.
///
/// `displaced-fallthrough` is the row the analysis said was the risk, and it
/// is the one that turned out to VALIDATE the fix. Retaining the action
/// makes `has_armed_user_drop` answer `true` where it answered `false`, and
/// that predicate gates the displaced-value leg (B-2026-07-30-11) which runs
/// a reassigned binding's old body. Firing it on a moved-from slot would
/// replay B-2026-07-31-38's shape — but control flow makes the proxy exact
/// here: if the `return` had executed, the function would have left, so any
/// path reaching the reassignment still owns the value. The row therefore
/// goes from a MISSING body to a correct one rather than to a stale-slot
/// read, and it is the fixture that would catch a regression either way.
///
/// `param-nested-return` is NOT fixed and is pinned at its current answer on
/// purpose: an owned by-value param is caller-drops, so its body is lost in
/// the CALLEE by a different channel — the interprocedural escape predicates
/// unioning over return sites, which is B-2026-08-28-22. All four surfaces
/// agree there, so it is aligned-wrong rather than run-vs-build, and this
/// row exists so that fixing -22 is noticed here rather than silently
/// changing an untested expectation.
///
/// Twin: `tests/codegen.rs`'s
/// `e2e_nested_return_local_user_drop_body_runs_on_the_fallthrough`, whose
/// expectations are these verbatim.
#[test]
fn test_nested_return_local_user_drop_body_runs_on_the_fallthrough() {
    const DROPPER: &str = "struct R { id: i64 }\n\
         impl Drop for R { fn drop(mut ref self) { println(f\"drop {self.id}\") } }\n\
         struct H { id: i64, name: String }\n\
         impl Drop for H { fn drop(mut ref self) { println(f\"drop {self.name}\") } }\n";
    for (label, body, want) in [
        // The row's own repro: the path that never takes the `return`.
        // Pre-fix the three compiled backends printed only `99` / `drop 99`.
        (
            "nested-return-fallthrough",
            "fn take(k: bool) -> R { let r = R { id: 41 }; if k { return r; } R { id: 99 } }\n\
             fn main() { let x = take(false); println(f\"{x.id}\") }\n",
            "drop 41\n99\ndrop 99\n",
        ),
        // The same program on the path that DOES return: `r` escapes, so
        // exactly one body must run, at the caller. A guard that failed to
        // clear would double it here.
        (
            "nested-return-taken",
            "fn take(k: bool) -> R { let r = R { id: 41 }; if k { return r; } R { id: 99 } }\n\
             fn main() { let x = take(true); println(f\"{x.id}\") }\n",
            "41\ndrop 41\n",
        ),
        // BOUNDARY — an UNCONDITIONAL `return r;` at the body's tail, where
        // the static removal is correct. Guarding every `return` doubles it.
        (
            "unconditional-return",
            "fn take() -> R { let r = R { id: 41 }; return r; }\n\
             fn main() { let x = take(); println(f\"{x.id}\") }\n",
            "41\ndrop 41\n",
        ),
        // The `match`-arm spelling of the repro — a different statement
        // form reaching the same nested `ExprKind::Return`.
        (
            "match-arm-return-fallthrough",
            "fn take(k: bool) -> R { let r = R { id: 41 };\n\
             \x20  match k { true => { return r; } false => {} }\n\
             \x20  R { id: 99 } }\n\
             fn main() { let x = take(false); println(f\"{x.id}\") }\n",
            "drop 41\n99\ndrop 99\n",
        ),
        // TWO levels of nesting, so the enclosing-frame test cannot be
        // reading only the immediately-enclosing scope.
        (
            "twice-nested-return-fallthrough",
            "fn take(k: bool, j: bool) -> R { let r = R { id: 41 };\n\
             \x20  if k { if j { return r; } }\n\
             \x20  R { id: 99 } }\n\
             fn main() { let x = take(true, false); println(f\"{x.id}\") }\n",
            "drop 41\n99\ndrop 99\n",
        ),
        // A `return` inside a LOOP body, where the fall-through reaches the
        // reassignment-free tail after the loop finishes.
        (
            "loop-return-fallthrough",
            "fn take(k: bool) -> R { let r = R { id: 41 }; let mut i = 0;\n\
             \x20  while i < 1 { if k { return r; } i = i + 1; }\n\
             \x20  R { id: 99 } }\n\
             fn main() { let x = take(false); println(f\"{x.id}\") }\n",
            "drop 41\n99\ndrop 99\n",
        ),
        // The DISPLACED-VALUE leg (B-2026-07-30-11) — the predicate the fix
        // changes the answer of. `r` is reassigned on the fall-through, so
        // the OLD value's body must run at the assignment.
        (
            "displaced-fallthrough",
            "fn take(k: bool) -> R { let mut r = R { id: 41 }; if k { return r; }\n\
             \x20  r = R { id: 99 }; r }\n\
             fn main() { let x = take(false); println(f\"{x.id}\") }\n",
            "drop 41\n99\ndrop 99\n",
        ),
        (
            "displaced-return-taken",
            "fn take(k: bool) -> R { let mut r = R { id: 41 }; if k { return r; }\n\
             \x20  r = R { id: 99 }; r }\n\
             fn main() { let x = take(true); println(f\"{x.id}\") }\n",
            "41\ndrop 41\n",
        ),
        // A SECOND local that is never returned must keep firing where it
        // always did, on both paths — the fix must disarm one binding on one
        // path, not a whole frame.
        (
            "two-locals-fallthrough",
            "fn take(k: bool) -> R { let a = R { id: 41 }; let b = R { id: 42 };\n\
             \x20  if k { return a; } b }\n\
             fn main() { let x = take(false); println(f\"{x.id}\") }\n",
            "drop 41\n42\ndrop 42\n",
        ),
        (
            "two-locals-return-taken",
            "fn take(k: bool) -> R { let a = R { id: 41 }; let b = R { id: 42 };\n\
             \x20  if k { return a; } b }\n\
             fn main() { let x = take(true); println(f\"{x.id}\") }\n",
            "drop 42\n41\ndrop 41\n",
        ),
        // A HEAP-carrying local, so the body reads a live buffer rather than
        // a moved-from husk. The memory twin is
        // `asan_nested_return_local_drop_body_is_memory_balanced`.
        (
            "heap-nested-return-fallthrough",
            "fn take(k: bool) -> H { let h = H { id: 41, name: f\"n{41}\" };\n\
             \x20  if k { return h; } H { id: 99, name: f\"n{99}\" } }\n\
             fn main() { let x = take(false); println(f\"{x.id}\") }\n",
            "drop n41\n99\ndrop n99\n",
        ),
        // WAS the tripwire for B-2026-08-28-22's channel — an owned by-value
        // PARAM is caller-drops, and this shape lost its body, aligned-wrong on
        // all four surfaces. It fires correctly since B-2026-08-29-21, which
        // taught `fn_conditionally_returns_param_bare` to read a `return`
        // operand as an exit leaf instead of declining the function outright;
        // the callee then claims the param and the per-path flag clears at the
        // `return`. Kept as the regression guard for that.
        (
            "param-nested-return",
            "fn take(r: R, k: bool) -> R { if k { return r; } R { id: 99 } }\n\
             fn main() { let x = take(R { id: 41 }, false); println(f\"{x.id}\") }\n",
            "drop 41\n99\ndrop 99\n",
        ),
    ] {
        assert_eq!(run(&format!("{DROPPER}{body}")), want, "{label}");
    }
}

/// B-2026-08-28-51, interpreter leg — a CONDITIONALLY-MOVED local runs its
/// user `Drop` body exactly once, on every path.
///
/// `fn take(k: bool) -> R { let r = R { id: 41 }; if k { r } else { R { id: 99 } } }`
/// with `k = true` printed `drop 41` / `41` / `drop 41`: the callee ran the
/// body on a value it had already handed to the caller, the caller then READ
/// that value, and its own drop ran the body a second time. The middle `41` is
/// a read of an already-dropped value, which is what made this the
/// high-severity half of the row.
///
/// WHY NEITHER EXISTING CHANNEL COULD FIX IT. A value moved on SOME paths and
/// dead on others needs runtime knowledge at the drop point, and the two
/// static channels guess in opposite directions. `merge_outer_states` re-marks
/// a conditionally-moved place `Owned` and leans on codegen's cap/null guard —
/// which protects MEMORY and has nothing to test for a user `Drop` BODY, so an
/// over-scheduled body simply runs twice. The move-suppression family
/// (`suppress_tail_expr_user_drop` and siblings) removes the action outright,
/// which disarms on ALL paths and can only under-fire. Teaching the suppressor
/// to descend into branch arms just moves a shape from the first failure to the
/// second.
///
/// The interpreter gets the missing bit for free: it evaluates only the TAKEN
/// arm, so reaching an arm's tail IS the proof that this path moved the value.
/// `record_conditional_move_tail` marks there. Codegen cannot do that — it
/// emits both arms — so its twin clears an `i1` flag in the arm's own basic
/// block and the drain tests it. Same classification, two idioms; the shared
/// `note_escaping_site` rule is what keeps them agreeing.
///
/// `discarded-if-statement` and `discarded-match-statement` are the rows that
/// CONSTRAIN the fix rather than reproduce the bug, and they are why the
/// marking is keyed to an escaping position instead of to "a block tail that is
/// an identifier". Their arm tails are the same bare `r`, but the value is
/// discarded rather than moved; marking it would take a program that runs one
/// body today to ZERO. Any version that keys on shape alone fails these two.
///
/// `two-locals-merge` pins the other direction: `s` must still fire where it
/// always did, so the fix cannot disarm a whole branch — only the arm that ran.
/// `heap-field-no-husk` pins the read: pre-fix the compiled backends printed
/// `drop ` (empty) because the spurious body read the moved-from slot, so this
/// row fails if the body ever runs on a husk again.
///
/// THE OTHER HORN, since closed. `fn take(k) -> R { let r = ...; if k { return r; } R { id: 99 } }`
/// with `k = false` lost `r`'s body on the COMPILED backends when this slice
/// landed — the UNDER-fire horn, a static removal in the `ExprKind::Return`
/// arm. It was filed as B-2026-08-28-65 and fixed by RETAINING the action and
/// clearing this slice's flag at the `return`, gated on the action living in
/// an enclosing frame so an unconditional `return r;` keeps the static
/// removal. `return-in-branch` below still covers only the `k = true`
/// direction; the fall-through direction lives in
/// `e2e_nested_return_local_user_drop_body_runs_on_the_fallthrough`.
///
/// Twin: `tests/codegen.rs`'s
/// `e2e_conditionally_moved_local_user_drop_body_runs_once`, whose expectations
/// are these verbatim.
#[test]
fn test_conditionally_moved_local_user_drop_body_runs_once() {
    const DROPPER: &str = "struct R { id: i64 }\n\
         impl Drop for R { fn drop(mut ref self) { println(f\"drop {self.id}\") } }\n";
    for (label, body, want) in [
        // The row's own repro: the arm that MOVES. Pre-fix `drop 41`/`41`/`drop 41`.
        (
            "branch-tail-moved",
            "fn take(k: bool) -> R { let r = R { id: 41 }; if k { r } else { R { id: 99 } } }\n\
             fn main() { let x = take(true); println(f\"{x.id}\") }\n",
            "41\ndrop 41\n",
        ),
        // The same program on the arm that does NOT move: `r` dies in the
        // callee and must still run its body there. A static disarm would
        // silence this row, which is why the marking has to be per-path.
        (
            "branch-tail-not-moved",
            "fn take(k: bool) -> R { let r = R { id: 41 }; if k { r } else { R { id: 99 } } }\n\
             fn main() { let x = take(false); println(f\"{x.id}\") }\n",
            "drop 41\n99\ndrop 99\n",
        ),
        // BOUNDARY — a DISCARDED `if` statement. Same bare `r` at the arm
        // tail, but the value goes nowhere, so it must keep running exactly
        // one body. Marking on shape alone takes this to zero.
        (
            "discarded-if-statement",
            "fn main() { let r = R { id: 41 }; let k = true; \
             if k { r } else { R { id: 99 } }; println(\"end\") }\n",
            "drop 41\nend\n",
        ),
        // BOUNDARY — the `match` twin of the row above.
        (
            "discarded-match-statement",
            "fn main() { let r = R { id: 41 }; let n = 0; \
             match n { 0 => r, _ => R { id: 9 } }; println(\"end\") }\n",
            "drop 41\nend\n",
        ),
        // Two locals, one `if`: the taken arm moves `r`, and `s` must still
        // die where it always did. Pre-fix this printed THREE bodies for two
        // objects.
        (
            "two-locals-merge",
            "fn main() { let r = R { id: 41 }; let s = R { id: 99 }; let k = true; \
             let y = if k { r } else { s }; println(f\"{y.id}\") }\n",
            "drop 99\n41\ndrop 41\n",
        ),
        // `else if` — the else branch is another `if`, so the escaping
        // property has to recurse through it to reach `b`.
        (
            "else-if-chain",
            "fn take(n: i64) -> R { let a = R { id: 1 }; let b = R { id: 2 }; \
             if n == 0 { a } else if n == 1 { b } else { R { id: 9 } } }\n\
             fn main() { let x = take(1); println(f\"{x.id}\") }\n",
            "drop 1\n2\ndrop 2\n",
        ),
        // An `if` nested INSIDE an arm: the outer arm's tail is itself a
        // branch, so its own arms are escaping too.
        (
            "nested-if-arm",
            "fn take(p: bool, q: bool) -> R { let a = R { id: 1 }; let b = R { id: 2 }; \
             if p { if q { a } else { b } } else { R { id: 9 } } }\n\
             fn main() { let x = take(true, false); println(f\"{x.id}\") }\n",
            "drop 1\n2\ndrop 2\n",
        ),
        // A bare-expression `match` arm never reaches the block-tail hook —
        // it is not a block — so it needs its own marking site.
        (
            "match-arm",
            "fn take(n: i64) -> R { let a = R { id: 1 }; let b = R { id: 2 }; \
             match n { 0 => a, 1 => b, _ => R { id: 9 } } }\n\
             fn main() { let x = take(0); println(f\"{x.id}\") }\n",
            "drop 2\n1\ndrop 1\n",
        ),
        // `return r;` nested in a branch is the same conditional move: the
        // static retraction targets the IF-block's cleanup, which does not
        // hold a binding declared in the enclosing function block.
        (
            "return-in-branch",
            "fn take(k: bool) -> R { let r = R { id: 41 }; if k { return r; } R { id: 99 } }\n\
             fn main() { let x = take(true); println(f\"{x.id}\") }\n",
            "41\ndrop 41\n",
        ),
        // The heap-field shape. Pre-fix the spurious body read the moved-from
        // slot, so the compiled backends printed an EMPTY name; this row fails
        // if a body ever runs on a husk again.
        (
            "heap-field-no-husk",
            "fn take2(k: bool) -> H { let h = H { id: 41, name: \"forty-one\" }; \
             if k { h } else { H { id: 99, name: \"ninety-nine\" } } }\n\
             fn main() { let x = take2(true); println(f\"{x.id} {x.name}\") }\n",
            "41 forty-one\ndrop forty-one\n",
        ),
        // A CLOSURE body's tail is returned too, so it is the same escaping
        // site as a function's. The interpreter reaches it through
        // `next_block_is_fn_body`, which covers closures; codegen needed the
        // seed planted separately, and without it this shape was the one place
        // the fix TURNED an aligned-wrong program into a run-vs-build
        // divergence.
        (
            "closure-branch-tail-moved",
            "fn main() { let f = || { let r = R { id: 41 }; let k = true; \
             if k { r } else { R { id: 99 } } }; let x = f(); println(f\"{x.id}\") }\n",
            "41\ndrop 41\n",
        ),
        (
            "closure-branch-tail-not-moved",
            "fn main() { let f = || { let r = R { id: 41 }; let k = false; \
             if k { r } else { R { id: 99 } } }; let x = f(); println(f\"{x.id}\") }\n",
            "drop 41\n99\ndrop 99\n",
        ),
        // A METHOD body, which codegen compiles through `compile_function`
        // like any other — recorded so the closure row above is not mistaken
        // for covering every non-free-function body.
        (
            "method-branch-tail",
            "struct Box2 { n: i64 }\n\
             impl Box2 { fn pick(ref self, k: bool) -> R { let r = R { id: 41 }; \
             if k { r } else { R { id: 99 } } } }\n\
             fn main() { let b = Box2 { n: 1 }; let x = b.pick(true); println(f\"{x.id}\") }\n",
            "41\ndrop 41\n",
        ),
        // CONTROL — a straight-line local tail return, correct before and
        // after. It owns its binding in THIS block, so it keeps taking the
        // static retraction and must not acquire a runtime mark.
        (
            "straight-line-control",
            "fn take() -> R { let r = R { id: 41 }; r }\n\
             fn main() { let x = take(); println(f\"{x.id}\") }\n",
            "41\ndrop 41\n",
        ),
        // CONTROL — a PARAM at a branch tail on the direction that MOVES.
        // Correct before and after: the caller declines its side (the callee
        // returns the param) and the callee's own guard is cleared on this
        // path, so exactly one body runs.
        (
            "param-branch-tail-moved",
            "fn take(r: R, k: bool) -> R { if k { r } else { R { id: 99 } } }\n\
             fn main() { let x = take(R { id: 41 }, true); println(f\"{x.id}\") }\n",
            "41\ndrop 41\n",
        ),
        // B-2026-08-28-22 — the SAME program on the direction that does NOT
        // move. `R{41}` dies inside the callee and must run its body there.
        //
        // This case was added alongside the one above as a second control, on
        // the belief stated in the old comment here — that a param at a branch
        // tail "was already correct on both directions". It was not: this
        // direction is B-2026-08-28-22's headline program, and the expectation
        // recorded was that row's defect (`99` / `drop 99`, R{41}'s body
        // lost), pinned as if it were intended. Both backends now run it.
        (
            "param-branch-tail-not-moved",
            "fn take(r: R, k: bool) -> R { if k { r } else { R { id: 99 } } }\n\
             fn main() { let x = take(R { id: 41 }, false); println(f\"{x.id}\") }\n",
            "drop 41\n99\ndrop 99\n",
        ),
    ] {
        let heap = "struct H { id: i64, name: String }\n\
             impl Drop for H { fn drop(mut ref self) { println(f\"drop {self.name}\") } }\n";
        assert_eq!(run(&format!("{DROPPER}{heap}{body}")), want, "{label}");
    }
}

/// B-2026-08-28-22 — a callee that returns an owned param on SOME tail paths
/// and not others now runs that param's user `Drop` body on the paths where it
/// dies, instead of nowhere.
///
/// An owned by-value param is caller-drops, and the caller declines wherever
/// `fn_returns_param` sees the value leaving. That predicate answers over the
/// UNION of a callee's return sites, so on a branchy callee the caller stood
/// down on EVERY path while only one path actually returned the value — and
/// whichever object died inside the call lost its body on all four surfaces.
/// The controls are what make it a static-vs-dynamic mismatch rather than a
/// missing case: the SAME program with `k` flipped was already correct, so the
/// callee, the argument and the predicate's answer are identical and only the
/// branch taken differs.
///
/// The fix is the callee-local ownership flip the row's addendum names, built
/// on B-2026-08-28-51's per-path conditional-move flag, with two constraints
/// that are the whole safety argument:
///
///   * BODIES ONLY (`emit_struct_user_drop_bodies_only_fn`). The caller still
///     owns the memory; installing the binding's own wrapper double-freed a
///     heap-carrying param. The row's own finding is that these channels lost a
///     BODY while the memory registrations stayed correct.
///   * ADMITTED ONLY WHERE THE FLAG CAN CLEAR
///     (`fn_conditionally_returns_param_bare`). `aggregate-return-declined`
///     below is why: `fn_returns_param` counts a struct/tuple-literal return as
///     an escape, the flag does not clear on one, and admitting it produced a
///     double body plus a read of the dropped value.
///
/// NOT the intersect-across-return-sites change the row warned about — the
/// union answer is untouched, so nothing depending on it moves. The
/// registration is added ARMED and then guarded, so `has_armed_user_drop`,
/// `has_armed_own_user_drop` and `has_armed_container_elem_bodies` answer for
/// these params where they previously had nothing to answer for.
///
/// Twin: `tests/codegen.rs`'s `e2e_conditionally_returned_param_user_drop_body_runs_once`, whose expectations are
/// these verbatim — the row is about the four surfaces agreeing, so a
/// divergence shows up as one of the two tests failing.
#[test]
fn test_conditionally_returned_param_user_drop_body_runs_once() {
    const DROPPER: &str = "struct R { id: i64 }\n\
         impl Drop for R { fn drop(mut ref self) { println(f\"drop {self.id}\") } }\n";
    for (label, body, want) in [
        // The row's headline program, on the direction where the param DIES
        // INSIDE THE CALLEE. Pre-fix `99` / `drop 99` on all four surfaces: the
        // caller declined its side because the callee returns `r` on SOME path,
        // and the callee had no registration at all, so R{41}'s body ran nowhere.
        (
            "if-else-not-moved",
            "fn take(r: R, k: bool) -> R { if k { r } else { R { id: 99 } } }\n\
             fn main() { let x = take(R { id: 41 }, false); println(f\"{x.id}\") }\n",
            "drop 41\n99\ndrop 99\n",
        ),
        // CONTROL — the same program on the direction that DOES move. Correct
        // before and after; the guard is cleared on this path, so adding the
        // registration must not make it fire twice.
        (
            "if-else-moved",
            "fn take(r: R, k: bool) -> R { if k { r } else { R { id: 99 } } }\n\
             fn main() { let x = take(R { id: 41 }, true); println(f\"{x.id}\") }\n",
            "41\ndrop 41\n",
        ),
        // `match` arms clear the flag through `control_flow_match.rs` rather
        // than the block-tail site, so both channels need covering.
        (
            "match-not-moved",
            "fn take(r: R, k: i64) -> R { match k { 1 => { r } _ => { R { id: 99 } } } }\n\
             fn main() { let x = take(R { id: 41 }, 2); println(f\"{x.id}\") }\n",
            "drop 41\n99\ndrop 99\n",
        ),
        // An `else if` chain — the escaping property has to recurse through the
        // nested `if` to reach the leaf tails.
        (
            "else-if-chain-not-moved",
            "fn take(r: R, k: i64) -> R { if k == 1 { r } else if k == 2 { R { id: 98 } } \
             else { R { id: 99 } } }\n\
             fn main() { let x = take(R { id: 41 }, 3); println(f\"{x.id}\") }\n",
            "drop 41\n99\ndrop 99\n",
        ),
        // TWO owned params, one returned. The one that dies runs its body where
        // it dies; the one handed back runs its body at the caller.
        (
            "two-params-one-returned",
            "fn take(a: R, b: R, k: bool) -> R { if k { a } else { b } }\n\
             fn main() { let x = take(R { id: 41 }, R { id: 42 }, true); \
             println(f\"{x.id}\") }\n",
            "drop 42\n41\ndrop 41\n",
        ),
        // The call DISCARDED rather than bound. Both objects die here and each
        // runs exactly one body.
        (
            "discarded-call",
            "fn take(r: R, k: bool) -> R { if k { r } else { R { id: 99 } } }\n\
             fn main() { take(R { id: 41 }, false) println(\"end\") }\n",
            "drop 41\ndrop 99\nend\n",
        ),
        // BOUNDARY — an AGGREGATE-LITERAL return route. `fn_returns_param`
        // counts `Holder { r: r }` as an escape (it recurses into struct and
        // tuple literals) but the conditional-move flag only clears on a BARE
        // identifier, so admitting this shape leaves the callee running a body
        // for a value that left the frame. Measured with it admitted:
        // `drop 41` / `41` / `drop 41` on all three compiled backends — a double
        // body plus a read of the dropped value.
        (
            "aggregate-return-declined",
            "struct Holder { r: R }\n\
             fn take(r: R, k: bool) -> Holder { if k { Holder { r: r } } \
             else { Holder { r: R { id: 99 } } } }\n\
             fn main() { let x = take(R { id: 41 }, true); println(f\"{x.r.id}\") }\n",
            "41\ndrop 41\n",
        ),
        // A `return` statement inside a branch — the MIXED spelling, one exit a
        // `return` and the other a block tail. This was declined outright on the
        // stated ground that "codegen clears the flag at match arms and block
        // tails but NOT at a `return` operand". That had stopped being true:
        // B-2026-08-28-65 added `guard_user_drop_for_nested_return`, which
        // stores `false` into the same per-path flag at a nested `return`, so
        // the mechanism did reach here and only the predicate was still
        // refusing. B-2026-08-29-21 lifted it. (-65 and -52 were the LOCAL
        // spelling of this; the param analogue is what stayed open.)
        (
            "return-in-branch-admitted",
            "fn take(r: R, k: bool) -> R { if k { return r } R { id: 99 } }\n\
             fn main() { let x = take(R { id: 41 }, false); println(f\"{x.id}\") }\n",
            "drop 41\n99\ndrop 99\n",
        ),
        // Its escaping twin — the path the flag must clear on. Correct before
        // and after; it is what proves the admission did not create a double.
        (
            "return-in-branch-admitted-escaping",
            "fn take(r: R, k: bool) -> R { if k { return r } R { id: 99 } }\n\
             fn main() { let x = take(R { id: 41 }, true); println(f\"{x.id}\") }\n",
            "41\ndrop 41\n",
        ),
        // BOTH exits spelled as `return` — B-2026-08-29-21's own program, in its
        // free-function form. The method form is in the codegen twin.
        (
            "return-both-exits-dying",
            "fn take(r: R, k: bool) -> R { if k { return r; } return R { id: 99 }; }\n\
             fn main() { let x = take(R { id: 41 }, false); println(f\"{x.id}\") }\n",
            "drop 41\n99\ndrop 99\n",
        ),
        (
            "return-both-exits-escaping",
            "fn take(r: R, k: bool) -> R { if k { return r; } return R { id: 99 }; }\n\
             fn main() { let x = take(R { id: 41 }, true); println(f\"{x.id}\") }\n",
            "41\ndrop 41\n",
        ),
        // B-2026-08-29-21's own program — the METHOD spelling of both-exits
        // `return`. This backend was the correct one (one body); the compiled
        // backends ran two on the escaping path. Kept in step with the codegen
        // twin's `method-return-both-exits-*`.
        (
            "method-return-both-exits-escaping",
            "struct T { n: i64 }\n\
             impl T { fn take(ref self, r: R, k: bool) -> R \
             { if k { return r; } return R { id: 98 }; } }\n\
             fn main() { let t = T { n: 1 }; \
             let a = t.take(R { id: 7 }, true); println(f\"got {a.id}\") }\n",
            "got 7\ndrop 7\n",
        ),
        (
            "method-return-both-exits-dying",
            "struct T { n: i64 }\n\
             impl T { fn take(ref self, r: R, k: bool) -> R \
             { if k { return r; } return R { id: 98 }; } }\n\
             fn main() { let t = T { n: 1 }; \
             let a = t.take(R { id: 7 }, false); println(f\"got {a.id}\") }\n",
            "drop 7\ngot 98\ndrop 98\n",
        ),
        // B-2026-08-29-11, PARAM LEG — the ORDER-DEPENDENT cell. One method
        // called three times, escaping in the middle: the escaping call marks
        // its param moved-out in a set keyed by bare NAME with no frame
        // qualifier, and before the fix that mark outlived the call and silenced
        // the THIRD call's identically-named param. Interp ran
        // `drop 4` / `drop 3` / (nothing) against `drop 4` / `drop 3` / `drop 5`
        // on all three compiled backends. Only an escaping call BEFORE a dying
        // one shows it, which is why no single-call fixture catches it.
        (
            "method-param-mark-does-not-outlive-the-call",
            "struct T { n: i64 }\n\
             impl T { fn take(ref self, r: R, k: bool) -> R { if k { return r } R { id: 9 } } }\n\
             fn main() { let t = T { n: 1 };\n\
             \x20  let a = t.take(R { id: 4 }, false); println(f\"got {a.id}\")\n\
             \x20  let b = t.take(R { id: 3 }, true); println(f\"got {b.id}\")\n\
             \x20  let c = t.take(R { id: 5 }, false); println(f\"got {c.id}\") }\n",
            "drop 4\ngot 9\ndrop 9\ngot 3\ndrop 3\ndrop 5\ngot 9\ndrop 9\n",
        ),
        // A GENERIC callee, admitted since B-2026-08-28-71. Both backends had
        // declined it: the monomorph is compiled through
        // `compile_mono_function`, whose own param loop had no registration, and
        // fixing this backend alone would have turned a shared gap into a
        // run-vs-build divergence. Keep these four in step with the codegen twin
        // verbatim — that is the whole point of the pairing.
        (
            "generic-callee-dying-path",
            "fn take[T](r: R, k: bool, t: T) -> R { if k { r } else { R { id: 99 } } }\n\
             fn main() { let x = take(R { id: 41 }, false, 7); println(f\"{x.id}\") }\n",
            "drop 41\n99\ndrop 99\n",
        ),
        // Control on this backend and pre-fix on both — the escaping path was
        // already right here. It earns its place as the interpreter half of the
        // codegen case that the intermediate, unseeded-escaping-site version of
        // the fix broke.
        (
            "generic-callee-escaping-path",
            "fn take[T](r: R, k: bool, t: T) -> R { if k { r } else { R { id: 99 } } }\n\
             fn main() { let x = take(R { id: 41 }, true, 7); println(f\"{x.id}\") }\n",
            "41\ndrop 41\n",
        ),
        // The heap-carrying generic pair — the shape B-2026-08-28-71 was filed
        // on. This backend was already free of the corruption the row measured
        // (it resolves a generic by its declared name), so what these pin is the
        // TARGET the compiled backends now meet.
        (
            "generic-heap-payload-dying-path",
            "fn take[T](h: H, k: bool, t: T) -> H { if k { h } \
             else { H { id: 99, name: f\"n99\" } } }\n\
             fn main() { let x = take(H { id: 41, name: f\"n41\" }, false, 7); \
             println(f\"{x.id}\") }\n",
            "drop n41\n99\ndrop n99\n",
        ),
        // B-2026-09-02-3 — the conditionally-returned param's declared type IS
        // the type parameter. Every generic case above declares it concretely
        // (`r: R`, `h: H`) and uses `T` only for a scalar argument, so all of
        // them reached a `drop_method_keys` lookup that was handed a real
        // struct name. With `a: T` the mono registration was handed the literal
        // "T", registered nothing, and the compiled lanes ran no body for the
        // value that died inside — while this backend, which resolves a generic
        // by its declared name, was already correct. So these three are the
        // TARGET the compiled backends now meet, exactly as the pair above.
        //
        // Keep them verbatim in step with the codegen twin.
        (
            "generic-param-is-T-dying-first-arg",
            "fn pick[T](a: T, k: bool, alt: T) -> T { if k { return alt; } return a; }\n\
             fn main() { let x = pick(H { id: 41, name: f\"n41\" }, true, \
             H { id: 42, name: f\"n42\" }); println(f\"{x.id}\") }\n",
            "drop n41\n42\ndrop n42\n",
        ),
        (
            "generic-param-is-T-dying-second-arg",
            "fn pick[T](a: T, k: bool, alt: T) -> T { if k { return alt; } return a; }\n\
             fn main() { let x = pick(H { id: 43, name: f\"n43\" }, false, \
             H { id: 44, name: f\"n44\" }); println(f\"{x.id}\") }\n",
            "drop n44\n43\ndrop n43\n",
        ),
        (
            "generic-param-is-bounded-T",
            "#[derive(Display)]\n\
             struct D { id: i64, name: String }\n\
             impl Drop for D { fn drop(mut ref self) { println(f\"drop {self.name}\") } }\n\
             fn pickd[T: Display](a: T, k: bool, alt: T) -> T \
             { if k { return alt; } return a; }\n\
             fn main() { let x = pickd(D { id: 45, name: f\"n45\" }, true, \
             D { id: 46, name: f\"n46\" }); println(f\"{x.id}\") }\n",
            "drop n45\n46\ndrop n46\n",
        ),
        (
            "generic-heap-payload-escaping-path",
            "fn take[T](h: H, k: bool, t: T) -> H { if k { h } \
             else { H { id: 99, name: f\"n99\" } } }\n\
             fn main() { let x = take(H { id: 41, name: f\"n41\" }, true, 7); \
             println(f\"{x.id}\") }\n",
            "41\ndrop n41\n",
        ),
        // B-2026-08-29-16 — the generic METHOD spelling of the same conditional
        // shape, which was wrong in BOTH directions and in OPPOSITE senses:
        // measured at 907b378^, param dies gave interp 0 bodies / compiled 1, and
        // param escapes gave interp 1 / compiled 2. The dying leg below is the
        // RED one on THIS backend; its codegen twin is the control, and the
        // escaping pair is the mirror image. Keep the two files in step.
        (
            "generic-method-conditional-param-dies",
            "struct G1 { n: i64 }\n\
             impl G1 { fn gm_pick[T](ref self, r: R, k: bool, t: T) -> R \
             { if k { r } else { R { id: 99 } } } }\n\
             fn main() { let g = G1 { n: 1 }; \
             let x = g.gm_pick(R { id: 41 }, false, 7); println(f\"{x.id}\") }\n",
            "drop 41\n99\ndrop 99\n",
        ),
        // Control here, RED on the compiled twin.
        (
            "generic-method-conditional-param-escapes",
            "struct G1 { n: i64 }\n\
             impl G1 { fn gm_pick[T](ref self, r: R, k: bool, t: T) -> R \
             { if k { r } else { R { id: 99 } } } }\n\
             fn main() { let g = G1 { n: 1 }; \
             let x = g.gm_pick(R { id: 41 }, true, 7); println(f\"{x.id}\") }\n",
            "41\ndrop 41\n",
        ),
        // BOUNDARY — the callee never returns the param, so the caller already
        // owned the drop and nothing changes.
        (
            "never-returns-param",
            "fn take(r: R, k: bool) -> i64 { if k { 1 } else { 2 } }\n\
             fn main() { let x = take(R { id: 41 }, false); println(f\"{x}\") }\n",
            "drop 41\n2\n",
        ),
        // BOUNDARY — an UNCONDITIONAL return. One leaf tail, no branch, nothing
        // to guard: the predicate requires a path that does NOT yield the param.
        (
            "unconditional-return",
            "fn take(r: R) -> R { r }\n\
             fn main() { let x = take(R { id: 41 }); println(f\"{x.id}\") }\n",
            "41\ndrop 41\n",
        ),
        // A HEAP-CARRYING param. The registration is BODIES-ONLY: the caller
        // still owns the memory, so installing the binding's own `Drop` wrapper
        // here (which frees the fields too) double-freed — measured `free():
        // double free detected in tcache 2` under the JIT. The ASAN twin
        // `asan_conditionally_returned_param_bodies_are_memory_balanced` pins
        // the memory side.
        (
            "heap-payload-not-moved",
            "fn take(h: H, k: bool) -> H { if k { h } \
             else { H { id: 99, name: f\"n99\" } } }\n\
             fn main() { let x = take(H { id: 41, name: f\"n41\" }, false); \
             println(f\"{x.id}\") }\n",
            "drop n41\n99\ndrop n99\n",
        ),
        // B-2026-09-12-26 — the `return` in TAIL position (the last exit written
        // WITHOUT a trailing semicolon), which `leaf_tails` pushed as a whole
        // `Return` node and `may_mention`'s catch-all then declined. This
        // backend shares `fn_conditionally_returns_param_bare` with codegen, so
        // its DYING cells moved with the fix too: before it, the param that died
        // inside the callee ran no body here either. The compiled twin
        // (`e2e_conditionally_returned_param_user_drop_body_runs_once`) carries
        // the full account and the per-call-position split; these expectations
        // are those verbatim, so a divergence shows as one of the two failing.
        (
            "assoc-return-tail-both-exits-escaping",
            "struct Sk { n: i64 }\n\
             impl Sk { fn pick(r: R, flag: bool) -> R \
             { if flag { return r } return R { id: 9 } } }\n\
             fn main() { let x = Sk.pick(R { id: 1 }, true); println(f\"{x.id}\") }\n",
            "1\ndrop 1\n",
        ),
        (
            "assoc-return-tail-both-exits-dying",
            "struct Sk { n: i64 }\n\
             impl Sk { fn pick(r: R, flag: bool) -> R \
             { if flag { return r } return R { id: 9 } } }\n\
             fn main() { let x = Sk.pick(R { id: 1 }, false); println(f\"{x.id}\") }\n",
            "drop 1\n9\ndrop 9\n",
        ),
        // THE DISCRIMINATOR — the same function with the semicolon, correct
        // before and after.
        (
            "assoc-return-STATEMENT-both-exits-escaping",
            "struct Sk { n: i64 }\n\
             impl Sk { fn pick(r: R, flag: bool) -> R \
             { if flag { return r; } return R { id: 9 }; } }\n\
             fn main() { let x = Sk.pick(R { id: 1 }, true); println(f\"{x.id}\") }\n",
            "1\ndrop 1\n",
        ),
        (
            "free-return-tail-both-exits-dying",
            "fn pickf(r: R, flag: bool) -> R { if flag { return r } return R { id: 9 } }\n\
             fn main() { let x = pickf(R { id: 1 }, false); println(f\"{x.id}\") }\n",
            "drop 1\n9\ndrop 9\n",
        ),
        // BOUNDARY — an UNCONDITIONAL `return r` in tail position stays declined.
        (
            "assoc-unconditional-return-tail",
            "struct Sk2 { n: i64 }\n\
             impl Sk2 { fn pick(r: R) -> R { return r } }\n\
             fn main() { let x = Sk2.pick(R { id: 1 }); println(f\"{x.id}\") }\n",
            "1\ndrop 1\n",
        ),
    ] {
        let heap = "struct H { id: i64, name: String }\n\
             impl Drop for H { fn drop(mut ref self) { println(f\"drop {self.name}\") } }\n";
        assert_eq!(run(&format!("{DROPPER}{heap}{body}")), want, "{label}");
    }
}

/// B-2026-08-28-70, interpreter leg — a METHOD's owned param runs its user
/// `Drop` body exactly once, wherever it dies.
///
/// The interpreter twin of `e2e_method_owned_param_user_drop_body_runs_once`
/// in `tests/codegen.rs`, which carries the same cases with the same expected
/// output. Keep the two in step verbatim: the row is about the four surfaces
/// agreeing, so a divergence shows up as one of the two tests failing.
///
/// The two halves fail in OPPOSITE directions, which is why both files need the
/// full matrix rather than half each. A method frame reaches no caller-side
/// `run_fresh_temp_arg_drops` in this backend, so before the fix it owned
/// nothing and every param that died inside ran ZERO bodies here — while the
/// compiled backends, whose caller always fired, ran TWO for any param the
/// method handed back.
#[test]
fn test_method_owned_param_user_drop_body_runs_once() {
    const DROPPER: &str = "struct R { id: i64 }\n\
         impl Drop for R { fn drop(mut ref self) { println(f\"drop {self.id}\") } }\n\
         struct B2 { n: i64 }\n";
    for (label, body, want) in [
        // The row's headline program, on the direction where the param DIES
        // INSIDE. Pre-fix this printed `99` / `drop 99` — R{41}'s body lost.
        (
            "cond-return-dies",
            "impl B2 { fn pick(ref self, r: R, k: bool) -> R { if k { r } else { R { id: 99 } } } }\n\
             fn main() { let b = B2 { n: 1 }; let x = b.pick(R { id: 41 }, false); \
             println(f\"{x.id}\") }\n",
            "drop 41\n99\ndrop 99\n",
        ),
        // The same program with `k` flipped: the param escapes, so the frame's
        // registration must be DISARMED by the per-path flag rather than fire.
        (
            "cond-return-escapes",
            "impl B2 { fn pick(ref self, r: R, k: bool) -> R { if k { r } else { R { id: 99 } } } }\n\
             fn main() { let b = B2 { n: 1 }; let x = b.pick(R { id: 41 }, true); \
             println(f\"{x.id}\") }\n",
            "41\ndrop 41\n",
        ),
        // NEVER RETURNED — the plainest shape, and the one showing this is not
        // only about conditional returns. Pre-fix: `7` alone, zero bodies.
        (
            "never-returned",
            "impl B2 { fn eat(ref self, r: R) -> i64 { 7 } }\n\
             fn main() { let b = B2 { n: 1 }; let v = b.eat(R { id: 41 }); println(f\"{v}\") }\n",
            "drop 41\n7\n",
        ),
        // UNCONDITIONALLY returned — the result binding owns it, so the frame
        // must NOT claim it. Correct here before and after; it is the compiled
        // side that doubled.
        (
            "always-returned",
            "impl B2 { fn id(ref self, r: R) -> R { r } }\n\
             fn main() { let b = B2 { n: 1 }; let x = b.id(R { id: 41 }); println(f\"{x.id}\") }\n",
            "41\ndrop 41\n",
        ),
        (
            "always-returned-aggregate",
            "struct H { r: R }\n\
             impl B2 { fn wrap(ref self, r: R) -> H { H { r: r } } }\n\
             fn main() { let b = B2 { n: 1 }; let h = b.wrap(R { id: 21 }); \
             println(f\"{h.r.id}\") }\n",
            "21\ndrop 21\n",
        ),
        (
            "return-stmt-all-exits-yield",
            "impl B2 { fn both(ref self, r: R, k: bool) -> R { if k { return r; } r } }\n\
             fn main() { let b = B2 { n: 1 }; let z = b.both(R { id: 23 }, false); \
             println(f\"{z.id}\") }\n",
            "23\ndrop 23\n",
        ),
        // A `return` that yields something ELSE, so the param dies on that
        // path and the caller must keep firing. Pre-fix this backend lost the
        // body; an earlier draft of the fix lost it on the COMPILED backends
        // too, by standing the caller down on the union predicate.
        (
            "return-stmt-other-value-keeps-caller-fire",
            "impl B2 { fn early(ref self, r: R, k: bool) -> R \
             { if k { return R { id: 98 } ; } r } }\n\
             fn main() { let b = B2 { n: 1 }; let y = b.early(R { id: 22 }, true); \
             println(f\"{y.id}\") }\n",
            "drop 22\n98\ndrop 98\n",
        ),
        (
            "two-params-one-returned",
            "impl B2 { fn two(ref self, a: R, b: R, k: bool) -> R { if k { a } else { b } } }\n\
             fn main() { let s = B2 { n: 1 }; \
             let x = s.two(R { id: 4 }, R { id: 5 }, false); println(f\"{x.id}\") }\n",
            "drop 4\n5\ndrop 5\n",
        ),
        // Receiver MODES — the ownership question is about the ARGUMENT, not
        // the receiver, so all three modes must answer alike. Pre-fix all three
        // printed zero bodies here.
        (
            "mut-ref-self-receiver",
            "impl B2 { fn eat(mut ref self, r: R) -> i64 { self.n = self.n + 1; 8 } }\n\
             fn main() { let mut c = B2 { n: 1 }; let v = c.eat(R { id: 2 }); println(f\"{v}\") }\n",
            "drop 2\n8\n",
        ),
        (
            "owned-self-receiver",
            "impl B2 { fn eat(self, r: R) -> i64 { 9 } }\n\
             fn main() { let d = B2 { n: 1 }; let v = d.eat(R { id: 3 }); println(f\"{v}\") }\n",
            "drop 3\n9\n",
        ),
        (
            "generic-method-param-dies",
            "struct G1 { n: i64 }\n\
             impl G1 { fn gm[T](ref self, r: R, t: T) -> i64 { 3 } }\n\
             fn main() { let g = G1 { n: 1 }; let a = g.gm(R { id: 31 }, 5); println(f\"{a}\") }\n",
            "drop 31\n3\n",
        ),
        // The CROSS-FRAME case that used to sit here is deliberately gone. The
        // moved-out sets are keyed by BINDING NAME with no frame qualifier, and
        // 277621a isolated the callee frame's copies to stop one method's mark
        // suppressing an unrelated later method's identically-named param. That
        // isolation was REVERTED in B-2026-08-29-9: the leak turns out to be
        // load-bearing — it is the only thing suppressing a payload bound out of
        // an owned enum param and returned, so isolating produced a DOUBLE
        // `Drop` body, which is the worse defect. The missed body it hides is
        // filed as its own row rather than pinned here as expected output; the
        // codegen twin keeps its case, because that backend is correct.
        // CONTROLS — the free-function twins, unanimous on all four surfaces
        // before and after. They are the oracle the method cases are measured
        // against, so a change that "fixed" methods by moving free functions
        // would fail here.
        (
            "free-fn-oracle-always-returned",
            "fn id2(r: R) -> R { r }\n\
             fn main() { let x = id2(R { id: 41 }); println(f\"{x.id}\") }\n",
            "41\ndrop 41\n",
        ),
        (
            "free-fn-oracle-never-returned",
            "fn eat2(r: R) -> i64 { 7 }\n\
             fn main() { let v = eat2(R { id: 41 }); println(f\"{v}\") }\n",
            "drop 41\n7\n",
        ),
    ] {
        assert_eq!(run(&format!("{DROPPER}{body}")), want, "{label}");
    }
}

/// B-2026-08-29-46, interpreter leg — the caller's fresh ARGUMENT temporaries
/// run their `Drop` bodies in REVERSE argument order, agreeing with JIT and AOT.
///
/// Every argument temporary in one call has the SAME live-range end —
/// design.md's temporary-lifetime table gives "Function/method call argument |
/// After the call returns" — so which of them dies first is settled by the rule
/// that sequences co-expiring values: the unified drop+defer stack is "a single
/// LIFO stack ordered by program-order of introduction" (design.md § Drop
/// ordering within a branch, rule 1). Arguments are introduced left to right,
/// so they pop right to left. The compiled backends got this for free — they
/// register the temps on a cleanup frame that drains LIFO — and this backend
/// walked the argument list forward, so `take(R { id: 1 }, R { id: 2 })` printed
/// `dR1 dR2` under `--interp` against `dR2 dR1` under `karac build`.
///
/// B-2026-08-30-51 — a SHADOWED binding drops its own value, exactly once, at
/// the shadowed name's NLL endpoint.
///
/// The interpreter's cleanup slots are NAME-KEYED and resolve through the env
/// when they fire. Two `let`s of one name pushed two slots with the same key,
/// and by drain time the env held only the survivor, so both fired on it: the
/// shadowed value's body never ran and the survivor's ran TWICE. A `Drop` body
/// is a user-visible effect, so the second half is a resource released twice,
/// not untidiness. The compiled backends key on the SLOT and were already
/// correct, which is what made this a run-vs-build divergence.
///
/// Every expectation here is the measured output of `karac run` and `karac
/// build` on the same program, so this is a parity pin as much as a behaviour
/// one. `move-rebind` is the guard rail: `let z = z;` is ONE object, and a
/// shadow slot must not resurrect the source's body after the move-suppression
/// helpers retracted it.
#[test]
fn test_shadowed_binding_drops_its_own_value() {
    const HDR: &str = "struct R { id: i64 }\n\
                       impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\") } }\n\
                       fn mk(n: i64) -> R { R { id: n } }\n";
    for (label, src, want) in [
        (
            "shadow-unread-first",
            format!(
                "{HDR}fn main() {{\n\
                 \x20   let t = mk(1);\n\
                 \x20   let t = mk(2);\n\
                 \x20   println(f\"t={{t.id}}\");\n\
                 }}\n"
            ),
            // B-2026-09-02-16 — each generation dies at its OWN live-range
            // end. `t`(1) is never read, so NLL kills it at its own `let`,
            // before `t`(2) exists; `t`(2) dies after the read. Until the
            // endpoint map was generation-keyed both shared the name's single
            // endpoint and drained LIFO there, giving `t=2 dR2 dR1`.
            "dR1\nt=2\ndR2\n",
        ),
        (
            "shadow-first-read-before-shadow",
            format!(
                "{HDR}fn main() {{\n\
                 \x20   let u = mk(3);\n\
                 \x20   println(f\"u={{u.id}}\");\n\
                 \x20   let u = mk(4);\n\
                 \x20   println(f\"u={{u.id}}\");\n\
                 }}\n"
            ),
            // B-2026-09-02-16 — the shadowed value IS dropped at its own last
            // read now. This cell used to read `u=3 u=4 dR4 dR3`, documenting
            // the name-keyed endpoint as a known deviation from design.md
            // § Drop ordering ("destructors fire at each binding's live-range
            // end"); generation-keying removed the deviation rather than the
            // documentation of it.
            "u=3\ndR3\nu=4\ndR4\n",
        ),
        (
            "shadow-three-deep",
            format!(
                "{HDR}fn main() {{\n\
                 \x20   let w = mk(5);\n\
                 \x20   let w = mk(6);\n\
                 \x20   let w = mk(7);\n\
                 \x20   println(f\"w={{w.id}}\");\n\
                 }}\n"
            ),
            "dR5\ndR6\nw=7\ndR7\n",
        ),
        (
            "shadow-neither-generation-read",
            format!(
                "{HDR}fn main() {{\n\
                 \x20   let b = mk(8);\n\
                 \x20   let b = mk(9);\n\
                 \x20   println(\"mid\");\n\
                 }}\n"
            ),
            // B-2026-09-02-16 — with no read anywhere BOTH generations die at
            // their own `let`, in declaration order. The old `dR9 dR8` came
            // from the two sharing one endpoint; the survivor's slot no longer
            // misses its own, because each generation now has one.
            "dR8\ndR9\nmid\n",
        ),
        (
            "shadow-inside-nested-block",
            format!(
                "{HDR}fn main() {{\n\
                 \x20   let x = {{ let y = mk(10); let y = mk(11); y.id }};\n\
                 \x20   println(f\"x={{x}}\");\n\
                 }}\n"
            ),
            "dR11\ndR10\nx=11\n",
        ),
        (
            "shadow-move-rebind-is-one-object",
            format!(
                "{HDR}fn main() {{\n\
                 \x20   let z = mk(12);\n\
                 \x20   let z = z;\n\
                 \x20   println(f\"z={{z.id}}\");\n\
                 }}\n"
            ),
            // GUARD RAIL. One object, one body: the move-suppression helpers
            // retract the source's slot before the shadow pass runs, so there
            // is nothing left to freeze. A pass that ran earlier would print
            // `dR12` twice.
            "z=12\ndR12\n",
        ),
        (
            "shadow-struct-with-drop-field",
            format!(
                "{HDR}struct W {{ r: R, n: i64 }}\n\
                 fn main() {{\n\
                 \x20   let a = W {{ r: mk(13), n: 1 }};\n\
                 \x20   let a = W {{ r: mk(14), n: 2 }};\n\
                 \x20   println(f\"a={{a.n}}\");\n\
                 }}\n"
            ),
            // The frozen value carries its fields, so the field walk reaches
            // the shadowed generation's `r` too — now at that generation's own
            // endpoint (B-2026-09-02-16), which for a never-read `a`(13) is its
            // own `let`, ahead of the surviving `a`(14)'s read.
            "dR13\na=2\ndR14\n",
        ),
        (
            "shadow-in-a-loop-body",
            format!(
                "{HDR}fn main() {{\n\
                 \x20   for i in 0..2 {{\n\
                 \x20       let h = mk(20 + i);\n\
                 \x20       let h = mk(30 + i);\n\
                 \x20       println(f\"h={{h.id}}\");\n\
                 \x20   }}\n\
                 }}\n"
            ),
            // Each iteration is its own scope, so the freeze must not leak
            // across them. Per iteration the never-read generation dies at its
            // own `let` and the survivor after its read (B-2026-09-02-16), so
            // the pair reads `dR20 h=30 dR30` rather than `h=30 dR30 dR20`.
            "dR20\nh=30\ndR30\ndR21\nh=31\ndR31\n",
        ),
        (
            "shadow-block-tail-moves-the-shadowed-name",
            format!(
                "{HDR}fn main() {{\n\
                 \x20   let x = {{ let y = mk(16); let y = mk(17); y }};\n\
                 \x20   println(f\"x={{x.id}}\");\n\
                 }}\n"
            ),
            "dR16\nx=17\ndR17\n",
        ),
        (
            "shadow-fn-tail-returns-the-shadowed-name",
            format!(
                "{HDR}fn f() -> R {{ let s = mk(18); let s = mk(19); s }}\n\
                 fn main() {{\n\
                 \x20   let q = f();\n\
                 \x20   println(f\"q={{q.id}}\");\n\
                 }}\n"
            ),
            "dR18\nq=19\ndR19\n",
        ),
        (
            "shadow-block-tail-field-read-is-not-a-move",
            format!(
                "{HDR}fn main() {{\n\
                 \x20   let z = {{ let w = mk(22); let w = mk(23); w.id }};\n\
                 \x20   println(f\"z={{z}}\");\n\
                 }}\n"
            ),
            "dR23\ndR22\nz=23\n",
        ),
        (
            "shadow-a-param",
            format!(
                "{HDR}fn f(r: R) -> i64 {{ let r = mk(99); r.id }}\n\
                 fn main() {{\n\
                 \x20   let d = f(mk(15));\n\
                 \x20   println(f\"d={{d}}\");\n\
                 }}\n"
            ),
            // A param shadowed inside its own frame. The caller still owns the
            // argument under caller-retains, so its body runs there.
            "dR99\ndR15\nd=99\n",
        ),
    ] {
        assert_eq!(run(&src), want, "case {label}");
    }
}

/// B-2026-09-15-34 — a shadowed binding whose `let` sends the value OUT THROUGH
/// A CALL AND BACK ran its user `Drop` body TWICE under `--interp`, against one
/// body on both compiled backends. Three-deep shadowing ran it three times.
///
/// The interpreter's cleanup slots are NAME-KEYED and resolve through the env
/// when they fire, so two live slots for one name both land on the survivor.
/// `let q = q` avoids that because `suppress_let_rebind_user_drop` retracts the
/// source's slot; that helper reads a bare identifier or a struct-literal field
/// initializer, and a value that travels out through `idr(q)` and back is moved
/// just as surely but was not seen. The repair retracts the stale slot for
/// exactly that spelling.
///
/// THE DISCRIMINATOR IS "DOES THE CALLEE HAND THAT ARGUMENT BACK", not "does the
/// RHS mention the name", and the three guard rails below are why. `let q =
/// mk(16)` does not mention `q` and must still freeze the shadowed value's body.
/// `takeb(q, mk(16))` and `fresh(q)` DO consume `q` and still must freeze,
/// because each hands back a DIFFERENT object — so a rule keyed on mentioning
/// the name, or on the new value being `Drop`-bearing, or on structural
/// equality (a callee may return an equal-but-distinct value) gets all three
/// wrong. `fn_always_returns_param` answers it directly, and its ALL-PATHS form
/// is deliberate: a callee that returns the argument on only some paths lets it
/// die inside on the others, where the frozen copy is what runs its body.
///
/// Every expectation here is the measured output of `karac run --interp` and
/// `karac build` on the same program, so this is a parity pin: the compiled
/// column was correct throughout and is what the interpreter is being held to.
///
/// TWO SPELLINGS ARE KNOWN TO REMAIN and are filed rather than pinned green
/// here: shadowing inside a NESTED BLOCK, where the outer binding's slot lives
/// in a cleanup list this retraction cannot reach, and an `if`/`match`-WRAPPED
/// RHS, which hands the value back on one path only and so is the "some paths"
/// case the all-paths predicate correctly declines.
#[test]
fn test_shadowed_rebind_through_a_call_drops_once() {
    const HDR: &str = "struct R { id: i64 }\n\
                       impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\") } }\n\
                       fn mk(n: i64) -> R { R { id: n } }\n\
                       fn idr(r: R) -> R { r }\n\
                       fn eat(r: R) -> i64 { r.id }\n\
                       fn two(a: R, b: R) -> R { a }\n\
                       fn takeb(a: R, b: R) -> R { b }\n\
                       fn fresh(a: R) -> R { R { id: 99 } }\n";
    for (label, src, want) in [
        (
            "shadow-through-call",
            format!("{HDR}fn main() {{ let q = mk(15); let q = idr(q); println(f\"q={{q.id}}\") }}\n"),
            "q=15\ndR15\n",
        ),
        (
            "shadow-through-call-unread",
            format!("{HDR}fn main() {{ let q = mk(15); let q = idr(q); println(\"mid\") }}\n"),
            "dR15\nmid\n",
        ),
        (
            "shadow-through-call-three-deep",
            format!(
                "{HDR}fn main() {{ let q = mk(15); let q = idr(q); let q = idr(q); println(f\"q={{q.id}}\") }}\n"
            ),
            "q=15\ndR15\n",
        ),
        (
            "shadow-through-two-arg-call",
            format!(
                "{HDR}fn main() {{ let q = mk(15); let b = mk(16); let q = two(q, b); println(f\"q={{q.id}}\") }}\n"
            ),
            "dR16\nq=15\ndR15\n",
        ),
        // Guard rail: the callee consumes `q` and hands back a DIFFERENT
        // object, so the shadowed value still needs its frozen body.
        (
            "guard-callee-returns-other-arg",
            format!(
                "{HDR}fn main() {{ let q = mk(15); let q = takeb(q, mk(16)); println(f\"q={{q.id}}\") }}\n"
            ),
            "q=16\ndR16\ndR15\n",
        ),
        (
            "guard-callee-returns-fresh",
            format!("{HDR}fn main() {{ let q = mk(15); let q = fresh(q); println(f\"q={{q.id}}\") }}\n"),
            "q=99\ndR99\ndR15\n",
        ),
        // Guard rail: the RHS does not mention `q` at all — the freeze is what
        // runs the shadowed value's body and must stay.
        (
            "guard-shadow-unrelated-value",
            format!("{HDR}fn main() {{ let q = mk(15); let q = mk(16); println(f\"q={{q.id}}\") }}\n"),
            // The never-read `q`(15) dies at the block's endpoint here rather
            // than at its own `let`, because the `println` is the block's FINAL
            // EXPRESSION; the statement spelling gives `dR15 q=16 dR16`. Both
            // backends agree on both spellings, which is what makes either a
            // legitimate pin.
            "q=16\ndR16\ndR15\n",
        ),
        // Guard rail: the call consumes `q` and returns a scalar, so the new
        // binding owns nothing and the frozen body is the only one.
        (
            "guard-call-returns-scalar",
            format!("{HDR}fn main() {{ let q = mk(15); let q = eat(q); println(f\"n={{q}}\") }}\n"),
            "n=15\ndR15\n",
        ),
        // Guard rail: the pre-existing bare-rebind and renamed spellings, which
        // were correct before this and must stay at one body.
        (
            "guard-bare-rebind",
            format!("{HDR}fn main() {{ let q = mk(15); let q = q; println(f\"q={{q.id}}\") }}\n"),
            "q=15\ndR15\n",
        ),
        (
            "guard-renamed-through-call",
            format!("{HDR}fn main() {{ let q = mk(15); let r = idr(q); println(f\"r={{r.id}}\") }}\n"),
            "r=15\ndR15\n",
        ),
    ] {
        assert_eq!(run(&src), want, "case {label}");
    }
}

/// The codegen twin is `e2e_owned_param_temps_drop_in_reverse_argument_order`
/// and asserts the SAME expected output for every case here, which is the whole
/// point: the counts always agreed, so nothing but an absolute expectation on
/// both sides can hold the order.
///
/// The guard rails matter as much as the fixed cases. The rule is program-order
/// of introduction, NOT argument position, and the two coincide only when every
/// argument is a fresh temp. A named binding passed as an argument is skipped by
/// this walk entirely and drops through its own owner, so `mixed-temp-first`
/// (`let b = …; take(R { id: 1 }, b)`) correctly prints FORWARD — `b` was
/// introduced first, so it pops last. A case list that only held all-temp calls
/// would be satisfied by a blanket reverse and would miss that.
#[test]
fn test_owned_param_temps_drop_in_reverse_argument_order() {
    const HDR: &str = "struct R { id: i64 }\n\
                       impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\") } }\n";
    for (label, src, want) in [
        (
            "two-fresh-temps",
            format!(
                "{HDR}fn take(r: R, q: R) -> i64 {{ 7 }}\n\
                 fn main() {{ let v = take(R {{ id: 1 }}, R {{ id: 2 }}); println(f\"v={{v}}\") }}\n"
            ),
            "dR2\ndR1\nv=7\n",
        ),
        (
            "three-fresh-temps",
            format!(
                "{HDR}fn take3(a: R, b: R, c: R) -> i64 {{ 7 }}\n\
                 fn main() {{\n\
                 \x20   let v = take3(R {{ id: 1 }}, R {{ id: 2 }}, R {{ id: 3 }});\n\
                 \x20   println(f\"v={{v}}\")\n\
                 }}\n"
            ),
            "dR3\ndR2\ndR1\nv=7\n",
        ),
        // The callee's OWN local is introduced after both params, so it pops
        // first and the params follow in reverse. Pre-fix this read
        // `dR3 dR1 dR2` — the local's position was already right and only the
        // two params were forward, which is the tell that one walk was at fault
        // rather than the whole cleanup order.
        (
            "callee-local-pops-before-params",
            format!(
                "{HDR}fn take(r: R, q: R) -> i64 {{ let z = R {{ id: 3 }}; 7 }}\n\
                 fn main() {{ let v = take(R {{ id: 1 }}, R {{ id: 2 }}); println(f\"v={{v}}\") }}\n"
            ),
            "dR3\ndR2\ndR1\nv=7\n",
        ),
        // A CALL-RESULT argument is a fresh temp by the same rule as a struct
        // literal; the walk resolves its type through the producing fn's
        // declared return head, so it is a separate arm of the same match.
        (
            "call-result-args",
            format!(
                "{HDR}fn mk(n: i64) -> R {{ R {{ id: n }} }}\n\
                 fn take(r: R, q: R) -> i64 {{ 7 }}\n\
                 fn main() {{ let v = take(mk(1), mk(2)); println(f\"v={{v}}\") }}\n"
            ),
            "dR2\ndR1\nv=7\n",
        ),
        (
            "two-calls-in-sequence",
            format!(
                "{HDR}fn take(r: R, q: R) -> i64 {{ 7 }}\n\
                 fn main() {{\n\
                 \x20   let v = take(R {{ id: 1 }}, R {{ id: 2 }});\n\
                 \x20   println(f\"v={{v}}\");\n\
                 \x20   let w = take(R {{ id: 3 }}, R {{ id: 4 }});\n\
                 \x20   println(f\"w={{w}}\")\n\
                 }}\n"
            ),
            "dR2\ndR1\nv=7\ndR4\ndR3\nw=7\n",
        ),
        // GUARD RAIL — a METHOD's arguments never reach this walk at all: they
        // are claimed by the callee frame through `method_param_drop_names`,
        // which already drained LIFO. These two were CORRECT pre-fix and are the
        // evidence that the interpreter was inconsistent with itself, not merely
        // with codegen.
        (
            "guard-method-two-temps",
            format!(
                "{HDR}struct H {{ n: i64 }}\n\
                 impl H {{ fn take(ref self, r: R, q: R) -> i64 {{ 7 }} }}\n\
                 fn main() {{\n\
                 \x20   let h = H {{ n: 0 }};\n\
                 \x20   let v = h.take(R {{ id: 1 }}, R {{ id: 2 }});\n\
                 \x20   println(f\"v={{v}}\")\n\
                 }}\n"
            ),
            "dR2\ndR1\nv=7\n",
        ),
        (
            "guard-method-three-temps",
            format!(
                "{HDR}struct H {{ n: i64 }}\n\
                 impl H {{ fn t3(ref self, a: R, b: R, c: R) -> i64 {{ 7 }} }}\n\
                 fn main() {{\n\
                 \x20   let h = H {{ n: 0 }};\n\
                 \x20   let v = h.t3(R {{ id: 1 }}, R {{ id: 2 }}, R {{ id: 3 }});\n\
                 \x20   println(f\"v={{v}}\")\n\
                 }}\n"
            ),
            "dR3\ndR2\ndR1\nv=7\n",
        ),
        // GUARD RAIL — MOVED LOCALS, not temps. This walk skips identifier
        // arguments outright, so these drop through the callee frame; they were
        // already reverse and must stay reverse.
        (
            "guard-moved-locals",
            format!(
                "{HDR}fn take(r: R, q: R) -> i64 {{ 7 }}\n\
                 fn main() {{\n\
                 \x20   let a = R {{ id: 1 }};\n\
                 \x20   let b = R {{ id: 2 }};\n\
                 \x20   let v = take(a, b);\n\
                 \x20   println(f\"v={{v}}\")\n\
                 }}\n"
            ),
            "dR2\ndR1\nv=7\n",
        ),
        // GUARD RAILS — MIXED. Program-order of introduction is the rule, and
        // here it diverges from argument position: the local is introduced at
        // its `let`, the temp during argument evaluation. `mixed-temp-first`
        // therefore prints FORWARD (`dR1 dR2`) and a blanket reverse would break
        // it. Both were already correct pre-fix and must stay so.
        (
            "guard-mixed-local-first",
            format!(
                "{HDR}fn take(r: R, q: R) -> i64 {{ 7 }}\n\
                 fn main() {{\n\
                 \x20   let a = R {{ id: 1 }};\n\
                 \x20   let v = take(a, R {{ id: 2 }});\n\
                 \x20   println(f\"v={{v}}\")\n\
                 }}\n"
            ),
            "dR2\ndR1\nv=7\n",
        ),
        // B-2026-08-29-54, formerly pinned below at the divergence: a STATIC
        // (associated) function's fresh-temp arguments ran no `Drop` body at all
        // on JIT and AOT — not misordered, MISSING — while this backend ran
        // both. The compiled arm now agrees, so this is an ordinary live case
        // and the two suites assert the same string, which is what closing the
        // divergence means.
        (
            "static-two-fresh-temps",
            format!(
                "{HDR}struct H {{ n: i64 }}\n\
                 impl H {{ fn s2(a: R, b: R) -> i64 {{ 7 }} }}\n\
                 fn main() {{ let v = H.s2(R {{ id: 1 }}, R {{ id: 2 }}); println(f\"v={{v}}\") }}\n"
            ),
            "dR2\ndR1\nv=7\n",
        ),
        (
            "guard-mixed-temp-first",
            format!(
                "{HDR}fn take(r: R, q: R) -> i64 {{ 7 }}\n\
                 fn main() {{\n\
                 \x20   let b = R {{ id: 2 }};\n\
                 \x20   let v = take(R {{ id: 1 }}, b);\n\
                 \x20   println(f\"v={{v}}\")\n\
                 }}\n"
            ),
            "dR1\ndR2\nv=7\n",
        ),
    ] {
        assert_eq!(run(&src), want, "case {label}");
    }
    // TIMING rather than order (B-2026-08-29-55, formerly pinned here as
    // `pin-arg-temps-die-at-call-return-not-statement-end` — this backend was
    // the CORRECT half of that divergence, and the pin recorded it while the
    // compiled half held all four temps to the statement's `;`). design.md's
    // table ends an argument temporary's live range "After the call returns",
    // so with two calls in one expression the first call's temps die before the
    // second call's are built.
    //
    // These strings are byte-identical to the codegen twin's, which is what
    // makes the pair a regression test rather than two opinions: every body ran
    // exactly once on both backends throughout, so only an absolute expectation
    // on both sides can hold the ORDERING. Nothing here changed when the
    // compiled side was fixed — this block is the oracle it was fixed against,
    // and it is kept in the live list so a future change to this backend has to
    // move both halves together.
    for (label, src, want) in [
        (
            "two-calls-one-statement-free",
            format!(
                "{HDR}fn take(r: R, q: R) -> i64 {{ println(\"in-take\"); 7 }}\n\
                 fn main() {{\n\
                 \x20   let v = take(R {{ id: 1 }}, R {{ id: 2 }}) + take(R {{ id: 3 }}, R {{ id: 4 }});\n\
                 \x20   println(f\"v={{v}}\")\n\
                 }}\n"
            ),
            "in-take\ndR2\ndR1\nin-take\ndR4\ndR3\nv=14\n",
        ),
        (
            "two-calls-one-statement-method",
            format!(
                "{HDR}struct H {{ n: i64 }}\n\
                 impl H {{ fn take(ref self, r: R, q: R) -> i64 {{ println(\"in-m\"); 7 }} }}\n\
                 fn main() {{\n\
                 \x20   let h = H {{ n: 0 }};\n\
                 \x20   let v = h.take(R {{ id: 1 }}, R {{ id: 2 }}) + h.take(R {{ id: 3 }}, R {{ id: 4 }});\n\
                 \x20   println(f\"v={{v}}\")\n\
                 }}\n"
            ),
            "in-m\ndR2\ndR1\nin-m\ndR4\ndR3\nv=14\n",
        ),
        (
            "two-calls-one-statement-assoc",
            format!(
                "{HDR}struct H {{ n: i64 }}\n\
                 impl H {{ fn s2(r: R, q: R) -> i64 {{ println(\"in-s\"); 7 }} }}\n\
                 fn main() {{\n\
                 \x20   let v = H.s2(R {{ id: 1 }}, R {{ id: 2 }}) + H.s2(R {{ id: 3 }}, R {{ id: 4 }});\n\
                 \x20   println(f\"v={{v}}\")\n\
                 }}\n"
            ),
            "in-s\ndR2\ndR1\nin-s\ndR4\ndR3\nv=14\n",
        ),
        (
            "mixed-wrapper-arg-fresh-branch-taken",
            format!(
                "{HDR}fn mk(n: i64) -> R {{ R {{ id: n }} }}\n\
                 fn one(r: R) -> i64 {{ println(\"in-one\"); r.id }}\n\
                 fn main() {{\n\
                 \x20   let k = mk(30);\n\
                 \x20   let v = one(if false {{ k }} else {{ mk(31) }});\n\
                 \x20   println(f\"v={{v}}\");\n\
                 }}\n"
            ),
            // TWO objects die here and both must run: the argument temp the
            // taken (minting) arm produced, and `k`, which the untaken arm
            // never handed over so it dies in place. B-2026-08-30-38 ran
            // only `k`'s.
            "in-one\ndR31\ndR30\nv=31\n",
        ),
        (
            "mixed-wrapper-arg-binding-branch-taken",
            format!(
                "{HDR}fn mk(n: i64) -> R {{ R {{ id: n }} }}\n\
                 fn one(r: R) -> i64 {{ println(\"in-one\"); r.id }}\n\
                 fn main() {{\n\
                 \x20   let j = mk(40);\n\
                 \x20   let v = one(if true {{ j }} else {{ mk(41) }});\n\
                 \x20   println(f\"v={{v}}\");\n\
                 }}\n"
            ),
            // ONE object, ONE body. The conditional-move flag disarms `j`
            // on this path, so the argument temp is the single owner —
            // this is the case that would double-fire if the binding kept
            // its own drop, and it is why the seed must reach here.
            "in-one\ndR40\nv=40\n",
        ),
        (
            "all-places-wrapper-arg-is-not-seeded",
            format!(
                "{HDR}fn mk(n: i64) -> R {{ R {{ id: n }} }}\n\
                 fn one(r: R) -> i64 {{ println(\"in-one\"); r.id }}\n\
                 fn main() {{\n\
                 \x20   let a1 = mk(1);\n\
                 \x20   let a2 = mk(2);\n\
                 \x20   let v = one(if true {{ a1 }} else {{ a2 }});\n\
                 \x20   println(f\"v={{v}}\");\n\
                 }}\n"
            ),
            // No tail MINTS, so nothing can name a type for an argument
            // temp — and the seed is withheld for exactly that reason.
            // Seeding it anyway disarmed `a1` with no registration to
            // replace it and its body vanished on all three compiled
            // surfaces; both bindings stay armed and die in place instead.
            "in-one\ndR2\ndR1\nv=1\n",
        ),
        (
            "mixed-wrapper-arg-in-statement-position",
            format!(
                "{HDR}fn mk(n: i64) -> R {{ R {{ id: n }} }}\n\
                 fn one(r: R) -> i64 {{ println(\"in-one\"); r.id }}\n\
                 fn main() {{\n\
                 \x20   let b = mk(10);\n\
                 \x20   one(if false {{ b }} else {{ mk(11) }});\n\
                 \x20   println(\"after\");\n\
                 }}\n"
            ),
            // A DISCARDED call still consumes its arguments, so its
            // wrapper arguments are seeded like a `let`'s. A discarded
            // `if` is deliberately not — its arm tails have no consumer.
            "in-one\ndR11\ndR10\nafter\n",
        ),
        (
            "mixed-wrapper-arg-method-call",
            format!(
                "{HDR}fn mk(n: i64) -> R {{ R {{ id: n }} }}\n\
                 struct H {{ n: i64 }}\n\
                 impl H {{ fn take(mut ref self, r: R) -> i64 {{ println(\"in-take\"); r.id }} }}\n\
                 fn main() {{\n\
                 \x20   let c = mk(20);\n\
                 \x20   let mut h = H {{ n: 0 }};\n\
                 \x20   let v = h.take(if false {{ c }} else {{ mk(21) }});\n\
                 \x20   println(f\"v={{v}}\");\n\
                 }}\n"
            ),
            "in-take\ndR21\ndR20\nv=21\n",
        ),
        (
            "mixed-wrapper-arg-passthrough-callee",
            format!(
                "{HDR}fn mk(n: i64) -> R {{ R {{ id: n }} }}\n\
                 fn pass(r: R) -> R {{ r }}\n\
                 fn main() {{\n\
                 \x20   let d = mk(30);\n\
                 \x20   let v = pass(if false {{ d }} else {{ mk(31) }});\n\
                 \x20   println(f\"v={{v.id}}\");\n\
                 }}\n"
            ),
            // The passthrough guard still holds through the widening: the
            // minting arm's value travels out of the call, so the RESULT's
            // owner runs its body at its own last use, after the read.
            "dR30\nv=31\ndR31\n",
        ),
        (
            "mixed-wrapper-arg-match-arm-binding",
            format!(
                "{HDR}fn mk(n: i64) -> R {{ R {{ id: n }} }}\n\
                 fn one(r: R) -> i64 {{ println(\"in-one\"); r.id }}\n\
                 fn main() {{\n\
                 \x20   let g = mk(50);\n\
                 \x20   let v = one(match 2 {{ 1 => g, _ => mk(51) }});\n\
                 \x20   println(f\"v={{v}}\");\n\
                 }}\n"
            ),
            "in-one\ndR51\ndR50\nv=51\n",
        ),
        (
            "wrapper-arg-struct-literal-tail",
            format!(
                "{HDR}fn one(r: R) -> i64 {{ println(\"in-one\"); r.id }}\n\
                 fn main() {{\n\
                 \x20   let v = one(if true {{ R {{ id: 1 }} }} else {{ R {{ id: 2 }} }});\n\
                 \x20   println(f\"v={{v}}\");\n\
                 }}\n"
            ),
            "in-one\ndR1\nv=1\n",
        ),
        (
            "wrapper-arg-method-receiver",
            format!(
                "{HDR}fn mk(n: i64) -> R {{ R {{ id: n }} }}\n\
                 struct H {{ n: i64 }}\n\
                 impl H {{ fn take(mut ref self, r: R) -> i64 {{ println(\"in-take\"); r.id }} }}\n\
                 fn main() {{\n\
                 \x20   let mut h = H {{ n: 0 }};\n\
                 \x20   let v = h.take(if true {{ mk(10) }} else {{ mk(11) }});\n\
                 \x20   println(f\"v={{v}}\");\n\
                 }}\n"
            ),
            "in-take\ndR10\nv=10\n",
        ),
        (
            "wrapper-arg-two-args-reverse-order",
            format!(
                "{HDR}fn mk(n: i64) -> R {{ R {{ id: n }} }}\n\
                 fn two(a: R, b: R) -> i64 {{ println(\"in-two\"); a.id + b.id }}\n\
                 fn main() {{\n\
                 \x20   let v = two(if true {{ mk(1) }} else {{ mk(2) }}, if true {{ mk(3) }} else {{ mk(4) }});\n\
                 \x20   println(f\"v={{v}}\");\n\
                 }}\n"
            ),
            // Both temps expire when the call returns, so program order of
            // introduction decides: they pop right to left (B-2026-08-29-46).
            "in-two\ndR3\ndR1\nv=4\n",
        ),
        (
            "wrapper-arg-enum-payload-tail",
            format!(
                "{HDR}fn mk(n: i64) -> R {{ R {{ id: n }} }}\n\
                 enum E {{ V(R), U }}\n\
                 fn takee(e: E) -> i64 {{ println(\"in-takee\"); match e {{ E.V(r) => r.id, E.U => 0 }} }}\n\
                 fn main() {{\n\
                 \x20   let v = takee(if true {{ E.V(mk(30)) }} else {{ E.U }});\n\
                 \x20   println(f\"v={{v}}\");\n\
                 }}\n"
            ),
            "in-takee\ndR30\nv=30\n",
        ),
        (
            "wrapper-arg-drop-bearing-field",
            format!(
                "{HDR}fn mk(n: i64) -> R {{ R {{ id: n }} }}\n\
                 struct W {{ r: R, n: i64 }}\n\
                 fn takew(w: W) -> i64 {{ println(\"in-takew\"); w.n }}\n\
                 fn main() {{\n\
                 \x20   let v = takew(if true {{ W {{ r: mk(50), n: 5 }} }} else {{ W {{ r: mk(51), n: 6 }} }});\n\
                 \x20   println(f\"v={{v}}\");\n\
                 }}\n"
            ),
            "in-takew\ndR50\nv=5\n",
        ),
        (
            "wrapper-arg-shared-tail-stays-with-rc",
            format!(
                "{HDR}shared struct S {{ id: i64 }}\n\
                 fn takes(s: S) -> i64 {{ println(\"in-takes\"); s.id }}\n\
                 fn main() {{\n\
                 \x20   let v = takes(if true {{ S {{ id: 40 }} }} else {{ S {{ id: 41 }} }});\n\
                 \x20   println(f\"v={{v}}\");\n\
                 }}\n"
            ),
            // A `shared` tail is refcounted; the rc machinery owns its release,
            // so the argument walk must NOT claim it.
            "in-takes\nv=40\n",
        ),
        (
            "wrapper-arg-block-local-shadowed-binding",
            format!(
                "{HDR}fn mk(n: i64) -> R {{ R {{ id: n }} }}\n\
                 fn one(r: R) -> i64 {{ println(\"in-one\"); r.id }}\n\
                 fn main() {{\n\
                 \x20   let v = one({{ let t = mk(90); let t = mk(91); t }});\n\
                 \x20   println(f\"v={{v}}\");\n\
                 }}\n"
            ),
            // The LAST binding produces the tail; taking the first would
            // classify on an RHS that no longer yields the handed-out value.
            //
            // `mk(90)`'s body was LOST here when B-2026-08-30-38 landed, noted
            // then as "a separate shadowing gap". B-2026-08-30-51 closed it on
            // THIS backend, so the shadowed value now dies inside the block,
            // before the call. The compiled twin still loses it — see the
            // deliberate divergence recorded there.
            "dR90\nin-one\ndR91\nv=91\n",
        ),
        (
            "wrapper-arg-passthrough-callee-still-defers",
            format!(
                "{HDR}fn mk(n: i64) -> R {{ R {{ id: n }} }}\n\
                 fn pass(r: R) -> R {{ r }}\n\
                 fn main() {{\n\
                 \x20   let v = pass(if true {{ mk(60) }} else {{ mk(61) }});\n\
                 \x20   println(f\"v={{v.id}}\");\n\
                 }}\n"
            ),
            // The wrapper redirect must not defeat the passthrough guard: the
            // value travels out of the call, so the RESULT's owner runs the
            // body, once, at its own last use.
            "v=60\ndR60\n",
        ),
        (
            "deep-nest-inner-call-drains-first",
            format!(
                "{HDR}fn mk(n: i64) -> R {{ R {{ id: n }} }}\n\
                 fn one(r: R) -> i64 {{ println(\"in-one\"); 1 }}\n\
                 fn main() {{\n\
                 \x20   let v = one(mk(one(mk(1)) + 1));\n\
                 \x20   println(f\"v={{v}}\")\n\
                 }}\n"
            ),
            "in-one\ndR1\nin-one\ndR2\nv=1\n",
        ),
    ] {
        assert_eq!(run(&src), want, "case {label}");
    }
    // B-2026-08-30-38, FIXED — the interpreter half, byte-identical to the
    // codegen twin (`e2e_owned_param_temps_drop_in_reverse_argument_order`).
    // An argument that is a CONTROL-FLOW or BLOCK expression rather than a
    // direct call/literal used to lose its `Drop` body ENTIRELY, on this
    // backend as much as on the compiled ones — which is what made it a
    // BOTH-BACKENDS fix rather than a parity repair, and why no A/B gate and no
    // kata could see it. `wrapper_tail_arg_type_name` now resolves the argument
    // through its tails. `one(mk(6))` and the let-bound `one(e0)` stay as
    // CONTROLS: both worked before the fix, which is precisely why a spot-check
    // of this bug came back clean.
    let pin_src = format!(
        "{HDR}fn mk(n: i64) -> R {{ R {{ id: n }} }}\n\
         fn one(r: R) -> i64 {{ println(\"in-one\"); r.id }}\n\
         enum P {{ A, B }}\n\
         fn main() {{\n\
         \x20   let a = one(if true {{ mk(1) }} else {{ mk(2) }});\n\
         \x20   println(f\"a={{a}}\")\n\
         \x20   let p = P.A;\n\
         \x20   let b = one(match p {{ P.A => mk(3), P.B => mk(4) }});\n\
         \x20   println(f\"b={{b}}\")\n\
         \x20   let c = one({{ let t = mk(5); t }});\n\
         \x20   println(f\"c={{c}}\")\n\
         \x20   let d = one(mk(6));\n\
         \x20   println(f\"d={{d}}\")\n\
         \x20   let e0 = if true {{ mk(7) }} else {{ mk(8) }};\n\
         \x20   let e = one(e0);\n\
         \x20   println(f\"e={{e}}\")\n\
         }}\n"
    );
    assert_eq!(
        run(&pin_src),
        // Every line runs its body exactly once, at the call's return.
        "in-one\ndR1\na=1\nin-one\ndR3\nb=3\nin-one\ndR5\nc=5\n\
         in-one\ndR6\nd=6\nin-one\ndR7\ne=7\n",
        "case control-flow-argument-runs-its-drop-body",
    );
}

/// B-2026-08-29-15, interpreter leg — a NAMED binding passed by value to a
/// callee that hands it straight back runs its user `Drop` body ONCE.
///
/// The sibling above pins the FRESH-TEMP spelling of the same call. This pins
/// the NAMED one, which is the axis that actually decided the count: pre-fix
/// every named case here printed `drop 41` / `41` / `drop 41` — two bodies for
/// one object — while every fresh-temp case printed one. Free function and
/// method, `return r;` and block tail, all four the same, and all four AGREEING
/// with the compiled backends, so no A/B gate could see any of it.
///
/// One object is not an assumption here, it is a measurement. With a
/// 256-element `Vec[i64]` field the named and fresh-temp spellings allocate
/// `11 allocs, 11 frees, 10,269 bytes` — byte-for-byte identical — so the entry
/// copy the compiled backends do make cannot be what makes their body counts
/// differ, and `fresh-temp-control` below has always run ONE body with that
/// copy present. That matters because the opposite inference ("a by-value
/// struct param is ENTRY-COPIED, so two values genuinely exist and two bodies
/// are consistent") was written into three places in the tree and is what held
/// this row open.
///
/// `caller-renamed` is the case that keeps a name-coincidence fix honest: the
/// caller spells its binding `qq` while the param is `r`. The interpreter's
/// moved-out sets are keyed by bare binding name (B-2026-08-29-11), so a fix
/// that worked only when the two names matched would pass every other row here.
///
/// B-2026-08-29-50 added the last four rows. The AGGREGATE shape
/// (`fn wrapf(r: R) -> Hh { Hh { r: r } }`) and the CONDITIONAL callee were
/// both excluded here while they still ran two bodies; both now run one, and
/// the gate that admits them is the union of `fn_always_returns_param` and
/// `fn_conditionally_returns_param_bare` — the two shapes where some other
/// frame is guaranteed to own the body on every path. The conditional rows
/// exercise BOTH of its paths in one program, because pre-fix each doubled
/// against a different second owner (the callee's own registration when the
/// param dies inside, the result binding when it is handed back).
///
/// Twin: `tests/codegen.rs`'s
/// `e2e_named_arg_returned_bare_user_drop_body_runs_once`, sharing these
/// expectations verbatim.
#[test]
fn test_named_arg_returned_bare_user_drop_body_runs_once() {
    const DROPPER: &str = "struct R { id: i64 }\n\
         impl Drop for R { fn drop(mut ref self) { println(f\"drop {self.id}\") } }\n\
         struct B6 { n: i64 }\n";
    for (label, body, want) in [
        // The row's shape, free-function spelling. Pre-fix two bodies.
        (
            "free-named-return",
            "fn takef(r: R) -> R { return r; }\n\
             fn main() { let a = R { id: 41 }; let x = takef(a); println(f\"{x.id}\") }\n",
            "41\ndrop 41\n",
        ),
        // The BLOCK-TAIL spelling. The row framed `return` vs tail as the
        // deciding axis; it is not one — both doubled, and both are fixed.
        (
            "free-named-tail",
            "fn keepf(r: R) -> R { r }\n\
             fn main() { let a = R { id: 41 }; let x = keepf(a); println(f\"{x.id}\") }\n",
            "41\ndrop 41\n",
        ),
        // The METHOD spelling. The row called this method-specific; it is not
        // that either — the free-function twin above doubled identically.
        (
            "method-named-return",
            "impl B6 { fn take(ref self, r: R) -> R { return r; } }\n\
             fn main() { let b = B6 { n: 1 }; let a = R { id: 41 }; let x = b.take(a); \
             println(f\"{x.id}\") }\n",
            "41\ndrop 41\n",
        ),
        (
            "method-named-tail",
            "impl B6 { fn keep(ref self, r: R) -> R { r } }\n\
             fn main() { let b = B6 { n: 1 }; let a = R { id: 41 }; let x = b.keep(a); \
             println(f\"{x.id}\") }\n",
            "41\ndrop 41\n",
        ),
        // The caller's binding spelled DIFFERENTLY from the param — see doc.
        (
            "caller-renamed",
            "impl B6 { fn take(ref self, r: R) -> R { return r; } }\n\
             fn main() { let b = B6 { n: 1 }; let qq = R { id: 41 }; let x = b.take(qq); \
             println(f\"{x.id}\") }\n",
            "41\ndrop 41\n",
        ),
        // CONTROL — the fresh-temp spelling, correct before this fix and the
        // oracle that makes ONE body the right answer rather than a preference.
        (
            "fresh-temp-control",
            "fn takef(r: R) -> R { return r; }\n\
             fn main() { let x = takef(R { id: 41 }); println(f\"{x.id}\") }\n",
            "41\ndrop 41\n",
        ),
        // BOUNDARY — the param DIES inside, so the caller must keep firing.
        // Over-widen the stand-down to every by-value arg and this body is lost
        // entirely: nobody else owns it.
        (
            "dies-inside-keeps-caller-fire",
            "fn eatf(r: R) -> i64 { println(f\"saw {r.id}\"); return 0; }\n\
             fn main() { let a = R { id: 41 }; let n = eatf(a); println(f\"{n}\") }\n",
            "saw 41\ndrop 41\n0\n",
        ),
        // B-2026-08-29-50 — the param moved into a RETURNED AGGREGATE, in
        // struct, method and tuple spellings. The returned value owns it, so
        // the caller's body is a duplicate exactly as in the bare rows above.
        (
            "aggregate-return-free",
            "struct Hh { r: R }\n\
             fn wrapf(r: R) -> Hh { return Hh { r: r }; }\n\
             fn main() { let a = R { id: 41 }; let h = wrapf(a); println(f\"{h.r.id}\") }\n",
            "41\ndrop 41\n",
        ),
        (
            "aggregate-tail-method",
            "struct Hh { r: R }\n\
             impl B6 { fn wrap(ref self, r: R) -> Hh { Hh { r: r } } }\n\
             fn main() { let b = B6 { n: 1 }; let a = R { id: 21 }; let h = b.wrap(a); \
             println(f\"{h.r.id}\") }\n",
            "21\ndrop 21\n",
        ),
        // The TUPLE spelling — `yields` recurses into tuple literals as well as
        // struct ones, so a fix handling only `StructLiteral` passes the two
        // rows above and fails this one.
        (
            "aggregate-tuple-free",
            "fn tupf(r: R) -> (R, i64) { (r, 9) }\n\
             fn main() { let a = R { id: 17 }; let t = tupf(a); println(f\"{t.1}\") }\n",
            "9\ndrop 17\n",
        ),
        // B-2026-08-29-50 — the CONDITIONAL callee on BOTH paths in one
        // program. `k = true` lets the param die inside, where the callee frame
        // owns the body; `k = false` hands it back, where the result binding
        // does. Pre-fix both doubled, against those two different owners, which
        // is why neither standing the caller down alone nor stopping the callee
        // registering alone would have fixed this shape.
        (
            "conditional-both-paths-free",
            "fn pick(r: R, k: bool) -> R { if k { return R { id: 98 }; } r }\n\
             fn main() { let a = R { id: 7 }; let x = pick(a, true); println(f\"{x.id}\"); \
             let b = R { id: 5 }; let y = pick(b, false); println(f\"{y.id}\") }\n",
            "drop 7\n98\ndrop 98\n5\ndrop 5\n",
        ),
        (
            "conditional-both-paths-method",
            "impl B6 { fn pick(ref self, r: R, k: bool) -> R \
             { if k { return R { id: 98 }; } r } }\n\
             fn main() { let t = B6 { n: 1 }; let a = R { id: 7 }; let x = t.pick(a, true); \
             println(f\"{x.id}\"); let b = R { id: 5 }; let y = t.pick(b, false); \
             println(f\"{y.id}\") }\n",
            "drop 7\n98\ndrop 98\n5\ndrop 5\n",
        ),
    ] {
        assert_eq!(run(&format!("{DROPPER}{body}")), want, "{label}");
    }
}

/// B-2026-08-28-17, interpreter leg — one user `Drop` body per object when the
/// callee returns a FIELD pulled out of an owned struct param. The struct twin
/// of `test_returned_tuple_param_element_user_drop_body_runs_once`.
///
/// Pre-fix this printed `drop 41` twice for a single `R`: once from
/// `run_fresh_temp_arg_drops`' SHAPE-2 arm, which walks every Drop-bearing
/// field of a struct-literal argument on the theory that the whole temp dies
/// inside the call, and once at the result binding's end. All three backends
/// did it, so no run-vs-build gate could see it — they agreed, at the wrong
/// number.
///
/// Unlike the tuple leg, the ORDER here matches the compiled backends on every
/// row, so these expectations are shared with the codegen twin verbatim rather
/// than being this backend's own. That is worth stating because the tuple leg's
/// order DOES split (B-2026-08-28-19) and the natural assumption is that the
/// struct leg inherits it; measured on all twelve rows, it does not.
///
/// `two-droppers-a` / `two-droppers-b` are the rows that constrain the fix: the
/// mask has to be per-FIELD, since suppressing the walk for the whole argument
/// silences the field that really does die in the call. Twin:
/// `tests/codegen.rs`'s
/// `e2e_user_drop_body_of_a_returned_struct_param_field_runs_once`, whose doc
/// lists the three neighbouring shapes (B-2026-08-28-21/-22/-23) left out of
/// both fixtures so neither pins a count that is still wrong.
#[test]
fn test_returned_struct_param_field_user_drop_body_runs_once() {
    const DROPPER: &str = "struct R { id: i64 }\n\
         impl Drop for R { fn drop(mut ref self) { println(f\"drop {self.id}\") } }\n";
    for (label, body, want) in [
        // The row's own repro.
        (
            "destructure-return",
            "struct W { r: R, n: i64 }\n\
             fn take(w: W) -> R { let W { r, n } = w; r }\n\
             fn main() { let x = take(W { r: R { id: 41 }, n: 1 }); println(f\"{x.id}\") }\n",
            "41\ndrop 41\n",
        ),
        // No destructure anywhere — a direct field projection, which is what
        // shows the trigger is a PART of the param escaping rather than the
        // `let` the row was filed against.
        (
            "projection-return",
            "struct W { r: R, n: i64 }\n\
             fn take(w: W) -> R { w.r }\n\
             fn main() { let x = take(W { r: R { id: 41 }, n: 1 }); println(f\"{x.id}\") }\n",
            "41\ndrop 41\n",
        ),
        (
            "explicit-return",
            "struct W { r: R, n: i64 }\n\
             fn take(w: W) -> R { let W { r, n } = w; return r; }\n\
             fn main() { let x = take(W { r: R { id: 41 }, n: 1 }); println(f\"{x.id}\") }\n",
            "41\ndrop 41\n",
        ),
        // The escaping part is keyed by the FIELD name, not by the binding the
        // pattern introduces for it.
        (
            "renamed-leaf",
            "struct W { r: R, n: i64 }\n\
             fn take(w: W) -> R { let W { r: inner, n } = w; inner }\n\
             fn main() { let x = take(W { r: R { id: 41 }, n: 1 }); println(f\"{x.id}\") }\n",
            "41\ndrop 41\n",
        ),
        // Both fields drop, only `a` escapes — the row that forces the mask to
        // be per-field: suppressing the whole argument fixes 41 and silences 42.
        (
            "two-droppers-a",
            "struct W { a: R, b: R }\n\
             fn take(w: W) -> R { let W { a, b } = w; a }\n\
             fn main() { let x = take(W { a: R { id: 41 }, b: R { id: 42 } }); println(f\"{x.id}\") }\n",
            "drop 42\n41\ndrop 41\n",
        ),
        // The same struct with the OTHER field escaping — pins the mask to the
        // right index rather than merely to "one of them".
        (
            "two-droppers-b",
            "struct W { a: R, b: R }\n\
             fn take(w: W) -> R { let W { a, b } = w; b }\n\
             fn main() { let x = take(W { a: R { id: 41 }, b: R { id: 42 } }); println(f\"{x.id}\") }\n",
            "drop 41\n42\ndrop 42\n",
        ),
        // The field escapes INSIDE a returned struct literal rather than bare.
        (
            "escape-via-struct-literal",
            "struct W { r: R, n: i64 }\n\
             struct Q { r: R }\n\
             fn take(w: W) -> Q { let W { r, n } = w; Q { r: r } }\n\
             fn main() { let x = take(W { r: R { id: 41 }, n: 1 }); println(f\"{x.r.id}\") }\n",
            "41\ndrop 41\n",
        ),
        (
            "result-discarded",
            "struct W { r: R, n: i64 }\n\
             fn take(w: W) -> R { let W { r, n } = w; r }\n\
             fn main() { take(W { r: R { id: 41 }, n: 1 }); println(\"end\") }\n",
            "drop 41\nend\n",
        ),
        // Two callees over the SAME struct type disagreeing about which field
        // escapes — neither may be handed the other's answer.
        (
            "two-callees",
            "struct W { r: R, n: i64 }\n\
             fn keep(w: W) -> R { let W { r, n } = w; r }\n\
             fn eat(w: W) -> i64 { let W { r, n } = w; n }\n\
             fn main() { let a = keep(W { r: R { id: 41 }, n: 1 }); println(f\"{a.id}\")\n\
             \x20           let b = eat(W { r: R { id: 42 }, n: 2 }); println(f\"{b}\") }\n",
            "41\ndrop 41\ndrop 42\n2\n",
        ),
        // CONTROL — a NON-field is returned, so the dropper dies in the call
        // and the caller-side walk is its only body.
        (
            "no-field-escapes-control",
            "struct W { r: R, n: i64 }\n\
             fn take(w: W) -> i64 { let W { r, n } = w; n }\n\
             fn main() { let x = take(W { r: R { id: 41 }, n: 1 }); println(f\"{x}\") }\n",
            "drop 41\n1\n",
        ),
        // CONTROL — param returned BARE, the pre-existing whole-param guard.
        (
            "bare-param-control",
            "fn take(r: R) -> R { r }\n\
             fn main() { let x = take(R { id: 41 }); println(f\"{x.id}\") }\n",
            "41\ndrop 41\n",
        ),
        // CONTROL — two droppers and NOTHING escapes; both fire in the call, in
        // reverse declaration order.
        (
            "nothing-escapes-control",
            "struct W { a: R, b: R }\n\
             fn take(w: W) -> i64 { let W { a, b } = w; 7 }\n\
             fn main() { let x = take(W { a: R { id: 41 }, b: R { id: 42 } }); println(f\"{x}\") }\n",
            "drop 42\ndrop 41\n7\n",
        ),
    ] {
        assert_eq!(run(&format!("{DROPPER}{body}")), want, "{label}");
    }
}

/// B-2026-08-28-21 — the same escaping-field mask for a parent that declares
/// its OWN `Drop`. Interpreter twin of `tests/codegen.rs`'s
/// `e2e_own_drop_parent_runs_a_returned_fields_body_once`, whose doc carries
/// the full reasoning.
///
/// The fixture above masks the field walk for a parent that carries a
/// Drop-bearing field but declares no `Drop` itself. A parent WITH one takes
/// the earlier `run_user_drop_body_on_value` branch and never reaches that
/// mask, so the escaping field's body ran here and again at the result's owner.
/// The helper is `run_user_drop_body_only` followed by the field walk, so only
/// the second half needed masking — the parent's own body still sees the WHOLE
/// value, because it may read the field it is about to hand back.
///
/// `result-discarded` is here and NOT in the codegen twin, deliberately. This
/// fix settles the shape's COUNTS on every backend, but the two compiled
/// backends emit the field's body before the parent's while this one emits the
/// parent's first — the order design.md § Drop ordering specifies. That
/// divergence predates this fix and is filed as B-2026-08-28-53; pinning the
/// right order here and leaving the compiled twin silent is what keeps this
/// fixture honest about which half is correct.
#[test]
fn test_own_drop_parent_runs_a_returned_fields_body_once() {
    const DROPPER: &str = "struct R { id: i64 }\n\
         impl Drop for R { fn drop(mut ref self) { println(f\"drop {self.id}\") } }\n\
         struct W { r: R, n: i64 }\n\
         impl Drop for W { fn drop(mut ref self) { println(f\"drop W{self.n}\") } }\n";
    for (label, body, want) in [
        (
            "destructure-return",
            "fn take(w: W) -> R { let W { r, n } = w; r }\n\
             fn main() { let x = take(W { r: R { id: 41 }, n: 1 }); println(f\"{x.id}\") }\n",
            "drop W1\n41\ndrop 41\n",
        ),
        (
            "projection-return",
            "fn take(w: W) -> R { return w.r; }\n\
             fn main() { let x = take(W { r: R { id: 42 }, n: 2 }); println(f\"{x.id}\") }\n",
            "drop W2\n42\ndrop 42\n",
        ),
        (
            "from-call",
            "fn mk() -> W { return W { r: R { id: 43 }, n: 3 }; }\n\
             fn take(w: W) -> R { let W { r, n } = w; r }\n\
             fn main() { let x = take(mk()); println(f\"{x.id}\") }\n",
            "drop W3\n43\ndrop 43\n",
        ),
        // TWO droppers, one escaping: the survivor's body must still run here.
        (
            "two-droppers",
            "struct Two { a: R, b: R }\n\
             impl Drop for Two { fn drop(mut ref self) { println(\"drop Two\") } }\n\
             fn take(t: Two) -> R { let Two { a, b } = t; a }\n\
             fn main() { let x = take(Two { a: R { id: 44 }, b: R { id: 45 } });\n\
             \x20           println(f\"{x.id}\") }\n",
            "drop Two\ndrop 45\n44\ndrop 44\n",
        ),
        // CONTROL — nothing escapes, so both bodies belong here.
        (
            "nothing-escapes-control",
            "fn use_n(w: W) -> i64 { return w.n; }\n\
             fn main() { let k = use_n(W { r: R { id: 46 }, n: 4 }); println(f\"{k}\") }\n",
            "drop W4\ndrop 46\n4\n",
        ),
        // The result is DISCARDED — one body each, parent first.
        (
            "result-discarded",
            "fn take(w: W) -> R { let W { r, n } = w; r }\n\
             fn main() { take(W { r: R { id: 47 }, n: 5 }); println(\"end\") }\n",
            "drop W5\ndrop 47\nend\n",
        ),
    ] {
        assert_eq!(run(&format!("{DROPPER}{body}")), want, "{label}");
    }
}

/// A by-value param that escapes through a CALL in return position runs its
/// `Drop` body once (B-2026-08-28-62). Interpreter twin of `tests/codegen.rs`'s
/// `e2e_param_escaping_through_a_forwarded_call_drops_once`, whose doc carries
/// the reasoning.
///
/// Unlike most of this family this backend was NOT the correct one — it doubled
/// too, which is what made the defect invisible to every run-vs-build gate.
/// `callee-consumes` is the row that keeps the fix honest in the expensive
/// direction: the callee genuinely consumes its argument there, so a predicate
/// keyed on syntax rather than on the callee's own answer would take that
/// value's only `Drop` away.
#[test]
fn test_param_escaping_through_a_forwarded_call_drops_once() {
    const H: &str = "struct R { id: i64 }\n\
         impl Drop for R { fn drop(mut ref self) { println(f\"drop {self.id}\") } }\n\
         struct BoxR { v: R }\n\
         fn src2(x: R) -> (BoxR, i64) { return (BoxR { v: x }, 1); }\n";
    for (label, body, want) in [
        (
            "forwarding",
            "fn outer2(y: R) -> (BoxR, i64) { return src2(y); }\n\
             fn main() { let (a, n) = outer2(R { id: 49 }); println(f\"{n}\") }\n",
            "drop 49\n1\n",
        ),
        (
            "forwarding-generic",
            "struct Box2[T] { v: T }\n\
             fn src[T](x: T) -> (Box2[T], i64) { return (Box2[T] { v: x }, 1); }\n\
             fn outerg[U](y: U) -> (Box2[U], i64) { return src(y); }\n\
             fn main() { let (b, n) = outerg(R { id: 51 }); println(f\"{n}\") }\n",
            "drop 51\n1\n",
        ),
        // CONTROL — the callee CONSUMES the argument. The ORDER differs from the
        // compiled twin's under B-2026-08-28-19 (the caller-side walk fires at
        // the call here and at scope exit compiled); the COUNT, which is what
        // this fix moves, agrees.
        (
            "callee-consumes",
            "fn uses(x: R) -> i64 { return x.id; }\n\
             fn consumer(y: R) -> i64 { return uses(y); }\n\
             fn main() { println(f\"{consumer(R { id: 50 })}\") }\n",
            "drop 50\n50\n",
        ),
        (
            "direct-call",
            "fn main() { let (e, n) = src2(R { id: 54 }); println(f\"{n}\") }\n",
            "drop 54\n1\n",
        ),
    ] {
        assert_eq!(run(&format!("{H}{body}")), want, "{label}");
    }
}

/// B-2026-09-02-6 — the ORACLE half of the in-loop drop-flag re-arm.
///
/// The interpreter is path-sensitive AND iteration-sensitive for free: it
/// re-runs the `let` on every pass and the binding simply owns whatever it was
/// last given, so there is no flag to go stale. It printed all of these
/// correctly before the codegen fix, and that is the point of keeping the twin
/// — it is what establishes that B-2026-09-02-6 was a codegen-only defect
/// rather than a shared misunderstanding of when the body is due.
#[test]
fn in_loop_decl_rearms_drop_flag_each_iteration() {
    const H: &str = "struct R { id: i64, tag: String }\n\
         impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\") } }\n\
         fn while_first(p: R) -> i64 {\n\
             let mut i: i64 = 0;\n\
             let mut acc: i64 = 0;\n\
             while i < 3 {\n\
                 println(f\"w{i}\");\n\
                 let mut out: R = R { id: 90 + i, tag: f\"t\" };\n\
                 if i == 0 { out = p; }\n\
                 acc = acc + out.id;\n\
                 i = i + 1;\n\
             }\n\
             return acc\n\
         }\n\
         fn while_middle(p: R) -> i64 {\n\
             let mut i: i64 = 0;\n\
             let mut acc: i64 = 0;\n\
             while i < 3 {\n\
                 println(f\"m{i}\");\n\
                 let mut out: R = R { id: 60 + i, tag: f\"t\" };\n\
                 if i == 1 { out = p; }\n\
                 acc = acc + out.id;\n\
                 i = i + 1;\n\
             }\n\
             return acc\n\
         }\n\
         fn outside(p: R) -> i64 {\n\
             let mut out: R = R { id: 70, tag: f\"t\" };\n\
             let mut i: i64 = 0;\n\
             while i < 3 {\n\
                 println(f\"o{i}\");\n\
                 if i == 1 { out = p; }\n\
                 i = i + 1;\n\
             }\n\
             return out.id\n\
         }\n\
         fn nested(p: R) -> i64 {\n\
             let mut acc: i64 = 0;\n\
             for a in 0..2 {\n\
                 for b in 0..2 {\n\
                     println(f\"n{a}{b}\")\n\
                     let mut out: R = R { id: 10 * a + b, tag: f\"t\" };\n\
                     if a == 0 and b == 1 { out = p; }\n\
                     acc = acc + out.id;\n\
                 }\n\
             }\n\
             return acc\n\
         }\n";
    for (label, body, want) in [
        // The row's repro: disarmed on iteration 0, so BOTH later iterations'
        // own values lost their bodies under codegen.
        (
            "disarm-on-first-iteration",
            "println(f\"a{while_first(R { id: 5, tag: f\"q\" })}\")\n",
            "w0\ndR90\nw1\ndR91\nw2\ndR92\ndR5\na188\n",
        ),
        // Disarmed in the MIDDLE, which separates the two halves: the pass
        // ahead of the disarm was always right, the pass behind it was not.
        (
            "disarm-on-middle-iteration",
            "println(f\"b{while_middle(R { id: 6, tag: f\"q\" })}\")\n",
            "m0\ndR60\nm1\ndR61\nm2\ndR62\ndR6\nb128\n",
        ),
        // Declared OUTSIDE the loop: a real hand-over, which stays handed over
        // for every later iteration. The boundary an over-eager re-arm moves.
        (
            "declared-outside-the-loop",
            "println(f\"c{outside(R { id: 7, tag: f\"q\" })}\")\n",
            "o0\no1\ndR70\no2\ndR7\nc7\n",
        ),
        // Nested: the inner `let` re-arms per INNER iteration.
        (
            "nested-loops",
            "println(f\"d{nested(R { id: 8, tag: f\"q\" })}\")\n",
            "n00\ndR0\nn01\ndR1\nn10\ndR10\nn11\ndR11\ndR8\nd29\n",
        ),
    ] {
        let src = format!("{H}fn main() {{ {body} }}\n");
        assert_eq!(run(&src), want, "{label}");
    }
}

/// B-2026-08-02-14 — interpreter twin of `tests/codegen.rs`'s
/// `e2e_generic_parent_drop_field_bodies`, same source and expected
/// string. A REAL pin on this backend too: the interp's walk was
/// declared-type-driven and skipped bare-generic-param fields (the
/// pre-revision B-2026-07-29-39 scoping), so it was silent pre-fix.
#[test]
fn test_generic_parent_drop_field_bodies() {
    assert_eq!(
        run("struct Res { id: i64, name: String }\n\
             impl Drop for Res {\n\
                 fn drop(mut ref self) { println(f\"drop {self.id} {self.name}\") }\n\
             }\n\
             struct Box2[T] { item: T, tag: i64 }\n\
             fn main() {\n\
                 println(\"a\");\n\
                 {\n\
                     let b: Box2[Res] = Box2 { item: Res { id: 3, name: f\"ggg{3}\" }, tag: 1 };\n\
                     println(f\"tag {b.tag}\");\n\
                 }\n\
                 println(\"mid\");\n\
                 {\n\
                     let mut v: Vec[Box2[Res]] = Vec.new();\n\
                     v.push(Box2 { item: Res { id: 4, name: f\"hhhhh{4}\" }, tag: 2 });\n\
                     println(f\"vlen {v.len()}\");\n\
                 }\n\
                 println(\"end\");\n\
             }\n"),
        "a\ntag 1\ndrop 3 ggg3\nmid\nvlen 1\ndrop 4 hhhhh4\nend\n"
    );
}

#[test]
fn test_truncate_and_reassign_run_displaced_drop_bodies() {
    // B-2026-08-03-2 (class 1, remainder) — interpreter twin of
    // `tests/codegen.rs`'s `e2e_truncate_and_reassign_run_displaced_drop_bodies`,
    // same source and expected string. The `truncate` case is the one that
    // pins the RANGE: the removed tail fires at the truncate, the survivor
    // still fires at binding death, and neither fires twice.
    assert_eq!(
        run("struct Res { id: i64, name: String }\n\
             impl Drop for Res {\n\
                 fn drop(mut ref self) { println(f\"drop {self.id} {self.name}\") }\n\
             }\n\
             fn main() {\n\
                 println(\"truncate:\");\n\
                 {\n\
                     let mut v: Vec[Res] = Vec.new();\n\
                     v.push(Res { id: 1, name: f\"a{1}\" });\n\
                     v.push(Res { id: 2, name: f\"b{2}\" });\n\
                     v.truncate(1);\n\
                     println(v.len());\n\
                 }\n\
                 println(\"truncate0:\");\n\
                 {\n\
                     let mut u: Vec[Res] = Vec.new();\n\
                     u.push(Res { id: 3, name: f\"c{3}\" });\n\
                     u.truncate(0);\n\
                     println(u.len());\n\
                 }\n\
                 println(\"reassign:\");\n\
                 {\n\
                     let mut w: Vec[Res] = Vec.new();\n\
                     w.push(Res { id: 4, name: f\"d{4}\" });\n\
                     let mut z: Vec[Res] = Vec.new();\n\
                     z.push(Res { id: 5, name: f\"e{5}\" });\n\
                     w = z;\n\
                     println(w.len());\n\
                 }\n\
                 println(\"end\");\n\
             }\n"),
        "truncate:\ndrop 2 b2\n1\ndrop 1 a1\ntruncate0:\ndrop 3 c3\n0\nreassign:\ndrop 4 d4\n1\ndrop 5 e5\nend\n"
    );
}

#[test]
fn test_container_clear_runs_element_drop_bodies() {
    // B-2026-08-03-2 (class 1) — interpreter twin of `tests/codegen.rs`'s
    // `e2e_container_clear_runs_element_drop_bodies`, same source and expected
    // string. Both backends were silent pre-fix for the same reason on each
    // side: the clear went straight to the underlying container operation,
    // which knows nothing about Kāra destructors.
    assert_eq!(
        run("struct Res { id: i64, name: String }\n\
             impl Drop for Res {\n\
                 fn drop(mut ref self) { println(f\"drop {self.id} {self.name}\") }\n\
             }\n\
             fn main() {\n\
                 println(\"vecclear:\");\n\
                 {\n\
                     let mut v: Vec[Res] = Vec.new();\n\
                     v.push(Res { id: 1, name: f\"a{1}\" });\n\
                     v.push(Res { id: 2, name: f\"b{2}\" });\n\
                     v.clear();\n\
                     println(v.len());\n\
                 }\n\
                 println(\"mapclear:\");\n\
                 {\n\
                     let mut m: Map[i64, Res] = Map.new();\n\
                     m.insert(5, Res { id: 3, name: f\"c{3}\" });\n\
                     m.clear();\n\
                     println(m.len());\n\
                 }\n\
                 println(\"reuse:\");\n\
                 {\n\
                     let mut w: Vec[Res] = Vec.new();\n\
                     w.push(Res { id: 4, name: f\"d{4}\" });\n\
                     w.clear();\n\
                     w.push(Res { id: 5, name: f\"e{5}\" });\n\
                     println(w.len());\n\
                 }\n\
                 println(\"end\");\n\
             }\n"),
        "vecclear:\ndrop 1 a1\ndrop 2 b2\n0\nmapclear:\ndrop 3 c3\n0\nreuse:\ndrop 4 d4\n1\ndrop 5 e5\nend\n"
    );
}

#[test]
fn test_nested_call_temp_owned_arg_drop() {
    // B-2026-08-02-28 — interpreter twin of `tests/codegen.rs`'s
    // `e2e_nested_call_temp_owned_arg_drop`, same source and expected string.
    // The interpreter never leaked here; the twin pins the ORDER the codegen
    // fix had to reproduce (body before the frees it reads) for the three
    // shapes where both backends agree.
    assert_eq!(
        run("struct Res { id: i64, name: String }\n\
             impl Drop for Res {\n\
                 fn drop(mut ref self) { println(f\"drop {self.id} {self.name}\") }\n\
             }\n\
             struct Holder { xs: Vec[Res], tag: i64 }\n\
             fn mk(v: Vec[Res]) -> Holder { Holder { xs: v, tag: 9 } }\n\
             fn mkh() -> Holder {\n\
                 let mut v: Vec[Res] = Vec.new();\n\
                 v.push(Res { id: 7, name: f\"g{7}\" });\n\
                 Holder { xs: v, tag: 3 }\n\
             }\n\
             fn use_it(h: Holder) -> i64 { h.tag }\n\
             fn main() {\n\
                 println(\"a\");\n\
                 {\n\
                     let mut xs: Vec[Res] = Vec.new();\n\
                     xs.push(Res { id: 1, name: f\"a{1}\" });\n\
                     let n = use_it(mk(xs));\n\
                     println(n);\n\
                 }\n\
                 println(\"b\");\n\
                 {\n\
                     let mut ys: Vec[Res] = Vec.new();\n\
                     ys.push(Res { id: 2, name: f\"b{2}\" });\n\
                     use_it(mk(ys));\n\
                 }\n\
                 println(\"c\");\n\
                 {\n\
                     let m = use_it(mkh());\n\
                     println(m);\n\
                 }\n\
                 println(\"end\");\n\
             }\n"),
        "a\ndrop 1 a1\n9\nb\ndrop 2 b2\nc\ndrop 7 g7\n3\nend\n"
    );
}

#[test]
fn test_tuple_binding_container_element_drop() {
    // B-2026-08-02-26 — interpreter twin of `tests/codegen.rs`'s
    // `e2e_tuple_binding_container_element_drop`, same source and expected
    // string. The interpreter was ALREADY correct here (its tuple walk is
    // value-driven, so it never lost the element type the way codegen's
    // head-name TypeExpr did); this pins the parity the fix restored, so a
    // future change to either side has to keep both.
    assert_eq!(
        run("struct Res { id: i64, name: String }\n\
             impl Drop for Res {\n\
                 fn drop(mut ref self) { println(f\"drop {self.id} {self.name}\") }\n\
             }\n\
             fn mkv() -> Vec[Res] {\n\
                 let mut v: Vec[Res] = Vec.new();\n\
                 v.push(Res { id: 2, name: f\"b{2}\" });\n\
                 v\n\
             }\n\
             fn main() {\n\
                 println(\"a\");\n\
                 {\n\
                     let mut xs: Vec[Res] = Vec.new();\n\
                     xs.push(Res { id: 1, name: f\"a{1}\" });\n\
                     let t = (xs, 9);\n\
                     println(t.1);\n\
                 }\n\
                 {\n\
                     let u = (mkv(), 8);\n\
                     println(u.1);\n\
                 }\n\
                 {\n\
                     let mut ys: Vec[Res] = Vec.new();\n\
                     ys.push(Res { id: 3, name: f\"c{3}\" });\n\
                     let w: (Vec[Res], i64) = (ys, 7);\n\
                     println(w.1);\n\
                 }\n\
                 println(\"end\");\n\
             }\n"),
        "a\n9\ndrop 1 a1\n8\ndrop 2 b2\n7\ndrop 3 c3\nend\n"
    );
}

#[test]
fn test_tuple_literal_own_drop_source_disarm() {
    // B-2026-08-02-27 — interpreter twin of `tests/codegen.rs`'s
    // `e2e_tuple_literal_own_drop_source_disarm`. Both backends double-fired
    // pre-fix, for the same reason on each side: the let-RHS aggregate move
    // recorder never put an own-`Drop` source on the whole-value channel
    // (interp) / used only the container-element disarm (codegen).
    assert_eq!(
        run("struct Res { id: i64, name: String }\n\
             impl Drop for Res {\n\
                 fn drop(mut ref self) { println(f\"drop {self.id} {self.name}\") }\n\
             }\n\
             fn main() {\n\
                 println(\"a\");\n\
                 {\n\
                     let r = Res { id: 1, name: f\"a{1}\" };\n\
                     let t = (r, 9);\n\
                     println(t.1);\n\
                 }\n\
                 {\n\
                     let u = (Res { id: 2, name: f\"b{2}\" }, 4);\n\
                     println(u.1);\n\
                 }\n\
                 println(\"end\");\n\
             }\n"),
        "a\n9\ndrop 1 a1\n4\ndrop 2 b2\nend\n"
    );
}

/// B-2026-08-01-5 (chain leg) — interpreter twin of `tests/codegen.rs`'s
/// `e2e_owned_self_chain_no_double_drop`, same source and expected string.
/// The interpreter already produced this output pre-fix (its hook gates
/// exclude owned-self and chain-link receivers) — the pin is the parity
/// target the codegen self-mode gating now meets.
#[test]
fn test_owned_self_chain_no_double_drop() {
    assert_eq!(
        run("struct Res { id: i64, name: String }\n\
             impl Drop for Res {\n\
                 fn drop(mut ref self) {\n\
                     println(f\"drop {self.id} {self.name}\")\n\
                 }\n\
             }\n\
             impl Res {\n\
                 fn plus(self, n: i64) -> Res {\n\
                     return Res { id: self.id + n, name: self.name.clone() };\n\
                 }\n\
                 fn me(self) -> Res {\n\
                     return self;\n\
                 }\n\
                 fn ident(ref self) -> i64 {\n\
                     return self.id;\n\
                 }\n\
             }\n\
             fn mk(n: i64) -> Res {\n\
                 return Res { id: n, name: f\"r{n}\" };\n\
             }\n\
             fn main() {\n\
                 println(\"a: rebuild-chain\");\n\
                 let x = mk(1).plus(10);\n\
                 println(f\"x={x.id}\");\n\
                 println(\"b: passthrough self\");\n\
                 let y = mk(2).me();\n\
                 println(f\"y={y.id}\");\n\
                 println(\"c: chain then ref-method\");\n\
                 let z = mk(3).me().ident();\n\
                 println(f\"z={z}\");\n\
                 println(\"end\");\n\
             }\n"),
        "a: rebuild-chain\nx=11\ndrop 11 r1\nb: passthrough self\ny=2\ndrop 2 r2\n\
         c: chain then ref-method\nz=3\nend\n"
    );
}

/// B-2026-07-31-38 — interpreter twin of `tests/codegen.rs`'s
/// `e2e_ctor_moved_binding_reassign_rearms_drop`, same source and expected
/// string. The interpreter was already correct on this shape (`D1 D2 x`) —
/// it is the oracle half; codegen lost D2 entirely pre-fix.
#[test]
fn test_ctor_moved_binding_reassign_rearms_drop() {
    assert_eq!(
        run("struct Res { id: i64 }\n\
             impl Drop for Res {\n\
                 fn drop(mut ref self) {\n\
                     println(f\"D{self.id}\")\n\
                 }\n\
             }\n\
             enum Slot { Empty, Held(Res) }\n\
             fn main() {\n\
                 let mut r = Res { id: 1 };\n\
                 let s = Slot.Held(r);\n\
                 r = Res { id: 2 };\n\
                 println(\"x\");\n\
             }\n"),
        "D1\nD2\nx\n"
    );
}

/// B-2026-07-30-12 — a by-value owned-struct arg that the callee RETURNS runs
/// its `Drop` body exactly once.
///
/// The interpreter was always correct here (its `run_fresh_temp_arg_drops`
/// skips any arg the callee can return), so this is the ORACLE half of the
/// pair: codegen registered the full drop wrapper on the passthrough path and
/// printed the body twice, a run/build divergence that shipped with
/// B-2026-07-08-6. Same source and expected string as `tests/codegen.rs`'s
/// `e2e_fnret_passthrough_arg_drop_fires_once`; pinning it here is what stops a
/// future codegen change from drifting away from the interpreter again.
#[test]
fn test_fnret_passthrough_arg_drop_fires_once() {
    assert_eq!(
        run("struct G { name: String, id: i64 }\n\
             impl Drop for G { fn drop(mut ref self) { println(f\"dG{self.id}\"); } }\n\
             fn pass(g: G) -> G { g }\n\
             fn mk(i: i64) -> G { G { name: \"a padded payload string here\", id: i } }\n\
             fn main() {\n\
                 let p = pass(G { name: \"a padded payload string here\", id: 1 });\n\
                 println(f\"{p.id}\");\n\
                 let q = pass(mk(2));\n\
                 println(f\"{q.id}\");\n\
                 println(\"end\");\n\
             }\n"),
        "1\ndG1\n2\ndG2\nend\n"
    );
}

/// B-2026-07-30-11 (tuple leg) — the interpreter half: a tuple's elements run
/// their user `impl Drop` bodies when the tuple binding dies.
///
/// Same source and expected output as `tests/codegen.rs`'s
/// `e2e_tuple_elements_run_user_drop_bodies`. Both backends cover exactly the
/// `let` position — the interpreter because `push_drops_for_stmt` registers a
/// Drop action only there, codegen because that is where the bodies fn is
/// registered — so the pair pins the coverage boundary as well as the values.
#[test]
fn test_tuple_elements_run_user_drop_bodies() {
    assert_eq!(
        run("struct Res { id: i64 }\n\
             impl Drop for Res { fn drop(mut ref self) { println(self.id); } }\n\
             struct W { r: Res }\n\
             fn main() {\n\
                 { let t: (Res, i64) = (Res { id: 21 }, 7); println(t.1); }\n\
                 { let u: (i64, Res, Res) = (1, Res { id: 22 }, Res { id: 23 }); println(u.0); }\n\
                 { let w: (W, i64) = (W { r: Res { id: 24 } }, 0); println(w.1); }\n\
                 { let p: (i64, i64) = (1, 2); println(p.0); }\n\
                 println(999);\n\
             }\n"),
        "7\n21\n1\n22\n23\n0\n24\n1\n999\n"
    );
}

// ── A by-value argument the callee STORES (B-2026-08-26-9) ──────────────
//
// The caller-drops convention runs the fresh temporary's `Drop` after the
// call. When the callee STORES the argument into a place the caller still
// holds, the value is alive in its new home when the call returns, so the
// caller's fire is a second body for one value.

/// A REGRESSION oracle for this backend, not a parity pin: the interpreter was
/// wrong here on its own. `run_fresh_temp_arg_drops` fires on the FREE-FUNCTION
/// call path, and its only escape guard was `fn_returns_param`, so a callee that
/// stored the argument instead of returning it printed `drop 7` at the call and
/// again at the container's drain — on this backend AND on AOT. Because both
/// agreed, no run-vs-build comparison would have surfaced it; only counting the
/// drops against the language's semantics does.
#[test]
fn free_fn_drops_a_stored_by_value_argument_once() {
    let out = run(r#"
struct Item { id: i64 }
impl Drop for Item {
    fn drop(mut ref self) { println(f"drop {self.id}") }
}
fn add_to(v: mut ref Vec[Item], x: Item) { v.push(x); }
fn main() {
    let mut v: Vec[Item] = Vec.new();
    add_to(mut v, Item { id: 7 });
    println("stored");
    while v.len() > 0 {
        match v.pop() { Some(e) => { println(f"pop {e.id}"); } None => {} }
    }
    println("end");
}
"#);
    assert_eq!(out, "stored\npop 7\ndrop 7\nend\n");
}

/// A PARITY pin, NOT a regression oracle — it passed before the fix too. The
/// interpreter never ran the fresh-temp argument drop on the METHOD path at
/// all, so it happened to be right for the wrong reason while AOT double-fired.
/// It earns its place by pinning the agreed answer: this is the shape whose
/// backends disagreed, and it is the one a future change to either side is most
/// likely to break. Its codegen twin
/// (`e2e_method_drops_a_stored_by_value_argument_once`) is the half that goes
/// red on a regression.
#[test]
fn method_drops_a_stored_by_value_argument_once_parity() {
    let out = run(r#"
struct Item { id: i64 }
impl Drop for Item {
    fn drop(mut ref self) { println(f"drop {self.id}") }
}
struct Bag { xs: Vec[Item] }
impl Bag {
    fn add(mut ref self, x: Item) { self.xs.push(x); }
}
fn main() {
    let mut b = Bag { xs: Vec.new() };
    b.add(Item { id: 7 });
    println("stored");
    while b.xs.len() > 0 {
        match b.xs.pop() { Some(e) => { println(f"pop {e.id}"); } None => {} }
    }
    println("end");
}
"#);
    assert_eq!(out, "stored\npop 7\ndrop 7\nend\n");
}

/// The negative side, mirroring the codegen fixture of the same shape: a callee
/// that only READS its by-value parameter still leaves the drop to the caller,
/// so exactly one `drop` must print. Fails if the escape predicate is ever
/// widened to key on the receiver's mode rather than on an actual store.
#[test]
fn by_value_argument_that_is_only_read_still_drops_in_the_caller() {
    let out = run(r#"
struct Item { id: i64 }
impl Drop for Item {
    fn drop(mut ref self) { println(f"drop {self.id}") }
}
fn look(x: Item) -> i64 { x.id }
fn main() {
    let n = look(Item { id: 7 });
    println(n);
    println("end");
}
"#);
    assert_eq!(out, "drop 7\n7\nend\n");
}

/// B-2026-08-29-25 — the interpreter half: a discarded `if` runs its branch
/// value's `Drop` body in both statement forms, and both discard sites see
/// through a block wrapper.
///
/// The `if` rows were agreed silence across all three backends. The WRAPPER
/// rows were a run-vs-build divergence in this backend's silent direction for
/// the shapes compiled already admitted — a wrapped call, a wrapped struct
/// literal, a wrapped tuple — because codegen's discard gates have peeled
/// block wrappers since slice 5 and neither of this backend's two sites ever
/// did. Every row constructs one `R` that nobody takes, so one body is due.
#[test]
fn test_discarded_if_and_block_wrapped_rhs_run_their_drop_body_once() {
    let hdr = "struct R { id: i64 }\n\
               impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\") } }\n";
    let rows: [(&str, &str, &str); 10] = [
        (
            "let n = 1;\n\
             let _ = if n == 1 { R { id: 7 } } else { R { id: 0 } };\n\
             println(\"end\");",
            "dR7\nend",
            "FIXED: `let _ = if …`, struct-literal branches",
        ),
        (
            "let n = 1;\n\
             if n == 1 { R { id: 7 } } else { R { id: 0 } };\n\
             println(\"end\");",
            "dR7\nend",
            "FIXED: bare-statement `if …` — predates B-2026-08-29-20",
        ),
        (
            "let n = 1;\n\
             let _ = if n == 1 { mk(7) } else { mk(0) };\n\
             println(\"end\");",
            "dR7\nend",
            "FIXED: `let _ = if …`, call branches",
        ),
        (
            "let n = 1;\n\
             if n == 1 { mk(7) } else { mk(0) };\n\
             println(\"end\");",
            "dR7\nend",
            "FIXED: bare-statement `if …`, call branches",
        ),
        (
            "let n = 1;\n\
             let _ = if n == 2 { R { id: 1 } } else if n == 1 { R { id: 7 } } else { R { id: 0 } };\n\
             println(\"end\");",
            "dR7\nend",
            "FIXED: `else if` chain — the else branch is another `If`",
        ),
        (
            "let n = 1;\n\
             let _ = { match n { 1 => { R { id: 7 } } _ => { R { id: 0 } } } };\n\
             println(\"end\");",
            "dR7\nend",
            "FIXED: block-wrapped match, `let _ =` form",
        ),
        (
            "let n = 1;\n\
             { match n { 1 => { R { id: 7 } } _ => { R { id: 0 } } } };\n\
             println(\"end\");",
            "dR7\nend",
            "FIXED: block-wrapped match, bare-statement form",
        ),
        (
            "let _ = { mk(7) };\n\
             println(\"end\");",
            "dR7\nend",
            "FIXED, was DIVERGENT: block-wrapped call, `let _ =` form",
        ),
        (
            "{ mk(7) };\n\
             println(\"end\");",
            "dR7\nend",
            "FIXED, was DIVERGENT: block-wrapped call, bare-statement form",
        ),
        (
            "let _ = { R { id: 7 } };\n\
             println(\"end\");",
            "dR7\nend",
            "FIXED, was DIVERGENT: block-wrapped struct literal",
        ),
    ];
    for (body, expected, label) in rows {
        let src = format!(
            "{hdr}fn mk(i: i64) -> R {{ return R {{ id: i }}; }}\nfn main() {{\n{body}\n}}\n"
        );
        assert_eq!(run(&src).trim(), expected, "[{label}]");
    }
    // The bare form is the GUARD RAIL for the liveness gate on the `If` arm:
    // `r` is still in scope, so its own scope-exit body already fires and is
    // correct at ONE. Without the gate the discard walker fires on top and this
    // prints `dR41` twice — which is exactly why B-2026-08-29-31 kept that gate
    // while widening the OTHER half of the same predicate to admit a tail whose
    // name has already left scope (`discard_arm_tail_is_ownable`). Both forms
    // now sit at one body; the `match` twin above is pinned identically.
    for (stmt, expected, label) in [
        (
            "if n == 0 { r } else { R { id: 9 } };",
            "dR41\nend",
            "guard: bare form — scope-exit body must stay at ONE",
        ),
        (
            "let _ = if n == 0 { r } else { R { id: 9 } };",
            "dR41\nend",
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
        assert_eq!(run(&src).trim(), expected, "[{label} — {stmt}]");
    }
    // FIXED (B-2026-08-29-30): an `if` with no `else` has no phi, so freeing
    // its branch value needed a registration inside the ARM rather than a
    // wider gate at the statement — this line pinned the shared silence until
    // codegen had one.
    {
        let src = format!(
            "{hdr}fn main() {{\n\
             let n = 1;\n\
             let _ = if n == 1 {{ R {{ id: 7 }} }};\n\
             println(\"end\");\n}}\n"
        );
        assert_eq!(run(&src).trim(), "dR7\nend", "[FIXED: `if` with no `else`]");
    }
    // FIXED (B-2026-08-29-31): `let _ = { r }` moves a local through a
    // wrapper. This pinned the shared silence, on the ground that no rebind
    // hook retracts the local's own slot through a wrapper so firing here
    // would double it. Both halves of that changed together: a wildcard `let`
    // no longer marks its RHS as an escaping position, so the local is never
    // recorded moved-out and keeps its own body — one fire, from the binding
    // itself rather than from a discard walk.
    {
        let src = format!(
            "{hdr}fn main() {{\n\
             let r = R {{ id: 41 }};\n\
             let _ = {{ r }};\n\
             println(\"end\");\n}}\n"
        );
        assert_eq!(
            run(&src).trim(),
            "dR41\nend",
            "[FIXED: moved local through a wrapper]"
        );
    }
}

#[test]
fn test_mixed_owndrop_literal_masks_only_the_view_body() {
    let hdr = "struct R { id: i64, name: String }\n\
               impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\") } }\n\
               fn mk(i: i64) -> R { return R { id: i, name: f\"heap-{i}\" }; }\n\
               struct Sd3 { a: R, b: R }\n\
               impl Drop for Sd3 { fn drop(mut ref self) { println(\"dSd3\") } }\n\
               struct Sd { r: R }\n\
               impl Drop for Sd { fn drop(mut ref self) { println(\"dSd\") } }\n\
               struct Plain3 { a: R, b: R }\n\
               struct Sd4 { a: R, b: R, c: R }\n\
               impl Drop for Sd4 { fn drop(mut ref self) { println(\"dSd4\") } }\n\
               struct Mix { a: R, n: i64 }\n\
               impl Drop for Mix { fn drop(mut ref self) { println(\"dMix\") } }\n";
    for (label, fns, main, want) in [
        (
            "the row: view + fresh",
            "fn take(r: R) -> i64 { let s = Sd3 { a: r, b: mk(2) }; return 7; }",
            "let v = take(mk(1)); println(f\"v={v}\");",
            "dSd3\ndR2\ndR1\nv=7\n",
        ),
        (
            "order reversed: fresh + view",
            "fn take(r: R) -> i64 { let s = Sd3 { a: mk(2), b: r }; return 7; }",
            "let v = take(mk(1)); println(f\"v={v}\");",
            "dSd3\ndR2\ndR1\nv=7\n",
        ),
        (
            "boundary: ALL views (the shape already fixed)",
            "fn take(r: R) -> i64 { let s = Sd { r: r }; return 7; }",
            "let v = take(mk(1)); println(f\"v={v}\");",
            "dSd\ndR1\nv=7\n",
        ),
        (
            "boundary: ALL fresh — nothing masked",
            "fn take(r: R) -> i64 { let s = Sd3 { a: mk(1), b: mk(2) }; return 7; }",
            "let v = take(mk(9)); println(f\"v={v}\");",
            "dSd3\ndR2\ndR1\ndR9\nv=7\n",
        ),
        (
            "boundary: MIXED with NO own Drop — the per-field path, untouched",
            "fn take(r: R) -> i64 { let s = Plain3 { a: r, b: mk(2) }; return 7; }",
            "let v = take(mk(1)); println(f\"v={v}\");",
            "dR2\ndR1\nv=7\n",
        ),
        (
            "three fields, ONE view: both fresh bodies survive",
            "fn take(r: R) -> i64 { let s = Sd4 { a: r, b: mk(2), c: mk(3) }; return 7; }",
            "let v = take(mk(1)); println(f\"v={v}\");",
            "dSd4\ndR3\ndR2\ndR1\nv=7\n",
        ),
        (
            "three fields, TWO views: the fresh body survives",
            "fn take(p: R, q: R) -> i64 { let s = Sd4 { a: p, b: q, c: mk(3) }; return 7; }",
            "let v = take(mk(1), mk(2)); println(f\"v={v}\");",
            "dSd4\ndR3\ndR2\ndR1\nv=7\n",
        ),
        (
            "view beside a NON-Drop field",
            "fn take(r: R) -> i64 { let s = Mix { a: r, n: 5 }; return 7; }",
            "let v = take(mk(1)); println(f\"v={v}\");",
            "dMix\ndR1\nv=7\n",
        ),
        (
            "the masked binding is still readable",
            "fn take(r: R) -> i64 { let s = Sd3 { a: r, b: mk(2) }; println(f\"rd{s.b.id}\"); return 7; }",
            "let v = take(mk(1)); println(f\"v={v}\");",
            "rd2\ndSd3\ndR2\ndR1\nv=7\n",
        ),
        (
            "two mixed literals in one frame keep separate masks",
            "fn take(p: R, q: R) -> i64 { let s = Sd3 { a: p, b: mk(2) }; let t = Sd3 { a: q, b: mk(4) }; return 7; }",
            "let v = take(mk(1), mk(3)); println(f\"v={v}\");",
            "dSd3\ndR2\ndSd3\ndR4\ndR3\ndR1\nv=7\n",
        ),
    ] {
        let src = format!("{hdr}{fns}\nfn main() {{ {main} }}\n");
        assert_eq!(run(&src), want, "[{label}]");
    }
}

/// B-2026-09-16-18 — a fresh-temp STRUCT scrutinee's UNBOUND fields run their
/// `Drop` bodies. Interpreter twin of `tests/codegen.rs`'s
/// `test_e2e_fresh_temp_struct_scrutinee_unbound_fields_run_their_drop_bodies`,
/// same cells and same expectations; that file's doc carries the measurements.
///
/// The property is that the two AGREE — and this row is the case where the
/// older way of writing that down was not enough. Its sibling fixture above
/// pinned `no-binding` at zero bodies with a note saying the backends agreed
/// and a later widening must not make one the odd one out. They did agree, and
/// they agreed on LOSING the husk's bodies and leaking the buffers under them:
/// the pin was watching for a divergence while the defect was an agreement. So
/// these cells pin VALUES.
///
/// `iflet-husk-both-bodies` WAS `iflet-agreed-gap`, PINNED AT `dR68\nz=68\n`,
/// an agreement that was still wrong on purpose because arming the compiled
/// side alone would have made it a divergence. B-2026-09-21-1 gave the
/// interpreter's other three spellings the same husk ownership and removed the
/// gate, so both sides moved together and the cell now pins both bodies.
///
/// `guarded-two-arm-per-arm-mask` WAS THAT SECOND GAP AND IS NOW CLOSED
/// (B-2026-09-21-2). This cell pinned `dR81` alone, and the paragraph here
/// explained it as forced: the husk's walker is registered once and fired at
/// the merge block after the phi, one function for every arm, so codegen could
/// only mask the UNION of what the arms bind, and a taken-arm mask in the
/// interpreter alone was measured MORE precise AND divergent.
///
/// The union was not merely imprecise. When it covered every body-bearing
/// field — which two arms naming different fields do between them — the
/// materializer concluded the husk owed nothing and declined outright, so the
/// temp got no bodies walker AND no memory walk: the taken arm's unbound body
/// was lost and its buffer leaked, 3 bytes per call.
///
/// Each arm now stores its own walker in a slot the single fire site loads, so
/// codegen has a per-arm answer and this backend masks by the TAKEN arm to
/// agree with it. The drop POINT is untouched — design.md § Temporary Lifetime
/// Rules puts a match scrutinee at "drops at match exit" and B-2026-08-29-28
/// placed it there deliberately — so only WHICH bodies run changed. Measured
/// after: `dR81 dR80` on all four surfaces, 13 allocs / 13 frees, nothing
/// lost, no invalid access.
///
/// AN ARM THAT BINDS EVERY BODY-BEARING FIELD owes nothing and says so with a
/// no-op walker, not by declining — `guarded-arm2-binds-all` pins that,
/// because declining would have put the lost body and the leak back for every
/// other arm of such a match.
#[test]
fn fresh_temp_struct_scrutinee_unbound_fields_run_their_drop_bodies() {
    const H: &str = "struct R { id: i64, name: String }\n\
         impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\") } }\n\
         fn mk(i: i64) -> R { return R { id: i, name: f\"n{i}\" }; }\n\
         struct S3 { a: R, b: R }\n\
         struct Plain { x: i64, y: i64 }\n\
         fn mks() -> S3 { return S3 { a: mk(51), b: mk(52) }; }\n";
    for (label, cell, want) in [
        (
            "temp-literal",
            "fn c() -> i64 { match S3 { a: mk(44), b: mk(45) } { S3 { a, .. } => { return a.id; } } }",
            "dR44\ndR45\nz=44\n",
        ),
        (
            "temp-call",
            "fn c() -> i64 { match mks() { S3 { a, .. } => { return a.id; } } }",
            "dR51\ndR52\nz=51\n",
        ),
        (
            "temp-all",
            "fn c() -> i64 { match S3 { a: mk(47), b: mk(48) } { S3 { a, b } => { return a.id + b.id; } } }",
            "dR48\ndR47\nz=95\n",
        ),
        (
            "temp-none",
            "fn c() -> i64 { match S3 { a: mk(49), b: mk(50) } { S3 { .. } => { return 1; } } }",
            "dR50\ndR49\nz=1\n",
        ),
        (
            "named-control",
            "fn c() -> i64 { let s: S3 = S3 { a: mk(62), b: mk(63) }; match s { S3 { a, .. } => { return a.id; } } }",
            "dR62\ndR63\nz=62\n",
        ),
        (
            "wildcard-field",
            "fn c() -> i64 { match S3 { a: mk(60), b: mk(61) } { S3 { a: _, b } => { return b.id; } } }",
            "dR61\ndR60\nz=61\n",
        ),
        (
            "no-drop-fields",
            "fn c() -> i64 { match Plain { x: 1, y: 2 } { Plain { .. } => { return 7; } } }",
            "z=7\n",
        ),
            (
            "guarded-two-arm-per-arm-mask",
            "fn c() -> i64 { match S3 { a: mk(80), b: mk(81) } { S3 { a, .. } if a.id > 100 => { return a.id; } S3 { b, .. } => { return b.id; } } }",
            "dR81\ndR80\nz=81\n",
        ),
        (
            "iflet-husk-both-bodies",
            "fn c() -> i64 { if let S3 { a, .. } = S3 { a: mk(68), b: mk(69) } { return a.id; } return 0; }",
            "dR68\ndR69\nz=68\n",
        ),
        // B-2026-09-21-2 — the per-arm mask. Measured on all four surfaces
        // through this fixture's own prelude and wrapper and refused unless
        // they agreed, so the tuple and the program its numbers came from
        // are one string rather than two transcriptions.
        (
            "guarded-two-arm-fallthrough",
            "fn c() -> i64 { let v = match S3 { a: mk(89), b: mk(90) } { S3 { a, .. } if a.id > 900 => { a.id } S3 { b, .. } => { b.id } }; return v; }",
            "dR90\ndR89\nz=90\n",
        ),
        (
            "guarded-two-arm-first-taken",
            "fn c() -> i64 { match S3 { a: mk(82), b: mk(83) } { S3 { a, .. } if a.id > 0 => { return a.id; } S3 { b, .. } => { return b.id; } } }",
            "dR82\ndR83\nz=82\n",
        ),
        (
            "guarded-two-arm-wildcard",
            "fn c() -> i64 { match S3 { a: mk(95), b: mk(96) } { S3 { a: _, b } if b.id > 900 => { return b.id; } S3 { a, b: _ } => { return a.id; } } }",
            "dR95\ndR96\nz=95\n",
        ),
        (
            "guarded-three-arm-s3",
            "fn c() -> i64 { match S3 { a: mk(86), b: mk(87) } { S3 { a, .. } if a.id > 900 => { return a.id; } S3 { b, .. } if b.id > 900 => { return b.id; } S3 { .. } => { return 3; } } }",
            "dR87\ndR86\nz=3\n",
        ),
        (
            "guarded-arm2-binds-all",
            "fn c() -> i64 { match S3 { a: mk(99), b: mk(100) } { S3 { a, .. } if a.id > 900 => { return a.id; } S3 { a, b } => { return a.id + b.id; } } }",
            "dR100\ndR99\nz=199\n",
        ),
        (
            "guarded-two-arm-same-field-control",
            "fn c() -> i64 { match S3 { a: mk(84), b: mk(85) } { S3 { a, .. } if a.id > 100 => { return a.id; } S3 { a, .. } => { return a.id + 1; } } }",
            "dR84\ndR85\nz=85\n",
        ),
    ] {
        let src = format!("{H}{cell}\nfn main() {{ let z: i64 = c(); println(f\"z={{z}}\"); }}\n");
        assert_eq!(run(&src), want, "{label}");
    }
}

/// B-2026-09-01-2 — a `let x = s.a` moving one field out leaves the source's
/// OTHER fields their `Drop` bodies.
///
/// One statement ran two recorders: `suppress_moved_out_drop_field` inserted a
/// COARSE whole-walk disarm keyed on the root name, and the `let` arm inserted
/// the PRECISE `(src, field)` pair. `drop_user_drop_fields_of_binding` tests the
/// coarse set first and returns, so the precise mask was dead code for every
/// depth-1 move and every OTHER field lost its body outright. Measured `dR1`
/// against every compiled backend's `dR2 dR1`.
///
/// `both-fresh` is the row that widens the report and belongs here for that
/// reason: the bug was filed as a "mixed wrap" (a param view beside a fresh
/// field), but a literal whose fields are BOTH fresh loses the body the same
/// way. The axis is the shadowing, not the param view it was found through.
///
/// `move-b` is the control that shows why the report looked narrower than it
/// was: moving the LAST field appears correct only because the surviving field
/// there is the param view, whose body another owner already runs.
///
/// `deep-chain` narrowed twice. B-2026-09-06-46 took the record from the whole
/// binding to the moved HOP, recovering `k`, a top-level sibling that never
/// moved. B-2026-09-06-55 took it the rest of the way, to the LEAF the chain
/// actually names: `moved_out_nested_field_bodies` keyed by the full path
/// `["h", "r"]`, so `q` -- the moved hop's sibling one level DOWN, which a
/// root-level mask took out along with `h` -- keeps its body too. The row reads
/// `dR1 dR3 dR2`, one body per object.
///
/// Both narrowings moved the two backends in ONE commit, which is what this
/// row and its compiled twin are pinned to force: each is a mask on one side
/// and a walker mask on the other, and landing either alone shows up here as a
/// divergence rather than as silence.
///
/// `three-hops` is the generalization the second narrowing makes possible and
/// the depth-1 record could not express at all: at `o.b.c.r` there is a sibling
/// to lose at EVERY level (`q` beside the leaf, `d` one up), and before the
/// path record both were dropped on the floor -- `dR4` alone, where four bodies
/// are due.
///
/// `discard-the-hop` is the shape that says the mask is a mask and not a
/// deletion. `let Outer { k, h: _ } = o` throws `h` away after `o.h.r` moved
/// out, so the discard still owes `q`'s body and must NOT re-run `r`'s, which
/// `x` already owns. The interpreter ran it twice the moment the path record
/// stopped declining the whole field, against the compiled backends' single
/// fire -- caught here, fixed by masking the leaf out of the discarded value
/// rather than skipping the field.
///
/// `enum-source` is the leg the row flagged as needing measurement before
/// narrowing, because the enum branch of the walker sits behind the same early
/// return. It cannot be reached: both inserts into the coarse set are gated on
/// a `Value::Struct` source. Pinned so a later widening of either insert has to
/// notice.
#[test]
fn moving_one_field_out_leaves_the_others_their_drop_bodies() {
    const H: &str = "struct R { id: i64 }\n\
         impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\") } }\n\
         struct S3 { a: R, b: R }\n\
         struct Inner { r: R, q: R }\n\
         struct Outer { h: Inner, k: R }\n\
         struct L3 { r: R, q: R }\n\
         struct L2 { c: L3, d: R }\n\
         struct L1 { b: L2, e: R }\n\
         enum E { A(R), Nil }\n\
         struct HasE { e: E, r: R }\n\
         fn mk(i: i64) -> R { return R { id: i }; }\n";
    for (label, body, want) in [
        (
            "mixed wrap, move a",
            "fn f(r: R) -> i64 { let s = S3 { a: r, b: mk(2) }; let x = s.a; return 7; }\n\
             fn main() { println(f(mk(1))) }",
            "dR2\ndR1\n7\n",
        ),
        (
            "both fresh, move a",
            "fn f() -> i64 { let s = S3 { a: mk(5), b: mk(6) }; let x = s.a; return 7; }\n\
             fn main() { println(f()) }",
            "dR5\ndR6\n7\n",
        ),
        (
            "move b (control)",
            "fn f(r: R) -> i64 { let s = S3 { a: r, b: mk(8) }; let x = s.b; return 7; }\n\
             fn main() { println(f(mk(7))) }",
            "dR8\ndR7\n7\n",
        ),
        (
            "no move (control)",
            "fn f(r: R) -> i64 { let s = S3 { a: r, b: mk(4) }; return 7; }\n\
             fn main() { println(f(mk(3))) }",
            "dR4\ndR3\n7\n",
        ),
        (
            "deep chain masks the moved LEAF, not the whole hop",
            "fn f() -> i64 { let o = Outer { h: Inner { r: mk(1), q: mk(2) }, k: mk(3) }; let x = o.h.r; return 7; }\n\
             fn main() { println(f()) }",
            "dR1\ndR3\ndR2\n7\n",
        ),
        (
            "deep chain, three hops: every sibling on the way keeps its body",
            "fn f() -> i64 { let o = L1 { b: L2 { c: L3 { r: mk(1), q: mk(2) }, d: mk(3) }, e: mk(4) }; let x = o.b.c.r; return 7; }\n\
             fn main() { println(f()) }",
            "dR1\ndR4\ndR3\ndR2\n7\n",
        ),
        (
            "deep chain then DISCARD the hop: the moved leaf runs once",
            "fn f() -> i64 { let o = Outer { h: Inner { r: mk(1), q: mk(2) }, k: mk(3) }; let x = o.h.r; let Outer { k, h: _ } = o; return k.id; }\n\
             fn main() { println(f()) }",
            "dR1\ndR2\ndR3\n3\n",
        ),
        (
            "enum-valued source field",
            "fn f() -> i64 { let s = HasE { e: E.A(mk(4)), r: mk(5) }; let x = s.r; return 7; }\n\
             fn main() { println(f()) }",
            "dR5\ndR4\n7\n",
        ),
        (
            "single Drop field",
            "fn f() -> i64 { let s = Inner { r: mk(6), q: mk(9) }; let x = s.r; return 7; }\n\
             fn main() { println(f()) }",
            "dR6\ndR9\n7\n",
        ),
    ] {
        assert_eq!(run(&format!("{H}{body}\n")), want, "{label}");
    }
}

/// B-2026-09-03-23 — A MONOMORPH BODY MUST ANSWER THE `Drop`-OWNERSHIP QUESTION
/// FROM ITS OWN PARAMETERS, NOT ITS CALLER'S.
///
/// `compile_generic_call` emits the monomorph body INLINE, mid-caller, and
/// populated neither `current_fn_param_names` nor `owned_struct_params` for it.
/// Those sets are per-FUNCTION and were still the ENCLOSING function's, so a
/// LOCAL inside `fn inner[T]` whose name matched ANY caller's parameter answered
/// `owner_runs_bodies = true`, took the tuple element's MEMORY, cap-zeroed the
/// source without recording the take, and left the source's own walk to run the
/// element's `Drop` body against the slot the cap-zeroing had just emptied.
///
/// Every body renders `tag` and `xs.len()`, so a body running on a cap-zeroed
/// husk prints `dRnn//0` and is distinguishable from a correct one — a test that
/// asserted body COUNTS would see one body on both sides and pass. Silent
/// otherwise: no crash, no leak, no diagnostic (valgrind reports the program
/// fully balanced either way).
///
/// ACTION AT A DISTANCE, AND IT SPREADS. The trigger is a name in a DIFFERENT
/// function — `takesH`'s parameter, never called from `main`. Because the body is
/// emitted ONCE and shared by every call site, the corruption reached
/// `plainLocal`, which has no parameter at all. `fresh` is the same shape under a
/// different name (`q`, colliding with `takesQ`) so the fixture cannot pass by
/// special-casing one identifier; `strfield` is the sibling whose struct carries a
/// `String` field, which is what makes the mono param loop's `owned_struct_params`
/// registration fire.
///
/// `marker` pins PLACEMENT, not only content: it extends the SOURCE's live range
/// past the read (`z{h.z}`), so a body belonging to the leaf prints before `z9`
/// and one belonging to the source prints after it. Pre-fix it printed
/// `dR64//0` after `z9` — wrong on both axes at once.
///
/// OVER-REACH CONTROLS, in the opposite direction, so a widened fix fails here
/// rather than passing quietly. `nongen` is the non-generic twin that was correct
/// throughout; `nodrop` instantiates the same shape at a `Drop`-less type and must
/// stay silent; `genparam` is the by-value param of a generic function that
/// B-2026-09-03-16 guarded and this fix un-guards — it must still read ONE body,
/// and is the same cell as that row's `genericfn`.
///
/// Measured pre-fix: `collide`, `fresh`, `strfield` and `marker` all print the
/// husk on all three compiled surfaces against a clean `--interp`; the three
/// controls are byte-identical either way.
///
/// Twin of `tests/codegen.rs`'s
/// `e2e_monomorph_body_answers_drop_ownership_from_its_own_params`, pinned to the
/// same string.
#[test]
fn test_monomorph_body_answers_drop_ownership_from_its_own_params() {
    assert_eq!(
        run(r#"struct R { id: i64, tag: String, xs: Vec[i64] }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}/{self.tag}/{self.xs.len()}") } }
fn mk(i: i64) -> R { return R { id: i, tag: f"t{i}", xs: [i] }; }
struct Hn { pe: (R, i64), z: i64 }
struct Hs { pe: (R, i64), s: String }
struct G[T] { pe: (T, i64), z: i64 }
struct Plain { a: i64, b: i64 }

fn genLocal[T](x: T) -> i64 { let h = Hn { pe: (mk(60), 0), z: 1 }; let (r, k) = h.pe; println(f"  b{r.id}/{r.tag}/{r.xs.len()}"); return k; }
fn genFresh[T](x: T) -> i64 { let q = Hn { pe: (mk(61), 0), z: 1 }; let (r, k) = q.pe; println(f"  b{r.id}/{r.tag}/{r.xs.len()}"); return k; }
fn genStr[T](x: T)   -> i64 { let h = Hs { pe: (mk(62), 0), s: f"s" }; let (r, k) = h.pe; println(f"  b{r.id}/{r.tag}/{r.xs.len()}"); return k; }
fn ngLocal(x: i64)   -> i64 { let h = Hn { pe: (mk(63), 0), z: 1 }; let (r, k) = h.pe; println(f"  b{r.id}/{r.tag}/{r.xs.len()}"); return k; }
fn genMark[T](x: T)  -> i64 { let h = Hn { pe: (mk(64), 0), z: 9 }; let (r, k) = h.pe; println(f"  b{r.id}/{r.tag}"); println(f"  z{h.z}"); return k; }
fn genNoDrop[T](x: T) -> i64 { let h = G[Plain] { pe: (Plain { a: 1, b: 2 }, 0), z: 1 }; let (p, k) = h.pe; println(f"  b{p.a}/{p.b}"); return k; }

fn takesH(h: Hn) -> i64 { return genLocal(5); }
fn takesQ(q: Hn) -> i64 { return genFresh(5); }
fn takesHs(h: Hs) -> i64 { return genStr(5); }
fn takesHm(h: Hn) -> i64 { return genMark(5); }
fn takesHn(h: G[Plain]) -> i64 { return genNoDrop(5); }

fn plainLocal() -> i64 { return genLocal(5); }
fn plainFresh() -> i64 { return genFresh(5); }
fn plainStr()   -> i64 { return genStr(5); }
fn plainMark()  -> i64 { return genMark(5); }
fn plainNoDrop() -> i64 { return genNoDrop(5); }

fn gfn[T](h: G[T]) -> i64 { let (r, k) = h.pe; println("  in"); return k; }
fn gfnLocal() { let h = G[R] { pe: (mk(65), 5), z: 9 }; let _ = gfn(h); }

fn main() {
    println("collide");  let _ = plainLocal();  println("collide end")
    println("fresh");    let _ = plainFresh();  println("fresh end")
    println("strfield"); let _ = plainStr();    println("strfield end")
    println("nongen");   let _ = ngLocal(5);    println("nongen end")
    println("marker");   let _ = plainMark();   println("marker end")
    println("nodrop");   let _ = plainNoDrop(); println("nodrop end")
    println("genparam"); gfnLocal();            println("genparam end")
    println("done")
}
"#),
        r#"collide
  b60/t60/1
dR60/t60/1
collide end
fresh
  b61/t61/1
dR61/t61/1
fresh end
strfield
  b62/t62/1
dR62/t62/1
strfield end
nongen
  b63/t63/1
dR63/t63/1
nongen end
marker
  b64/t64
dR64/t64/1
  z9
marker end
nodrop
  b1/2
nodrop end
genparam
  in
dR65/t65/1
genparam end
done
"#
    );
}

/// B-2026-09-04-29 — a by-value param destructure leaf REBOUND (`let c = b;`)
/// runs the payload's body exactly once.
///
/// The caller retains a by-value param's field bodies (it runs them on its temp
/// after the call), which is why the callee's own leaves are param views taking
/// memory-only drops. The let-site rebind gave `c` a bodies walker of its own,
/// so the body ran in the callee and again in the caller — on both compiled
/// backends, for the identifier source (`rebind`, `rebindu`, `rebindcall`,
/// `twice`) and its projection (`prebind`). `orebind` / `porebind` are the
/// `Option` twins (always correct — the boxed path — kept as controls), `direct`
/// the un-rebound leaf.
///
/// Interpreter twin of `e2e_param_destructure_leaf_rebind_runs_body_once` (tests/codegen.rs) — same program string, same
/// pin.
/// B-2026-09-04-30 — a by-value `self` receiver on a TEMP runs its `Drop`
/// bodies, and the fresh-temp receiver's field walk runs BEFORE its memory is
/// freed.
///
/// Codegen treats a by-value `self` exactly like a by-value param —
/// caller-retained — while B-2026-08-01-5 had excluded owned `self` from the
/// caller's receiver-temp registration to stop a passthrough chain
/// double-firing. A local receiver still had an owner and a by-value param temp
/// always had one (`param-twin`), but a TEMP receiver had none on any surface.
/// The three guard cells (`returns-self`, `hands-field-out`, `generic-return`)
/// pin that a return which can carry the receiver still stands the caller down.
/// `temp-recv-refself` pins the second half — the no-own-`Drop` arm's
/// bodies-then-memory registration order let the LIFO drain free the fields
/// before the walk read them.
///
/// Interpreter twin of `e2e_owned_self_temp_receiver_runs_drop_bodies`
/// (tests/codegen.rs) — same program string, same pin.
#[test]
fn test_owned_self_temp_receiver_runs_drop_bodies() {
    let out = run(r#"struct R { id: i64, tag: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}/{self.tag}") } }
fn mk(n: i64) -> R { return R { id: n, tag: f"t{n}" }; }
struct HoRes { a: R, b: Result[R, String] }
struct Pair { a: R, b: R }
struct OwnD { a: R, n: i64 }
impl Drop for OwnD { fn drop(mut ref self) { println(f"dOwnD{self.n}") } }

impl HoRes { fn plain(self) { println(f"  rd{self.a.id}") } }
impl Pair { fn eat(self) { println(f"  pr{self.a.id}") } fn peek(ref self) { println(f"  bo{self.a.id}") } }
impl OwnD { fn eat(self) { println(f"  od{self.n}") } }
impl R {
  fn ident(self) { println(f"  id{self.id}") }
  fn area(self) -> i64 { return self.id * 2; }
  fn me(self) -> R { return self; }
}
struct W { a: R }
impl W { fn unwrap_a(self) -> R { return self.a; } fn opt(self) -> Option[R] { return Option.Some(self.a); } }

fn p_plain(h: HoRes) { println(f"  pd{h.a.id}") }

fn main() {
  println("temp-recv-fields");  HoRes { a: mk(1), b: Result.Ok(mk(101)) }.plain()
  println("temp-recv-own");     mk(2).ident()
  println("temp-recv-ownd");    OwnD { a: mk(3), n: 3 }.eat()
  println("temp-recv-pair");    Pair { a: mk(4), b: mk(104) }.eat()
  println("temp-recv-refself"); Pair { a: mk(5), b: mk(105) }.peek()
  println("param-twin");        p_plain(HoRes { a: mk(6), b: Result.Ok(mk(106)) })
  println("local-recv");        let h = HoRes { a: mk(7), b: Result.Ok(mk(107)) }; h.plain()
  println("scalar-return");     let v = mk(8).area(); println(f"  v{v}")
  println("returns-self");      let m = mk(9).me(); println(f"  m{m.id}")
  println("hands-field-out");   let g = W { a: mk(10) }.unwrap_a(); println(f"  g{g.id}")
  println("generic-return");    let o = W { a: mk(11) }.opt(); println("  built")
  println("done")
}
"#);
    assert_eq!(
        out.trim_end(),
        r#"temp-recv-fields
  rd1
dR101/t101
dR1/t1
temp-recv-own
  id2
dR2/t2
temp-recv-ownd
  od3
dOwnD3
dR3/t3
temp-recv-pair
  pr4
dR104/t104
dR4/t4
temp-recv-refself
  bo5
dR105/t105
dR5/t5
param-twin
  pd6
dR106/t106
dR6/t6
local-recv
  rd7
dR107/t107
dR7/t7
scalar-return
dR8/t8
  v16
returns-self
  m9
dR9/t9
hands-field-out
  g10
dR10/t10
generic-return
dR11/t11
  built
done"#
    );
}

/// B-2026-09-04-4's ORACLE. The interpreter has always run a generic
/// `impl[T] Drop for S[T]` body; the compiled backends ran none, because
/// `Drop::drop` is the one impl method with no source call site and the mono
/// pipeline instantiates from call sites. This pins the side of the divergence
/// that was already right, so a future change to the compiled half has a fixed
/// target rather than a moving one — and so a regression HERE is caught too.
///
/// The cells are the codegen twin's, verbatim: `two` for two live monomorphs
/// (`dB9`/`dB8`, distinct field offsets), `field` for a generic parent whose
/// own field is Drop-bearing, `vec`/`nest` for a bare-`T` collection and a
/// nested generic arg, `plain` for the non-generic control, and `str` for
/// PLACEMENT — `dB7` between `v8` and `after`, the binding's last use.
#[test]
fn test_generic_impl_drop_runs_its_body_per_monomorph() {
    assert_eq!(
        run(r#"struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }

struct Box3[T] { v: T, tag: String }
impl[T] Drop for Box3[T] { fn drop(mut ref self) { println(f"dB{self.tag.len()}") } }

struct G[T] { v: T, r: R }
impl[T] Drop for G[T] { fn drop(mut ref self) { println("dG") } }

struct H[T] { items: Vec[T] }
impl[T] Drop for H[T] { fn drop(mut ref self) { println(f"dH{self.items.len()}") } }

struct P { s: String }
impl Drop for P { fn drop(mut ref self) { println("dP") } }

fn cell_str() {
    let b: Box3[String] = Box3 { v: f"vvvvvvvv", tag: f"ttttttt" };
    println(f"  v{b.v.len()}");
    println("  after");
}
fn cell_two() {
    let a: Box3[String] = Box3 { v: f"aaaaaaaa", tag: f"ttttttttt" };
    println(f"  a{a.v.len()}");
    let b: Box3[i64] = Box3 { v: 7, tag: f"uuuuuuuu" };
    println(f"  b{b.v}");
}
fn cell_field() { let g: G[String] = G { v: f"gggggggg", r: R { id: 3 } }; println(f"  g{g.v.len()}"); }
fn cell_vec()   { let h: H[String] = H { items: [f"aaaaaaaa", f"bbbbbbbb"] }; println(f"  h{h.items.len()}"); }
fn cell_nest()  { let d: Box3[Vec[String]] = Box3 { v: [f"zzzzzzzz"], tag: f"wwwwww" }; println(f"  d{d.v.len()}"); }
fn cell_plain() { let p = P { s: f"pppppppp" }; println(f"  p{p.s.len()}"); }

fn main() {
    println("str");   cell_str();
    println("two");   cell_two();
    println("field"); cell_field();
    println("vec");   cell_vec();
    println("nest");  cell_nest();
    println("plain"); cell_plain();
    println("done");
}
"#),
        r#"str
  v8
dB7
  after
two
  a8
dB9
  b7
dB8
field
  g8
dG
dR3
vec
  h2
dH2
nest
  d1
dB6
plain
  p8
dP
done
"#
    );
}

/// The interpreter oracle for B-2026-09-04-4's argument half. Unchanged by the
/// fix — it always ran these bodies, in these frames — and pinned so the
/// compiled half has a fixed target. See the codegen twin for what each cell
/// is for and for the double-free this shape produced mid-fix.
#[test]
fn test_generic_impl_drop_survives_a_by_value_argument() {
    assert_eq!(
        run(r#"struct Box3[T] { v: T, tag: String }
impl[T] Drop for Box3[T] { fn drop(mut ref self) { println(f"dB{self.tag.len()}") } }
struct Pl { v: String, tag: String }
impl Drop for Pl { fn drop(mut ref self) { println(f"dP{self.tag.len()}") } }

fn take(b: Box3[String]) { println(f"  t{b.v.len()}") }
fn takei(b: Box3[i64]) { println(f"  ti{b.v}") }
fn takep(b: Pl) { println(f"  tp{b.v.len()}") }
fn make() -> Box3[String] { return Box3 { v: f"mmmmmmmm", tag: f"rrrrrrr" }; }

fn c_param() { let b: Box3[String] = Box3 { v: f"pppppppp", tag: f"qqqqqq" }; take(b); println("  after") }
fn c_parami() { let b: Box3[i64] = Box3 { v: 5, tag: f"iiiii" }; takei(b) }
fn c_ret()   { let r = make(); println(f"  r{r.v.len()}") }
fn c_temp()  { take(Box3 { v: f"tttttttt", tag: f"sssss" }) }
fn c_plain() { let b = Pl { v: f"pppppppp", tag: f"qqqqqq" }; takep(b) }

fn main() {
    println("param");  c_param();
    println("parami"); c_parami();
    println("ret");    c_ret();
    println("temp");   c_temp();
    println("plain");  c_plain();
    println("done");
}
"#),
        r#"param
  t8
dB6
  after
parami
  ti5
dB5
ret
  r8
dB7
temp
  t8
dB5
plain
  tp8
dP6
done
"#
    );
}

/// B-2026-09-15-5 — a map/set LOOKUP key temporary's user `Drop` body runs at
/// the lookup, which is where its live range ends.
///
/// A lookup BORROWS its key and discards it. The reclaim added by
/// B-2026-08-26-32 / B-2026-09-13-20 / B-2026-09-13-30 resolves a MEMORY walk at
/// each of the five lookup entry points; a type's user `Drop` hook is a SEPARATE
/// `Type.drop` call that every other drop site pairs with its walk, and the key
/// sites emitted the walk alone. So a key's storage was reclaimed exactly once
/// and its body never ran.
///
/// design.md § Drop is what makes the lookup the OWED position rather than merely
/// an early one: destructors fire at a value's live-range end, not at lexical
/// scope end, and "a value whose last use is mid-scope is dropped at that use".
/// A key temporary's last use IS the lookup.
///
/// A BOTH-BACKENDS GAP, not a run/build divergence — measured byte-identical on
/// `karac run --interp` and `karac build` before the fix, which is why an A/B
/// kata could never have caught it and why this fixture is PAIRED instead.
/// Invisible to ASAN and to both ratchet legs too, since storage is freed exactly
/// once either way; only an output comparison sees it.
///
/// THE FIVE CONTROL CELLS ARE WHAT LOCALIZE IT. A fresh temp's body already ran
/// at an ordinary consuming position, at a bare discard, at a binding's
/// live-range end, at container destruction, and for an inserted key — so the
/// machinery worked everywhere except this one position, and the fix belongs at
/// the key sites rather than in any walker. They are also the cells that fail if
/// the fix ever DOUBLES a body.
///
/// THE `dD1` POSITION IN EVERY MAP CELL IS THE LIVE-RANGE RULE, NOT AN ODDITY,
/// and predicting it wrong is easy: the stored element's body fires BEFORE
/// `post`, not at the end of `main`, because the map's own last use is the
/// lookup, so the map dies there too. Every `want` here was measured and then
/// checked against that rule rather than assumed from lexical nesting.
///
/// A TUPLE key needs its own arm and nearly became a regression: the
/// `TypeKind::Path`-keyed leaf walker declines a nameless type, while the
/// interpreter's value-driven walk recurses into tuple elements — so fixing only
/// the named case would have traded a symmetric gap for a run/build divergence,
/// which is strictly worse. Cells 5 and 6 pin both tuple spellings.
///
/// NOT FIXED HERE, and deliberately not asserted: an enum stored in a `Map` or
/// `Set` never runs its user `Drop` body at all — `s.insert(mke(0))` with no
/// lookup anywhere prints nothing, while `Vec[Tg]` is correct. That is a
/// STORED-ELEMENT defect independent of this row's lookup question, filed
/// separately; the lookup half of the enum spelling IS fixed by this change.
#[test]
fn test_a_lookup_key_temporarys_user_drop_body_runs_at_the_lookup() {
    let hdr = "#[derive(Hash, Eq, PartialEq)]\n\
               struct Dk { a: String, b: i64 }\n\
               impl Drop for Dk { fn drop(mut ref self) { println(f\"dD{self.b}\") } }\n\
               #[derive(Hash, Eq, PartialEq)]\n\
               struct Nest { i: Dk, b: i64 }\n\
               struct Mk { p: String }\n\
               impl Mk { fn mkd(ref self, n: i64) -> Dk { return Dk { a: f\"heap-{n}\", b: n }; } }\n\
               impl Mk { fn build(n: i64) -> Dk { return Dk { a: f\"heap-{n}\", b: n }; } }\n\
               fn mkd(n: i64) -> Dk { return Dk { a: f\"heap-{n}\", b: n }; }\n\
               fn mkn(n: i64) -> Nest { return Nest { i: mkd(n), b: n }; }\n\
               fn mkt(n: i64) -> (Dk, i64) { return (mkd(n), n); }\n\
               fn eat(d: Dk) -> i64 { return d.b; }\n";
    for (label, stmts, want) in [
        (
            "the row's shape: a struct key at Map.get",
            "let mut m: Map[Dk, i64] = Map.new();\n\
             m.insert(mkd(1), 1);\n\
             println(\"pre\");\n\
             match m.get(mkd(2)) { Some(v) => { println(f\"g{v}\"); } None => { println(\"miss\"); } }\n\
             println(\"post\");\n",
            "pre\ndD2\nmiss\ndD1\npost\nend\n",
        ),
        (
            "a Drop-bearing field ONE LEVEL DOWN -- the row's second question",
            "let mut m: Map[Nest, i64] = Map.new();\n\
             m.insert(mkn(1), 1);\n\
             println(\"pre\");\n\
             match m.get(mkn(2)) { Some(v) => { println(f\"g{v}\"); } None => { println(\"miss\"); } }\n\
             println(\"post\");\n",
            "pre\ndD2\nmiss\ndD1\npost\nend\n",
        ),
        (
            "a METHOD-call key, the spelling B-2026-09-13-30 made memory-clean",
            "let mut m: Map[Dk, i64] = Map.new();\n\
             let g: Mk = Mk { p: f\"x\" };\n\
             m.insert(g.mkd(1), 1);\n\
             println(\"pre\");\n\
             match m.get(g.mkd(2)) { Some(v) => { println(f\"g{v}\"); } None => { println(\"miss\"); } }\n\
             println(\"post\");\n",
            "pre\ndD2\nmiss\ndD1\npost\nend\n",
        ),
        (
            "an ASSOC-FN key: a Path callee, through the same shared resolver",
            "let mut m: Map[Dk, i64] = Map.new();\n\
             m.insert(Mk.build(1), 1);\n\
             println(\"pre\");\n\
             match m.get(Mk.build(2)) { Some(v) => { println(f\"g{v}\"); } None => { println(\"miss\"); } }\n\
             println(\"post\");\n",
            "pre\ndD2\nmiss\ndD1\npost\nend\n",
        ),
        (
            "a TUPLE key from a call -- nameless, so the per-ELEMENT walker",
            "let mut m: Map[(Dk, i64), i64] = Map.new();\n\
             m.insert(mkt(1), 1);\n\
             println(\"pre\");\n\
             match m.get(mkt(2)) { Some(v) => { println(f\"g{v}\"); } None => { println(\"miss\"); } }\n\
             println(\"post\");\n",
            "pre\ndD2\nmiss\ndD1\npost\nend\n",
        ),
        (
            "a TUPLE LITERAL key, fresh by construction",
            "let mut m: Map[(Dk, i64), i64] = Map.new();\n\
             m.insert((mkd(1), 1), 1);\n\
             println(\"pre\");\n\
             match m.get((mkd(2), 2)) { Some(v) => { println(f\"g{v}\"); } None => { println(\"miss\"); } }\n\
             println(\"post\");\n",
            "pre\ndD2\nmiss\ndD1\npost\nend\n",
        ),
        (
            "Set.contains",
            "let mut s: Set[Dk] = Set.new();\n\
             s.insert(mkd(1));\n\
             println(\"pre\");\n\
             if s.contains(mkd(2)) { println(\"hit\"); } else { println(\"miss\"); }\n\
             println(\"post\");\n",
            "pre\ndD2\nmiss\ndD1\npost\nend\n",
        ),
        (
            "contains_key + remove: THREE bodies -- two key temps and the stored key",
            "let mut m: Map[Dk, i64] = Map.new();\n\
             m.insert(mkd(1), 1);\n\
             println(\"pre\");\n\
             if m.contains_key(mkd(2)) { println(\"has\"); } else { println(\"no\"); }\n\
             println(\"mid\");\n\
             m.remove(mkd(1));\n\
             println(\"post\");\n",
            "pre\ndD2\nno\nmid\ndD1\ndD1\npost\nend\n",
        ),
        (
            "CONTROL: an ordinary consuming position was always correct",
            "println(\"pre\");\n\
             let r: i64 = eat(mkd(3));\n\
             println(f\"r{r}\");\n\
             println(\"post\");\n",
            "pre\ndD3\nr3\npost\nend\n",
        ),
        (
            "CONTROL: a bare discard was always correct",
            "println(\"pre\");\n\
             mkd(4);\n\
             println(\"post\");\n",
            "pre\ndD4\npost\nend\n",
        ),
        (
            "CONTROL: a BOUND key -- the body is the binding's, at ITS live-range end",
            "let mut m: Map[Dk, i64] = Map.new();\n\
             m.insert(mkd(1), 1);\n\
             println(\"pre\");\n\
             let k: Dk = mkd(2);\n\
             match m.get(k) { Some(v) => { println(f\"g{v}\"); } None => { println(\"miss\"); } }\n\
             println(\"post\");\n",
            "pre\nmiss\ndD2\ndD1\npost\nend\n",
        ),
        (
            "CONTROL: insert MOVES its key -- one body, at the map's destruction",
            "let mut m: Map[Dk, i64] = Map.new();\n\
             println(\"pre\");\n\
             m.insert(mkd(1), 1);\n\
             println(\"post\");\n",
            "pre\ndD1\npost\nend\n",
        ),
        (
            "CONTROL: Vec storage was always correct",
            "let mut v: Vec[Dk] = Vec.new();\n\
             println(\"pre\");\n\
             v.push(mkd(5));\n\
             println(\"post\");\n",
            "pre\ndD5\npost\nend\n",
        ),
    ] {
        let src = format!("{hdr}fn main() {{\n{stmts}\nprintln(\"end\");\n}}\n");
        assert_eq!(run(&src), want, "[{label}]");
    }
}

/// B-2026-09-15-17 / B-2026-09-20-55 — an `Array[R, N]` boxed inside an enum
/// variant runs its element `Drop` bodies where the CALLER sequences them, at
/// both variant arities.
///
/// Two faults, one clause. `declarations.rs` classified such a field
/// `EnumDropKind::BoxedArray` only in a SINGLE-field variant, and that
/// classification flips `enum_param_owned_by_transfer`, which used to hand the
/// bodies to the callee — so the single-field spelling ran them at the callee's
/// frame exit, BEFORE the caller's own statement finished, while `--interp` ran
/// them after (`dR1 dR2 r1` compiled against `r1 dR1 dR2` interpreted). The
/// multi-field spelling was declined by the clause for exactly that reason, and
/// so had no free at all: 136 bytes (64 direct, 72 indirect) per value at
/// `-O0`, with or without a call in the program.
///
/// Both settings of that clause were wrong, which is why neither row could be
/// fixed alone. The class is caller-sequenced now
/// (`enum_boxed_array_payload_runs_user_drop`), which is where a bare
/// `Array[R, N]` param has always put it — `array_param_elem_is_callee_owned`
/// answers FALSE for a `Drop`-running element and its doc states the rule: a
/// by-value aggregate's body prints AFTER the call statement on all four
/// surfaces. Admitting the multi-field field then moves nothing, so the arity
/// clause came out and the leak closed with it.
///
/// THE THIRD CELL HAS NO CALL, and it is the one that says the leak was never
/// about the param path: `c2_concrete_two` leaked the same 136 bytes with no
/// callee in the program.
///
/// THIS FIXTURE PASSES ON THE UNFIXED TREE, and that is not a reason to delete
/// it. The interpreter was the CORRECT side of both rows — the fix moved the
/// compiled backends onto this order, so this is the oracle the compiled twin
/// is measured against, not a regression guard for the interpreter. Deleting
/// it would leave that twin asserting a string with nothing behind it.
///
/// The compiled twin is `codegen`'s `drop_order::
/// codegen_boxed_array_enum_payload_bodies_are_caller_sequenced`, asserting this
/// same string — both rows are run-vs-build divergences, so a fixture on one
/// backend alone cannot see them. The memory half is
/// `memory_sanitizer`'s `enums::asan_multi_field_boxed_array_enum_payload_is_freed`,
/// which must run at `-O0`: at the default opt level LLVM deletes an allocation
/// nothing observes and the leak is invisible.
#[test]
fn interp_boxed_array_enum_payload_bodies_are_caller_sequenced() {
    assert_eq!(
        run("struct R { id: i64, s: String }\n\
     impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\") } }\n\
     enum C1 { X(Array[R, 2]) }\n\
     enum C2 { X(Array[R, 2], i64) }\n\
     fn mkarr(b: i64) -> Array[R, 2] {\n\
     \x20   return [R { id: b, s: f\"pay-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-{b}\" },\n\
     \x20           R { id: b + 1, s: f\"pay-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-{b}\" }];\n\
     }\n\
     fn eat1(w: C1) -> i64 { match w { C1.X(a) => { return a[0].id } } }\n\
     fn eat2(w: C2) -> i64 { match w { C2.X(a, n) => { return a[0].id + n } } }\n\
     fn main() {\n\
     \x20   { let w: C1 = C1.X(mkarr(1)); println(f\"r{eat1(w)}\") }\n\
     \x20   println(\"mid\");\n\
     \x20   { let v: C2 = C2.X(mkarr(3), 20); println(f\"s{eat2(v)}\") }\n\
     \x20   println(\"mid2\");\n\
     \x20   { let u: C2 = C2.X(mkarr(5), 1); println(\"held\") }\n\
     \x20   println(\"end\");\n\
     }\n"),
        "r1\ndR1\ndR2\nmid\ns23\ndR3\ndR4\nmid2\ndR5\ndR6\nheld\nend\n"
    );
}

/// B-2026-09-22-8 -- a by-value `Array` (or `Vec`) parameter that the callee
/// wraps into a constructor, a struct literal or a tuple has its element
/// `Drop` bodies run by the CALLER on every backend (the caller-retains
/// convention for such a param). The interpreter also gave the wrapped value
/// an owner of its own inside the callee, so it ran every element body twice
/// (`d1 d2 d1 d2` where one pair is due) in 13 of these 15 cells. `user` and
/// `vec` were already right there.
///
/// Four interpreter gates were at fault. The seeded-constructor scrutinee
/// (`match Some(a)`, `if let`, `let .. else`) never counted a payload slot
/// holding the param as a view. The named-local wrap (`let o = Some(a)`, the
/// struct literal, the tuple) asked `value_runs_user_drop`, which answers
/// `false` for any bare container, so an `Array` payload never qualified as a
/// view at all.
///
/// Two compiled halves moved with it, because fixing the interpreter alone
/// would have left the build divergent. `if let` and `let .. else` never set
/// the seeded-array flag `match` sets (B-2026-09-19-61), so `iflet` and
/// `letelse` ran the bodies twice on every compiled surface. `named_mixed` is
/// the named-`let` spelling of B-2026-09-23-1: the box-only twin already
/// skipped only the caller's field, but the payload-bodies walker was
/// withheld WHOLE, so the fresh field's `d90 d91` ran nowhere compiled. Memory
/// was balanced throughout.
///
/// Measured on this tree: the stdout below is byte-identical across
/// `--interp`, jit, `karac build` and `KARAC_OPT_LEVEL=0 karac build`, with 0
/// bytes in 0 blocks and 0 errors under `valgrind --leak-check=full` at `-O0`.
/// A bare STRUCT param wrapped the same way is deliberately absent: it runs
/// its body twice on EVERY surface, and that agreed defect is filed on its own.
#[test]
fn interp_array_param_wrapped_by_the_callee_runs_its_element_bodies_once() {
    assert_eq!(
        run(r#"struct R { id: i64, s: String }
impl Drop for R { fn drop(mut ref self) { println(f"  d{self.id}") } }
fn mkr(i: i64) -> R { return R { id: i, s: f"aaaaaaaa{i}" } }
enum Pw { P(Array[R, 2]), Q }
enum Pw2 { P2(Array[R, 2], Array[R, 2]), Q2 }
enum Pv { Pv1(Vec[R]), Qv }
struct Bx { v: Array[R, 2] }
struct Hm { k: i64 }
impl Hm {
    fn seeded(ref self, a: Array[R, 2]) -> i64 { match Option.Some(a) { Option.Some(v) => { println("  arm"); return 1 }, Option.None => { return 0 } } }
    fn named(ref self, a: Array[R, 2]) -> i64 { let o = Option.Some(a); println("  mid"); return 1 }
}
fn c_opt(a: Array[R, 2]) -> i64 { match Option.Some(a) { Option.Some(v) => { println(f"  r{v[0].id}"); return 1 }, Option.None => { return 0 } } }
fn c_bare(a: Array[R, 2]) -> i64 { match Some(a) { Some(v) => { println("  arm"); return 1 }, None => { return 0 } } }
fn c_res(a: Array[R, 2]) -> i64 { match Result.Ok(a) { Result.Ok(v) => { println("  arm"); return 1 }, Result.Err(e) => { return 0 } } }
fn c_user(a: Array[R, 2]) -> i64 { match Pw.P(a) { Pw.P(v) => { println(f"  r{v[1].id}"); return 1 }, Pw.Q => { return 0 } } }
fn c_vec(a: Vec[R]) -> i64 { match Pv.Pv1(a) { Pv.Pv1(v) => { println("  arm"); return 1 }, Pv.Qv => { return 0 } } }
fn c_iflet(a: Array[R, 2]) -> i64 { if let Option.Some(v) = Option.Some(a) { println(f"  r{v[1].id}"); return 1 } return 0 }
fn c_letelse(a: Array[R, 2]) -> i64 { let Option.Some(v) = Option.Some(a) else { return 0 }; println(f"  r{v[0].id}"); return 1 }
fn c_named(a: Array[R, 2]) -> i64 { let o = Option.Some(a); match o { Option.Some(v) => { println("  arm"); return 1 }, Option.None => { return 0 } } }
fn c_named_user(a: Array[R, 2]) -> i64 { let o = Pw.P(a); println("  mid"); return 1 }
fn c_named_mixed(a: Array[R, 2]) -> i64 { let o = Pw2.P2(a, [mkr(90), mkr(91)]); println("  mid"); return 1 }
fn c_named_rebind(a: Array[R, 2]) -> i64 { let o = Option.Some(a); let o2 = o; println("  mid"); return 1 }
fn c_struct(a: Array[R, 2]) -> i64 { let b = Bx { v: a }; println(f"  r{b.v[0].id}"); return 1 }
fn c_tuple(a: Array[R, 2]) -> i64 { let t = (a, 5); println(f"  r{t.1}"); return 1 }
fn main() {
    println("opt");         { let a: Array[R, 2] = [mkr(1), mkr(2)]; let z = c_opt(a); }
    println("bare");        { let a: Array[R, 2] = [mkr(3), mkr(4)]; let z = c_bare(a); }
    println("res");         { let a: Array[R, 2] = [mkr(5), mkr(6)]; let z = c_res(a); }
    println("user");        { let a: Array[R, 2] = [mkr(7), mkr(8)]; let z = c_user(a); }
    println("vec");         { let a: Vec[R] = [mkr(9), mkr(10)]; let z = c_vec(a); }
    println("iflet");       { let a: Array[R, 2] = [mkr(11), mkr(12)]; let z = c_iflet(a); }
    println("letelse");     { let a: Array[R, 2] = [mkr(13), mkr(14)]; let z = c_letelse(a); }
    println("named");       { let a: Array[R, 2] = [mkr(15), mkr(16)]; let z = c_named(a); }
    println("named_user");  { let a: Array[R, 2] = [mkr(17), mkr(18)]; let z = c_named_user(a); }
    println("named_mixed"); { let a: Array[R, 2] = [mkr(19), mkr(20)]; let z = c_named_mixed(a); }
    println("named_rebind");{ let a: Array[R, 2] = [mkr(21), mkr(22)]; let z = c_named_rebind(a); }
    println("struct");      { let a: Array[R, 2] = [mkr(23), mkr(24)]; let z = c_struct(a); }
    println("tuple");       { let a: Array[R, 2] = [mkr(25), mkr(26)]; let z = c_tuple(a); }
    println("m_seeded");    { let h = Hm { k: 1 }; let a: Array[R, 2] = [mkr(27), mkr(28)]; let z = h.seeded(a); }
    println("m_named");     { let h = Hm { k: 1 }; let a: Array[R, 2] = [mkr(29), mkr(30)]; let z = h.named(a); }
    println("end")
}"#),
        "opt\n  r1\n  d1\n  d2\nbare\n  arm\n  d3\n  d4\nres\n  arm\n  d5\n  d6\nuser\n  r8\n  d7\n  d8\nvec\n  arm\n  d9\n  d10\niflet\n  r12\n  d11\n  d12\nletelse\n  r13\n  d13\n  d14\nnamed\n  arm\n  d15\n  d16\nnamed_user\n  mid\n  d17\n  d18\nnamed_mixed\n  d90\n  d91\n  mid\n  d19\n  d20\nnamed_rebind\n  mid\n  d21\n  d22\nstruct\n  r23\n  d23\n  d24\ntuple\n  r5\n  d25\n  d26\nm_seeded\n  arm\n  d27\n  d28\nm_named\n  mid\n  d29\n  d30\nend\n"
    );
}

/// B-2026-09-23-5 -- a by-value `Array` param whose element runs a user `Drop`
/// is CALLER-RETAINED: the caller runs the element bodies and frees the
/// buffers after the call. A REBIND of it inside the callee is therefore only
/// another name for the caller's array, and so is a seeded arm binding over it
/// (`match Some(a) { Some(v) => .. }`) and any rebind of that binding.
///
/// Codegen treated the rebind's destination as a new owner. `let m = a;` gave
/// `m` the element-bodies walker, so every body ran twice (`d1 d2 in d1 d2`
/// on the JIT, `-O0` and `-O2`). `Some(v) => { let u = v; .. }` also handed
/// `u` the MEMORY drop, because the arm binding sat in the "interior withheld"
/// set that makes a rebind take the buffers over, and it aborted with
/// `free(): double free detected in tcache 2` on every compiled surface. The
/// interpreter was right in every cell. The fix records such names in
/// `caller_retained_array_views`, and a rebind of a member takes neither the
/// bodies nor the memory.
///
/// `shadow` checks that a later `let` of the same name owning a FRESH array
/// leaves the view set. `str` is the callee-owned control: an
/// `Array[String, 2]` param is not caller-retained and must be unchanged.
/// `ctl` is the by-value control with no rebind. Measured on this tree: the
/// stdout below is byte-identical across `--interp`, jit, `karac build` and
/// `KARAC_OPT_LEVEL=0 karac build`, and `valgrind --leak-check=full` at
/// `-O0` with `KARAC_AUTO_PAR=0` reports 0 errors and 0 bytes in use at exit.
///
/// Deliberately ABSENT, each measured split and filed on its own: a `Vec[R]`
/// param rebound the same way (bodies twice on build), the seeded arm rebind
/// over an `Array[String, 2]` param (a double free whose owner is in this
/// frame, B-2026-09-19-60's mechanism), a user-enum seeded arm rebind (bodies
/// twice on build), and `return` of the param, which double-frees without
/// any rebind.
#[test]
fn interp_array_param_rebound_in_the_callee_is_a_view_of_the_callers_array() {
    assert_eq!(
        run(r#"struct R { id: i64, s: String }
impl Drop for R { fn drop(mut ref self) { println(f"  d{self.id}") } }
fn mkr(i: i64) -> R { return R { id: i, s: f"heap-string-longer-than-sso-{i}" } }
fn take(x: Array[R, 2]) -> i64 { println("  take"); return 1 }
fn let_rb(a: Array[R, 2]) -> i64 { let m = a; println("  in"); return 1 }
fn let_annot(a: Array[R, 2]) -> i64 { let m: Array[R, 2] = a; println("  in"); return 1 }
fn let_chain(a: Array[R, 2]) -> i64 { let m = a; let m2 = m; println("  in"); return 1 }
fn let_read(a: Array[R, 2]) -> i64 { let m = a; println(f"  r{m[0].id}:{m[1].s.len()}"); return 1 }
fn let_onward(a: Array[R, 2]) -> i64 { let m = a; let t = take(m); println("  in"); return t }
fn let_shadow(a: Array[R, 2]) -> i64 { let m = a; let m = [mkr(90), mkr(91)]; println(f"  in{m[0].id}"); return 1 }
fn alias_seed(a: Array[R, 2]) -> i64 { let m = a; match Option.Some(m) { Option.Some(v) => { println("  arm"); return 1 }, Option.None => { return 0 } } }
fn alias_seed_rb(a: Array[R, 2]) -> i64 { let m = a; match Option.Some(m) { Option.Some(v) => { let u = v; println("  arm"); return 1 }, Option.None => { return 0 } } }
fn arm_rb(a: Array[R, 2]) -> i64 { match Option.Some(a) { Option.Some(v) => { let u = v; println("  arm"); return 1 }, Option.None => { return 0 } } }
fn arm_read(a: Array[R, 2]) -> i64 { match Option.Some(a) { Option.Some(v) => { let u = v; println(f"  arm{u[1].id}"); return 1 }, Option.None => { return 0 } } }
fn arm_chain(a: Array[R, 2]) -> i64 { match Option.Some(a) { Option.Some(v) => { let u = v; let w = u; println("  arm"); return 1 }, Option.None => { return 0 } } }
fn arm_onward(a: Array[R, 2]) -> i64 { match Option.Some(a) { Option.Some(v) => { let u = v; let t = take(u); println("  arm"); return t }, Option.None => { return 0 } } }
fn iflet_rb(a: Array[R, 2]) -> i64 { if let Option.Some(v) = Option.Some(a) { let u = v; println("  arm") }; return 1 }
fn letelse_rb(a: Array[R, 2]) -> i64 { let Option.Some(v) = Option.Some(a) else { return 0 }; let u = v; println("  arm"); return 1 }
fn str_rb(a: Array[String, 2]) -> i64 { let m = a; println(f"  in{m[1].len()}"); return 1 }
fn ctl(a: Array[R, 2]) -> i64 { println("  in"); return 1 }
fn main() {
    println("let");       { let a: Array[R, 2] = [mkr(1), mkr(2)]; let z = let_rb(a); }
    println("annot");     { let a: Array[R, 2] = [mkr(3), mkr(4)]; let z = let_annot(a); }
    println("chain");     { let a: Array[R, 2] = [mkr(5), mkr(6)]; let z = let_chain(a); }
    println("read");      { let a: Array[R, 2] = [mkr(7), mkr(8)]; let z = let_read(a); }
    println("onward");    { let a: Array[R, 2] = [mkr(9), mkr(10)]; let z = let_onward(a); }
    println("shadow");    { let a: Array[R, 2] = [mkr(11), mkr(12)]; let z = let_shadow(a); }
    println("aliasseed"); { let a: Array[R, 2] = [mkr(13), mkr(14)]; let z = alias_seed(a); }
    println("aliasrb");   { let a: Array[R, 2] = [mkr(15), mkr(16)]; let z = alias_seed_rb(a); }
    println("arm");       { let a: Array[R, 2] = [mkr(17), mkr(18)]; let z = arm_rb(a); }
    println("armread");   { let a: Array[R, 2] = [mkr(19), mkr(20)]; let z = arm_read(a); }
    println("armchain");  { let a: Array[R, 2] = [mkr(21), mkr(22)]; let z = arm_chain(a); }
    println("armonward"); { let a: Array[R, 2] = [mkr(23), mkr(24)]; let z = arm_onward(a); }
    println("iflet");     { let a: Array[R, 2] = [mkr(25), mkr(26)]; let z = iflet_rb(a); }
    println("letelse");   { let a: Array[R, 2] = [mkr(27), mkr(28)]; let z = letelse_rb(a); }
    println("str");       { let a: Array[String, 2] = [f"heap-string-longer-than-sso-p", f"heap-string-longer-than-sso-qq"]; let z = str_rb(a); }
    println("ctl");       { let a: Array[R, 2] = [mkr(29), mkr(30)]; let z = ctl(a); }
    println("end")
}"#),
        "let\n  in\n  d1\n  d2\nannot\n  in\n  d3\n  d4\nchain\n  in\n  d5\n  d6\nread\n  r7:29\n  d7\n  d8\nonward\n  take\n  in\n  d9\n  d10\nshadow\n  in90\n  d90\n  d91\n  d11\n  d12\naliasseed\n  arm\n  d13\n  d14\naliasrb\n  arm\n  d15\n  d16\narm\n  arm\n  d17\n  d18\narmread\n  arm20\n  d19\n  d20\narmchain\n  arm\n  d21\n  d22\narmonward\n  take\n  arm\n  d23\n  d24\niflet\n  arm\n  d25\n  d26\nletelse\n  arm\n  d27\n  d28\nstr\n  in30\nctl\n  in\n  d29\n  d30\nend\n"
    );
}

/// B-2026-09-23-12 -- a by-value `Array` param whose element runs a user `Drop`
/// is CALLER-RETAINED, so the caller keeps its memory drop for the argument
/// after the call. When the callee hands the array straight back
/// (`fn eat(a: Array[R, 2]) -> Array[R, 2] { return a }`), the caller's
/// result binding registers its own drop over the same buffers. The element
/// bodies already followed the value to the result, but the memory did not,
/// so `let b = eat(a)` freed every element twice: `free(): double free
/// detected in tcache 2` on the JIT and at `-O0`. The interpreter was right.
///
/// The fix retracts the argument's memory drop at the call when the callee
/// returns that parameter BARE on every exit: directly, through a rebind
/// (`rebind`), or through one further call (`via`, `twice`). BARE means
/// the declared return type is the parameter's own type. `wrap` returns
/// `Some(a)` and is deliberately declined, because an `Option` result frees
/// its box but not the elements' heap, and retracting there leaked 58 B in 2
/// blocks. `str` is the callee-owned control, which was already retracted as
/// a plain argument move. `method` goes through the method registrar and was
/// already right. `loop` re-runs the handover per iteration.
///
/// Measured on this tree: the stdout below is byte-identical across
/// `--interp`, jit, `karac build` and `KARAC_OPT_LEVEL=0 karac build`, and
/// `valgrind --leak-check=full` at `-O0` with `KARAC_AUTO_PAR=0` reports 0
/// errors and 0 bytes in use at exit.
///
/// Deliberately ABSENT: a TEMPORARY result handed on (`take(eat(a))`) loses its
/// bodies and leaks on every surface, which is B-2026-09-20-24; a MIXED-path
/// callee (`if c { return a }; return [..]`), which this fix does not reach
/// and which is filed on its own.
#[test]
fn interp_array_param_handed_back_by_the_callee_is_freed_once() {
    assert_eq!(
        run(r#"struct R { id: i64, s: String }
impl Drop for R { fn drop(mut ref self) { println(f"  d{self.id}") } }
fn mkr(i: i64) -> R { return R { id: i, s: f"heap-string-longer-than-sso-{i}" } }
fn take(x: Array[R, 2]) -> i64 { println("  take"); return 1 }
fn eat(a: Array[R, 2]) -> Array[R, 2] { println("  in"); return a }
fn eat_rb(a: Array[R, 2]) -> Array[R, 2] { let m = a; println("  in"); return m }
fn via(a: Array[R, 2]) -> Array[R, 2] { return eat(a) }
fn pick(a: Array[R, 2], k: i64) -> Array[R, 2] { println(f"  in{k}"); return a }
fn wrap(a: Array[R, 2]) -> Option[Array[R, 2]] { return Option.Some(a) }
fn eat_s(a: Array[String, 2]) -> Array[String, 2] { println("  in"); return a }
struct H { k: i64 }
impl H { fn eat(ref self, a: Array[R, 2]) -> Array[R, 2] { println("  in"); return a } }
fn main() {
    println("bound");   { let a: Array[R, 2] = [mkr(1), mkr(2)]; let b = eat(a); println(f"  y{b[0].id}:{b[1].s.len()}"); }
    println("annot");   { let a: Array[R, 2] = [mkr(3), mkr(4)]; let b: Array[R, 2] = eat(a); println(f"  y{b[1].id}"); }
    println("rebind");  { let a: Array[R, 2] = [mkr(5), mkr(6)]; let b = eat_rb(a); println(f"  y{b[0].id}"); }
    println("via");     { let a: Array[R, 2] = [mkr(7), mkr(8)]; let b = via(a); println(f"  y{b[0].id}"); }
    println("twoarg");  { let a: Array[R, 2] = [mkr(9), mkr(10)]; let b = pick(a, 4); println(f"  y{b[0].id}"); }
    println("chain");   { let a: Array[R, 2] = [mkr(11), mkr(12)]; let b = eat(a); let c = b; println(f"  y{c[0].id}"); }
    println("twice");   { let a: Array[R, 2] = [mkr(15), mkr(16)]; let b = eat(eat(a)); println(f"  y{b[1].id}"); }
    println("method");  { let h = H { k: 1 }; let a: Array[R, 2] = [mkr(17), mkr(18)]; let b = h.eat(a); println(f"  y{b[0].id}"); }
    println("wrap");    { let a: Array[R, 2] = [mkr(19), mkr(20)]; let o = wrap(a); println("  y"); }
    println("str");     { let a: Array[String, 2] = [f"heap-string-longer-than-sso-p", f"heap-string-longer-than-sso-qq"]; let b = eat_s(a); println(f"  y{b[1].len()}"); }
    println("loop");    { let mut i = 0; while i < 3 { let a: Array[R, 2] = [mkr(30 + i), mkr(40 + i)]; let b = eat(a); println(f"  y{b[0].id}"); i = i + 1; } }
    println("end")
}"#),
        "bound\n  in\n  y1:29\n  d1\n  d2\nannot\n  in\n  y4\n  d3\n  d4\nrebind\n  in\n  y5\n  d5\n  d6\nvia\n  in\n  y7\n  d7\n  d8\ntwoarg\n  in4\n  y9\n  d9\n  d10\nchain\n  in\n  y11\n  d11\n  d12\ntwice\n  in\n  in\n  y16\n  d15\n  d16\nmethod\n  in\n  y17\n  d17\n  d18\nwrap\n  d19\n  d20\n  y\nstr\n  in\n  y30\nloop\n  in\n  y30\n  d30\n  d40\n  in\n  y31\n  d31\n  d41\n  in\n  y32\n  d32\n  d42\nend\n"
    );
}

/// B-2026-09-23-15 -- a by-value `Array` param whose element runs a user `Drop`
/// is CALLER-RETAINED, and a callee that returns it on SOME exits and a fresh
/// array on others (`fn ret(a: Array[R, 2], c: bool) -> Array[R, 2] { if c
/// { return a }; .. return [mkr(8), mkr(9)] }`) was wrong on both paths: the
/// hand-back exit double-freed every element on the JIT and at `-O0`, and the
/// exit where `a` died inside ran none of its element bodies, on every
/// surface. The interpreter agreed on the lost bodies.
///
/// The fix makes the callee the owner, as the struct spelling already was
/// (B-2026-08-28-22): the callee registers one flag-guarded bodies-then-memory
/// slot for the param, a hand-back exit clears the flag, and the caller stands
/// the argument down on the same predicate. The cells cover each shape a
/// hand-back can take: an early `return` (`ret`), an `if` tail (`itail`), a
/// `match` tail bare and in a block (`mtch`, `mblk`), a rebind that is then
/// handed back (`rebind`), a `let` of an `if` (`letif`, the dies-inside path
/// only; its hand-back path double-frees on every surface and is filed on its
/// own), a read before the exit (`read`), an instance method (`method`, its
/// hand-back path only: the dies-inside path is an interpreter gap filed on
/// its own), a discarded result and a loop alternating both paths.
///
/// Measured on this tree: the stdout below is byte-identical across
/// `--interp`, jit, `karac build` and `KARAC_OPT_LEVEL=0 karac build`, and
/// `valgrind --leak-check=full` at `-O0` with `KARAC_AUTO_PAR=0` reports 0
/// errors and 0 bytes in use at exit.
#[test]
fn interp_array_param_handed_back_on_some_exits_is_dropped_once() {
    assert_eq!(
        run(r#"struct R { id: i64, s: String }
impl Drop for R { fn drop(mut ref self) { println(f"  d{self.id}") } }
fn mkr(i: i64) -> R { return R { id: i, s: f"heap-string-longer-than-sso-{i}" } }
fn ret(a: Array[R, 2], c: bool) -> Array[R, 2] { if c { return a }; println("  dies"); return [mkr(8), mkr(9)] }
fn itail(a: Array[R, 2], c: bool) -> Array[R, 2] { if c { a } else { [mkr(8), mkr(9)] } }
fn mtch(a: Array[R, 2], k: i64) -> Array[R, 2] { match k { 1 => a, _ => [mkr(8), mkr(9)] } }
fn mblk(a: Array[R, 2], k: i64) -> Array[R, 2] { match k { 1 => { println("  one"); a }, _ => { println("  other"); [mkr(8), mkr(9)] } } }
fn rebind(a: Array[R, 2], c: bool) -> Array[R, 2] { let m = a; if c { return m }; println("  dies"); return [mkr(8), mkr(9)] }
fn letif(a: Array[R, 2], c: bool) -> Array[R, 2] { let r: Array[R, 2] = if c { a } else { [mkr(8), mkr(9)] }; println("  mid"); r }
fn read(a: Array[R, 2], c: bool) -> Array[R, 2] { println(f"  r{a[1].id}"); if c { return a }; return [mkr(8), mkr(9)] }
struct H { k: i64 }
impl H { fn mix(ref self, a: Array[R, 2], c: bool) -> Array[R, 2] { if c { return a }; return [mkr(8), mkr(9)] } }
fn main() {
    println("ret-t");   { let a: Array[R, 2] = [mkr(1), mkr(2)]; let b = ret(a, true); println(f"  y{b[0].id}:{b[1].s.len()}"); }
    println("ret-f");   { let a: Array[R, 2] = [mkr(1), mkr(2)]; let b = ret(a, false); println(f"  y{b[0].id}"); }
    println("itail-t"); { let a: Array[R, 2] = [mkr(1), mkr(2)]; let b = itail(a, true); println(f"  y{b[0].id}"); }
    println("itail-f"); { let a: Array[R, 2] = [mkr(1), mkr(2)]; let b = itail(a, false); println(f"  y{b[0].id}"); }
    println("mtch-t");  { let a: Array[R, 2] = [mkr(1), mkr(2)]; let b = mtch(a, 1); println(f"  y{b[0].id}"); }
    println("mtch-f");  { let a: Array[R, 2] = [mkr(1), mkr(2)]; let b = mtch(a, 0); println(f"  y{b[0].id}"); }
    println("mblk-t");  { let a: Array[R, 2] = [mkr(1), mkr(2)]; let b = mblk(a, 1); println(f"  y{b[0].id}"); }
    println("mblk-f");  { let a: Array[R, 2] = [mkr(1), mkr(2)]; let b = mblk(a, 0); println(f"  y{b[0].id}"); }
    println("rebind-t"); { let a: Array[R, 2] = [mkr(1), mkr(2)]; let b = rebind(a, true); println(f"  y{b[0].id}"); }
    println("rebind-f"); { let a: Array[R, 2] = [mkr(1), mkr(2)]; let b = rebind(a, false); println(f"  y{b[0].id}"); }
    println("letif-f"); { let a: Array[R, 2] = [mkr(1), mkr(2)]; let b = letif(a, false); println(f"  y{b[0].id}"); }
    println("read-t");  { let a: Array[R, 2] = [mkr(1), mkr(2)]; let b = read(a, true); println(f"  y{b[0].id}"); }
    println("read-f");  { let a: Array[R, 2] = [mkr(1), mkr(2)]; let b = read(a, false); println(f"  y{b[0].id}"); }
    println("method-t"); { let h = H { k: 1 }; let a: Array[R, 2] = [mkr(1), mkr(2)]; let b = h.mix(a, true); println(f"  y{b[0].id}"); }
    println("discard"); { let a: Array[R, 2] = [mkr(1), mkr(2)]; let b = ret(a, false); println("  y"); }
    println("loop");    { let mut i = 0; while i < 2 { let a: Array[R, 2] = [mkr(30 + i), mkr(40 + i)]; let b = ret(a, i == 0); println(f"  y{b[0].id}"); i = i + 1; } }
    println("end")
}"#),
        "ret-t\n  y1:29\n  d1\n  d2\nret-f\n  dies\n  d1\n  d2\n  y8\n  d8\n  d9\nitail-t\n  y1\n  d1\n  d2\nitail-f\n  d1\n  d2\n  y8\n  d8\n  d9\nmtch-t\n  y1\n  d1\n  d2\nmtch-f\n  d1\n  d2\n  y8\n  d8\n  d9\nmblk-t\n  one\n  y1\n  d1\n  d2\nmblk-f\n  other\n  d1\n  d2\n  y8\n  d8\n  d9\nrebind-t\n  y1\n  d1\n  d2\nrebind-f\n  d1\n  d2\n  dies\n  y8\n  d8\n  d9\nletif-f\n  mid\n  d1\n  d2\n  y8\n  d8\n  d9\nread-t\n  r2\n  y1\n  d1\n  d2\nread-f\n  r2\n  d1\n  d2\n  y8\n  d8\n  d9\nmethod-t\n  y1\n  d1\n  d2\ndiscard\n  dies\n  d1\n  d2\n  d8\n  d9\n  y\nloop\n  y30\n  d30\n  d40\n  dies\n  d31\n  d41\n  y8\n  d8\n  d9\nend\n"
    );
}

/// B-2026-09-23-16 -- a LOCAL `Array` whose element runs a user `Drop`, handed
/// back on every exit (`let x: Array[R, 1] = [..]; return x`), was freed twice:
/// `free(): double free detected in tcache 2` on the JIT and at `-O0` while
/// the interpreter was right. The `return` retracted the local's memory drop
/// only when a CALLEE PARAMETER of that element type would own it, and a
/// by-value param of an element with a user `Drop` is caller-retained, so the
/// callee freed the buffers and the caller's binding freed them again.
///
/// The fix asks the hand-back question at the `return` instead, but only for a
/// local that EVERY exit hands back (`both` returns it on two exits): the
/// retraction is static, so for a local returned on one exit it would strand
/// the value on the others. That shape still double-frees, with `String`
/// elements too, and is filed on its own. The caller's discard walk admits the
/// same shape (`discard`), since the callee no longer frees it: without that
/// the discarded result leaked, and the `String` spelling had always leaked
/// there (`strs();` leaked its element buffers before this).
///
/// Measured on this tree: the stdout below is byte-identical across
/// `--interp`, jit, `karac build` and `KARAC_OPT_LEVEL=0 karac build`, and
/// `valgrind --leak-check=full` at `-O0` with `KARAC_AUTO_PAR=0` reports 0
/// errors and 0 bytes in use at exit.
#[test]
fn interp_local_array_returned_on_every_exit_is_freed_once() {
    assert_eq!(
        run(r#"struct R { id: i64, s: String }
impl Drop for R { fn drop(mut ref self) { println(f"  d{self.id}") } }
fn mkr(i: i64) -> R { return R { id: i, s: f"heap-string-longer-than-sso-{i}" } }
fn one() -> Array[R, 1] { let x: Array[R, 1] = [mkr(1)]; return x }
fn two(k: i64) -> Array[R, 2] { let x: Array[R, 2] = [mkr(k), mkr(k + 1)]; return x }
fn tail() -> Array[R, 2] { let x: Array[R, 2] = [mkr(5), mkr(6)]; x }
fn both(c: bool) -> Array[R, 2] { let x: Array[R, 2] = [mkr(7), mkr(8)]; if c { return x }; return x }
fn strs() -> Array[String, 2] { let x: Array[String, 2] = [f"heap-string-longer-than-sso-p", f"heap-string-longer-than-sso-qq"]; return x }
fn main() {
    println("one");    { let b = one(); println(f"  y{b[0].id}:{b[0].s.len()}"); }
    println("two");    { let b = two(2); let c = two(20); println(f"  y{b[1].id}{c[0].id}"); }
    println("tail");   { let b = tail(); println(f"  y{b[1].id}"); }
    println("both-t"); { let b = both(true); println(f"  y{b[0].id}"); }
    println("both-f"); { let b = both(false); println(f"  y{b[1].id}"); }
    println("discard"); { two(40); let _ = strs(); strs(); }
    println("str");    { let b = strs(); println(f"  y{b[1].len()}"); }
    println("end")
}"#),
        "one\n  y1:29\n  d1\ntwo\n  y320\n  d20\n  d21\n  d2\n  d3\ntail\n  y6\n  d5\n  d6\nboth-t\n  y7\n  d7\n  d8\nboth-f\n  y8\n  d7\n  d8\ndiscard\nstr\n  y30\nend\n"
    );
}

/// B-2026-09-23-17 — an owning local `Array` returned on SOME exits (bare,
/// through a rebind, from an `if` tail, from inside a loop) runs its element
/// bodies and frees its buffers exactly once on each exit, and an array passed
/// to a callee that keeps nothing of it on the dies-inside exit stays with the
/// caller (local and parameter spellings). Before the fix the local kept a
/// static drop that the handing-back `return` never retracted: a double free at
/// every opt level, `String` elements included, and a return inside a loop was
/// not seen as an exit at all.
#[test]
fn interp_local_array_returned_on_some_exits_is_dropped_once() {
    assert_eq!(
        run(r#"struct R { id: i64, s: String }
impl Drop for R { fn drop(mut ref self) { println(f"  d{self.id}") } }
fn mkr(i: i64) -> R { return R { id: i, s: f"heap-string-longer-than-sso-{i}" } }
fn mka(k: i64) -> Array[R, 2] { return [mkr(k), mkr(k + 1)] }
fn consume(a: Array[R, 2]) { println(f"  c{a[0].id}") }
fn loc(c: bool) -> Array[R, 2] { let x: Array[R, 2] = [mkr(1), mkr(2)]; let m = x; if c { return m }; println("  dies"); return mka(8) }
fn tailif(c: bool) -> Array[R, 2] { let x: Array[R, 2] = mka(3); if c { x } else { mka(8) } }
fn inloop(c: bool) -> Array[R, 2] { let x: Array[R, 2] = mka(5); for i in 0..3 { if c and i == 1 { return x } }; println("  dies"); return mka(8) }
fn passed(c: bool) -> Array[R, 2] { let x: Array[R, 2] = mka(11); if c { return x }; consume(x); println("  dies"); return mka(8) }
fn ppass(a: Array[R, 2], c: bool) -> Array[R, 2] { if c { return a }; consume(a); println("  dies"); return mka(8) }
fn strs(c: bool) -> Array[String, 2] { let x: Array[String, 2] = [f"heap-string-longer-than-sso-p", f"heap-string-longer-than-sso-qq"]; for i in 0..2 { if c and i == 1 { return x } }; [f"heap-string-longer-than-sso-rrr", f"heap-string-longer-than-sso-ssss"] }
fn main() {
    println("loc-t");    { let b = loc(true); println(f"  y{b[0].id}"); }
    println("loc-f");    { let b = loc(false); println(f"  y{b[0].id}"); }
    println("tailif-t"); { let b = tailif(true); println(f"  y{b[1].id}"); }
    println("tailif-f"); { let b = tailif(false); println(f"  y{b[1].id}"); }
    println("inloop-t"); { let b = inloop(true); println(f"  y{b[0].id}"); }
    println("inloop-f"); { let b = inloop(false); println(f"  y{b[0].id}"); }
    println("passed-t"); { let b = passed(true); println(f"  y{b[0].id}"); }
    println("passed-f"); { let b = passed(false); println(f"  y{b[0].id}"); }
    println("ppass-t");  { let b = ppass(mka(20), true); println(f"  y{b[0].id}"); }
    println("ppass-f");  { let b = ppass(mka(20), false); println(f"  y{b[0].id}"); }
    println("str-t");    { let b = strs(true); println(f"  y{b[1].len()}"); }
    println("str-f");    { let b = strs(false); println(f"  y{b[1].len()}"); }
    println("end")
}"#),
        "loc-t\n  y1\n  d1\n  d2\nloc-f\n  d1\n  d2\n  dies\n  y8\n  d8\n  d9\ntailif-t\n  y4\n  d3\n  d4\ntailif-f\n  d3\n  d4\n  y9\n  d8\n  d9\ninloop-t\n  y5\n  d5\n  d6\ninloop-f\n  d5\n  d6\n  dies\n  y8\n  d8\n  d9\npassed-t\n  y11\n  d11\n  d12\npassed-f\n  c11\n  d11\n  d12\n  dies\n  y8\n  d8\n  d9\nppass-t\n  y20\n  d20\n  d21\nppass-f\n  c20\n  dies\n  d20\n  d21\n  y8\n  d8\n  d9\nstr-t\n  y30\nstr-f\n  y32\nend\n"
    );
}

/// B-2026-09-23-18 — a by-value parameter moved into a local through an `if`
/// or `match` arm (`let r = if c { a } else { mk() }`) and that local handed
/// back is owned once on each path: callee-side on the path where the arm does
/// not take it, by the result on the path where it does. Before the fix the
/// single leaf `r` read as "never returns the param", so the caller kept its
/// argument while `r` handed the same value back — both element bodies twice
/// under `--interp` and a double free on every compiled surface for an
/// `Array`, and an agreed double body for a struct.
#[test]
fn interp_param_moved_into_local_through_branch_is_dropped_once() {
    assert_eq!(
        run(r#"struct R { id: i64, s: String }
impl Drop for R { fn drop(mut ref self) { println(f"  d{self.id}") } }
struct H { v: i64 }
fn mkr(i: i64) -> R { return R { id: i, s: f"heap-string-longer-than-sso-{i}" } }
fn mka(k: i64) -> Array[R, 2] { return [mkr(k), mkr(k + 1)] }
fn letif(a: Array[R, 2], c: bool) -> Array[R, 2] { let r: Array[R, 2] = if c { a } else { mka(8) }; println("  mid"); r }
fn letmatch(a: Array[R, 2], c: bool) -> Array[R, 2] { let r: Array[R, 2] = match c { true => a, false => mka(8) }; println("  mid"); return r }
fn someret(a: Array[R, 2], c: bool, d: bool) -> Array[R, 2] { let r: Array[R, 2] = if c { a } else { mka(8) }; if d { return r }; println("  mid"); mka(5) }
fn sletif(a: R, c: bool) -> R { let r: R = if c { a } else { mkr(8) }; println("  mid"); r }
impl H { fn m(self, a: R, c: bool) -> R { let r: R = match c { true => a, false => mkr(8) }; println("  mid"); r } }
fn main() {
    println("letif-t");   { let b = letif(mka(1), true); println(f"  y{b[0].id}"); }
    println("letif-f");   { let b = letif(mka(1), false); println(f"  y{b[0].id}"); }
    println("match-t");   { let a: Array[R, 2] = mka(1); let b = letmatch(a, true); println(f"  y{b[1].id}"); }
    println("match-f");   { let a: Array[R, 2] = mka(1); let b = letmatch(a, false); println(f"  y{b[1].id}"); }
    println("some-tt");   { let b = someret(mka(1), true, true); println(f"  y{b[0].id}"); }
    println("some-tf");   { let b = someret(mka(1), true, false); println(f"  y{b[0].id}"); }
    println("struct-t");  { let a = mkr(1); let b = sletif(a, true); println(f"  y{b.id}"); }
    println("struct-f");  { let a = mkr(1); let b = sletif(a, false); println(f"  y{b.id}"); }
    println("method-t");  { let h = H { v: 0 }; let b = h.m(mkr(1), true); println(f"  y{b.id}"); }
    println("end")
}"#),
        "letif-t\n  mid\n  y1\n  d1\n  d2\nletif-f\n  mid\n  d1\n  d2\n  y8\n  d8\n  d9\nmatch-t\n  mid\n  y2\n  d1\n  d2\nmatch-f\n  mid\n  d1\n  d2\n  y9\n  d8\n  d9\nsome-tt\n  y1\n  d1\n  d2\nsome-tf\n  d1\n  d2\n  mid\n  y5\n  d5\n  d6\nstruct-t\n  mid\n  y1\n  d1\nstruct-f\n  mid\n  d1\n  y8\n  d8\nmethod-t\n  mid\n  y1\n  d1\nend\n"
    );
}

/// B-2026-09-23-19 — an INSTANCE METHOD's by-value `Array` param of user-`Drop`
/// elements, handed back on some exits, runs its element bodies exactly once
/// on the exit where it dies inside (`return` and tail spellings, owned and
/// `ref` receivers, a fresh-temp argument, a loop return, a pass to a callee
/// that keeps nothing, and a dying call after a handing-back one). The
/// interpreter's method frame adopted only struct and enum params, so it ran
/// none of them (`dies y8 d8 d9`) while every compiled surface ran both.
#[test]
fn interp_method_array_param_dying_on_some_exits_runs_its_bodies() {
    assert_eq!(
        run(r#"struct R { id: i64, s: String }
impl Drop for R { fn drop(mut ref self) { println(f"  d{self.id}") } }
fn mkr(i: i64) -> R { return R { id: i, s: f"heap-string-longer-than-sso-{i}" } }
fn mka(k: i64) -> Array[R, 2] { return [mkr(k), mkr(k + 1)] }
fn consume(a: Array[R, 2]) { println(f"  c{a[0].id}") }
struct H { k: i64 }
impl H {
    fn ret(ref self, a: Array[R, 2], c: bool) -> Array[R, 2] { if c { return a }; println("  dies"); mka(8) }
    fn tail(ref self, a: Array[R, 2], c: bool) -> Array[R, 2] { match c { true => a, false => mka(8) } }
    fn byval(self, a: Array[R, 2], c: bool) -> Array[R, 2] { for i in 0..2 { if c and i == 1 { return a } }; println("  dies"); mka(8) }
    fn pass(ref self, a: Array[R, 2], c: bool) -> Array[R, 2] { if c { return a }; consume(a); println("  dies"); mka(8) }
}
fn main() {
    let h = H { k: 1 };
    println("ret-t");  { let a: Array[R, 2] = mka(1); let b = h.ret(a, true); println(f"  y{b[0].id}"); }
    println("ret-f");  { let a: Array[R, 2] = mka(1); let b = h.ret(a, false); println(f"  y{b[0].id}"); }
    println("temp-f"); { let b = h.ret(mka(1), false); println(f"  y{b[0].id}"); }
    println("tail-f"); { let b = h.tail(mka(1), false); println(f"  y{b[1].id}"); }
    println("byval-f");  { let g = H { k: 2 }; let b = g.byval(mka(1), false); println(f"  y{b[0].id}"); }
    println("pass-f"); { let b = h.pass(mka(1), false); println(f"  y{b[0].id}"); }
    println("twice");  { let b1 = h.ret(mka(1), true); let b2 = h.ret(mka(3), false); println(f"  y{b1[0].id}{b2[0].id}"); }
    println("end")
}"#),
        "ret-t\n  y1\n  d1\n  d2\nret-f\n  dies\n  d1\n  d2\n  y8\n  d8\n  d9\ntemp-f\n  dies\n  d1\n  d2\n  y8\n  d8\n  d9\ntail-f\n  d1\n  d2\n  y9\n  d8\n  d9\nbyval-f\n  dies\n  d1\n  d2\n  y8\n  d8\n  d9\npass-f\n  c1\n  dies\n  d1\n  d2\n  y8\n  d8\n  d9\ntwice\n  dies\n  d3\n  d4\n  y18\n  d8\n  d9\n  d1\n  d2\nend\n"
    );
}

/// B-2026-09-23-20 — an `Array` / `Vec` bound out of an `Option` / `Result` by
/// a NON-block arm that only indexes it (`Some(a) => println(f"y{a[0].id}")`)
/// ran no element `Drop` body under `--interp`: the arm-end leftover gate
/// counted `a[0]` as a mention outside a field projection, and the leftover
/// walker had no `Array` arm. The JIT, `-O0` and `-O2` all print this output.
#[test]
fn interp_read_only_arm_over_array_payload_runs_its_bodies() {
    assert_eq!(
        run(r#"struct R { id: i64, s: String }
impl Drop for R { fn drop(mut ref self) { println(f"  d{self.id}") } }
fn mkr(i: i64) -> R { return R { id: i, s: f"heap-string-longer-than-sso-{i}" } }
fn mka(k: i64) -> Array[R, 2] { return [mkr(k), mkr(k + 1)] }
fn some_arr(k: i64) -> Option[Array[R, 2]] { let x = mka(k); return Some(x) }
fn consume(a: Array[R, 2]) { println(f"  c{a[0].id}") }
enum E { A(Array[R, 2]), B }
fn main() {
    println("lit");   { let b: Option[Array[R, 2]] = Some([mkr(1), mkr(2)]); match b { Some(a) => println(f"  y{a[0].id}"), None => println("  n") }; println("  after") }
    println("local"); { let x = mka(3); let b: Option[Array[R, 2]] = Some(x); match b { Some(a) => println(f"  y{a[1].id}"), None => println("  n") }; println("  after") }
    println("call");  { let b = some_arr(5); match b { Some(a) => println(f"  y{a[0].id}"), None => println("  n") }; println("  after") }
    println("temp");  { match some_arr(7) { Some(a) => println(f"  y{a[0].id}"), None => println("  n") }; println("  after") }
    println("vec");   { let b: Option[Vec[R]] = Some(vec![mkr(9), mkr(10)]); match b { Some(a) => println(f"  y{a[0].id}"), None => println("  n") }; println("  after") }
    println("move");  { let b = some_arr(11); match b { Some(a) => consume(a), None => println("  n") }; println("  after") }
    println("guard"); { let b = some_arr(13); match b { Some(a) if a[0].id > 100 => println("  big"), Some(a) => println(f"  y{a[1].id}"), None => println("  n") }; println("  after") }
    println("value"); { let b = some_arr(15); let v = match b { Some(a) => a[1].id, None => 0 }; println(f"  v{v}") }
    println("res");   { let r: Result[Array[R, 2], i64] = Ok(mka(17)); match r { Ok(a) => println(f"  y{a[0].id}"), Err(e) => println(f"  e{e}") }; println("  after") }
    println("enum");  { let e = E.A(mka(19)); match e { E.A(a) => println(f"  y{a[0].id}"), E.B => println("  b") }; println("  after") }
    println("none");  { let b: Option[Array[R, 2]] = None; match b { Some(a) => println(f"  y{a[0].id}"), None => println("  n") }; println("  after") }
    println("end")
}"#),
        "lit\n  y1\n  d1\n  d2\n  after\nlocal\n  y4\n  d3\n  d4\n  after\ncall\n  y5\n  d5\n  d6\n  after\ntemp\n  y7\n  d7\n  d8\n  after\nvec\n  y9\n  d9\n  d10\n  after\nmove\n  c11\n  d11\n  d12\n  after\nguard\n  y14\n  d13\n  d14\n  after\nvalue\n  d15\n  d16\n  v16\nres\n  y17\n  d17\n  d18\n  after\nenum\n  y19\n  d19\n  d20\n  after\nnone\n  n\n  after\nend\n"
    );
}

/// B-2026-09-23-23 — a SHADOWED owning `Array` local whose elements run a
/// user `Drop`, where every exit hands back the LAST generation: bare, as a
/// tail, three generations deep, wrapped in `Some`, and with an early
/// `return` of the same generation. Before the fix the returned generation's
/// memory drop stayed armed (a shadowed name was never "returned on every
/// exit"), so the caller freed its buffers a second time: `free(): double free
/// detected in tcache 2` on the JIT, `-O0` and `-O2`. The shadowed generation
/// must still run its bodies once, on every path.
#[test]
fn interp_shadowed_array_local_returned_is_dropped_once() {
    assert_eq!(
        run(r#"struct R { id: i64, s: String }
impl Drop for R { fn drop(mut ref self) { println(f"  d{self.id}") } }
fn mkr(i: i64) -> R { return R { id: i, s: f"heap-string-longer-than-sso-{i}" } }
fn mks(i: i64) -> String { return f"heap-string-longer-than-sso-{i}" }
fn last() -> Array[R, 2] { let x: Array[R, 2] = [mkr(1), mkr(2)]; let x: Array[R, 2] = [mkr(8), mkr(9)]; println("  dies"); return x }
fn tail() -> Array[R, 2] { let x: Array[R, 2] = [mkr(1), mkr(2)]; println(f"  a{x[0].id}"); let x: Array[R, 2] = [mkr(8), mkr(9)]; x }
fn three() -> Array[R, 2] { let x: Array[R, 2] = [mkr(1), mkr(2)]; let x: Array[R, 2] = [mkr(4), mkr(5)]; let x: Array[R, 2] = [mkr(8), mkr(9)]; println("  dies"); x }
fn wrap() -> Option[Array[R, 2]] { let x: Array[R, 2] = [mkr(1), mkr(2)]; let x: Array[R, 2] = [mkr(8), mkr(9)]; println("  dies"); return Some(x) }
fn early(c: bool) -> Array[R, 2] { let x: Array[R, 2] = [mkr(1), mkr(2)]; let x: Array[R, 2] = [mkr(8), mkr(9)]; if c { return x }; println("  dies"); x }
fn strs() -> Array[String, 2] { let x: Array[String, 2] = [mks(1), mks(2)]; let x: Array[String, 2] = [mks(8), mks(9)]; println("  dies"); x }
fn main() {
    println("last");    { let y = last(); println(f"  y{y[0].id}"); }
    println("tail");    { let y = tail(); println(f"  y{y[1].id}"); }
    println("three");   { let y = three(); println(f"  y{y[0].id}"); }
    println("wrap");    { let y = wrap(); match y { Some(a) => { println(f"  y{a[0].id}") }, None => println("  n") } }
    println("early-t"); { let y = early(true); println(f"  y{y[0].id}"); }
    println("early-f"); { let y = early(false); println(f"  y{y[0].id}"); }
    println("strs");    { let y = strs(); println(f"  y{y[0].len()}"); }
    println("end")
}"#),
        "last\n  dies\n  d1\n  d2\n  y8\n  d8\n  d9\ntail\n  a1\n  d1\n  d2\n  y9\n  d8\n  d9\nthree\n  dies\n  d4\n  d5\n  d1\n  d2\n  y8\n  d8\n  d9\nwrap\n  dies\n  d1\n  d2\n  y8\n  d8\n  d9\nearly-t\n  d1\n  d2\n  y8\n  d8\n  d9\nearly-f\n  dies\n  d1\n  d2\n  y8\n  d8\n  d9\nstrs\n  dies\n  y29\nend\n"
    );
}

/// B-2026-09-23-25 — a by-value `Array` param returned on SOME exits only,
/// for an element this frame owns outright (`String`, `Vec[i64]`) and for one
/// that runs a user `Drop`, with the other exit built from `vec![..]` /
/// `Vec[v; n]`. Before the fix the owned-element param kept its static
/// scope-exit memory drop, which the hand-back exit never retracted, so the
/// caller freed the same buffers again: `free(): double free detected in
/// tcache 2` on the JIT, `-O0` and `-O2`. And a `vec![..]` in the other exit
/// declined the per-path flag outright, so the `Drop` element lost its bodies
/// on the path where it dies inside the callee, on every surface.
#[test]
fn interp_conditional_array_param_handback_owns_once() {
    assert_eq!(
        run(r#"struct R { id: i64, v: Vec[i64] }
impl Drop for R { fn drop(mut ref self) { println(f"  d{self.id}") } }
fn mks(i: i64) -> String { return f"heap-string-longer-than-sso-{i}" }
fn strs(a: Array[String, 2], c: bool) -> Array[String, 2] { if c { a } else { [mks(8), mks(9)] } }
fn early(a: Array[String, 2], c: bool) -> Array[String, 2] { if c { return a } [mks(8), mks(9)] }
fn vecs(a: Array[Vec[i64], 2], c: bool) -> Array[Vec[i64], 2] { if c { a } else { [vec![8, 8], Vec[9; 4]] } }
fn rs(a: Array[R, 2], c: bool) -> Array[R, 2] { if c { a } else { [R { id: 8, v: vec![8] }, R { id: 9, v: vec![9, 9] }] } }
fn main() {
    for c in [true, false] {
        println(f"c={c}");
        { let y = strs([mks(1), mks(2)], c); println(f"  s{y[1].len()}"); }
        { let y = early([mks(1), mks(2)], c); println(f"  e{y[0].len()}"); }
        { let a: Array[String, 2] = [mks(1), mks(2)]; strs(a, c); println("  discarded"); }
        { let y = vecs([vec![1, 2, 3], vec![4]], c); println(f"  v{y[0].len()} {y[1].len()}"); }
        { let y = rs([R { id: 1, v: vec![1] }, R { id: 2, v: vec![2] }], c); println(f"  r{y[0].id}"); }
    }
    println("end")
}"#),
        "c=true\n  s29\n  e29\n  discarded\n  v3 1\n  r1\n  d1\n  d2\nc=false\n  s29\n  e29\n  discarded\n  v2 4\n  d1\n  d2\n  r8\n  d8\n  d9\nend\n"
    );
}

/// B-2026-09-23-26 — a by-value `Option[R]` / `Result[R, i64]` param whose
/// payload runs a user `Drop`, returned on some exits only: a tail `if`, a
/// `let`-bound `if`, a tail `match` with bare arms, and a `let`-bound `match`.
/// Before the fix the `let`-bound spellings kept the caller's argument armed
/// while the local handed it back (a segfault on every compiled surface, the
/// body twice under `--interp`), and the tail spellings ran no body at all on
/// the exit where the value died inside the callee, on every surface.
#[test]
fn interp_conditional_optres_param_handback_runs_one_body() {
    assert_eq!(
        run(r#"struct R { id: i64, s: String }
impl Drop for R { fn drop(mut ref self) { println(f"  d{self.id}") } }
fn mkr(i: i64) -> R { return R { id: i, s: f"heap-string-longer-than-sso-{i}" } }
fn tl(a: Option[R], c: bool) -> Option[R] { if c { a } else { None } }
fn lt(a: Option[R], c: bool) -> Option[R] { let r: Option[R] = if c { a } else { None }; println("  mid"); r }
fn mt(a: Option[R], c: bool) -> Option[R] { match c { true => a, false => None } }
fn ml(a: Option[R], c: bool) -> Option[R] { let r: Option[R] = match c { true => a, false => None }; println("  mid"); r }
fn rs(a: Result[R, i64], c: bool) -> Result[R, i64] { let r: Result[R, i64] = if c { a } else { Err(5) }; r }
fn show(o: Option[R]) { match o { Some(x) => println(f"  y{x.id}"), None => println("  none") } }
fn main() {
    for c in [true, false] {
        println(f"c={c}");
        { let a = Some(mkr(1)); let b = tl(a, c); show(b); }
        { let a = Some(mkr(2)); let b = lt(a, c); show(b); }
        { let a = Some(mkr(3)); let b = mt(a, c); show(b); }
        { let a = Some(mkr(4)); let b = ml(a, c); show(b); }
        { let a: Result[R, i64] = Ok(mkr(5)); let b = rs(a, c); match b { Ok(x) => println(f"  y{x.id}"), Err(e) => println(f"  e{e}") } }
    }
    { let b = lt(Some(mkr(9)), true); show(b); }
    println("end")
}"#),
        "c=true\n  y1\n  d1\n  mid\n  y2\n  d2\n  y3\n  d3\n  mid\n  y4\n  d4\n  y5\n  d5\nc=false\n  d1\n  none\n  mid\n  d2\n  none\n  d3\n  none\n  mid\n  d4\n  none\n  d5\n  e5\n  mid\n  y9\n  d9\nend\n"
    );
}
