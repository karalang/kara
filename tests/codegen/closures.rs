//! closures, captures, heap-env bindings, fn pointers -- fixtures for `tests/codegen.rs`.
//!
//! Split out of `tests/codegen.rs` on 2026-09-21. The TEST TARGET is
//! unchanged: this file is a module of that target, so
//! `cargo test --features llvm --test codegen` still runs everything
//! and CI needs no edit. Run this area alone with:
//!
//!     cargo test --features llvm --test codegen closures::
//!
//! New fixtures about closures, captures, heap-env bindings, fn pointers belong in this file.

use super::*;

/// Parse a snippet, run resolve+typecheck+lowering, then compile to LLVM IR.
/// B-2026-06-22-2 Slice 0 follow-up: the escaping-capturing-closure guard
/// also covers an explicit mid-body `return <capturing closure>` and a
/// capturing closure returned inside an aggregate literal — both were
/// silent miscompiles that the tail-only guard missed.
#[test]
fn escaping_capturing_closure_explicit_return_is_rejected() {
    let err = ir_result(
        "fn make(k: i64, c: bool) -> Fn(i64) -> i64 {\n\
                 if c { return |x| x + k; }\n\
                 |x| x - k\n\
             }\n\
             fn main() { let f = make(10i64, true); println(f\"{f(5i64)}\"); }\n",
    )
    .expect_err("explicit `return <capturing closure>` must be rejected");
    assert!(err.contains("E_ESCAPING_CLOSURE_NOT_YET"), "got: {err}");
}

/// B-2026-08-16-13 (message half) — the two sides of the argument-passing
/// distinction the refusal message used to conflate. It listed "passing it
/// as a call argument" as unsupported while its own workaround sentence
/// recommended the `Fn(..)`-param hand-off; measured, the BINDING form
/// builds and runs, and only the UNBOUND-call form is refused. One test
/// per side so a future epic slice that widens either moves exactly one
/// assertion, and the message's lists move with it (see the comment at the
/// refusal site).
#[test]
fn heap_env_binding_passed_to_fn_param_builds_and_runs() {
    assert_eq!(
        run_program(
            "fn min_len(n: i64) -> Fn(ref String) -> bool { |s| (s.len() as i64) >= n }\n\
                 fn use_it(f: Fn(ref String) -> bool) -> bool { let s = \"abcd\"; return f(s); }\n\
                 fn main() { let f = min_len(2); println(f\"{use_it(f)}\"); }\n",
        ),
        Some("true\n".to_string())
    );
}

#[test]
fn unbound_heap_env_call_as_argument_is_rejected() {
    let err = ir_result(
        "fn min_len(n: i64) -> Fn(ref String) -> bool { |s| (s.len() as i64) >= n }\n\
             fn use_it(f: Fn(ref String) -> bool) -> bool { let s = \"abcd\"; return f(s); }\n\
             fn main() { println(f\"{use_it(min_len(3))}\"); }\n",
    )
    .expect_err("an unbound producing call as an argument must be rejected");
    assert!(err.contains("E_ESCAPING_CLOSURE_NOT_YET"), "got: {err}");
    // The message must not re-grow the stale claim: the binding form it
    // used to forbid is the workaround it recommends.
    assert!(
        !err.contains("passing it as a call argument"),
        "the refusal list must not contradict the Fn-param workaround; got: {err}"
    );
}

#[test]
fn escaping_capturing_closure_in_struct_literal_is_rejected() {
    let err = ir_result(
        "struct H { f: Fn(i64) -> i64 }\n\
             fn make(k: i64) -> H { H { f: |x| x + k } }\n\
             fn main() { let h = make(10i64); println(f\"{(h.f)(5i64)}\"); }\n",
    )
    .expect_err("a capturing closure returned inside a struct literal must be rejected");
    assert!(err.contains("E_ESCAPING_CLOSURE_NOT_YET"), "got: {err}");
}

/// B-2026-06-22-2 residual close-out: a capturing closure stored into a
/// LOCAL aggregate that is then returned via an identifier
/// (`let h = H { f: |x| x+k }; h`) was the last silent miscompile the
/// return-position-only guard missed — it built and ran, printing garbage
/// (`0`) instead of `x+k`. The source-ordered `capturing_vars` builder now
/// marks `h`, so the `Identifier`-return arm fires.
#[test]
fn escaping_capturing_closure_local_struct_then_return_is_rejected() {
    let err = ir_result(
        "struct H { f: Fn(i64) -> i64 }\n\
             fn make(k: i64) -> H { let h = H { f: |x| x + k }; h }\n\
             fn main() { let r = make(10i64); println(f\"{(r.f)(5i64)}\"); }\n",
    )
    .expect_err("a capturing closure stored in a local struct then returned must be rejected");
    assert!(err.contains("E_ESCAPING_CLOSURE_NOT_YET"), "got: {err}");
}

/// Same residual through an identifier chain: the closure is bound to a
/// local, that local is stored in a struct bound to a second local, and the
/// second local is returned. Source-order processing propagates the
/// capturing mark `g` → `h`.
#[test]
fn escaping_capturing_closure_local_identifier_chain_is_rejected() {
    let err = ir_result(
        "struct H { f: Fn(i64) -> i64 }\n\
             fn make(k: i64) -> H { let g: Fn(i64) -> i64 = |x| x + k; let h = H { f: g }; h }\n\
             fn main() { let r = make(10i64); println(f\"{(r.f)(5i64)}\"); }\n",
    )
    .expect_err(
        "a capturing closure chained through locals into a returned struct must be rejected",
    );
    assert!(err.contains("E_ESCAPING_CLOSURE_NOT_YET"), "got: {err}");
}

/// Adversarial for the residual close-out: a NON-capturing closure stored
/// in a local aggregate then returned must STILL compile — the strengthened
/// `capturing_vars` builder must only mark *capturing* bindings, so it never
/// rejects this sound shape.
#[test]
fn non_capturing_closure_local_struct_then_return_still_ok() {
    assert!(
        ir_result(
            "struct H { f: Fn(i64) -> i64 }\n\
                 fn make() -> H { let h = H { f: |x| x * 2i64 }; h }\n"
        )
        .is_ok(),
        "non-capturing closure stored in a local struct then returned should compile"
    );
}

/// Adversarial: a NON-capturing closure in the same positions must still
/// compile (the guard fires only on captures).
#[test]
fn non_capturing_closure_explicit_return_and_struct_still_ok() {
    assert!(
        ir_result(
            "fn make(c: bool) -> Fn(i64) -> i64 { if c { return |x| x + 1i64; } |x| x - 1i64 }\n"
        )
        .is_ok(),
        "non-capturing explicit return should compile"
    );
    assert!(
        ir_result(
            "struct H { f: Fn(i64) -> i64 }\n\
                 fn make() -> H { H { f: |x| x * 2i64 } }\n"
        )
        .is_ok(),
        "non-capturing closure in a struct literal should compile"
    );
}

/// B-2026-06-22-2 residual close-out: a capturing closure stored in a local
/// struct then projected back out and returned (`let h = H { f: |x| x+k };
/// return h.f`) was the last *silent* miscompile the guard missed — it built
/// and ran, printing garbage (`-1`) instead of `x+k`. The `capturing_fields`
/// builder now records that `h.f` holds a capturing closure, so the
/// `FieldAccess` return arm fires.
#[test]
fn escaping_capturing_closure_field_projection_is_rejected() {
    let err = ir_result(
        "struct H { f: Fn(i64) -> i64 }\n\
             fn make(k: i64) -> Fn(i64) -> i64 { let h = H { f: |x| x + k }; return h.f; }\n\
             fn main() { let g = make(21i64); println(f\"{g(21i64)}\"); }\n",
    )
    .expect_err("projecting a captured closure field out of a local struct must be rejected");
    assert!(err.contains("E_ESCAPING_CLOSURE_NOT_YET"), "got: {err}");
}

/// Same projection reached as the bare function TAIL (no explicit `return`)
/// and through an identifier-chained field init — both must still reject.
#[test]
fn escaping_capturing_closure_field_projection_tail_and_chain_is_rejected() {
    let tail = ir_result(
        "struct H { f: Fn(i64) -> i64 }\n\
             fn make(k: i64) -> Fn(i64) -> i64 { let h = H { f: |x| x + k }; h.f }\n",
    )
    .expect_err("a capturing closure field projected as the tail must be rejected");
    assert!(tail.contains("E_ESCAPING_CLOSURE_NOT_YET"), "got: {tail}");
    let chain = ir_result(
            "struct H { f: Fn(i64) -> i64 }\n\
             fn make(k: i64) -> Fn(i64) -> i64 { let g: Fn(i64) -> i64 = |x| x + k; let h = H { f: g }; h.f }\n",
        )
        .expect_err("a chained capturing closure field projection must be rejected");
    assert!(chain.contains("E_ESCAPING_CLOSURE_NOT_YET"), "got: {chain}");
}

// ── B-2026-06-22-2: comprehensive store-escape guard ──
// A capturing closure STORED into a place that then escapes the frame —
// a collection (`v.push(clo)` / `v.insert(.., clo)`), an index slot
// (`v[i] = clo`), or a struct field (`h.f = clo`) — was a silent
// miscompile (built, ran garbage). The guard now marks the rooted local
// so the existing return-position check fires.

#[test]
fn escaping_capturing_closure_container_push_is_rejected() {
    // Annotated collection local.
    let ann = ir_result(
            "fn make(k: i64) -> Vec[Fn(i64) -> i64] { let v: Vec[Fn(i64) -> i64] = Vec.new(); v.push(|x: i64| x + k); return v; }\n",
        )
        .expect_err("a capturing closure pushed into a returned Vec must be rejected");
    assert!(ann.contains("E_ESCAPING_CLOSURE_NOT_YET"), "got: {ann}");
    // Un-annotated `Vec.new()` constructor — the collection is recognized
    // from the `Vec.new()` RHS.
    let noann = ir_result(
            "fn make(k: i64) -> Vec[Fn(i64) -> i64] { let v = Vec.new(); v.push(|x: i64| x + k); return v; }\n",
        )
        .expect_err("un-annotated Vec.new() then push of a capturing closure must be rejected");
    assert!(noann.contains("E_ESCAPING_CLOSURE_NOT_YET"), "got: {noann}");
}

#[test]
fn escaping_capturing_closure_container_insert_is_rejected() {
    let err = ir_result(
            "fn make(k: i64) -> Vec[Fn(i64) -> i64] { let v: Vec[Fn(i64) -> i64] = Vec.new(); v.insert(0i64, |x: i64| x + k); return v; }\n",
        )
        .expect_err("a capturing closure inserted into a returned Vec must be rejected");
    assert!(err.contains("E_ESCAPING_CLOSURE_NOT_YET"), "got: {err}");
}

#[test]
fn escaping_capturing_closure_index_store_is_rejected() {
    let err = ir_result(
            "fn make(k: i64) -> Vec[Fn(i64) -> i64] { let v: Vec[Fn(i64) -> i64] = [|x: i64| x * 2i64]; v[0] = |x: i64| x + k; return v; }\n",
        )
        .expect_err("a capturing closure index-stored into a returned Vec must be rejected");
    assert!(err.contains("E_ESCAPING_CLOSURE_NOT_YET"), "got: {err}");
}

#[test]
fn escaping_capturing_closure_field_store_is_rejected() {
    // Returning the whole struct after a capturing field-store.
    let whole = ir_result(
            "struct H { f: Fn(i64) -> i64 }\n\
             fn make(k: i64) -> H { let h = H { f: |x: i64| x * 2i64 }; h.f = |x: i64| x + k; return h; }\n",
        )
        .expect_err("field-storing a capturing closure then returning the struct must be rejected");
    assert!(whole.contains("E_ESCAPING_CLOSURE_NOT_YET"), "got: {whole}");
    // Returning just the field-stored projection.
    let proj = ir_result(
            "struct H { f: Fn(i64) -> i64 }\n\
             fn make(k: i64) -> Fn(i64) -> i64 { let h = H { f: |x: i64| x * 2i64 }; h.f = |x: i64| x + k; return h.f; }\n",
        )
        .expect_err("projecting a field-stored capturing closure must be rejected");
    assert!(proj.contains("E_ESCAPING_CLOSURE_NOT_YET"), "got: {proj}");
}

/// Heap-closure-env epic Slice 1 (B-2026-06-22-2): a function can now
/// RETURN a capturing closure with POD captures — its environment is
/// reference-counted on the heap, and the caller's binding frees it. The
/// demonstrated repro that was a silent miscompile (printed -1) before
/// Slice 0's guard, and a hard error under Slice 0, now runs and prints 42.
#[test]
fn heap_env_returned_capturing_closure_runs() {
    let out = run_program(
        "fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }\n\
             fn main() { let f = make(21i64); println(f\"{f(21i64)}\"); }\n",
    );
    assert_eq!(out.as_deref(), Some("42\n"));
}

/// A heap-env closure binding may be CALLED more than once; its RC env is
/// freed once at scope exit.
#[test]
fn heap_env_binding_called_multiple_times_runs() {
    let out = run_program(
            "fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }\n\
             fn main() { let f = make(20i64); let a = f(1i64); let b = f(2i64); println(f\"{a + b}\"); }\n",
        );
    assert_eq!(out.as_deref(), Some("43\n"));
}

/// Shared-ownership inc-on-copy (B-2026-06-22-2): a heap-env closure binding
/// may be COPIED to another binding (`let g = f`). The copy increments the
/// shared RC env's refcount; BOTH bindings stay callable and the env is
/// freed exactly once at scope exit. Was rejected by the Slice 1 misuse
/// guard; now supported.
#[test]
fn heap_env_binding_copied_runs() {
    let out = run_program(
            "fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }\n\
             fn main() { let f = make(20i64); let g = f; let a = f(1i64); let b = g(2i64); println(f\"{a + b}\"); }\n",
        );
    assert_eq!(out.as_deref(), Some("43\n"));
}

/// A copy-of-a-copy chain (`let g = f; let h = g`) is transitively all
/// heap-env bindings — each copy increments the one shared RC env, so three
/// owners free it exactly once.
#[test]
fn heap_env_binding_copy_chain_runs() {
    let out = run_program(
            "fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }\n\
             fn main() { let f = make(10i64); let g = f; let h = g; println(f\"{f(1i64) + g(2i64) + h(3i64)}\"); }\n",
        );
    assert_eq!(out.as_deref(), Some("36\n"));
}

/// Slice 2 (B-2026-06-22-2): an escaping closure may capture a heap
/// `String`/`Vec` value — the ownership pass promotes the read-only capture
/// to an `Own` move-into-env and codegen frees the env's buffer via a
/// per-closure env-drop fn at RC-zero. A captured owned PARAM is deep-copied
/// into the env (caller keeps its buffer); a captured LOCAL is moved. All
/// strings exceed the SSO inline limit so the heap path runs. Value-
/// correctness here; memory cleanliness in `tests/memory_sanitizer.rs`
/// (`asan_heap_env_*_capture_*`).
#[test]
fn heap_env_string_param_capture_runs() {
    let out = run_program(
            "fn make(p: String) -> Fn(i64) -> i64 { |n| p.len() + n }\n\
             fn main() { let name = String.from(\"a heap-backed string beyond the sso inline limit\"); let f = make(name); println(f\"{f(0i64)}\"); }\n",
        );
    assert_eq!(out.as_deref(), Some("48\n"));
}

#[test]
fn heap_env_vec_param_capture_runs() {
    let out = run_program(
            "fn make(v: Vec[i64]) -> Fn(i64) -> i64 { |n| v.len() + n }\n\
             fn main() { let mut xs = Vec.new(); xs.push(1i64); xs.push(2i64); xs.push(3i64); let f = make(xs); println(f\"{f(10i64)}\"); }\n",
        );
    assert_eq!(out.as_deref(), Some("13\n"));
}

#[test]
fn heap_env_local_string_capture_runs() {
    let out = run_program(
            "fn make() -> Fn(i64) -> i64 { let s = String.from(\"a heap-backed string beyond the sso inline limit\"); |n| s.len() + n }\n\
             fn main() { let f = make(); println(f\"{f(0i64)}\"); }\n",
        );
    assert_eq!(out.as_deref(), Some("48\n"));
}

#[test]
fn heap_env_local_vec_capture_runs() {
    let out = run_program(
            "fn make() -> Fn(i64) -> i64 { let mut v = Vec.new(); v.push(5i64); v.push(6i64); |n| v.len() + n }\n\
             fn main() { let f = make(); println(f\"{f(1i64)}\"); }\n",
        );
    assert_eq!(out.as_deref(), Some("3\n"));
}

#[test]
fn heap_env_mixed_pod_string_capture_runs() {
    let out = run_program(
            "fn make(p: String, base: i64) -> Fn(i64) -> i64 { |n| p.len() + base + n }\n\
             fn main() { let f = make(String.from(\"a heap-backed string beyond the sso inline limit\"), 100i64); println(f\"{f(5i64)}\"); }\n",
        );
    assert_eq!(out.as_deref(), Some("153\n"));
}

/// Slice 2 gate: a heap capture OUTSIDE the supported set (whole String /
/// Vec-of-POD) — a `Vec[String]` (heap element), a `Map`, or a `shared` — is
/// an honest `E_ESCAPING_CLOSURE_HEAP_CAPTURE_NOT_YET`, never a UAF / leak.
/// A `Vec[String]`'s elements would need per-element deep clone/drop the
/// shallow env clone/free can't provide.
#[test]
fn heap_env_unsupported_heap_capture_is_rejected() {
    let cases: &[(&str, &str)] = &[
            (
                "Vec[String] (heap element)",
                "fn make(v: Vec[String]) -> Fn(i64) -> i64 { |n| v.len() + n }\n\
                 fn main() { let mut xs = Vec.new(); xs.push(String.from(\"a heap-backed string beyond the sso inline limit\")); let f = make(xs); println(f\"{f(0i64)}\"); }\n",
            ),
            (
                "Map capture",
                "fn make(m: Map[i64, i64]) -> Fn(i64) -> i64 { |n| m.len() + n }\n\
                 fn main() { let mut mm = Map.new(); mm.insert(1i64, 2i64); let f = make(mm); println(f\"{f(0i64)}\"); }\n",
            ),
        ];
    for (label, src) in cases {
        let err = ir_result(src).expect_err(&format!(
            "unsupported heap capture must be rejected: {label}"
        ));
        assert!(
            err.contains("E_ESCAPING_CLOSURE_HEAP_CAPTURE_NOT_YET"),
            "{label}: wrong diagnostic: {err}"
        );
    }
}

/// Slice 1 misuse guard: every NOT-yet-supported use of a heap-env closure
/// binding (or an unbound `make(..)`) is an honest error, never a UAF /
/// double-free / leak. Each would otherwise corrupt or leak the single-owner
/// RC environment.
#[test]
fn heap_env_misuse_is_rejected() {
    let cases: &[(&str, &str)] = &[
        (
            "unbound make() leaks",
            "fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }\n\
                 fn main() { make(21i64); println(f\"x\"); }\n",
        ),
        (
            "directly returned unbound make()",
            "fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }\n\
                 fn relay(k: i64) -> Fn(i64) -> i64 { return make(k); }\n",
        ),
    ];
    for (label, src) in cases {
        let err = ir_result(src).expect_err(&format!("heap-env misuse must be rejected: {label}"));
        assert!(
            err.contains("E_ESCAPING_CLOSURE_NOT_YET"),
            "{label}: wrong diagnostic: {err}"
        );
    }
}

#[test]
fn heap_env_binding_returned_again_runs() {
    let out = run_program(
        "fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }\n\
             fn relay(k: i64) -> Fn(i64) -> i64 { let f = make(k); return f; }\n\
             fn main() { let r = relay(21i64); println(f\"{r(21i64)}\"); }\n",
    );
    assert_eq!(out.as_deref(), Some("42\n"));
}

/// The bare-identifier TAIL form of return-again (`{ let f = make(k); f }`)
/// goes through the tail-return move-out hub rather than the explicit-return
/// arm — both neutralize the source. Runs and frees once.
#[test]
fn heap_env_binding_returned_again_tail_runs() {
    let out = run_program(
        "fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }\n\
             fn relay(k: i64) -> Fn(i64) -> i64 { let f = make(k); f }\n\
             fn main() { let r = relay(20i64); println(f\"{r(22i64)}\"); }\n",
    );
    assert_eq!(out.as_deref(), Some("42\n"));
}

/// Relay-of-a-relay: the heap-env-returning set is a FIXPOINT, so a function
/// returning the result of another heap-env-returning function is itself
/// recognized. The one env box flows through both relays to the caller.
#[test]
fn heap_env_binding_relay_of_relay_runs() {
    let out = run_program(
        "fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }\n\
             fn relay1(k: i64) -> Fn(i64) -> i64 { let f = make(k); f }\n\
             fn relay2(k: i64) -> Fn(i64) -> i64 { let g = relay1(k); g }\n\
             fn main() { let r = relay2(20i64); println(f\"{r(22i64)}\"); }\n",
    );
    assert_eq!(out.as_deref(), Some("42\n"));
}

/// Copy-then-return in one function (`let f = make(k); let g = f; g`): the
/// copy increments the shared box (rc 2); returning `g` moves it out (g's
/// drop neutralized), `f`'s scope-exit drop decs to 1, the caller frees the
/// last ref — freed exactly once.
#[test]
fn heap_env_binding_copy_then_return_runs() {
    let out = run_program(
        "fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }\n\
             fn relay(k: i64) -> Fn(i64) -> i64 { let f = make(k); let g = f; g }\n\
             fn main() { let r = relay(20i64); println(f\"{r(22i64)}\"); }\n",
    );
    assert_eq!(out.as_deref(), Some("42\n"));
}

/// A BRANCH-BURIED return of a heap-env binding stays rejected — the
/// move-out neutralization is only wired for a top-level tail / `return`, so
/// detection and the misuse guard agree on the under-approximation (sound:
/// an honest error, never a miscompile).
#[test]
fn heap_env_binding_branch_buried_return_is_rejected() {
    let err = ir_result(
            "fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }\n\
             fn relay(c: bool, k: i64) -> Fn(i64) -> i64 { let f = make(k); if c { return f; } else { return f; } }\n",
        )
        .expect_err("branch-buried return of a heap-env binding must be rejected");
    assert!(
        err.contains("E_ESCAPING_CLOSURE_NOT_YET"),
        "wrong diagnostic: {err}"
    );
}

// ── Store-in-struct slice (B-2026-06-22-2) ──
// A FRESH heap-env closure may be STORED into a struct literal field
// (`let h = H { f: make(k) }`); the field is RC-dropped per-instance at the
// struct local's scope exit. The struct may be field-called and have its
// non-closure fields read, but must NOT escape (return / copy-out / store /
// pass). A BINDING source (`H { f: f }`) is now supported too: the store inc's
// the shared RC env so the source binding and the field co-own it.

/// `let h = H { f: make(k) }; (h.f)(x)` — a fresh heap-env closure stored in a
/// struct field, called through the field, freed exactly once at scope exit.
#[test]
fn heap_env_stored_in_struct_field_runs() {
    let out = run_program(
        "struct H { f: Fn(i64) -> i64 }\n\
             fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }\n\
             fn main() { let h = H { f: make(21i64) }; println(f\"{(h.f)(21i64)}\"); }\n",
    );
    assert_eq!(out.as_deref(), Some("42\n"));
}

/// The struct may carry non-closure fields alongside the heap-env field; a
/// read of a non-closure field (`h.n`) is allowed by the guard.
#[test]
fn heap_env_stored_in_struct_with_data_field_runs() {
    let out = run_program(
            "struct H { f: Fn(i64) -> i64, n: i64 }\n\
             fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }\n\
             fn main() { let h = H { f: make(20i64), n: 2i64 }; println(f\"{(h.f)(20i64) + h.n}\"); }\n",
        );
    assert_eq!(out.as_deref(), Some("42\n"));
}

/// Aggregate-escape slice (B-2026-06-22-2): a function may RETURN a struct
/// that OWNS a heap-env closure field (`fn build(k) -> H { let h = H { f:
/// make(k) }; return h }`). The env box MOVES OUT to the caller — `build`
/// neutralizes `h`'s field env slot on the returning path, so its scope-exit
/// `FreeClosureEnv` no-ops; `build` is registered as aggregate-returning so the
/// caller's `let r = build(..)` binding frees the field env. Freed exactly once.
/// Was rejected by the store-in-struct slice; now supported (explicit return).
#[test]
fn heap_env_struct_returned_runs() {
    let out = run_program(
        "struct H { f: Fn(i64) -> i64 }\n\
             fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }\n\
             fn build(k: i64) -> H { let h = H { f: make(k) }; return h; }\n\
             fn main() { let r = build(20i64); println(f\"{(r.f)(22i64)}\"); }\n",
    );
    assert_eq!(out.as_deref(), Some("42\n"));
}

/// The bare-identifier TAIL form of aggregate escape
/// (`{ let h = H { f: make(k) }; h }`) goes through the tail-return move-out
/// hub rather than the explicit-return arm — both neutralize the owner's field
/// env slots. Runs and frees once.
#[test]
fn heap_env_struct_returned_tail_runs() {
    let out = run_program(
        "struct H { f: Fn(i64) -> i64 }\n\
             fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }\n\
             fn build(k: i64) -> H { let h = H { f: make(k) }; h }\n\
             fn main() { let r = build(20i64); println(f\"{(r.f)(22i64)}\"); }\n",
    );
    assert_eq!(out.as_deref(), Some("42\n"));
}

/// The returned owner may carry a sibling non-closure field, read through the
/// caller's binding (`r.n`) alongside the field call (`(r.f)(x)`).
#[test]
fn heap_env_struct_returned_with_data_field_runs() {
    let out = run_program(
        "struct H { f: Fn(i64) -> i64, n: i64 }\n\
             fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }\n\
             fn build(k: i64) -> H { let h = H { f: make(k), n: 2i64 }; h }\n\
             fn main() { let r = build(20i64); println(f\"{(r.f)(20i64) + r.n}\"); }\n",
    );
    assert_eq!(out.as_deref(), Some("42\n"));
}

/// Relay-of-aggregate: the aggregate-returning set is a FIXPOINT, so a function
/// returning the result of another aggregate-returning function is itself
/// recognized. The one env box flows through both relays inside the struct to
/// the final caller, freed exactly once.
#[test]
fn heap_env_aggregate_relay_runs() {
    let out = run_program(
        "struct H { f: Fn(i64) -> i64 }\n\
             fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }\n\
             fn build(k: i64) -> H { let h = H { f: make(k) }; h }\n\
             fn relay(k: i64) -> H { let r = build(k); return r; }\n\
             fn main() { let r = relay(20i64); println(f\"{(r.f)(22i64)}\"); }\n",
    );
    assert_eq!(out.as_deref(), Some("42\n"));
}

/// Projecting the closure field OUT of the struct as a value
/// (`let g = h.f`) aliases the env out of the owner — rejected (only a
/// field-CALL `(h.f)(x)` is sanctioned).
#[test]
fn heap_env_struct_field_projection_is_rejected() {
    let err = ir_result(
        "struct H { f: Fn(i64) -> i64 }\n\
             fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }\n\
             fn main() { let h = H { f: make(21i64) }; let g = h.f; println(f\"{g(21i64)}\"); }\n",
    )
    .expect_err("projecting a heap-env struct field out must be rejected");
    assert!(err.contains("E_ESCAPING_CLOSURE_NOT_YET"), "got: {err}");
}

/// Binding-source slice (B-2026-06-22-2): a heap-env BINDING may now be
/// STORED into a struct field (`let h = H { f: f }`). Closures are
/// copy-semantics, so the source binding `f` stays usable AND the struct field
/// co-owns the env: codegen bumps the shared RC env's refcount at the store, so
/// both `f`'s scope-exit drop and `h`'s field drop fire and the box is freed
/// exactly once. Was rejected by the store-in-struct slice; now supported.
#[test]
fn heap_env_binding_stored_in_struct_field_runs() {
    let out = run_program(
        "struct H { f: Fn(i64) -> i64 }\n\
             fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }\n\
             fn main() { let f = make(20i64); let h = H { f: f }; \
              let a = f(1i64); let b = (h.f)(1i64); println(f\"{a + b}\"); }\n",
    );
    assert_eq!(out.as_deref(), Some("42\n"));
}

/// Binding source alongside a non-closure field, and through a COPY of the
/// binding (`let g = f; H { f: g }`): the copy and the store each inc the one
/// shared env, so the three owners (`f`, `g`, the field) free it exactly once.
#[test]
fn heap_env_binding_copy_stored_in_struct_with_data_field_runs() {
    let out = run_program(
        "struct H { f: Fn(i64) -> i64, n: i64 }\n\
             fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }\n\
             fn main() { let f = make(18i64); let g = f; let h = H { f: g, n: 2i64 }; \
              println(f\"{f(0i64) + (h.f)(4i64) + h.n}\"); }\n",
    );
    assert_eq!(out.as_deref(), Some("42\n"));
}

/// Composite soundness: store a binding into a struct field, THEN move the
/// source binding out via a tail return. The store inc's to rc 2; the tail
/// move-out neutralizes the source's drop and hands the box to the caller; the
/// owner struct's field drop decs to 1 at relay's scope exit, and the caller's
/// binding frees the last ref — freed exactly once across the move boundary.
#[test]
fn heap_env_binding_stored_then_source_returned_runs() {
    let out = run_program(
        "struct H { f: Fn(i64) -> i64 }\n\
             fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }\n\
             fn relay(k: i64) -> Fn(i64) -> i64 { let f = make(k); let h = H { f: f }; f }\n\
             fn main() { let r = relay(20i64); println(f\"{r(22i64)}\"); }\n",
    );
    assert_eq!(out.as_deref(), Some("42\n"));
}

/// Aggregate escape composes with a BINDING-source field: `build` co-owns the
/// env (store inc → rc 2), then moves the owner out (`return h` neutralizes the
/// field env slot). At `build`'s scope exit the source binding decs rc 2→1; the
/// caller's `r` field drop frees the last ref. Was rejected; now supported.
#[test]
fn heap_env_binding_owner_struct_returned_runs() {
    let out = run_program(
        "struct H { f: Fn(i64) -> i64 }\n\
             fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }\n\
             fn build(k: i64) -> H { let f = make(k); let h = H { f: f }; return h; }\n\
             fn main() { let r = build(20i64); println(f\"{(r.f)(22i64)}\"); }\n",
    );
    assert_eq!(out.as_deref(), Some("42\n"));
}

/// Passing the owning struct BY VALUE to a borrows-only callee (one that only
/// CALLS its closure field) is a BORROW — now supported (owner by-value arg-pass
/// slice below). `(h.f)(5) = 5 + 21 = 26`; the env is freed once at the caller's
/// scope exit. A callee that RETURNS or PROJECTS the owner is still rejected (see
/// the `_arg_to_returning_callee` / `_arg_to_projecting_callee` rejections).
#[test]
fn heap_env_struct_passed_by_value_to_borrows_only_callee_runs() {
    let out = run_program(
        "struct H { f: Fn(i64) -> i64 }\n\
             fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }\n\
             fn use_h(h: H) -> i64 { (h.f)(5i64) }\n\
             fn main() { let h = H { f: make(21i64) }; println(f\"{use_h(h)}\"); }\n",
    );
    assert_eq!(out.as_deref(), Some("26\n"));
}

// (A heap-env BINDING passed as an arg to a borrows-only aggregate BUILDER
// — `build2(f)` — now RUNS; see `heap_env_binding_passed_to_borrows_only_builder_runs`
// in the by-value arg-pass block below. A builder that RETAINS the arg is
// still rejected via the same borrows-only check.)

// ── Tuple-store slice (B-2026-06-22-2) ──
// A heap-env closure may be STORED in a tuple element (`let t = (make(k), n)`
// or a binding `(f, n)`), then tuple-index-CALLED (`(t.0)(x)`) and its
// non-closure elements read. The element env is RC-dropped per-INSTANCE via a
// `FreeClosureEnv` on the tuple element GEP. The owning tuple must NOT escape.

/// A FRESH heap-env closure stored in a tuple element, called through the
/// element, with a sibling non-closure element read — freed once at scope exit.
#[test]
fn heap_env_stored_in_tuple_runs() {
    let out = run_program(
        "fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }\n\
             fn main() { let t = (make(20i64), 2i64); println(f\"{(t.0)(20i64) + t.1}\"); }\n",
    );
    assert_eq!(out.as_deref(), Some("42\n"));
}

/// A heap-env BINDING stored in a tuple element co-owns the env (store inc →
/// rc 2): the source binding stays usable AND the tuple element drops it, so the
/// box is freed exactly once.
#[test]
fn heap_env_binding_stored_in_tuple_runs() {
    let out = run_program(
        "fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }\n\
             fn main() { let f = make(20i64); let t = (f, 1i64); \
              let a = f(1i64); let b = (t.0)(0i64); println(f\"{a + b + t.1}\"); }\n",
    );
    assert_eq!(out.as_deref(), Some("42\n"));
}

/// Two heap-env closures in one tuple, each called through its element — both
/// envs freed exactly once.
#[test]
fn heap_env_two_closures_in_tuple_runs() {
    let out = run_program(
        "fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }\n\
             fn main() { let t = (make(10i64), make(20i64)); \
              println(f\"{(t.0)(1i64) + (t.1)(11i64)}\"); }\n",
    );
    assert_eq!(out.as_deref(), Some("42\n"));
}

/// Returning the tuple that owns the heap-env element is an ESCAPE — the
/// container-escape slice MOVES the env boxes out to the caller (the source's
/// element env slots are neutralized; the caller's binding adopts the drops).
/// Was rejected by the tuple-store slice; now runs and frees once.
#[test]
fn heap_env_tuple_returned_runs() {
    let out = run_program(
        "fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }\n\
             fn build(k: i64) -> (Fn(i64) -> i64, i64) { let t = (make(k), 2i64); t }\n\
             fn main() { let r = build(20i64); println(f\"{(r.0)(20i64) + r.1}\"); }\n",
    );
    assert_eq!(out.as_deref(), Some("42\n"));
}

/// Projecting the closure element OUT of the tuple (`let g = t.0`) aliases the
/// env out of the owner — rejected (only a tuple-index CALL `(t.0)(x)` is ok).
#[test]
fn heap_env_tuple_elem_projection_is_rejected() {
    let err = ir_result(
        "fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }\n\
             fn main() { let t = (make(21i64), 0i64); let g = t.0; println(f\"{g(21i64)}\"); }\n",
    )
    .expect_err("projecting a heap-env tuple element out must be rejected");
    assert!(err.contains("E_ESCAPING_CLOSURE_NOT_YET"), "got: {err}");
}

// ── Array-store slice (B-2026-06-22-2) ──
// A heap-env closure may be STORED in a FIXED-SIZE array element
// (`let a: Array[Fn,N] = [make(k), ..]` or a binding `[f, ..]`), then
// array-index-CALLED (`(a[i])(x)`, constant OR dynamic index). The element env
// is RC-dropped per-INSTANCE via a `FreeClosureEnv` on the element GEP. Arrays
// are homogeneous, so an array-of-closures has no non-closure sibling read. The
// owning array must NOT escape. A bare `[..]` lowers to a Vec (deferred slice),
// so only the explicitly-typed `Array[T, N]` form is sanctioned here.

/// A FRESH heap-env closure stored in a one-element array, called through the
/// element — freed once at scope exit.
#[test]
fn heap_env_stored_in_array_runs() {
    let out = run_program(
        "fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }\n\
             fn main() { let a: Array[Fn(i64) -> i64, 1] = [make(20i64)]; \
              println(f\"{(a[0])(22i64)}\"); }\n",
    );
    assert_eq!(out.as_deref(), Some("42\n"));
}

/// Two FRESH heap-env closures in one array, each called through its element —
/// both envs freed exactly once.
#[test]
fn heap_env_two_closures_in_array_runs() {
    let out = run_program(
        "fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }\n\
             fn main() { let a: Array[Fn(i64) -> i64, 2] = [make(10i64), make(20i64)]; \
              println(f\"{(a[0])(1i64) + (a[1])(11i64)}\"); }\n",
    );
    assert_eq!(out.as_deref(), Some("42\n"));
}

/// A heap-env BINDING stored in an array element co-owns the env (store inc →
/// rc 2): the source binding stays usable AND the array element drops it, so the
/// box is freed exactly once.
#[test]
fn heap_env_binding_stored_in_array_runs() {
    let out = run_program(
        "fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }\n\
             fn main() { let f = make(20i64); let a: Array[Fn(i64) -> i64, 1] = [f]; \
              let x = f(1i64); let y = (a[0])(0i64); println(f\"{x + y + 1i64}\"); }\n",
    );
    assert_eq!(out.as_deref(), Some("42\n"));
}

/// An array element may be called through a DYNAMIC index `(a[i])(x)` — invoking
/// through the element doesn't move the env, so it is sanctioned like the
/// constant-index call.
#[test]
fn heap_env_array_dynamic_index_call_runs() {
    let out = run_program(
        "fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }\n\
             fn main() { let a: Array[Fn(i64) -> i64, 2] = [make(10i64), make(20i64)]; \
              let i = 1i64; println(f\"{(a[i])(22i64)}\"); }\n",
    );
    assert_eq!(out.as_deref(), Some("42\n"));
}

/// Returning the array that owns the heap-env element is an ESCAPE — the
/// container-escape slice moves the env boxes out to the caller (the array twin
/// of the tuple escape). Was rejected by the array-store slice; now runs.
#[test]
fn heap_env_array_returned_runs() {
    let out = run_program(
        "fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }\n\
             fn build(k: i64) -> Array[Fn(i64) -> i64, 1] { \
              let a: Array[Fn(i64) -> i64, 1] = [make(k)]; a }\n\
             fn main() { let r = build(20i64); println(f\"{(r[0])(22i64)}\"); }\n",
    );
    assert_eq!(out.as_deref(), Some("42\n"));
}

/// Projecting the closure element OUT of the array (`let g = a[0]`) aliases the
/// env out of the owner — rejected (only an array-index CALL `(a[0])(x)` is ok).
#[test]
fn heap_env_array_elem_projection_is_rejected() {
    let err = ir_result(
        "fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }\n\
             fn main() { let a: Array[Fn(i64) -> i64, 1] = [make(21i64)]; \
              let g = a[0]; println(f\"{g(21i64)}\"); }\n",
    )
    .expect_err("projecting a heap-env array element out must be rejected");
    assert!(err.contains("E_ESCAPING_CLOSURE_NOT_YET"), "got: {err}");
}

/// A DYNAMIC-index projection (`let g = a[i]`) can't be proven to land on a
/// non-closure element, so it is conservatively rejected (the homogeneous
/// array is all closures anyway).
#[test]
fn heap_env_array_dynamic_index_projection_is_rejected() {
    let err = ir_result(
        "fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }\n\
             fn main() { let a: Array[Fn(i64) -> i64, 2] = [make(21i64), make(22i64)]; \
              let i = 1i64; let g = a[i]; println(f\"{g(20i64)}\"); }\n",
    )
    .expect_err("a dynamic-index projection of a heap-env array element must be rejected");
    assert!(err.contains("E_ESCAPING_CLOSURE_NOT_YET"), "got: {err}");
}

// ── Vec-store slice (B-2026-06-22-2) ──
// A heap-env closure may be STORED in a `Vec[Fn]` element via `push`
// (`let v: Vec[Fn] = Vec.new(); v.push(make(k))` fresh, or `v.push(f)` for a
// heap-env binding), called back through the index (`(v[i])(x)`), and counted
// (`v.len()`). The element envs are RC-dropped by a DYNAMIC `0..len` drop loop
// at the Vec's scope exit — the dynamic-length analog of the array/tuple
// per-slot drops. The owning Vec must NOT escape, and a closure element can't
// be projected out. Only a fresh `Vec.new()`/`Vec.with_capacity` + `Vec[Fn]`
// annotation qualifies; a bare `[..]` lowers to a Vec but isn't the slice's
// entry shape.

/// A FRESH heap-env closure pushed into a `Vec[Fn]`, called through the index
/// — freed once at scope exit.
#[test]
fn heap_env_stored_in_vec_runs() {
    let out = run_program(
        "fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }\n\
             fn main() { let mut v: Vec[Fn(i64) -> i64] = Vec.new(); v.push(make(20i64)); \
              println(f\"{(v[0])(22i64)}\"); }\n",
    );
    assert_eq!(out.as_deref(), Some("42\n"));
}

/// A DYNAMIC count of pushes in a loop — the per-element drop loop frees all
/// `len` element envs (the dynamic-length case the array/tuple slices can't do).
#[test]
fn heap_env_vec_loop_push_runs() {
    let out = run_program(
        "fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }\n\
             fn main() {\n\
             let mut v: Vec[Fn(i64) -> i64] = Vec.new();\n\
             let mut i = 0i64;\n\
             while i < 5i64 { v.push(make(i)); i = i + 1i64; }\n\
             let mut acc = 0i64; let mut j = 0i64;\n\
             while j < v.len() { acc = acc + (v[j])(10i64); j = j + 1i64; }\n\
             println(f\"{acc}\");\n\
             }\n",
    );
    assert_eq!(out.as_deref(), Some("60\n"));
}

/// A heap-env BINDING pushed into a `Vec[Fn]` co-owns the env (push inc →
/// rc 2): the source binding stays usable AND the Vec element drops it, so the
/// box is freed exactly once. (Exercises the auto-par bail too: `let f =
/// make(..)` grouped with `let v = Vec.new()` falls back to sequential.)
#[test]
fn heap_env_binding_stored_in_vec_runs() {
    let out = run_program(
        "fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }\n\
             fn main() { let f = make(20i64); let mut v: Vec[Fn(i64) -> i64] = Vec.new(); \
              v.push(f); let a = f(1i64); let b = (v[0])(0i64); println(f\"{a + b + 1i64}\"); }\n",
    );
    assert_eq!(out.as_deref(), Some("42\n"));
}

// ── Vec-escape slice (B-2026-06-22-2) ──
// A function may RETURN a closure-owning `Vec[Fn]` (bare tail / `return v`). The
// callee moves the BUFFER out by value — its tail-return cap-zero suppresses its
// own dynamic per-element drop loop — and the caller's `let r = build(..)`
// binding ADOPTS that drop loop (the Vec twin of the tuple/array escape). The
// owning Vec must still not be projected from / popped at the caller.

/// Returning the Vec that owns the heap-env elements — was rejected by the
/// Vec-store slice; now runs (explicit `return v`).
#[test]
fn heap_env_vec_returned_runs() {
    let out = run_program(
        "fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }\n\
             fn build(k: i64) -> Vec[Fn(i64) -> i64] { \
              let mut v: Vec[Fn(i64) -> i64] = Vec.new(); v.push(make(k)); return v; }\n\
             fn main() { let r = build(22i64); println(f\"{(r[0])(20i64)}\"); }\n",
    );
    assert_eq!(out.as_deref(), Some("42\n"));
}

/// Bare-tail return of a closure-owning Vec, called through at the caller.
#[test]
fn heap_env_vec_returned_tail_runs() {
    let out = run_program(
        "fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }\n\
             fn build(k: i64) -> Vec[Fn(i64) -> i64] { \
              let mut v: Vec[Fn(i64) -> i64] = Vec.new(); v.push(make(k)); v }\n\
             fn main() { let r = build(20i64); println(f\"{(r[0])(22i64)}\"); }\n",
    );
    assert_eq!(out.as_deref(), Some("42\n"));
}

/// A DYNAMIC-length escaping Vec (loop-built) — the caller's adopted drop loop
/// frees all `len` element envs.
#[test]
fn heap_env_vec_loop_built_escape_runs() {
    let out = run_program(
            "fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }\n\
             fn build(n: i64) -> Vec[Fn(i64) -> i64] { \
              let mut v: Vec[Fn(i64) -> i64] = Vec.new(); let mut i = 0i64; \
              while i < n { v.push(make(i)); i = i + 1i64; } v }\n\
             fn main() { let r = build(4i64); let mut acc = 0i64; let mut j = 0i64; \
              while j < r.len() { acc = acc + (r[j])(10i64); j = j + 1i64; } println(f\"{acc}\"); }\n",
        );
    assert_eq!(out.as_deref(), Some("46\n"));
}

/// A RELAY re-returns a Vec built by another fn (`let r = build(k); r`) — the
/// detection fixpoint recognizes `relay` once `build` is known; the buffer flows
/// caller→caller, the dynamic drop loop adopted at the outermost binding.
#[test]
fn heap_env_vec_escape_relay_runs() {
    let out = run_program(
        "fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }\n\
             fn build(k: i64) -> Vec[Fn(i64) -> i64] { \
              let mut v: Vec[Fn(i64) -> i64] = Vec.new(); v.push(make(k)); v }\n\
             fn relay(k: i64) -> Vec[Fn(i64) -> i64] { let r = build(k); r }\n\
             fn main() { let q = relay(20i64); println(f\"{(q[0])(22i64)}\"); }\n",
    );
    assert_eq!(out.as_deref(), Some("42\n"));
}

/// Projecting a closure element out of the ADOPTED Vec at the caller
/// (`let g = r[0]`) still escapes the env — rejected (only call-through is ok).
#[test]
fn heap_env_vec_escape_caller_projection_is_rejected() {
    let err = ir_result(
        "fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }\n\
             fn build(k: i64) -> Vec[Fn(i64) -> i64] { \
              let mut v: Vec[Fn(i64) -> i64] = Vec.new(); v.push(make(k)); v }\n\
             fn main() { let r = build(21i64); let g = r[0]; println(f\"{g(21i64)}\"); }\n",
    )
    .expect_err("projecting an element out of an adopted heap-env Vec must be rejected");
    assert!(err.contains("E_ESCAPING_CLOSURE_NOT_YET"), "got: {err}");
}

/// Projecting a closure element OUT of the Vec (`let g = v[0]`) aliases the env
/// out of the owner — rejected (only an index CALL `(v[0])(x)` is ok).
#[test]
fn heap_env_vec_elem_projection_is_rejected() {
    let err = ir_result(
        "fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }\n\
             fn main() { let mut v: Vec[Fn(i64) -> i64] = Vec.new(); v.push(make(21i64)); \
              let g = v[0]; println(f\"{g(21i64)}\"); }\n",
    )
    .expect_err("projecting a heap-env Vec element out must be rejected");
    assert!(err.contains("E_ESCAPING_CLOSURE_NOT_YET"), "got: {err}");
}

/// A moving/aliasing method on the owner Vec (`v.pop()`) would hand out an
/// element without env accounting — rejected (only push / index-call / len are
/// sanctioned).
#[test]
fn heap_env_vec_pop_is_rejected() {
    let err = ir_result(
        "fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }\n\
             fn main() { let mut v: Vec[Fn(i64) -> i64] = Vec.new(); v.push(make(21i64)); \
              let g = v.pop(); println(\"x\"); }\n",
    )
    .expect_err("a moving method on a heap-env Vec owner must be rejected");
    assert!(err.contains("E_ESCAPING_CLOSURE_NOT_YET"), "got: {err}");
}

// ── Container-escape slice (B-2026-06-22-2) ──
// A function may RETURN a TUPLE or fixed-size ARRAY that owns heap-env closure
// elements (bare tail or top-level `return t`). The callee MOVES the env boxes
// out at the same refcount (its return neutralizes the owner's element env
// slots), and the caller's `let r = build(..)` binding ADOPTS a per-element
// `FreeClosureEnv` — freed exactly once at the caller. The by-value twin of the
// already-landed struct aggregate-escape; Vec escape stays rejected (its
// buffer-transfer + drop-loop relocation is a separate slice).

/// Explicit `return t;` of a tuple owner — the explicit-return move-out arm.
#[test]
fn heap_env_tuple_returned_explicit_runs() {
    let out = run_program(
        "fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }\n\
             fn build(k: i64) -> (Fn(i64) -> i64, i64) { let t = (make(k), 0i64); return t; }\n\
             fn main() { let r = build(22i64); println(f\"{(r.0)(20i64)}\"); }\n",
    );
    assert_eq!(out.as_deref(), Some("42\n"));
}

/// Two heap-env closures in an escaping tuple — both env boxes move out, each
/// freed once at the caller.
#[test]
fn heap_env_two_closure_tuple_escape_runs() {
    let out = run_program(
        "fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }\n\
             fn build(k: i64) -> (Fn(i64) -> i64, Fn(i64) -> i64) { \
              let t = (make(k), make(k + 1i64)); t }\n\
             fn main() { let r = build(10i64); println(f\"{(r.0)(1i64) + (r.1)(20i64)}\"); }\n",
    );
    assert_eq!(out.as_deref(), Some("42\n"));
}

/// A heap-env BINDING element in an escaping tuple (`let f = make(k); let t =
/// (f, ..)`): the store inc'd (rc 2), then the callee's source `f` drop (rc 1)
/// and the caller's adopted element drop (rc 0) free it exactly once.
#[test]
fn heap_env_binding_element_tuple_escape_runs() {
    let out = run_program(
        "fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }\n\
             fn build(k: i64) -> (Fn(i64) -> i64, i64) { let f = make(k); let t = (f, 0i64); t }\n\
             fn main() { let r = build(22i64); println(f\"{(r.0)(20i64)}\"); }\n",
    );
    assert_eq!(out.as_deref(), Some("42\n"));
}

/// A RELAY re-returns a container built by another fn (`let r = build(k); r`) —
/// the detection fixpoint recognizes `relay` once `build` is known, and the env
/// boxes flow caller→caller, freed once at the outermost binding.
#[test]
fn heap_env_tuple_escape_relay_runs() {
    let out = run_program(
        "fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }\n\
             fn build(k: i64) -> (Fn(i64) -> i64, i64) { let t = (make(k), 0i64); t }\n\
             fn relay(k: i64) -> (Fn(i64) -> i64, i64) { let r = build(k); r }\n\
             fn main() { let q = relay(21i64); println(f\"{(q.0)(21i64)}\"); }\n",
    );
    assert_eq!(out.as_deref(), Some("42\n"));
}

// ── By-value arg-pass slice (B-2026-06-22-2) ──
// A heap-env closure BINDING may be passed BY VALUE to a free function whose
// matching parameter is BORROWS-ONLY — one that only CALLS the closure and
// never returns / stores / re-binds / captures it. The callee borrows the
// shared RC env (no inc), the CALLER retains sole ownership and RC-drops it
// once at scope exit. A callee that retains the param (returns / stores it)
// is NOT borrows-only and the arg-pass stays rejected.

/// The canonical borrow: `apply(f, x)` where `apply(g, x) { g(x) }` only
/// calls `g`. `f` is still usable after the borrow. No inc / move-out — the
/// box is freed exactly once at `f`'s scope exit.
#[test]
fn heap_env_binding_passed_to_borrows_only_fn_runs() {
    let out = run_program(
        "fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }\n\
             fn apply(g: Fn(i64) -> i64, x: i64) -> i64 { g(x) }\n\
             fn apply_twice(g: Fn(i64) -> i64, x: i64) -> i64 { g(g(x)) }\n\
             fn main() { let f = make(10i64); \
              let a = apply(f, 5i64); let b = apply_twice(f, 1i64); let c = f(100i64); \
              println(f\"{a + b + c}\"); }\n",
    );
    assert_eq!(out.as_deref(), Some("146\n"));
}

/// A COPY of a heap-env binding (`let g = f`) is itself a heap-env binding and
/// may likewise be passed by value to a borrows-only callee.
#[test]
fn heap_env_copy_passed_to_borrows_only_fn_runs() {
    let out = run_program(
        "fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }\n\
             fn apply(g: Fn(i64) -> i64, x: i64) -> i64 { g(x) }\n\
             fn main() { let f = make(20i64); let g = f; let a = apply(g, 10i64); \
              println(f\"{a + f(1i64)}\"); }\n",
    );
    assert_eq!(out.as_deref(), Some("51\n"));
}

/// Borrow then RETURN the same binding: `apply(f, ..)` borrows `f`, then the
/// fn returns `f` (move-out to caller). The borrow leaves ownership untouched,
/// so the move-out still transfers the single box; freed once at the caller.
#[test]
fn heap_env_binding_borrowed_then_returned_runs() {
    let out = run_program(
        "fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }\n\
             fn apply(g: Fn(i64) -> i64, x: i64) -> i64 { g(x) }\n\
             fn outer(k: i64) -> Fn(i64) -> i64 { let f = make(k); let _a = apply(f, 1i64); f }\n\
             fn main() { let h = outer(10i64); println(f\"{h(1i64)}\"); }\n",
    );
    assert_eq!(out.as_deref(), Some("11\n"));
}

/// A borrows-only callee may call the param inside a loop / branch (the
/// borrow-call sanction descends into nested blocks). `g(0)+g(1)+g(2)+g(3)`
/// with `g = |x| x + 5` = 5+6+7+8 = 26.
#[test]
fn heap_env_binding_loop_borrowed_runs() {
    let out = run_program(
        "fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }\n\
             fn sumcalls(g: Fn(i64) -> i64, n: i64) -> i64 { \
              let mut s = 0i64; let mut i = 0i64; \
              while i < n { s = s + g(i); i = i + 1i64; } s }\n\
             fn main() { let f = make(5i64); println(f\"{sumcalls(f, 4i64)}\"); }\n",
    );
    assert_eq!(out.as_deref(), Some("26\n"));
}

/// A borrows-only callee that ALSO returns an aggregate owner: `build2` makes
/// its OWN closure for the returned struct and merely BORROW-CALLS the passed
/// `g`. The arg-pass is sanctioned through the aggregate-owner Call branch
/// (which now walks the whole call). `(r.f)(4) = 4 + 5 = 9`; `f`'s box and
/// `r.f`'s box are distinct and each freed once. (Was rejected pre-slice.)
#[test]
fn heap_env_binding_passed_to_borrows_only_builder_runs() {
    let out = run_program(
        "struct H { f: Fn(i64) -> i64 }\n\
             fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }\n\
             fn build2(g: Fn(i64) -> i64) -> H { let local = make(5i64); let h = H { f: local }; \
              let _u = g(0i64); h }\n\
             fn main() { let f = make(3i64); let r = build2(f); println(f\"{(r.f)(4i64)}\"); }\n",
    );
    assert_eq!(out.as_deref(), Some("9\n"));
}

/// A callee that RETURNS its closure param retains the env past the call, so
/// it is NOT borrows-only — passing a heap-env binding to it stays rejected
/// (the env would outlive / be double-freed by the caller's owner).
#[test]
fn heap_env_arg_to_returning_callee_is_rejected() {
    let err = ir_result(
        "fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }\n\
             fn relay(g: Fn(i64) -> i64) -> Fn(i64) -> i64 { g }\n\
             fn main() { let f = make(3i64); let r = relay(f); println(f\"{r(1i64)}\"); }\n",
    )
    .expect_err("passing a heap-env binding to a param-returning callee must be rejected");
    assert!(err.contains("E_ESCAPING_CLOSURE_NOT_YET"), "got: {err}");
}

/// A callee that STORES its closure param into a returned collection retains
/// the env, so it is NOT borrows-only — the arg-pass stays rejected.
#[test]
fn heap_env_arg_to_storing_callee_is_rejected() {
    let err = ir_result(
        "fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }\n\
             fn keep(g: Fn(i64) -> i64) -> Vec[Fn(i64) -> i64] { \
              let mut v: Vec[Fn(i64) -> i64] = Vec.new(); v.push(g); v }\n\
             fn main() { let f = make(3i64); let r = keep(f); println(f\"{(r[0])(1i64)}\"); }\n",
    )
    .expect_err("passing a heap-env binding to a param-storing callee must be rejected");
    assert!(err.contains("E_ESCAPING_CLOSURE_NOT_YET"), "got: {err}");
}

// ── Owner by-value arg-pass slice (B-2026-06-22-2) ──
// A heap-env STRUCT OWNER (`let a = H { f: make(k) }`) passed BY VALUE to a
// borrows-only callee — one that only CALLS the owner's closure field(s) via
// `(h.f)(x)` and never returns / projects / stores / re-binds the owner or its
// fields — is a pure BORROW: the callee touches the shared RC env but never
// frees it (a param gets no Fn-field `FreeClosureEnv`), and the CALLER retains
// sole ownership and RC-drops the env once at scope exit (no inc, no move-out —
// a call arg is not a return move-out, so the owner's env slot is not
// neutralized). The owner stays a live, usable owner after the call. Tuple /
// array / Vec owner args are a later slice and stay rejected.

/// The canonical owner borrow: `use_it(h) { (h.f)(5) }` only calls the field.
/// `(h.f)(5) = 5 + 10 = 15`; the env box is freed exactly once at `a`'s scope exit.
#[test]
fn heap_env_struct_owner_passed_by_value_runs() {
    let out = run_program(
        "struct H { f: Fn(i64) -> i64 }\n\
             fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }\n\
             fn use_it(h: H) -> i64 { (h.f)(5i64) }\n\
             fn main() { let a = H { f: make(10i64) }; println(f\"{use_it(a)}\"); }\n",
    );
    assert_eq!(out.as_deref(), Some("15\n"));
}

/// A two-closure owner borrowed — both fields are called; each env is freed once.
/// `(h.f)(1) + (h.g)(2) = 11 + 22 = 33`.
#[test]
fn heap_env_struct_owner_arg_two_closures_runs() {
    let out = run_program(
        "struct H { f: Fn(i64) -> i64, g: Fn(i64) -> i64 }\n\
             fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }\n\
             fn use_it(h: H) -> i64 { (h.f)(1i64) + (h.g)(2i64) }\n\
             fn main() { let a = H { f: make(10i64), g: make(20i64) }; \
              println(f\"{use_it(a)}\"); }\n",
    );
    assert_eq!(out.as_deref(), Some("33\n"));
}

/// A sibling POD field rides along; the borrows-only callee ignores it and only
/// calls the closure. `(h.f)(5) = 15`.
#[test]
fn heap_env_struct_owner_arg_pod_sibling_runs() {
    let out = run_program(
        "struct H { f: Fn(i64) -> i64, count: i64 }\n\
             fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }\n\
             fn use_it(h: H) -> i64 { (h.f)(5i64) }\n\
             fn main() { let a = H { f: make(10i64), count: 7i64 }; \
              println(f\"{use_it(a)}\"); }\n",
    );
    assert_eq!(out.as_deref(), Some("15\n"));
}

/// The owner stays usable AFTER the borrow call (a borrow leaves ownership
/// untouched): `use_it(a)` then `(a.f)(1)`. `15 + 11 = 26`; freed once.
#[test]
fn heap_env_struct_owner_arg_then_reused_runs() {
    let out = run_program(
        "struct H { f: Fn(i64) -> i64 }\n\
             fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }\n\
             fn use_it(h: H) -> i64 { (h.f)(5i64) }\n\
             fn main() { let a = H { f: make(10i64) }; let r = use_it(a); \
              println(f\"{r + (a.f)(1i64)}\"); }\n",
    );
    assert_eq!(out.as_deref(), Some("26\n"));
}

/// A callee that RETURNS the owner (`keep(h: H) -> H { h }`) retains the env
/// past the call, so it is NOT borrows-only — the owner arg-pass stays rejected.
#[test]
fn heap_env_struct_owner_arg_to_returning_callee_is_rejected() {
    let err = ir_result(
        "struct H { f: Fn(i64) -> i64 }\n\
             fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }\n\
             fn keep(h: H) -> H { h }\n\
             fn main() { let a = H { f: make(10i64) }; let b = keep(a); \
              println(f\"{(b.f)(5i64)}\"); }\n",
    )
    .expect_err("passing an owner to a param-returning callee must be rejected");
    assert!(err.contains("E_ESCAPING_CLOSURE_NOT_YET"), "got: {err}");
}

/// A callee that PROJECTS the closure field out (`steal(h) -> Fn { h.f }`)
/// escapes the env, so it is NOT borrows-only — the arg-pass stays rejected
/// (only the CALL form `(h.f)(x)` is a borrow; `h.f` in value position escapes).
#[test]
fn heap_env_struct_owner_arg_to_projecting_callee_is_rejected() {
    let err = ir_result(
        "struct H { f: Fn(i64) -> i64 }\n\
             fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }\n\
             fn steal(h: H) -> Fn(i64) -> i64 { h.f }\n\
             fn main() { let a = H { f: make(10i64) }; let g = steal(a); \
              println(f\"{g(5i64)}\"); }\n",
    )
    .expect_err("passing an owner to a field-projecting callee must be rejected");
    assert!(err.contains("E_ESCAPING_CLOSURE_NOT_YET"), "got: {err}");
}

// ── Container owner by-value arg-pass (B-2026-06-22-2) ──
// Extends the struct owner arg-pass borrow to TUPLE / array / `Vec[Fn]`
// owners. A container owner passed BY VALUE to a borrows-only callee — one
// that only CALLS its closure element(s) via `(t.0)(x)` / `(a[i])(x)` /
// `(v[i])(x)` and never returns / projects / stores / re-binds it — is a pure
// BORROW: the callee receives a shallow copy of the container header pointing
// at the SAME RC env box(es), reads through it, and never frees it (a param
// is not a let-bound owner, so it gets no FreeClosureEnv / Vec drop loop); the
// caller retains sole ownership and RC-drops each env once at scope exit (no
// inc, no move-out — a call arg is not a return move-out, so the owner's env
// slot is never neutralized). The owner stays a live, usable owner after the
// call. Guard-only (zero codegen emission). Crucially this holds for `Vec[Fn]`
// too — by-value arg-pass passes the Vec header by value and does NOT zero the
// caller's cap (unlike `let w = v`, which is a move), so it is a borrow, not a
// move. Field / element REASSIGNMENT is the remaining piece and stays rejected.

/// The canonical TUPLE owner borrow: `use_t(t) { (t.0)(5) }` only calls the
/// element. `(t.0)(5) = 5 + 10 = 15`; the env box is freed once at `a`'s exit.
#[test]
fn heap_env_tuple_owner_passed_by_value_runs() {
    let out = run_program(
        "fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }\n\
             fn use_t(t: (Fn(i64) -> i64, i64)) -> i64 { (t.0)(5i64) }\n\
             fn main() { let a = (make(10i64), 0i64); println(f\"{use_t(a)}\"); }\n",
    );
    assert_eq!(out.as_deref(), Some("15\n"));
}

/// The TUPLE owner stays usable AFTER the borrow call: `use_t(a)` then
/// `(a.0)(1)`. `15 + 11 = 26`; the shared env is freed once.
#[test]
fn heap_env_tuple_owner_arg_then_reused_runs() {
    let out = run_program(
        "fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }\n\
             fn use_t(t: (Fn(i64) -> i64, i64)) -> i64 { (t.0)(5i64) }\n\
             fn main() { let a = (make(10i64), 0i64); let r = use_t(a); \
              println(f\"{r + (a.0)(1i64)}\"); }\n",
    );
    assert_eq!(out.as_deref(), Some("26\n"));
}

/// The ARRAY owner borrow: `use_a(a) { (a[0])(5) }`. `(a[0])(5) = 15`.
#[test]
fn heap_env_array_owner_passed_by_value_runs() {
    let out = run_program(
        "fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }\n\
             fn use_a(a: Array[Fn(i64) -> i64, 1]) -> i64 { (a[0])(5i64) }\n\
             fn main() { let a: Array[Fn(i64) -> i64, 1] = [make(10i64)]; \
              println(f\"{use_a(a)}\"); }\n",
    );
    assert_eq!(out.as_deref(), Some("15\n"));
}

/// A two-element `Fn` ARRAY borrowed — the callee calls both elements; each
/// shared env is freed once at the caller's scope exit. The owner is reused
/// after the call. `(a[0])(1)+(a[1])(1) = 11+21 = 32` in the callee, then
/// `(a[0])(0)=10` after → `32 + 10 = 42`.
#[test]
fn heap_env_array_owner_arg_multi_then_reused_runs() {
    let out = run_program(
        "fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }\n\
             fn use_a(a: Array[Fn(i64) -> i64, 2]) -> i64 { (a[0])(1i64) + (a[1])(1i64) }\n\
             fn main() { let a: Array[Fn(i64) -> i64, 2] = [make(10i64), make(20i64)]; \
              let r = use_a(a); println(f\"{r + (a[0])(0i64)}\"); }\n",
    );
    assert_eq!(out.as_deref(), Some("42\n"));
}

/// The `Vec[Fn]` owner borrow — the critical case: by-value arg-pass passes the
/// Vec header by value WITHOUT zeroing the caller's cap (unlike `let w = v`,
/// a move), so it is a borrow. The callee reads `(v[0])(5)=15` and frees
/// nothing; the caller's drop loop frees the one element env once.
#[test]
fn heap_env_vec_owner_passed_by_value_runs() {
    let out = run_program(
        "fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }\n\
             fn use_v(v: Vec[Fn(i64) -> i64]) -> i64 { (v[0])(5i64) }\n\
             fn main() { let mut v: Vec[Fn(i64) -> i64] = Vec.new(); v.push(make(10i64)); \
              println(f\"{use_v(v)}\"); }\n",
    );
    assert_eq!(out.as_deref(), Some("15\n"));
}

/// The `Vec[Fn]` owner stays usable AFTER the borrow call (a borrow does not
/// move it / zero its cap): `use_v(v)` then `(v[0])(1)`. Multi-element to
/// exercise the caller's dynamic drop loop. `(v[0])(1)+(v[1])(1) = 11+21 = 32`
/// in the callee, `(v[0])(0)=10` after → `42`.
#[test]
fn heap_env_vec_owner_arg_multi_then_reused_runs() {
    let out = run_program(
        "fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }\n\
             fn use_v(v: Vec[Fn(i64) -> i64]) -> i64 { (v[0])(1i64) + (v[1])(1i64) }\n\
             fn main() { let mut v: Vec[Fn(i64) -> i64] = Vec.new(); v.push(make(10i64)); \
              v.push(make(20i64)); let r = use_v(v); println(f\"{r + (v[0])(0i64)}\"); }\n",
    );
    assert_eq!(out.as_deref(), Some("42\n"));
}

/// A TUPLE owner with a sibling HEAP String element rides along; the
/// borrows-only callee ignores it and only calls the closure. The owned
/// String element is passed by value (its buffer shared for the call); freed
/// once. `(t.0)(5) = 15`.
#[test]
fn heap_env_tuple_owner_arg_string_sibling_runs() {
    let out = run_program(
        "fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }\n\
             fn use_t(t: (Fn(i64) -> i64, String)) -> i64 { (t.0)(5i64) }\n\
             fn main() { let a = (make(10i64), \"a long enough heap string payload\"); \
              println(f\"{use_t(a)}\"); }\n",
    );
    assert_eq!(out.as_deref(), Some("15\n"));
}

/// A callee that RETURNS the tuple owner (`keep(t) -> (..) { t }`) retains the
/// env past the call, so it is NOT borrows-only — the arg-pass stays rejected.
#[test]
fn heap_env_tuple_owner_arg_to_returning_callee_is_rejected() {
    let err = ir_result(
        "fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }\n\
             fn keep(t: (Fn(i64) -> i64, i64)) -> (Fn(i64) -> i64, i64) { t }\n\
             fn main() { let a = (make(10i64), 0i64); let b = keep(a); \
              println(f\"{(b.0)(5i64)}\"); }\n",
    )
    .expect_err("passing a tuple owner to a param-returning callee must be rejected");
    assert!(err.contains("E_ESCAPING_CLOSURE_NOT_YET"), "got: {err}");
}

/// A callee that PROJECTS the closure element out (`steal(t) -> Fn { t.0 }`)
/// escapes the env, so it is NOT borrows-only — rejected (only the CALL form
/// `(t.0)(x)` is a borrow; `t.0` in value position escapes).
#[test]
fn heap_env_tuple_owner_arg_to_projecting_callee_is_rejected() {
    let err = ir_result(
        "fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }\n\
             fn steal(t: (Fn(i64) -> i64, i64)) -> Fn(i64) -> i64 { t.0 }\n\
             fn main() { let a = (make(10i64), 0i64); let g = steal(a); \
              println(f\"{g(5i64)}\"); }\n",
    )
    .expect_err("passing a tuple owner to an element-projecting callee must be rejected");
    assert!(err.contains("E_ESCAPING_CLOSURE_NOT_YET"), "got: {err}");
}

/// The `Vec[Fn]` twin of the projecting-callee rejection: `steal(v) -> Fn
/// { v[0] }` moves the element env out of the borrowed Vec — not borrows-only.
#[test]
fn heap_env_vec_owner_arg_to_projecting_callee_is_rejected() {
    let err = ir_result(
        "fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }\n\
             fn steal(v: Vec[Fn(i64) -> i64]) -> Fn(i64) -> i64 { v[0] }\n\
             fn main() { let mut v: Vec[Fn(i64) -> i64] = Vec.new(); v.push(make(10i64)); \
              let g = steal(v); println(f\"{g(5i64)}\"); }\n",
    )
    .expect_err("passing a Vec owner to an element-projecting callee must be rejected");
    assert!(err.contains("E_ESCAPING_CLOSURE_NOT_YET"), "got: {err}");
}

// ── Heap-env closure binding REASSIGNMENT (B-2026-06-22-2) ──
// `g = make(j)` (fresh env, a MOVE) or `g = f` (binding source, the SHARED
// env, a COPY) where `g` is a heap-env closure binding. Codegen drops `g`'s
// CURRENT env (RC setter rule: inc new → store → release old), stores the new
// fat pointer, and incs the new env on a binding copy — so each env is freed
// EXACTLY once and (on a copy) the source `f` stays a live co-owner. Works at
// the top level, nested in a branch / loop (the drop-old fires once per
// execution), and composes with the return-of-binding escape. Field / element
// reassignment (`r.f = g`, `v[i] = make(j)`) is a later slice and stays
// rejected.

/// `g = f` is a COPY: `g` now invokes `f`'s closure (`g(5) = 15`) and `f`
/// stays usable (`f(1) = 11`) — the shared env is inc'd then freed once. 26.
#[test]
fn heap_env_binding_reassigned_copy_runs() {
    let out = run_program(
        "fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }\n\
             fn main() { let f = make(10i64); let mut g = make(20i64); g = f; \
              println(f\"{g(5i64) + f(1i64)}\"); }\n",
    );
    assert_eq!(out.as_deref(), Some("26\n"));
}

/// `g = make(j)` is a MOVE to a fresh env: `g`'s old env is dropped, the new
/// one freed once at scope exit. `g(5) = 5 + 30 = 35`.
#[test]
fn heap_env_binding_reassigned_to_fresh_runs() {
    let out = run_program(
        "fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }\n\
             fn main() { let mut g = make(20i64); g = make(30i64); \
              println(f\"{g(5i64)}\"); }\n",
    );
    assert_eq!(out.as_deref(), Some("35\n"));
}

/// A reassignment nested in a branch (`if true { g = f }`): the drop-old fires
/// only when the branch runs. `g(5) = 5 + 100 = 105`.
#[test]
fn heap_env_binding_reassigned_in_branch_runs() {
    let out = run_program(
        "fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }\n\
             fn main() { let f = make(100i64); let mut g = make(20i64); \
              if true { g = f; } println(f\"{g(5i64)}\"); }\n",
    );
    assert_eq!(out.as_deref(), Some("105\n"));
}

/// A reassignment in a LOOP (`while .. { g = make(i*10) }`): each iteration
/// drops the prior env before storing the next, so every intermediate env is
/// freed once. Last is `make(30)`; `g(5) = 35`.
#[test]
fn heap_env_binding_reassigned_in_loop_runs() {
    let out = run_program(
        "fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }\n\
             fn main() { let mut g = make(0i64); let mut i = 1i64; \
              while i <= 3i64 { g = make(i * 10i64); i = i + 1i64; } \
              println(f\"{g(5i64)}\"); }\n",
    );
    assert_eq!(out.as_deref(), Some("35\n"));
}

/// Reassignment composes with the return-of-binding escape: `build` reassigns
/// `g` then returns it (move-out neutralizes `g`'s slot, the caller adopts the
/// fresh env). `g = make(40)`; `r(2) = 42`.
#[test]
fn heap_env_binding_reassigned_then_returned_runs() {
    let out = run_program(
        "fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }\n\
             fn build(b: i64) -> Fn(i64) -> i64 \
              { let mut g = make(20i64); g = make(b); g }\n\
             fn main() { let r = build(40i64); println(f\"{r(2i64)}\"); }\n",
    );
    assert_eq!(out.as_deref(), Some("42\n"));
}

// ── Heap-env struct FIELD reassignment (B-2026-06-22-2) ──
// `r.f = make(j)` (fresh env, a MOVE) or `r.f = g` (binding source, the
// SHARED env, a COPY) where `r` is a heap-env struct owner and `f` a closure
// field. The binding-reassignment shape with the slot = a field GEP: codegen
// drops r.f's CURRENT env, incs the new env on a copy, and stores the new fat
// into the field slot, so each env is freed exactly once (the field's
// already-registered scope-exit `FreeClosureEnv` frees whatever ends up
// stored). Vec element reassignment is the final remaining form and stays
// rejected.

/// `r.f = make(j)` is a MOVE to a fresh field env: r.f's old env is dropped,
/// the new one freed once at scope exit. `(h.f)(5) = 5 + 20 = 25`.
#[test]
fn heap_env_struct_field_reassigned_to_fresh_runs() {
    let out = run_program(
        "struct H { f: Fn(i64) -> i64 }\n\
             fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }\n\
             fn main() { let mut h = H { f: make(10i64) }; h.f = make(20i64); \
              println(f\"{(h.f)(5i64)}\"); }\n",
    );
    assert_eq!(out.as_deref(), Some("25\n"));
}

/// `r.f = g` is a COPY: the field shares `g`'s env (inc'd, freed once) and `g`
/// stays usable. `(h.f)(5) = 105`, `g(1) = 101` → 206.
#[test]
fn heap_env_struct_field_reassigned_copy_runs() {
    let out = run_program(
        "struct H { f: Fn(i64) -> i64 }\n\
             fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }\n\
             fn main() { let g = make(100i64); let mut h = H { f: make(10i64) }; h.f = g; \
              println(f\"{(h.f)(5i64) + g(1i64)}\"); }\n",
    );
    assert_eq!(out.as_deref(), Some("206\n"));
}

/// A POD sibling field rides along untouched by the closure-field reassign.
/// `(h.f)(5) = 25`, `h.n = 7` → 32.
#[test]
fn heap_env_struct_field_reassigned_pod_sibling_runs() {
    let out = run_program(
        "struct H { f: Fn(i64) -> i64, n: i64 }\n\
             fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }\n\
             fn main() { let mut h = H { f: make(10i64), n: 7i64 }; h.f = make(20i64); \
              println(f\"{(h.f)(5i64) + h.n}\"); }\n",
    );
    assert_eq!(out.as_deref(), Some("32\n"));
}

/// Reassigning a field in a LOOP drops the prior env each iteration, so every
/// intermediate env is freed once. Last is `make(30)`; `(h.f)(5) = 35`.
#[test]
fn heap_env_struct_field_reassigned_in_loop_runs() {
    let out = run_program(
        "struct H { f: Fn(i64) -> i64 }\n\
             fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }\n\
             fn main() { let mut h = H { f: make(0i64) }; let mut i = 1i64; \
              while i <= 3i64 { h.f = make(i * 10i64); i = i + 1i64; } \
              println(f\"{(h.f)(5i64)}\"); }\n",
    );
    assert_eq!(out.as_deref(), Some("35\n"));
}

/// In a two-closure-field owner, reassigning ONE field leaves the sibling
/// closure field intact. `(h.f)(1) = 51` (reassigned), `(h.g)(2) = 22` → 73.
#[test]
fn heap_env_struct_field_reassigned_one_of_two_runs() {
    let out = run_program(
        "struct H { f: Fn(i64) -> i64, g: Fn(i64) -> i64 }\n\
             fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }\n\
             fn main() { let mut h = H { f: make(10i64), g: make(20i64) }; h.f = make(50i64); \
              println(f\"{(h.f)(1i64) + (h.g)(2i64)}\"); }\n",
    );
    assert_eq!(out.as_deref(), Some("73\n"));
}

// ── Heap-env Vec ELEMENT reassignment (B-2026-06-22-2) — closes the epic ──
// `v[i] = make(j)` (fresh env, a MOVE) or `v[i] = g` (binding source, the
// SHARED env, a COPY) where `v` is a heap-env `Vec[Fn]` owner. The
// binding-reassignment shape with the slot = the bounds-checked element ptr.
// Codegen drops v[i]'s CURRENT env, incs the new env on a copy, and stores the
// new fat into the element slot; the Vec's dynamic (refcount-aware) element
// drop loop then frees whatever ends up stored once at scope exit. This is the
// last reassignment form — every heap-env closure place is now supported.

/// `v[i] = make(j)` is a MOVE to a fresh element env: v[i]'s old env is dropped,
/// the new one freed once by the drop loop. `(v[0])(5) = 5 + 20 = 25`.
#[test]
fn heap_env_vec_element_reassigned_to_fresh_runs() {
    let out = run_program(
        "fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }\n\
             fn main() { let mut v: Vec[Fn(i64) -> i64] = Vec.new(); v.push(make(10i64)); \
              v[0i64] = make(20i64); println(f\"{(v[0i64])(5i64)}\"); }\n",
    );
    assert_eq!(out.as_deref(), Some("25\n"));
}

/// `v[i] = g` is a COPY: the element shares `g`'s env (inc'd, freed once) and
/// `g` stays usable. `(v[0])(5) = 105`, `g(1) = 101` → 206.
#[test]
fn heap_env_vec_element_reassigned_copy_runs() {
    let out = run_program(
        "fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }\n\
             fn main() { let g = make(100i64); let mut v: Vec[Fn(i64) -> i64] = Vec.new(); \
              v.push(make(10i64)); v[0i64] = g; \
              println(f\"{(v[0i64])(5i64) + g(1i64)}\"); }\n",
    );
    assert_eq!(out.as_deref(), Some("206\n"));
}

/// In a multi-element Vec, reassigning ONE element leaves the others intact.
/// `(v[0])(1) = 51` (reassigned), `(v[1])(2) = 22` → 73.
#[test]
fn heap_env_vec_element_reassigned_multi_runs() {
    let out = run_program(
        "fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }\n\
             fn main() { let mut v: Vec[Fn(i64) -> i64] = Vec.new(); \
              v.push(make(10i64)); v.push(make(20i64)); v[0i64] = make(50i64); \
              println(f\"{(v[0i64])(1i64) + (v[1i64])(2i64)}\"); }\n",
    );
    assert_eq!(out.as_deref(), Some("73\n"));
}

/// Reassigning the same element in a LOOP drops the prior env each iteration,
/// so every intermediate env is freed once. Last is `make(30)`; `(v[0])(5) = 35`.
#[test]
fn heap_env_vec_element_reassigned_in_loop_runs() {
    let out = run_program(
        "fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }\n\
             fn main() { let mut v: Vec[Fn(i64) -> i64] = Vec.new(); v.push(make(0i64)); \
              let mut i = 1i64; while i <= 3i64 { v[0i64] = make(i * 10i64); i = i + 1i64; } \
              println(f\"{(v[0i64])(5i64)}\"); }\n",
    );
    assert_eq!(out.as_deref(), Some("35\n"));
}

/// A DYNAMIC index reassigns each element once over the loop (the element ptr is
/// computed from the runtime index). `(v[0])(0)+(v[1])(0)+(v[2])(0) =
/// 0 + 100 + 200 = 300`.
#[test]
fn heap_env_vec_element_reassigned_dynamic_index_runs() {
    let out = run_program(
        "fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }\n\
             fn main() { let mut v: Vec[Fn(i64) -> i64] = Vec.new(); \
              v.push(make(1i64)); v.push(make(2i64)); v.push(make(3i64)); \
              let mut i = 0i64; while i < 3i64 { v[i] = make(i * 100i64); i = i + 1i64; } \
              println(f\"{(v[0i64])(0i64) + (v[1i64])(0i64) + (v[2i64])(0i64)}\"); }\n",
    );
    assert_eq!(out.as_deref(), Some("300\n"));
}

// ── Owner-copy slice (B-2026-06-22-2) ──
// `let s = a` where `a` is a heap-env STRUCT owner. Kāra struct copy is COPY
// semantics in codegen (heap Vec/String fields deep-copy to independent
// buffers; a `Fn` field is shallow-copied, so `s` aliases `a`'s SAME RC env
// box). The copy INCs the shared env and registers `s`'s own instance
// `FreeClosureEnv`, so each owner RC-drops once and the box is freed exactly
// once. `a` stays a live, usable owner. Tuple / array / Vec owner copy is the
// next slice and stays rejected.

/// Both the source and the copy are usable (COPY semantics): `(a.f)(1) = 11`,
/// `(s.f)(2) = 12`. The inc balances each owner's RC-drop.
#[test]
fn heap_env_owner_struct_copied_runs() {
    let out = run_program(
        "struct H { f: Fn(i64) -> i64 }\n\
             fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }\n\
             fn main() { let a = H { f: make(10i64) }; let s = a; \
              println(f\"{(a.f)(1i64)}\"); println(f\"{(s.f)(2i64)}\"); }\n",
    );
    assert_eq!(out.as_deref(), Some("11\n12\n"));
}

/// A copy-of-a-copy chain `let s = a; let t = s` is transitively all owners of
/// the one shared RC env — each copy increments it, so three owners free it
/// exactly once. `(a.f)(1)+(s.f)(1)+(t.f)(1) = 101*3 = 303`.
#[test]
fn heap_env_owner_struct_copy_chain_runs() {
    let out = run_program(
        "struct H { f: Fn(i64) -> i64 }\n\
             fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }\n\
             fn main() { let a = H { f: make(100i64) }; let s = a; let t = s; \
              println(f\"{(a.f)(1i64) + (s.f)(1i64) + (t.f)(1i64)}\"); }\n",
    );
    assert_eq!(out.as_deref(), Some("303\n"));
}

/// A sibling POD data field copies trivially; `(s.f)(5)+s.count+a.count =
/// 15+7+7 = 29`. The type-driven struct copy duplicates `count`; only the `Fn`
/// field's env is RC-shared.
#[test]
fn heap_env_owner_struct_copy_with_data_field_runs() {
    let out = run_program(
        "struct H { f: Fn(i64) -> i64, count: i64 }\n\
             fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }\n\
             fn main() { let a = H { f: make(10i64), count: 7i64 }; let s = a; \
              println(f\"{(s.f)(5i64) + s.count + a.count}\"); }\n",
    );
    assert_eq!(out.as_deref(), Some("29\n"));
}

/// A sibling HEAP String field is DEEP-copied by the normal struct copy (each
/// owner gets an independent buffer), composing with the closure-env inc — no
/// double-free of the string, no leak of the env.
#[test]
fn heap_env_owner_struct_copy_with_string_field_runs() {
    let out = run_program(
            "struct H { f: Fn(i64) -> i64, name: String }\n\
             fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }\n\
             fn main() { let a = H { f: make(10i64), name: \"a long enough heap string payload\" }; \
              let s = a; println(f\"{(s.f)(5i64)}\"); println(s.name); }\n",
        );
    assert_eq!(
        out.as_deref(),
        Some("15\na long enough heap string payload\n")
    );
}

/// Owner copy then ESCAPE: `let a = ..; let s = a; s` — the copy `s` is itself a
/// returnable owner (move-out neutralizes `s`; the caller adopts). With the
/// inc, the box is freed once across `a`'s scope-exit drop and the caller's.
#[test]
fn heap_env_owner_struct_copy_then_escape_runs() {
    let out = run_program(
        "struct H { f: Fn(i64) -> i64 }\n\
             fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }\n\
             fn build(k: i64) -> H { let a = H { f: make(k) }; let s = a; s }\n\
             fn main() { let r = build(20i64); println(f\"{(r.f)(22i64)}\"); }\n",
    );
    assert_eq!(out.as_deref(), Some("42\n"));
}

/// A TUPLE owner may be COPIED (`let s = t`): the `Fn` element is an inline fat
/// pointer shallow-copied (shared env box), so both `t` and `s` invoke it; the
/// inc-on-copy balances the two scope-exit RC drops. The POD sibling (`0i64`)
/// copies trivially. `(a.0)(1)=11`, `(s.0)(2)=12`.
#[test]
fn heap_env_tuple_owner_copied_runs() {
    let out = run_program(
        "fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }\n\
             fn main() { let a = (make(10i64), 0i64); let s = a; \
              println(f\"{(a.0)(1i64)}\"); println(f\"{(s.0)(2i64)}\"); }\n",
    );
    assert_eq!(out.as_deref(), Some("11\n12\n"));
}

/// The closure element at a NON-zero tuple index copies correctly (the
/// per-element GEP picks index 1). `(a.1)(1)=11`, `(s.1)(2)=12`.
#[test]
fn heap_env_tuple_owner_copied_at_index_one_runs() {
    let out = run_program(
        "fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }\n\
             fn main() { let a = (0i64, make(10i64)); let s = a; \
              println(f\"{(a.1)(1i64)}\"); println(f\"{(s.1)(2i64)}\"); }\n",
    );
    assert_eq!(out.as_deref(), Some("11\n12\n"));
}

/// The ARRAY twin: a fixed-size `Array[Fn,1]` owner copied; both `a` and `s`
/// invoke the shared closure. The array element GEP is `[0, idx]` (vs the
/// tuple's `build_struct_gep`).
#[test]
fn heap_env_array_owner_copied_runs() {
    let out = run_program(
        "fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }\n\
             fn main() { let a: Array[Fn(i64) -> i64, 1] = [make(10i64)]; let s = a; \
              println(f\"{(a[0])(1i64)}\"); println(f\"{(s[0])(2i64)}\"); }\n",
    );
    assert_eq!(out.as_deref(), Some("11\n12\n"));
}

/// A two-element `Fn` array copied — every element's env is inc'd, so both
/// `a` and `s` invoke both closures. `(s[0])(1)+(s[1])(1)+(a[0])(1) =
/// 11+21+11 = 43`.
#[test]
fn heap_env_array_owner_copied_multi_runs() {
    let out = run_program(
        "fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }\n\
             fn main() { let a: Array[Fn(i64) -> i64, 2] = [make(10i64), make(20i64)]; let s = a; \
              println(f\"{(s[0])(1i64) + (s[1])(1i64) + (a[0])(1i64)}\"); }\n",
    );
    assert_eq!(out.as_deref(), Some("43\n"));
}

/// A tuple copy-of-a-copy chain `let s = a; let t = s` — three owners of one
/// shared env box, each inc'ing it, so it is freed exactly once.
/// `(a.0)(1)+(s.0)(1)+(t.0)(1) = 101*3 = 303`.
#[test]
fn heap_env_tuple_owner_copy_chain_runs() {
    let out = run_program(
        "fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }\n\
             fn main() { let a = (make(100i64), 0i64); let s = a; let t = s; \
              println(f\"{(a.0)(1i64) + (s.0)(1i64) + (t.0)(1i64)}\"); }\n",
    );
    assert_eq!(out.as_deref(), Some("303\n"));
}

/// Tuple owner copy then ESCAPE: `let a = ..; let s = a; s` — the copy `s` is
/// itself a returnable tuple owner (the container-return fixpoint sees the copy
/// via `collect_tuple_array_owners`; move-out neutralizes `s`, the caller
/// adopts). With the inc, the box is freed once across `a`'s scope-exit drop
/// and the caller's.
#[test]
fn heap_env_tuple_owner_copy_then_escape_runs() {
    let out = run_program(
        "fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }\n\
             fn build(k: i64) -> (Fn(i64) -> i64, i64) \
              { let a = (make(k), 0i64); let s = a; s }\n\
             fn main() { let r = build(20i64); println(f\"{(r.0)(22i64)}\"); }\n",
    );
    assert_eq!(out.as_deref(), Some("42\n"));
}

/// A tuple owner copy composes with a sibling HEAP String element: the
/// String's buffer is SHARED (the source's drop is suppressed via cap-zero, the
/// copy frees it once) and readable from BOTH owners, while the `Fn` element's
/// env is RC-inc'd — no double-free of the string, no leak of the env. Unlike
/// the struct owner copy (which DEEP-copies the String to independent buffers),
/// the tuple copy shares the read-only buffer; both forms are sound. `(s.0)(5)=15`.
#[test]
fn heap_env_tuple_owner_copy_with_string_field_runs() {
    let out = run_program(
        "fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }\n\
             fn main() { let a = (make(10i64), \"a long enough heap string payload\"); let s = a; \
              println(f\"{(s.0)(5i64)}\"); println(s.1); println(a.1); }\n",
    );
    assert_eq!(
        out.as_deref(),
        Some("15\na long enough heap string payload\na long enough heap string payload\n")
    );
}

/// The ARRAY twin of owner-copy-then-escape.
#[test]
fn heap_env_array_owner_copy_then_escape_runs() {
    let out = run_program(
        "fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }\n\
             fn build(k: i64) -> Array[Fn(i64) -> i64, 1] \
              { let a: Array[Fn(i64) -> i64, 1] = [make(k)]; let s = a; s }\n\
             fn main() { let r = build(20i64); println(f\"{(r[0])(22i64)}\"); }\n",
    );
    assert_eq!(out.as_deref(), Some("42\n"));
}

/// A `Vec[Fn]` owner may be MOVED (`let w = v`): unlike the struct / tuple /
/// array owner COPY (inc-on-copy), a Vec binding-to-binding is a MOVE — codegen
/// zeroes `v`'s cap, which the `cap > 0` guard in the `FreeVecBuffer` cleanup
/// uses to skip v's WHOLE cleanup (the per-element env-drop loop AND the buffer
/// free), while `w` registers its own dynamic env-drop loop. No inc; the buffer
/// and its element envs transfer to `w`, freed exactly once. `(w[0])(5)=15`.
#[test]
fn heap_env_vec_owner_moved_runs() {
    let out = run_program(
        "fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }\n\
             fn main() { let mut v: Vec[Fn(i64) -> i64] = Vec.new(); v.push(make(10i64)); \
              let w = v; println(f\"{(w[0])(5i64)}\"); }\n",
    );
    assert_eq!(out.as_deref(), Some("15\n"));
}

/// A multi-element `Vec[Fn]` owner moved — the dynamic drop loop frees every
/// element env once. `(w[0])(1)+(w[1])(1) = 11+21 = 32`.
#[test]
fn heap_env_vec_owner_move_multi_runs() {
    let out = run_program(
        "fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }\n\
             fn main() { let mut v: Vec[Fn(i64) -> i64] = Vec.new(); v.push(make(10i64)); \
              v.push(make(20i64)); let w = v; println(f\"{(w[0])(1i64) + (w[1])(1i64)}\"); }\n",
    );
    assert_eq!(out.as_deref(), Some("32\n"));
}

/// A move chain `let w = v; let x = w` — the buffer hops owner twice; each move
/// zeroes the prior owner's cap, so only the final owner `x` frees it (once).
/// `(x[0])(1) = 101`.
#[test]
fn heap_env_vec_owner_move_chain_runs() {
    let out = run_program(
        "fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }\n\
             fn main() { let mut v: Vec[Fn(i64) -> i64] = Vec.new(); v.push(make(100i64)); \
              let w = v; let x = w; println(f\"{(x[0])(1i64)}\"); }\n",
    );
    assert_eq!(out.as_deref(), Some("101\n"));
}

/// Vec owner move then ESCAPE: `let w = v; w` — the moved owner `w` is itself a
/// returnable Vec owner (the Vec-return fixpoint sees the move-dest via
/// `collect_vec_owners`; the drop loop relocates to the caller). The buffer is
/// freed once at the caller's scope exit.
#[test]
fn heap_env_vec_owner_move_then_escape_runs() {
    let out = run_program(
        "fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }\n\
             fn build(k: i64) -> Vec[Fn(i64) -> i64] \
              { let mut v: Vec[Fn(i64) -> i64] = Vec.new(); v.push(make(k)); let w = v; w }\n\
             fn main() { let r = build(20i64); println(f\"{(r[0])(22i64)}\"); }\n",
    );
    assert_eq!(out.as_deref(), Some("42\n"));
}

#[test]
fn escaping_capturing_closure_via_let_is_rejected() {
    let err = ir_result(
        "fn make(k: i64) -> Fn(i64) -> i64 { let f = |x| x + k; f }\n\
             fn main() { let g = make(10i64); println(f\"{g(5i64)}\"); }\n",
    )
    .expect_err("a returned capturing closure bound to a local must be rejected");
    assert!(err.contains("E_ESCAPING_CLOSURE_NOT_YET"), "got: {err}");
}

/// A NON-capturing closure returned is sound (null env) — must still compile
/// and run (this is the B-2026-06-21-2 path; the guard must not touch it).
#[test]
fn non_capturing_closure_returned_still_runs() {
    let out = run_program(
        "fn pick() -> Fn(i64) -> i64 { |x| x * 2i64 }\n\
             fn main() { let f = pick(); println(f\"{f(21i64)}\"); }\n",
    );
    assert_eq!(out.as_deref(), Some("42\n"));
}

/// A capturing closure used WITHIN the same frame (called locally, and
/// passed DOWN by a `Fn(..)` parameter) is sound — the frame stays live —
/// and must keep working.
#[test]
fn capturing_closure_same_frame_and_passed_down_still_run() {
    let local = run_program(
        "fn main() { let base = 10i64; let f = |x| x + base; println(f\"{f(5i64)}\"); }\n",
    );
    assert_eq!(local.as_deref(), Some("15\n"));
    let passed = run_program(
        "fn apply(f: Fn(i64) -> i64, x: i64) -> i64 { f(x) }\n\
             fn main() { let base = 10i64; println(f\"{apply(|x| x + base, 5i64)}\"); }\n",
    );
    assert_eq!(passed.as_deref(), Some("15\n"));
}

// ── Closure-value call through a non-identifier callee (B-2026-06-22-4) ──
// A closure stored in a struct field / Vec element / tuple slot and invoked
// through a parenthesized place-expression callee — `(h.f)(x)`, `v[i](x)`,
// `(t.0)(x)` — must lower to the env-first fat-pointer indirect call, not
// the old const-0 stub. These tests actually RUN the program (the only
// `(x.f)(..)` snippets that existed before were `expect_err` escape-
// rejection tests that never executed), asserting the codegen output equals
// the known-correct interpreter (`karac run`) result.

/// The canonical repro: a non-capturing closure stored in a struct field,
/// called same-frame through `(h.f)(arg)`. Built+ran printing `0` before
/// the fix; the interpreter always printed `42`.
#[test]
fn struct_field_closure_call_runs() {
    let out = run_program(
        "struct H { f: Fn(i64) -> i64 }\n\
             fn main() { let h = H { f: |x| x * 2i64 }; println(f\"{(h.f)(21i64)}\"); }\n",
    );
    assert_eq!(out.as_deref(), Some("42\n"));
}

/// Same struct-field call but the stored closure CAPTURES a local — the
/// fat pointer's env slot is non-null, so this also proves the env pointer
/// is threaded through the indirect call (used same-frame, so the env
/// alloca is still live — the B-2026-06-22-2 escape guard only fires on
/// return-position escapes).
#[test]
fn struct_field_capturing_closure_call_runs() {
    let out = run_program(
            "struct H { f: Fn(i64) -> i64 }\n\
             fn main() { let m = 3i64; let h = H { f: |x| x * m }; println(f\"{(h.f)(14i64)}\"); }\n",
        );
    assert_eq!(out.as_deref(), Some("42\n"));
}

/// A `Vec[Fn(i64) -> i64]` element invoked through an index callee
/// `v[i](arg)`.
#[test]
fn vec_indexed_closure_call_runs() {
    let out = run_program(
        "fn main() {\n\
             let mut v: Vec[Fn(i64) -> i64] = Vec.new();\n\
             v.push(|x| x + 1i64);\n\
             v.push(|x| x * 10i64);\n\
             println(f\"{v[1](4i64)}\");\n\
             }\n",
    );
    assert_eq!(out.as_deref(), Some("40\n"));
}

/// A closure in a tuple slot invoked through a tuple-index callee
/// `(t.0)(arg)`.
#[test]
fn tuple_index_closure_call_runs() {
    let out =
        run_program("fn main() { let t = (|x| x + 1i64, 7i64); println(f\"{(t.0)(41i64)}\"); }\n");
    assert_eq!(out.as_deref(), Some("42\n"));
}

/// B-2026-08-08-5 — a `Vec[weak T]` push DOWNGRADES, takes no strong count,
/// and the container weak-drops each element at scope exit.
///
/// Asserted on the IR rather than only end-to-end, and that is deliberate:
/// the LSan fixture for this
/// (`asan_vec_of_weak_back_edges_reclaims_a_cycle_no_leak`) gates the
/// TYPECHECK half but NOT these two codegen halves. Reverting them leaves a
/// program that leaks its whole graph — 288 bytes, measured under valgrind
/// — while LeakSanitizer still reports it clean (B-2026-08-08-4 records why
/// LSan under-reports this class). So without the assertions below, a
/// codegen regression here would be invisible to the whole suite.
///
/// Three things, and each is a way this went wrong while being built:
///
/// * `karac_weak_downgrade` at the push — without it the container holds a
///   STRONG pointer in a slot the source declares `weak`, so the cycle
///   leaks while the program says it does not;
/// * NO strong retain from the push — the first cut left the ordinary
///   transfer inc in place, and the payloads then freed while every control
///   block stayed, because the strong count never reached zero;
/// * `__karac_vec_elem_weak_drop` registered — without it the container
///   never releases what its pushes took.
///
/// The strong-`Vec[N]` control row is what makes the first two mean
/// anything: it must show the opposite of each.
#[test]
fn test_e2e_optres_map_unannotated_closure_returning_a_string() {
    // B-2026-08-08-21 — `out.first().map(|s| s.to_uppercase())` passed
    // `karac check` and ran under `--interp`, then `karac build` refused it
    // and told the author to annotate the closure parameter. The two
    // backends disagreed about what is valid Kara, and the disagreement was
    // invisible until the DEFAULT execution path.
    //
    // The cause was upstream of codegen: `Option/Result.map` inferred its
    // closure argument WITHOUT publishing a `closure_param_seeds` entry,
    // while every sibling that takes a payload closure (`map_or`,
    // `map_or_else`, `map_err`, `and_then`) seeds through
    // `infer_closure_ret`. So the param stayed a metavar, the closure's
    // surface types were not recoverable, and the heap-payload path bailed
    // loudly rather than miscompile — the bail was doing its job over a gap
    // one phase up.
    //
    // Every method body the old gate named is covered here, on the ONE
    // spelling the gate rejected (un-annotated). An interpolated literal is
    // included because the old predicate caught it too and it lowers fine.
    let out = run_program(
        r#"
fn main() {
    let mut out: Vec[String] = Vec.new();
    out.push("  Hello  ".to_string());
    match out.first().map(|s| s.to_uppercase()) { Some(v) => println(v), None => println("-") }
    match out.first().map(|s| s.trim()) { Some(v) => println(v), None => println("-") }
    match out.first().map(|s| s.to_lowercase()) { Some(v) => println(v), None => println("-") }
    match out.first().map(|s| s.replace("l", "L")) { Some(v) => println(v), None => println("-") }
    match out.first().map(|s| s.to_string()) { Some(v) => println(v), None => println("-") }
    match out.first().map(|s| f"[{s}]") { Some(v) => println(v), None => println("-") }
    match out.first().map(|s| { let t = s.to_uppercase(); t }) { Some(v) => println(v), None => println("-") }
    let empty: Vec[String] = Vec.new();
    match empty.first().map(|s| s.to_uppercase()) { Some(v) => println(v), None => println("-") }
}
"#,
    );
    if let Some(out) = out {
        // `trim_end` only — the first line legitimately begins with the
        // receiver's own leading spaces.
        assert_eq!(
            out.trim_end(),
            "  HELLO  \nHello\n  hello  \n  HeLLo  \n  Hello  \n[  Hello  ]\n  HELLO  \n-"
        );
    }
}

/// B-2026-08-08-30, defect 2 in isolation: the closure's param borrow mark must
/// not outlive the closure.
///
/// Each case reuses the mapper's parameter name in the ENCLOSING scope
/// afterwards. `len_after_map` is the miscompile that made this high-severity —
/// it produced a wrong ANSWER rather than a crash, so a build that merely
/// compiles proves nothing here and the expected values are the point.
///
/// `captured_borrow_still_derefs` is the counter-test for how the fix is
/// written: the registries are CLONED and restored, not taken, so a captured
/// `ref` param keeps its deref inside the closure body. Taking them would pass
/// every case above and silently un-deref this one.
#[test]
fn test_e2e_closure_param_borrow_mark_does_not_leak() {
    let out = run_program(
        r#"
fn shadowed_by_match(v: ref Vec[i64]) {
    match v.first().map(|x| x + 1) { Some(x) => println(x), None => println("-") }
}

fn shadowed_by_let(v: ref Vec[i64]) {
    let q = v.first().map(|x| x + 1);
    let x = 5i64;
    println(x);
    match q { Some(y) => println(y), None => println("-") }
}

fn len_after_map(v: ref Vec[i64]) {
    let q = v.first().map(|x| x + 1);
    let x: Vec[i64] = vec![1, 2, 3];
    println(x.len());
    match q { Some(y) => println(y), None => println("-") }
}

fn captured_borrow_still_derefs(w: ref Vec[i64]) -> i64 {
    let f = |i: i64| w[i] + 1i64;
    return f(0i64);
}

fn main() {
    let v: Vec[i64] = vec![7, 9];
    shadowed_by_match(v);
    shadowed_by_let(v);
    len_after_map(v);
    println(captured_borrow_still_derefs(v));
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim_end(), "8\n5\n8\n3\n8\n8");
    }
}

#[test]
fn e2e_closure_returns_captured_heap_no_double_free() {
    // B-2026-07-18-42: a closure that captures a whole heap String/Vec and
    // RETURNS it double-freed under AOT/JIT (interp correct) — the captured
    // value's buffer is owned by the enclosing frame (stack-env alias) or the
    // RC env box (heap-env), never by the return value, so handing back the
    // alias made the receiver's free collide with that owner's free. Marking
    // captured heap vars as borrowed aliases routes the body return through
    // the same defensive deep-copy every other consume site uses. Covers a
    // stack-env param capture (immediately invoked), a stack-env LOCAL
    // capture, and an ESCAPING (heap-env) closure returned then invoked.
    if let Some(out) = run_program(
        "fn dup(x: String) -> String { let g = || x; g() }\n\
             fn loc() -> String { let s = \"L\".to_string(); let g = || s; g() }\n\
             fn mk(x: String) -> Fn() -> String { || x }\n\
             fn main() {\n\
                 println(dup(\"P\".to_string()));\n\
                 println(loc());\n\
                 let g = mk(\"E\".to_string());\n\
                 println(g());\n\
                 println(g());\n\
             }",
    ) {
        assert_eq!(out, "P\nL\nE\nE\n");
    }
}

#[test]
fn e2e_closure_captures_heap_struct_returned_no_double_free() {
    // B-2026-07-18-46: a (stack-env) closure capturing a whole heap-bearing
    // STRUCT/ENUM and returning it printed garbage under AOT/JIT — the env's
    // bit-copy shallow-aliased the source struct's field buffers (freed by
    // the frame's owner drop), so the returned struct handed back dangling
    // pointers. The Vec/String sibling was B-2026-07-18-42; a struct capture
    // isn't in `vec_elem_types`, so it needs the type-aware
    // `emit_clone_fn_for_type_expr` deep clone at the tail instead of the
    // flat Vec/String defensive copy. Covers a single- and two-heap-field
    // struct, a struct with a Vec field, and an enum payload. (An ESCAPING
    // heap-env struct capture stays the documented
    // E_ESCAPING_CLOSURE_HEAP_CAPTURE_NOT_YET deferral, B-2026-06-22-2.)
    if let Some(out) = run_program(
        "struct W { s: String }\n\
             struct W2 { a: String, b: String }\n\
             struct Wv { v: Vec[i64] }\n\
             enum E { A(String) }\n\
             fn f(w: W) -> W { let g = || w; g() }\n\
             fn f2(w: W2) -> W2 { let g = || w; g() }\n\
             fn fv(w: Wv) -> Wv { let g = || w; g() }\n\
             fn fe(e: E) -> E { let g = || e; g() }\n\
             fn main() {\n\
                 println(f(W { s: \"one\".to_string() }).s);\n\
                 let r = f2(W2 { a: \"two\".to_string(), b: \"three\".to_string() });\n\
                 println(r.a);\n\
                 println(r.b);\n\
                 println(fv(Wv { v: [1, 2, 3] }).v.len());\n\
                 match fe(E.A(\"four\".to_string())) { E.A(s) => println(s) }\n\
             }",
    ) {
        assert_eq!(out, "one\ntwo\nthree\n3\nfour\n");
    }
}

/// B-2026-07-29-27 / B-2026-07-29-31 — `#[derive(Clone)]` synthesizes a
/// CALLABLE `.clone()`, not just a satisfiable `T: Clone` bound.
///
/// Pre-fix the two halves of `Clone` had drifted apart: bound discharge
/// honoured the derive (`type_supports_clone`) while method resolution went
/// through a pure free fn with a stdlib-collection allowlist
/// (`clone_self_type_for`), so `fn dup[T: Clone](x: T) -> T { x }`
/// type-checked with a derived argument but `{ x.clone() }` was rejected
/// `no method 'clone' on type 'T'`. The bound was decorative.
///
/// The clone must be DEEP — the assertions below mutate the copy and read
/// the source back, so a shallow `{ptr,len,cap}` bitcopy (which would also
/// double-free at the two drops) fails here rather than only under ASAN.
#[test]
fn e2e_derived_clone_is_callable_and_deep() {
    let Some(out) = run_program(
        "#[derive(Clone)]\n\
             struct S { n: i64, s: String, v: Vec[i64] }\n\
             #[derive(Clone)]\n\
             enum E { A(i64), B(String) }\n\
             fn dup[T: Clone](x: T) -> T { x.clone() }\n\
             fn main() {\n\
             \x20   let mut a = S { n: 1, s: \"hi\", v: Vec.new() };\n\
             \x20   a.v.push(10);\n\
             \x20   let mut b = a.clone();\n\
             \x20   b.v.push(20);\n\
             \x20   b.n = 99;\n\
             \x20   println(f\"{a.n} {a.v.len()} {b.n} {b.v.len()} {b.s}\");\n\
             \x20   let c = dup(a);\n\
             \x20   println(f\"{c.s} {c.v.len()}\");\n\
             \x20   let e = E.B(\"payload\");\n\
             \x20   let f = dup(e);\n\
             \x20   match f {\n\
             \x20       E.A(k) => println(f\"{k}\"),\n\
             \x20       E.B(t) => println(t),\n\
             \x20   }\n\
             \x20   println(f\"{dup(7)}\");\n\
             }\n",
    ) else {
        return;
    };
    assert_eq!(out, "1 1 99 2 hi\nhi 1\npayload\n7\n");
}

/// B-2026-08-28-38 — a by-value STRUCT argument to a closure runs its user
/// `Drop` body, matching the free-function spelling and the interpreter.
///
/// Pre-fix it ran ZERO bodies on both compiled backends while `--interp`
/// ran one. Under caller-retains a by-value struct param's body belongs to
/// the CALLER: the function path gets it from
/// `track_inline_owned_aggregate_arg` at the call site, and
/// `compile_closure_call` had no equivalent. The only callee-side
/// registration in `functions.rs` is coroutine-only, and its comment
/// records that dropping every owned struct param there broke an E2E — so
/// the caller is the right owner, not the closure body.
///
/// `tuple-arg-control` is the constraint rather than a passing detail. A
/// tuple argument ALREADY had an owner, so registering unconditionally gave
/// it two: measured 1 -> 2, and the two-dropper tuple 2 -> 4, on both
/// compiled backends. Tuple-shaped arguments are therefore excluded, and
/// this row and `two-dropper-tuple-control` are what hold that boundary.
/// The memory consequence — a double free at a heap-carrying element — is
/// pinned by `asan_closure_by_value_aggregate_arg_is_owned_once`.
#[test]
fn e2e_closure_by_value_struct_arg_runs_its_drop_body() {
    const H: &str = "struct R { id: i64 }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"drop {self.id}\") } }\n";
    for (label, body, want) in [
            // The row's own repro.
            (
            // ORDER CORRECTED by B-2026-08-29-55 -- the argument temp dies AS THE
            // CALL RETURNS (design.md's "Function/method call argument | After the
            // call returns"), which is BEFORE the enclosing `println` runs. The old
            // expectation held it to the statement's `;`; `--interp` printed the
            // body first all along, so that line recorded a run-vs-build divergence
            // rather than a decision.
                "struct-arg",
                "fn main() { let f = |r: R| { r.id };\n\
                 \x20            println(f\"{f(R { id: 41 })}\"); println(\"end\"); }\n",
                "drop 41\n41\nend\n",
            ),
            // The same with a destructure in the body.
            (
            // ORDER CORRECTED by B-2026-08-29-55 -- the argument temp dies AS THE
            // CALL RETURNS (design.md's "Function/method call argument | After the
            // call returns"), which is BEFORE the enclosing `println` runs. The old
            // expectation held it to the statement's `;`; `--interp` printed the
            // body first all along, so that line recorded a run-vs-build divergence
            // rather than a decision.
                "struct-arg-destructured",
                "struct W { r: R, n: i64 }\n\
                 fn main() { let f = |w: W| { let W { r, n } = w; r.id + n };\n\
                 \x20            println(f\"{f(W { r: R { id: 41 }, n: 1 })}\"); println(\"end\"); }\n",
                "drop 41\n42\nend\n",
            ),
            // CONTROL — the free-function spelling, correct all along, and what
            // identifies the closure as the variable.
            (
            // ORDER CORRECTED by B-2026-08-29-55 -- the argument temp dies AS THE
            // CALL RETURNS (design.md's "Function/method call argument | After the
            // call returns"), which is BEFORE the enclosing `println` runs. The old
            // expectation held it to the statement's `;`; `--interp` printed the
            // body first all along, so that line recorded a run-vs-build divergence
            // rather than a decision.
                "free-fn-control",
                "fn take(r: R) -> i64 { r.id }\n\
                 fn main() { println(f\"{take(R { id: 41 })}\"); println(\"end\"); }\n",
                "drop 41\n41\nend\n",
            ),
            // CONTROL — a TUPLE argument, which already had an owner. This is
            // the row that goes to two bodies if the registration is not
            // shape-gated.
            (
                "tuple-arg-control",
                "fn main() { let f = |p: (R, i64)| { let (r, n) = p; r.id + n };\n\
                 \x20            println(f\"{f((R { id: 41 }, 1))}\"); println(\"end\"); }\n",
                "drop 41\n42\nend\n",
            ),
            // CONTROL — two droppers in one tuple argument: 2, never 4.
            (
                "two-dropper-tuple-control",
                "fn main() { let f = |p: (R, R)| { let (a, b) = p; a.id + b.id };\n\
                 \x20            println(f\"{f((R { id: 41 }, R { id: 42 }))}\");\n\
                 \x20            println(\"end\"); }\n",
                // Reverse element order, as measured — the tuple's elements are
                // walked back to front by their existing owner. Recorded rather
                // than assumed; the count is what this row constrains.
                "drop 42\ndrop 41\n83\nend\n",
            ),
        ] {
            let prog = format!("{H}{body}");
            assert_eq!(run_program(&prog).as_deref(), Some(want), "{label}");
        }
}

/// B-2026-08-01-26 — a LOCAL closure binding shadows prelude/stdlib
/// free-fn names at call dispatch. Pre-fix, `let take = |x| ..; take(v)`
/// compiled a DIRECT call to the spliced `std.mem::take` (the generic-fn
/// path ran before the closure-binding check), returning the param (i64)
/// or its pointer word (String — pointer garbage) instead of the closure
/// result, while the typechecker and interpreter resolved the local.
/// The unshadowed stdlib `take` in a sibling fn must keep working (the
/// hoisted check is gated on a live local slot). Twin of
/// `tests/interpreter.rs`'s `test_local_closure_shadows_stdlib_names`.
#[test]
fn e2e_local_closure_shadows_stdlib_names() {
    let Some(out) = run_program(
        "fn shadowed() -> i64 {\n\
             \x20   let take = |x: i64| {\n\
             \x20       let mut v: Vec[i64] = Vec.new();\n\
             \x20       v.push(x);\n\
             \x20       v.len()\n\
             \x20   };\n\
             \x20   take(9)\n\
             }\n\
             fn heap_shadowed() -> i64 {\n\
             \x20   let take = |x: String| {\n\
             \x20       let mut v: Vec[String] = Vec.new();\n\
             \x20       v.push(x);\n\
             \x20       v.len()\n\
             \x20   };\n\
             \x20   take(String.from(\"a\"))\n\
             }\n\
             fn unshadowed() -> i64 {\n\
             \x20   let mut z = 7;\n\
             \x20   take(mut z)\n\
             }\n\
             fn main() {\n\
             \x20   println(shadowed());\n\
             \x20   println(heap_shadowed());\n\
             \x20   println(unshadowed());\n\
             }\n",
    ) else {
        return;
    };
    assert_eq!(out, "1\n1\n7\n");
}

#[test]
fn test_e2e_closure_captured_result_shared_correct() {
    // B-2026-07-12-24 (residual) closure-capture correctness pin. A
    // `Result[shared]` binding captured by a closure that only `match`es it
    // inside must NOT be given a producer-side dec (that would use-after-free
    // the closure's env — the escape analysis treats a capture as escaping).
    // A mis-classification would dec the node at `caller` exit while the
    // closure still holds it → a corrupt read or crash. This pins the value
    // stays correct (the leak-vs-clean gate is intentionally omitted: a
    // captured binding is a documented residual leak, never a UAF).
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
fn caller() -> i64 {
    let d = take();
    let c = || {
        match d {
            Err(e) => e,
            Ok(n) => n.val,
        }
    };
    c()
}
fn main() {
    let mut i: i64 = 0;
    let mut t: i64 = 0;
    while i < 200 {
        t = t + caller();
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
fn test_e2e_oncelock_get_or_init_runs_closure_once() {
    // `get_or_init(|| ...)` for a scalar `T`: the first call runs the
    // closure and seals the cell; a second call returns the existing value
    // WITHOUT running the closure (a different closure body would prove a
    // re-run). The closure captures a local (`base`), exercising the
    // fat-pointer env path. Build==run parity with the interpreter.
    let out = run_program(
        r#"
fn main() {
    let base = 100;
    let c: OnceLock[i64] = OnceLock.new();
    println(c.get_or_init(|| base + 5));
    println(c.get_or_init(|| base + 999));
    println(c.is_set());
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "105\n105\ntrue");
    }
}

#[test]
fn test_e2e_closure_returns_struct_literal_direct() {
    // General closure fix (drives the get_or_init aggregate case): a closure
    // whose body is a struct literal is compiled with the struct as its
    // return type (was: `-> i64`, tripping the LLVM verifier). Exercised via
    // a stored closure value invoked twice.
    if let Some(out) = run_program(
        "struct V { a: i64, b: i64 }\n\
             fn main() {\n\
                 let make = || V { a: 10i64, b: 20i64 };\n\
                 let v = make();\n\
                 let w = make();\n\
                 println((v.a + v.b + w.a).to_string());\n\
             }",
    ) {
        assert_eq!(out, "40\n");
    }
}

#[test]
fn test_e2e_closure_block_body_returns_heap_collection() {
    // B-2026-07-13-20 — a closure with a BLOCK body whose tail is a local
    // built by a collection/String constructor (`let mut v = Vec.new(); …;
    // v`). `infer_closure_return_type` resolved the tail `v` to its
    // `let v = Vec.new()` value, then inferred `Vec.new()` as the `i64`
    // fallback (the `Vec.new()` assoc call wasn't recognized), so the
    // closure fn was declared `-> i64` while the body returned the
    // `{ptr,len,cap}` Vec aggregate → LLVM verifier "return type does not
    // match operand type of return inst". Now the Vec/String/Map/Set
    // constructor arm returns the container's real heap type. Covers a Vec
    // return (with a capture) and a String return; a single-EXPRESSION body
    // (`|| make()`) already resolved via the module-fn return type and is
    // unaffected. Interpreter parity: both print the same.
    if let Some(out) = run_program(
        "fn main() {\n\
                 let base: Vec[i64] = [10, 20, 30];\n\
                 let build = || {\n\
                     let mut out = Vec.new();\n\
                     let mut i = 0;\n\
                     while i < base.len() { out.push(base[i] * 2); i = i + 1; }\n\
                     out\n\
                 };\n\
                 let r = build();\n\
                 println((r[0] + r[1] + r[2]).to_string());\n\
                 let greet = || {\n\
                     let s = String.from(\"hi\");\n\
                     s\n\
                 };\n\
                 println(greet());\n\
             }",
    ) {
        assert_eq!(out, "120\nhi\n");
    }
}

#[test]
fn test_e2e_closure_heap_return_type_siblings() {
    // B-2026-07-13-20 siblings — `infer_closure_return_type` had further gaps
    // for heap-typed closure tails, each declaring the closure fn `-> i64`
    // against a heap body (LLVM verifier "return type does not match operand
    // type of return inst"): (1) a `match` tail (no Match arm → i64 default);
    // (2) an `if` returning `Some(String)`/`None` (bare Some/None constructors
    // unrecognized); (3) a tuple tail with a block-local heap element (`(v,
    // 100)` where `v = Vec.new()` — the bare-identifier-only tail resolution
    // didn't reach a nested position). Fixed by a Match arm, Some/None/Ok/Err
    // → Option/Result layout, and generalizing the Block arm to extend the
    // inference scope with all block `let` bindings. Interpreter parity.
    if let Some(out) = run_program(
            "enum Dir { N, S }\n\
             fn main() {\n\
                 let m = |d: Dir| { match d { Dir.N => String.from(\"north\"), Dir.S => String.from(\"south\") } };\n\
                 println(m(Dir.N));\n\
                 let opt = |n: i64| { if n > 0 { Some(f\"pos-{n}\") } else { None } };\n\
                 match opt(3) { Some(s) => println(s), None => println(\"none\") }\n\
                 let pair = || { let mut v = Vec.new(); v.push(9); (v, 100) };\n\
                 let (vv, k) = pair();\n\
                 println((vv[0] + k).to_string());\n\
             }",
        ) {
            assert_eq!(out, "north\npos-3\n109\n");
        }
}

#[test]
fn test_e2e_closure_captures_and_calls_another_closure() {
    // B-2026-07-15-8: a closure that CAPTURES another closure and calls it
    // (`let base = |x| x + 1; let composed = |x| base(x) * 10`). Compiling
    // the closure body does `mem::take(&mut closure_fn_types)`, so the
    // captured `base` — re-registered in `self.variables` by the
    // capture-load — was NOT in the (emptied) `closure_fn_types` at the
    // body-scope call site, so `base(x)` missed the indirect-call dispatch
    // and fell to the const-0 stub: codegen printed 0 where the interpreter
    // computed 50 — a SILENT wrong result. Fix re-registers captured
    // closure-valued free vars' `FunctionType` into the body-scope map.
    // Also covers a String-returning captured closure (`|s| wrap(wrap(s))`)
    // whose return type must be inferred from the callee closure's
    // `FunctionType` (else the enclosing closure fn is declared `-> i64`
    // against a `{ptr,i64,i64}` String body → LLVM verifier failure).
    if let Some(out) = run_program(
        "fn make_adder(n: i64) -> Fn(i64) -> i64 {\n\
                 |x| x + n\n\
             }\n\
             fn main() {\n\
                 let base = |x: i64| x + 1;\n\
                 let composed = |x: i64| base(x) * 10;\n\
                 println(composed(4));\n\
                 // 3-level capture chain\n\
                 let mid = |x: i64| base(x) * 2;\n\
                 let top = |x: i64| mid(x) + 100;\n\
                 println(top(4));\n\
                 // capture a returned closure and call it inside a closure\n\
                 let add5 = make_adder(5);\n\
                 let via = |x: i64| add5(x) + 1;\n\
                 println(via(10));\n\
                 // String-returning captured closure (return-type inference)\n\
                 let wrap = |s: String| f\"[{s}]\";\n\
                 let deco = |s: String| wrap(s);\n\
                 println(deco(\"x\"));\n\
             }",
    ) {
        assert_eq!(out, "50\n110\n16\n[x]\n");
    }
}

#[test]
fn test_e2e_for_loop_closure_element_call_dispatches_indirect() {
    // B-2026-07-23-28: calling a closure bound as a `for`-loop ELEMENT
    // (`for op in ops { v = op(v) }` over `Array`/`Vec[Fn(i64) -> i64]`)
    // silently returned 0 under `karac build`/JIT while the interpreter was
    // correct. The element BINDING was fine all along — the IR stored the
    // right `{fn_ptr, env}` fat pointer into the loop slot — but the loop
    // var was never registered in `closure_fn_types`, so `op(v)` missed
    // `compile_call`'s indirect-call dispatch and fell to the unknown-callee
    // path, folding the whole call to `store i64 0`. (`let op = ops[i];
    // op(v)` always worked: the `let` path has its own registration via
    // `let_binding_fn_value_type` — which is exactly why the bug looked
    // for-loop-specific.) Fixed by registering a `Fn(..)`-typed binding in
    // `register_var_from_type_expr`, the shared registrar every binding form
    // routes through. Covers: the array-literal repro, a `Vec[Fn]` built by
    // push, a single-element array, CAPTURING closures, and a String
    // (heap-returning) closure applied through the loop binding.
    if let Some(out) = run_program(
        "fn main() {\n\
                 let ops = [|x| x + 1, |x| x * 10];\n\
                 let mut v = 5;\n\
                 for op in ops { v = op(v); }\n\
                 println(v);\n\
                 let mut vs: Vec[Fn(i64) -> i64] = Vec.new();\n\
                 vs.push(|x| x + 1);\n\
                 vs.push(|x| x * 10);\n\
                 vs.push(|x| x - 3);\n\
                 let mut w = 5;\n\
                 for op in vs { w = op(w); }\n\
                 println(w);\n\
                 let one = [|x| x + 100];\n\
                 let mut u = 5;\n\
                 for op in one { u = op(u); }\n\
                 println(u);\n\
                 let base = 100;\n\
                 let caps = [|x| x + base, |x| x * base];\n\
                 let mut c = 1;\n\
                 for op in caps { c = op(c); }\n\
                 println(c);\n\
                 let fs = [|s: String| s + \"-a\", |s: String| s + \"-b\"];\n\
                 let mut acc = \"x\".to_string();\n\
                 for f in fs { acc = f(acc); }\n\
                 println(acc);\n\
             }",
    ) {
        assert_eq!(out, "60\n57\n105\n10100\nx-a-b\n");
    }
}

#[test]
fn test_ir_for_loop_closure_element_emits_indirect_call_not_const_zero() {
    // B-2026-07-23-28 structural guard: the `for op in ops` body must emit a
    // real indirect `call` through the loaded fat pointer's fn slot. Before
    // the fix the body was just `store { ptr, ptr } %elem, ptr %op` followed
    // by `store i64 0` — the call folded away entirely, so asserting on the
    // OUTPUT alone could regress silently if a future default changed. Pin
    // the emitted shape: an `extractvalue` of the closure fat pointer inside
    // the loop body plus at least one indirect call.
    let ir = ir_for(
        "fn main() {\n\
                 let ops = [|x| x + 1, |x| x * 10];\n\
                 let mut v = 5;\n\
                 for op in ops { v = op(v); }\n\
                 println(v);\n\
             }",
    );
    assert!(
        ir.contains("closure_call"),
        "the for-loop closure element call must lower to an indirect \
             closure call, not a folded constant:\n{ir}"
    );
}

#[test]
fn test_e2e_closure_mutates_captured_collection_via_method() {
    // B-2026-07-15-13: a closure that mutates a captured collection through
    // a mutating METHOD (`acc.push`, `buf.push_str`, `m.insert`) captures
    // the receiver by mut-ref (design.md § Closures line 4940 — a
    // `mut ref self` method call captures its receiver mut-ref), so the
    // mutation must write through to the outer binding. Codegen's
    // `mutref_caps` was computed from `collect_assigned_roots` (which sees
    // `=` / `[i] =` targets only, NOT method mutation), so the collection
    // was captured BY VALUE and the closure grew a private copy — the outer
    // binding stayed stale (`acc.len()` read back 0; interp propagated Vec
    // via Rc-sharing). Fixed by the shared
    // `ast::collect_mut_method_receiver_roots` marking the receiver mut-ref
    // in both backends. A read-only capture (`data.iter()`) must NOT be
    // over-marked — verified by the `count_big` closure still capturing by
    // value.
    // Vec uses MULTI-call (accumulation across calls) — `karac check`-clean.
    // String / Map use a SINGLE call: multi-call of a `push_str` / `insert`
    // closure currently trips a separate ownership over-strictness
    // (push_str / insert classified consuming → once-callable, B-2026-07-15-14),
    // so a single mutating call keeps the program check-clean while still
    // proving the mutation writes through to the outer binding.
    if let Some(out) = run_program(
        "fn main() {\n\
                 let mut acc: Vec[i64] = Vec.new();\n\
                 let mut push = |x: i64| { acc.push(x); };\n\
                 push(1);\n\
                 push(2);\n\
                 push(3);\n\
                 println(acc.len());\n\
                 let mut sum: i64 = 0;\n\
                 for x in acc.iter() { sum = sum + x; }\n\
                 println(sum);\n\
                 let mut buf: String = \"\";\n\
                 let mut append = |s: String| { buf.push_str(s); };\n\
                 append(\"hello\");\n\
                 println(buf);\n\
                 let mut m: Map[String, i64] = Map.new();\n\
                 let mut record = |k: String, v: i64| { m.insert(k, v); };\n\
                 record(\"x\", 10);\n\
                 println(m.len());\n\
                 // read-only capture stays by-value (no over-mark / no crash)\n\
                 let data: Vec[i64] = [1, 2, 3, 4];\n\
                 let count_big = |t: i64| {\n\
                     let mut c: i64 = 0;\n\
                     for x in data.iter() { if x > t { c = c + 1; } }\n\
                     c\n\
                 };\n\
                 println(count_big(2));\n\
             }",
    ) {
        assert_eq!(out, "3\n6\nhello\n1\n2\n");
    }
}

#[test]
fn test_e2e_struct_pattern_iterator_closure_param() {
    // B-2026-08-17-24 — the struct sibling of B-2026-07-14-21's tuple fix.
    // `v.iter().map(|P { x, y }| x + y).collect()` bailed out of the chain
    // peel, and the fallthrough MISATTRIBUTED it to the terminal method
    // ("no handler for method 'collect' on non-identifier receiver"), which
    // pointed a fixer at the wrong dispatcher entirely. Covers the shapes
    // the row measured plus the shorthand/rename/wildcard field forms, and
    // `fold`, whose element param goes through the widened helper.
    let src = "struct P { x: i64, y: i64 }\n\
                   fn main() {\n\
                       let mut v: Vec[P] = Vec.new();\n\
                       v.push(P { x: 1, y: 2 });\n\
                       v.push(P { x: 3, y: 6 });\n\
                       let a: Vec[i64] = v.iter().map(|P { x, y }| x + y).collect();\n\
                       println(a[0]);\n\
                       println(a[1]);\n\
                       println(v.iter().fold(0, |acc, P { x, y }| acc + x + y));\n\
                       let r: Vec[i64] = v.iter().map(|P { x: x1, y: y1 }| x1 * y1).collect();\n\
                       println(r[0]);\n\
                       println(r[1]);\n\
                       let w: Vec[i64] = v.iter().map(|P { x, y: _ }| x).collect();\n\
                       println(w[0]);\n\
                       let ft: Vec[i64] = v.iter().filter(|P { x, y }| x + y > 5)\n\
                           .map(|P { x, y }| x + y).collect();\n\
                       println(ft[0]);\n\
                   }";
    let parsed = karac::parse(src);
    assert!(
        parsed.errors.is_empty(),
        "parse errors: {:?}",
        parsed.errors
    );
    if let Some(out) = run_program(src) {
        assert_eq!(out, "3\n9\n12\n2\n18\n1\n9\n");
    }
}

#[test]
fn test_e2e_tuple_pattern_fold_closure_param() {
    // The tuple form through `fold` — unsupported before B-2026-08-17-24
    // too, since `fold` fails closed on any destructuring element param;
    // the shared helper covers both pattern kinds, so this shape came
    // along with the struct one and is pinned so it cannot regress.
    let src = "fn main() {\n\
                       let mut v: Vec[(i64, i64)] = Vec.new();\n\
                       v.push((1, 2));\n\
                       v.push((3, 6));\n\
                       println(v.iter().fold(0, |acc, (a, b)| acc + a + b));\n\
                   }";
    if let Some(out) = run_program(src) {
        assert_eq!(out, "12\n");
    }
}

#[test]
fn e2e_wp_closure_return_is_closure_scoped_nontail() {
    // The B-2026-07-31-16 repro: the closure's return value feeds the
    // binding, and the enclosing fn continues — 9, not 8.
    if let Some(out) = run_program(&format!(
        "{WP_PREAMBLE}\
             fn f() -> i64 with reads(Ctr) {{\n\
                 let x = with_provider[Ctr](InMem {{ n: 8 }}, || {{ return read(); }});\n\
                 x + 1\n\
             }}\n\
             fn main() with reads(Ctr) {{ println(f\"{{f()}}\"); }}",
    )) {
        assert_eq!(out, "9\n");
    }
}

#[test]
fn e2e_wp_closure_conditional_return_joins_fall_through() {
    // Both dynamic paths of one body: the return edge and the
    // fall-through tail join at the merge with the right values.
    if let Some(out) = run_program(&format!(
        "{WP_PREAMBLE}\
             fn cond(n: i64) -> i64 with reads(Ctr) {{\n\
                 let x = with_provider[Ctr](InMem {{ n: n }}, || {{\n\
                     if read() == 8 {{ return 100; }}\n\
                     read()\n\
                 }});\n\
                 x + 1\n\
             }}\n\
             fn main() with reads(Ctr) {{\n\
                 println(f\"{{cond(8)}}\");\n\
                 println(f\"{{cond(3)}}\");\n\
             }}",
    )) {
        assert_eq!(out, "101\n4\n");
    }
}

#[test]
fn e2e_wp_nested_closure_return_inner_only() {
    // A return in the INNER body exits only the inner closure: inner wp
    // = 20, outer body = 20 + read(=1) = 21, fn = 1021.
    if let Some(out) = run_program(&format!(
            "{WP_PREAMBLE}\
             fn nested() -> i64 with reads(Ctr) {{\n\
                 let outer = with_provider[Ctr](InMem {{ n: 1 }}, || {{\n\
                     let inner = with_provider[Ctr](InMem {{ n: 2 }}, || {{ return read() * 10; }});\n\
                     inner + read()\n\
                 }});\n\
                 outer + 1000\n\
             }}\n\
             fn main() with reads(Ctr) {{ println(f\"{{nested()}}\"); }}",
        )) {
            assert_eq!(out, "1021\n");
        }
}

#[test]
fn e2e_wp_closure_return_heap_string() {
    // A heap String moves out through the closure return and is used
    // (not returned) by the enclosing fn.
    if let Some(out) = run_program(&format!(
            "{WP_PREAMBLE}\
             fn heap() -> String with reads(Ctr) {{\n\
                 let s = with_provider[Ctr](InMem {{ n: 42 }}, || {{ return f\"v-{{read()}}\"; }});\n\
                 f\"got-{{s}}\"\n\
             }}\n\
             fn main() with reads(Ctr) {{ println(f\"{{heap()}}\"); }}",
        )) {
            assert_eq!(out, "got-v-42\n");
        }
}

#[test]
fn e2e_wp_closure_valueless_return_gates_side_effect() {
    // `return;` in a unit closure skips the println without exiting the
    // enclosing fn.
    if let Some(out) = run_program(&format!(
        "{WP_PREAMBLE}\
             fn quiet(n: i64) with reads(Ctr) {{\n\
                 with_provider[Ctr](InMem {{ n: n }}, || {{\n\
                     if read() == 3 {{ return; }}\n\
                     println(f\"seen-{{read()}}\");\n\
                 }});\n\
                 println(\"after\");\n\
             }}\n\
             fn main() with reads(Ctr) {{\n\
                 quiet(3);\n\
                 quiet(5);\n\
             }}",
    )) {
        assert_eq!(out, "after\nseen-5\nafter\n");
    }
}

#[test]
fn e2e_wp_closure_return_drops_body_local_before_join() {
    // A heap body-local is live at the return: the retarget drain frees
    // it (then pops the provider) on the return edge, and the ordinary
    // frame drain covers the fall-through edge. \"pad-9\".len() = 5.
    if let Some(out) = run_program(&format!(
        "{WP_PREAMBLE}\
             fn local(n: i64) -> i64 with reads(Ctr) {{\n\
                 let x = with_provider[Ctr](InMem {{ n: n }}, || {{\n\
                     let pad = f\"pad-{{read()}}\";\n\
                     if read() == 9 {{ return pad.len() * 100; }}\n\
                     read()\n\
                 }});\n\
                 x + 1\n\
             }}\n\
             fn main() with reads(Ctr) {{\n\
                 println(f\"{{local(9)}}\");\n\
                 println(f\"{{local(4)}}\");\n\
             }}",
    )) {
        assert_eq!(out, "501\n5\n");
    }
}

#[test]
fn e2e_wp_closure_question_is_closure_scoped() {
    // B-2026-07-31-19: `?` inside the wp closure returns the Err FROM
    // THE CLOSURE — the wp call's value is that Err, and the enclosing
    // fn's match handles it (-1). Pre-fix the fail path early-returned
    // from the whole fn (unwrap_or saw the Err: -99).
    if let Some(out) = run_program(&format!(
        "{WP_PREAMBLE}\
             fn might_fail(x: i64) -> Result[i64, String] {{\n\
                 if x > 5 {{ return Err(\"too big\"); }}\n\
                 Ok(x)\n\
             }}\n\
             fn probe(n: i64) -> Result[i64, String] with reads(Ctr) {{\n\
                 let r = with_provider[Ctr](InMem {{ n: n }}, || {{\n\
                     let v = might_fail(read())?;\n\
                     Ok(v * 10)\n\
                 }});\n\
                 match r {{\n\
                     Ok(v) => Ok(v),\n\
                     Err(_) => Ok(-1),\n\
                 }}\n\
             }}\n\
             fn main() with reads(Ctr) {{\n\
                 println(f\"{{probe(3).unwrap()}}\");\n\
                 println(f\"{{probe(9).unwrap_or(-99)}}\");\n\
             }}",
    )) {
        assert_eq!(out, "30\n-1\n");
    }
}

#[test]
fn e2e_wp_closure_question_err_string_payload_survives() {
    // The Err's String payload must round-trip through the closure's
    // Result shape (multi-word payload words copied into the merge slot).
    if let Some(out) = run_program(&format!(
        "{WP_PREAMBLE}\
             fn might_fail(x: i64) -> Result[i64, String] {{\n\
                 if x > 5 {{ return Err(f\"big-{{x}}\"); }}\n\
                 Ok(x)\n\
             }}\n\
             fn probe(n: i64) -> String with reads(Ctr) {{\n\
                 let r = with_provider[Ctr](InMem {{ n: n }}, || {{\n\
                     let v = might_fail(read())?;\n\
                     Ok(v * 10)\n\
                 }});\n\
                 match r {{\n\
                     Ok(v) => f\"ok-{{v}}\",\n\
                     Err(e) => f\"err-{{e}}\",\n\
                 }}\n\
             }}\n\
             fn main() with reads(Ctr) {{\n\
                 println(f\"{{probe(3)}}\");\n\
                 println(f\"{{probe(9)}}\");\n\
             }}",
    )) {
        assert_eq!(out, "ok-30\nerr-big-9\n");
    }
}

#[test]
fn e2e_wp_closure_question_propagation_shape_unchanged() {
    // The established `wp(...)?` continuation: the closure's Err becomes
    // the wp value, and the OUTER `?` propagates it from the fn — same
    // observable behavior as the old fn-level lowering, now via the
    // closure-scoped route.
    if let Some(out) = run_program(&format!(
        "{WP_PREAMBLE}\
             fn might_fail(x: i64) -> Result[i64, String] {{\n\
                 if x > 5 {{ return Err(\"big\"); }}\n\
                 Ok(x)\n\
             }}\n\
             fn q(x: i64) -> Result[i64, String] with reads(Ctr) {{\n\
                 let v = with_provider[Ctr](InMem {{ n: x }}, || {{\n\
                     let got = might_fail(read())?;\n\
                     Ok(got * 10)\n\
                 }})?;\n\
                 Ok(v + 1)\n\
             }}\n\
             fn main() with reads(Ctr) {{\n\
                 println(f\"{{q(2).unwrap()}}\");\n\
                 println(f\"{{q(7).unwrap_or(-1)}}\");\n\
             }}",
    )) {
        assert_eq!(out, "21\n-1\n");
    }
}

#[test]
fn e2e_wp_closure_question_drains_body_local_on_fail_edge() {
    // A heap body-local live at the `?`: the fail edge's bounded drain
    // frees it (then pops the provider) before joining the merge —
    // asan twin covers the leak check; this pins the values.
    if let Some(out) = run_program(&format!(
        "{WP_PREAMBLE}\
             fn might_fail(x: i64) -> Result[i64, String] {{\n\
                 if x > 5 {{ return Err(\"big\"); }}\n\
                 Ok(x)\n\
             }}\n\
             fn probe(n: i64) -> i64 with reads(Ctr) {{\n\
                 let r = with_provider[Ctr](InMem {{ n: n }}, || {{\n\
                     let pad = f\"pad-{{read()}}\";\n\
                     let v = might_fail(read())?;\n\
                     Ok(v + pad.len())\n\
                 }});\n\
                 match r {{\n\
                     Ok(v) => v,\n\
                     Err(_) => -1,\n\
                 }}\n\
             }}\n\
             fn main() with reads(Ctr) {{\n\
                 println(f\"{{probe(3)}}\");\n\
                 println(f\"{{probe(9)}}\");\n\
             }}",
    )) {
        assert_eq!(out, "8\n-1\n");
    }
}

#[test]
fn e2e_wp_closure_return_provider_stack_healthy_after() {
    // The retargeted return must still pop the provider frame: a second
    // with_provider after the first must resolve its own provider, and
    // an outer resource read after an inner early-return must see the
    // OUTER provider again.
    if let Some(out) = run_program(&format!(
        "{WP_PREAMBLE}\
             fn f() -> i64 with reads(Ctr) {{\n\
                 let a = with_provider[Ctr](InMem {{ n: 5 }}, || {{ return read(); }});\n\
                 let b = with_provider[Ctr](InMem {{ n: 6 }}, || {{ read() }});\n\
                 a * 10 + b\n\
             }}\n\
             fn main() with reads(Ctr) {{ println(f\"{{f()}}\"); }}",
    )) {
        assert_eq!(out, "56\n");
    }
}

#[test]
fn test_e2e_iter_adaptor_closure_live_capture_parity() {
    // B-2026-07-14-20: an adaptor predicate that reads a variable the
    // LOOP BODY mutates sees the LIVE value each iteration (design.md
    // § Closures Rule 2: read → capture by reference). Codegen's fused
    // inlining always had this behavior; the interpreter now matches
    // (its adaptor closures capture via live SharedCell aliases). This
    // e2e pins the codegen side of the parity — the interpreter twins
    // are `test_iter_adaptor_closure_reads_live_captured_var` /
    // `test_iter_take_while_reads_live_captured_var`, and the plain
    // stored-closure snapshot guard is
    // `test_plain_stored_closure_keeps_snapshot_semantics` (both
    // backends keep snapshot semantics there — pinned by the last
    // println).
    if let Some(out) = run_program(
        "fn main() {\n\
                 let xs: Vec[i64] = [1, 2, 3, 10, 4];\n\
                 let mut lim: i64 = 5;\n\
                 let mut s: i64 = 0;\n\
                 for x in xs.iter().filter(|v| v < lim) {\n\
                     lim = lim - 1;\n\
                     s = s + x;\n\
                 }\n\
                 println(s);\n\
                 let ys: Vec[i64] = [1, 2, 3, 4, 5, 6];\n\
                 let mut seen: i64 = 0;\n\
                 let mut t: i64 = 0;\n\
                 for x in ys.iter().take_while(|v| seen < 3) {\n\
                     seen = seen + 1;\n\
                     t = t + x;\n\
                 }\n\
                 println(t);\n\
                 let mut k: i64 = 1;\n\
                 let f = |x| x + k;\n\
                 k = 10;\n\
                 println(f(1));\n\
             }\n",
    ) {
        assert_eq!(out, "3\n6\n2\n");
    }
}

#[test]
fn test_e2e_closure_option_returning_method_tail() {
    // B-2026-07-15-17: a closure whose tail is an Option-returning method
    // call (`|k, v| m.insert(k, v)` — Map.insert -> Option[V]; `|vv|
    // vv.pop()` — Vec.pop -> Option[T]) must declare its fn return type as
    // the 4-word Option layout, not the i64 fallback (which failed the LLVM
    // verifier "return type does not match operand type of return inst").
    // Covers building the closure AND consuming the returned Option.
    if let Some(out) = run_program(
        "fn main() {\n\
                 let mut m: Map[String, i64] = Map.new();\n\
                 m.insert(\"a\".to_string(), 1);\n\
                 let put = |k: String, v: i64| m.insert(k, v);\n\
                 let old = put(\"a\".to_string(), 9);\n\
                 match old { Some(x) => println(x), None => println(0), }\n\
                 let mut v: Vec[i64] = Vec.new();\n\
                 v.push(7); v.push(8);\n\
                 let take_last = |vv: Vec[i64]| vv.pop();\n\
                 match take_last(v) { Some(x) => println(x), None => println(-1), }\n\
             }",
    ) {
        // insert over an existing key returns the old value Some(1); pop → Some(8)
        assert_eq!(out, "1\n8\n");
    }
}

#[test]
fn test_e2e_option_result_combinators_closure() {
    // B-2026-07-14-6: the closure combinators whose codegen uses only the
    // present payload `T` — `map_or`, `and_then`, `filter` — lowered like
    // `map` (reconstruct payload → invoke closure → branch/phi). Scalar
    // payload. Must match the interpreter oracle.
    if let Some(out) = run_program(
        "fn main() {\n\
                 let s: Option[i64] = Some(5);\n\
                 println(s.map_or(0, |x: i64| x + 1));\n\
                 let n: Option[i64] = None;\n\
                 println(n.map_or(0, |x: i64| x + 1));\n\
                 let s2: Option[i64] = Some(5);\n\
                 println(s2.and_then(|x: i64| Some(x + 1)).unwrap_or(0));\n\
                 let n2: Option[i64] = None;\n\
                 println(n2.and_then(|x: i64| Some(x + 1)).unwrap_or(0 - 7));\n\
                 let s3: Option[i64] = Some(5);\n\
                 println(s3.filter(|x: i64| x > 3).unwrap_or(0));\n\
                 let s4: Option[i64] = Some(2);\n\
                 println(s4.filter(|x: i64| x > 3).unwrap_or(0 - 1));\n\
                 let r: Result[i64, i64] = Ok(5);\n\
                 println(r.and_then(|x: i64| Ok(x * 2)).unwrap_or(0));\n\
                 let er: Result[i64, i64] = Err(3);\n\
                 println(er.map_err(|x: i64| x * 10).unwrap_err());\n\
                 let ok: Result[i64, i64] = Ok(7);\n\
                 println(ok.map_err(|x: i64| x * 10).unwrap_or(0));\n\
                 let no: Option[i64] = None;\n\
                 println(no.unwrap_or_else(|| 42));\n\
                 let so: Option[i64] = Some(5);\n\
                 println(so.map_or_else(|| 0, |x: i64| x * 2));\n\
                 let no2: Option[i64] = None;\n\
                 println(no2.or_else(|| Some(9)).unwrap_or(0));\n\
                 let re: Result[i64, i64] = Err(7);\n\
                 println(re.unwrap_or_else(|x: i64| x + 1));\n\
                 let re2: Result[i64, i64] = Err(3);\n\
                 println(re2.map_or_else(|x: i64| x * 100, |x: i64| x + 1));\n\
                 let re3: Result[i64, i64] = Err(9);\n\
                 println(re3.or_else(|x: i64| Ok(x * 2)).unwrap_or(0));\n\
             }",
    ) {
        assert_eq!(
            out,
            "6\n0\n6\n-7\n5\n-1\n10\n30\n7\n42\n10\n9\n8\n300\n18\n"
        );
    }
}

#[test]
fn test_e2e_option_result_combinators_nonclosure() {
    // B-2026-07-14-6: `ok`/`err` (Result→Option), `or`/`and` (select),
    // `ok_or` (Option→Result), `flatten` (Option un-nest) — the closure-free
    // combinator batch. Option and Result share the type-erased 4-word
    // layout, so these lower to tag manipulations / selects on the shared
    // struct. Must match the interpreter oracle.
    // Fresh operand per call: the combinators consume `self` (and the eager
    // arg), and `Option[i64]` is not Copy, so reusing a moved receiver/arg
    // would (correctly) fail the ownership checker.
    if let Some(out) = run_program(
        "fn main() {\n\
                 let r: Result[i64, i64] = Ok(5);\n\
                 println(r.ok().unwrap_or(0));\n\
                 let e: Result[i64, i64] = Err(7);\n\
                 println(e.ok().unwrap_or(0));\n\
                 let e2: Result[i64, i64] = Err(7);\n\
                 println(e2.err().unwrap_or(0));\n\
                 let r2: Result[i64, i64] = Ok(5);\n\
                 println(r2.err().unwrap_or(0));\n\
                 let n: Option[i64] = None;\n\
                 println(n.or(Some(9)).unwrap_or(0));\n\
                 let s: Option[i64] = Some(9);\n\
                 println(s.and(Some(3)).unwrap_or(0));\n\
                 let n2: Option[i64] = None;\n\
                 println(n2.and(Some(9)).unwrap_or(-1));\n\
                 let s2: Option[i64] = Some(9);\n\
                 println(s2.ok_or(99).unwrap());\n\
                 let n3: Option[i64] = None;\n\
                 println(n3.ok_or(99).unwrap_err());\n\
                 let oo: Option[Option[i64]] = Some(Some(42));\n\
                 println(oo.flatten().unwrap_or(0));\n\
                 let on: Option[Option[i64]] = Some(None);\n\
                 println(on.flatten().unwrap_or(-1));\n\
             }",
    ) {
        assert_eq!(out, "5\n0\n7\n0\n9\n3\n-1\n9\n99\n42\n-1\n");
    }
}

#[test]
fn test_e2e_iter_fold_capture_mutation_inlined() {
    // B-2026-07-11-23 — a capture-MUTATING closure in a `fold` terminal. The
    // fold codegen INLINES the body into the fused loop, so the mutation of
    // the captured `count` propagates (design-correct). Guards that the new
    // "stored closure that mutates a capture" codegen refusal does NOT fire
    // for the inlined terminals (which never construct a closure value).
    if let Some(out) = run_program(
        "fn main() {\n\
                 let v: Vec[i64] = [1, 2, 3, 4];\n\
                 let mut count = 0;\n\
                 let s = v.iter().fold(0, |a, x| { count = count + 1; a + x });\n\
                 println(f\"{s}\");\n\
                 println(f\"{count}\");\n\
             }",
    ) {
        assert_eq!(out, "10\n4\n");
    }
}

#[test]
fn test_stored_mutating_closure_by_ref_capture() {
    // B-2026-07-11-23 — a NON-escaping STORED closure that mutates a captured
    // local now captures that local BY REFERENCE (a `ptr` to the outer slot
    // in the env; the body reads / writes through it), so the mutation
    // propagates to the outer binding and codegen agrees with the
    // interpreter's shared-cell semantics. `f(3); f(4)` over `c = c + x`
    // yields 7. (Correctness pin; the ASAN/leak gate lives in
    // tests/memory_sanitizer.rs::asan_closure_mut_ref_capture_no_leak.)
    if let Some(out) = run_program(
        "fn main() {\n\
                 let mut c: i64 = 0;\n\
                 let f = |x: i64| { c = c + x; };\n\
                 f(3i64);\n\
                 f(4i64);\n\
                 println(f\"{c}\");\n\
             }",
    ) {
        assert_eq!(out.trim(), "7");
    }
}

#[test]
fn test_escaping_mutating_closure_still_rejected_in_codegen() {
    // B-2026-07-11-23 — the by-reference capture is sound only for a
    // NON-escaping closure (the outer slot must outlive it). A closure that
    // BOTH mutates a captured local AND escapes via the function return
    // would dangle, so it is still refused loudly rather than compiled to a
    // use-after-free.
    let err = ir_result(
        "fn mk() -> Fn(i64) {\n\
                 let mut c: i64 = 0;\n\
                 |x: i64| { c = c + x; }\n\
             }\n\
             fn main() { let f = mk(); println(f\"done\"); }",
    )
    .expect_err("expected a codegen error for an escaping capture-mutating closure");
    assert!(
        err.contains("mut ref") && err.contains("escapes"),
        "expected a loud escaping-mut-ref-capture refusal, got: {err}"
    );
}

#[test]
fn test_e2e_iter_chain_wildcard_closure_param() {
    // B-2026-07-11-19 — `|_|` wildcard closure params on the fused-chain
    // terminals. The interpreter already accepted them; codegen's collect /
    // fold / any / all engines required a `PatternKind::Binding` and bailed on
    // a wildcard (`map(|_| 7).collect()` -> "no handler for collect"), a
    // run-vs-build divergence. Codegen now binds a `_` param to a fresh
    // throwaway name, both in the map/filter ADAPTERS and the terminal
    // closures (`fold(0, |a, _| a + 1)` count idiom, `any(|_| ..)`).
    if let Some(out) = run_program(
        "fn main() {\n\
                 let v: Vec[i64] = [1, 2, 3, 4];\n\
                 let m: Vec[i64] = v.iter().map(|_| 7).collect();\n\
                 println(f\"{m.len()}\");\n\
                 let k: Vec[i64] = v.iter().filter(|_| true).collect();\n\
                 println(f\"{k.len()}\");\n\
                 println(f\"{v.iter().fold(0, |a, _| a + 1)}\");\n\
                 println(f\"{v.iter().filter(|x| x > 2).fold(0, |a, _| a + 1)}\");\n\
                 println(f\"{v.iter().any(|_| true)}\");\n\
                 println(f\"{v.iter().map(|_| 1).all(|x| x == 1)}\");\n\
             }",
    ) {
        // m.len()=4, k.len()=4, count=4, filtered-count(>2)=2, any=true, all(==1)=true
        assert_eq!(out, "4\n4\n4\n2\ntrue\ntrue\n");
    }
}

#[test]
fn test_e2e_closure_unannotated_param_inferred_from_body() {
    // B-2026-07-12-10: a let-bound closure with an un-annotated numeric
    // param inferred from its arithmetic body (`|x| x + 1` -> Fn(i64) ->
    // i64). build == run — the closure's Function type carries the solved
    // `i64`, so codegen lays out the param at the right width.
    if let Some(out) = run_program(
        "fn main() {\n\
                 let f = |x| x + 1;\n\
                 println(f(5));\n\
                 let g = |x| x * 2;\n\
                 println(g(10));\n\
                 let h = |y| y + 1.5;\n\
                 println(h(2.0));\n\
             }",
    ) {
        assert_eq!(out, "6\n20\n3.5\n");
    }
}

#[test]
fn test_e2e_closure_param_inferred_from_call_site() {
    // B-2026-07-12-20: a let-bound closure with an un-annotated param and NO
    // body constraint (`let id = |x| x`) infers the param from the call
    // site. The call-site solve lands after the closure literal's type is
    // recorded, so `finalize_closure_expr_types` re-resolves the recorded
    // `Fn` type through the substitutions — without it codegen defaulted the
    // param to i64 and a `String` identity closure failed module
    // verification. build == run; the `String` closure is heap and
    // valgrind-clean.
    if let Some(out) = run_program(
        "fn main() {\n\
                 let id = |x| x;\n\
                 let n: i64 = id(5);\n\
                 println(n + 1);\n\
                 let s = |y| y;\n\
                 println(s(\"hello\"));\n\
             }",
    ) {
        assert_eq!(out, "6\nhello\n");
    }
}

#[test]
fn test_e2e_owned_struct_option_shared_field_captured_from_builder() {
    // #48 (phase-12 self-hosting, parser stage): an OWNED (non-`shared`)
    // struct carrying an `Option[shared T]` field by value — the value-
    // struct `Block { stmts, tail: Option[Expr], span }` shape — built in
    // a helper and RETURNED, then wrapped in the shared enum and read,
    // SIGSEGV'd at the tail read. The capture-inc for an `Option[shared]`
    // field value was wired only into the SHARED-struct literal path
    // (`compile_struct_init`); the non-shared path inserted the field
    // without inc'ing, so the source local's scope-exit
    // `FreeInlineOptionPayload` dec dropped the inner `Expr` to refcount 0
    // and freed it before the caller read it (a use-after-free: the tail
    // read garbage / crashed). `compile_struct_init`'s non-shared branch
    // now mirrors the shared branch's `emit_rc_inc_for_captured_option`.
    // The builder-fn return is essential to the repro — an inline literal
    // in the same scope balances by luck; the cross-fn move exposes it.
    if let Some(out) = run_program(
            "struct Span { line: i64, column: i64, offset: i64, length: i64 }\n\
             enum Stmt { Empty }\n\
             shared enum Expr { Num(i64), Blk(Block), Error }\n\
             struct Block { stmts: Vec[Stmt], tail: Option[Expr], span: Span }\n\
             fn mk() -> Block {\n\
                 let s: Vec[Stmt] = [];\n\
                 let e = Expr.Num(7);\n\
                 let tail: Option[Expr] = Some(e);\n\
                 Block { stmts: s, tail: tail, span: Span { line: 0, column: 0, offset: 0, length: 5 } }\n\
             }\n\
             fn render_block(b: Block) -> String {\n\
                 let Block { stmts, tail, span } = b;\n\
                 match tail { Some(e) => render_expr(e), None => \"no-tail\".to_string() }\n\
             }\n\
             fn render_expr(e: Expr) -> String {\n\
                 match e {\n\
                     Num(n) => n.to_string(),\n\
                     Blk(b) => render_block(b),\n\
                     Error => \"error\".to_string(),\n\
                 }\n\
             }\n\
             fn main() {\n\
                 let blk = mk();\n\
                 println(render_expr(Expr.Blk(blk)));\n\
             }",
        ) {
            assert_eq!(out, "7\n");
        }
}

#[test]
fn e2e_iter_adaptor_destructuring_closure_collect_codegen() {
    // B-2026-07-04-2 sub-part 2 (the destructuring half): a `map`/`filter`
    // closure with a TUPLE-destructuring param — `enumerate().map(|(i, x)|
    // …)`, `pairs.iter().map(|(a, b)| …)` — fell through to the loud
    // dispatch-fail under `karac build` (the pipeline accepted only a
    // single-`Binding` param), though `karac run` handled it. The fix binds
    // a fresh `__dp` to the element and desugars the destructuring into
    // leading `let`s in a block body, reusing the single-binding pipeline.
    // Exercises: POD enumerate map, a heap keep-element map (`|(i, s)| s`
    // over `Vec[String]`, source survives), a `filter().map()` over the
    // enumerate index, a wildcard sub-pattern, and a direct `Vec[(i64,i64)]`
    // tuple source with no enumerate.
    if let Some(out) = run_program(
        r#"
fn main() {
    let v: Vec[i64] = Vec[10i64, 20i64, 30i64];
    let a: Vec[i64] = v.iter().enumerate().map(|(i, x)| i + x).collect();
    println(f"{a.len()} {a[0]} {a[1]} {a[2]}");
    let words: Vec[String] = Vec["aa".to_string(), "bb".to_string(), "cc".to_string()];
    let b: Vec[String] = words.iter().enumerate().map(|(i, s)| s).collect();
    println(f"{b.len()} {b[0]}{b[2]} {words.len()}");
    let c: Vec[i64] = v.iter().enumerate().filter(|(i, x)| i % 2i64 == 0i64).map(|(i, x)| x).collect();
    println(f"{c.len()} {c[0]} {c[1]}");
    let d: Vec[i64] = v.iter().enumerate().map(|(_, x)| x * 2i64).collect();
    println(f"{d[0]} {d[2]}");
    let pairs: Vec[(i64, i64)] = Vec[(1i64, 10i64), (2i64, 20i64)];
    let e: Vec[i64] = pairs.iter().map(|(k, val)| k + val).collect();
    println(f"{e[0]} {e[1]}");
}
"#,
    ) {
        assert_eq!(out, "3 10 21 32\n3 aacc 3\n2 10 30\n20 60\n11 22\n");
    }
}

#[test]
fn test_e2e_with_provider_nested_block_closures_two_resources() {
    // Two trait-less user resources via NESTED `with_provider`, where each
    // provider is bound in the OUTER block and the closures are
    // block-bodied. The eager pre-pass must inherit the outer-block
    // provider bindings into the nested-block scan — it used to reset the
    // binding map per block, so the inner `with_provider[ResB](pb, ...)`
    // couldn't resolve `pb` and dropped the override ("no method order for
    // ResB"). `karac run` prints 1 then 2.
    let out = run_program(
        r#"
effect resource ResA;
effect resource ResB;
struct A { t: i64 }
impl A { fn now(self) -> i64 { self.t } }
struct B { t: i64 }
impl B { fn now(self) -> i64 { self.t } }
fn main() reads(ResA) reads(ResB) {
    let pa = A { t: 1 };
    let pb = B { t: 2 };
    with_provider[ResA](pa, || {
        with_provider[ResB](pb, || {
            println(ResA.now());
            println(ResB.now());
        });
    });
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "1\n2");
    }
}

// ── Closure compilation ───────────────────────────────────────────

#[test]
fn test_ir_closure_simple() {
    let ir = ir_for(
        r#"
fn main() {
    let f = |x: i64| x + 1;
    println(f(5));
}
"#,
    );
    // A closure function should be generated.
    assert!(
        ir.contains("__closure_0"),
        "should define a closure function"
    );
    // The closure function pointer is extracted from the fat-pointer struct.
    assert!(
        ir.contains("extractvalue"),
        "should extract fn_ptr/env_ptr from fat pointer"
    );
}

/// A let-bound fn value and a direct-argument reify of the same fn share one
/// memoized `__karac_fnval_<name>` trampoline (a single `define`).
#[test]
fn fn_value_let_and_arg_reify_share_one_trampoline() {
    let ir = ir_for(
        "fn doubler(n: i64) -> i64 { n * 2i64 }\n\
             fn apply(f: Fn(i64) -> i64, x: i64) -> i64 { f(x) }\n\
             fn main() {\n\
                 let g = doubler;\n\
                 let a = apply(g, 1i64);\n\
                 let b = apply(doubler, 2i64);\n\
                 println(f\"{a + b}\");\n\
             }\n",
    );
    let tramp_defs = ir
        .lines()
        .filter(|l| l.starts_with("define") && l.contains("@__karac_fnval_doubler"))
        .count();
    assert_eq!(tramp_defs, 1, "expected one memoized trampoline:\n{ir}");
}

#[test]
fn e2e_closure_literal_with_a_ref_vec_param_indexes_it() {
    // B-2026-08-05-15 leg 2, the CALLEE-side mirror: a closure param's side
    // tables were registered from the BORROW type, and
    // `register_var_from_type_expr` has no `Ref` arm — so `w` never reached
    // `vec_elem_types` and `w[i]` failed to build with "Index operator
    // applied to non-array type".
    let out = run_program(
        "fn main() {\n\
                 let mut v: Vec[u8] = Vec.new();\n\
                 let mut k = 0i64;\n\
                 while k < 4i64 { v.push((k + 1i64) as u8); k = k + 1i64; }\n\
                 let f = |w: ref Vec[u8], i: i64| { return w[i] as i64; };\n\
                 println(f(v, 2i64));\n\
             }",
    );
    assert_eq!(out, Some("3\n".to_string()));
}

/// Bonus path the same fix enables: a closure *literal* flowing into a
/// `Fn(...)` parameter slot. The param now lowers to the fat-pointer ABI
/// and the body call routes through the indirect-call path, so the closure
/// value passes through and runs (no reify needed — it is already a fat
/// pointer).
#[test]
fn fn_value_closure_literal_into_fn_typed_param_run() {
    let out = run_program(
        "fn apply(f: Fn(i64) -> i64, x: i64) -> i64 { f(x) }\n\
             fn main() { println(f\"{apply(|n| n * 2i64, 21i64)}\"); }\n",
    );
    assert_eq!(out.as_deref(), Some("42\n"));
}

/// The reify path synthesizes a per-fn env-ignoring trampoline so the bare
/// fn name conforms to the env-first closure-call ABI.
#[test]
fn test_ir_fn_value_named_fn_reified_trampoline() {
    let ir = ir_for(
        "fn doubler(n: i64) -> i64 { n * 2i64 }\n\
             fn apply(f: Fn(i64) -> i64, x: i64) -> i64 { f(x) }\n\
             fn main() { let r = apply(doubler, 21i64); println(f\"{r}\"); }\n",
    );
    assert!(
        ir.contains("__karac_fnval_doubler"),
        "should synthesize an env-ignoring trampoline for the bare fn value:\n{ir}"
    );
}

#[test]
fn test_ir_closure_no_captures() {
    let ir = ir_for(
        r#"
fn main() {
    let double = |x: i64| x * 2;
    println(double(3));
}
"#,
    );
    assert!(ir.contains("__closure_0"));
    assert!(ir.contains("smul.with.overflow"));
}

#[test]
fn test_ir_closure_captures_variable() {
    let ir = ir_for(
        r#"
fn main() {
    let base = 10;
    let add_base = |x: i64| x + base;
    println(add_base(5));
}
"#,
    );
    // Closure should be generated and capture `base`.
    assert!(
        ir.contains("__closure_0"),
        "should define a closure function"
    );
    // The function takes an env pointer and the param.
    assert!(
        ir.contains("sadd.with.overflow") || ir.contains("add i64"),
        "should add"
    );
}

#[test]
fn test_ir_closure_two_params() {
    let ir = ir_for(
        r#"
fn main() {
    let add = |x: i64, y: i64| x + y;
    println(add(3, 4));
}
"#,
    );
    assert!(ir.contains("__closure_0"));
}

#[test]
fn test_ir_closure_float() {
    let ir = ir_for(
        r#"
fn main() {
    let scale = |x: f64| x * 2.0;
    println(scale(3.0));
}
"#,
    );
    assert!(ir.contains("__closure_0"));
    assert!(ir.contains("fmul"));
}

#[test]
fn test_ir_closure_bool_return() {
    let ir = ir_for(
        r#"
fn main() {
    let is_pos = |x: i64| x > 0;
    println(is_pos(5));
}
"#,
    );
    assert!(ir.contains("__closure_0"));
    assert!(ir.contains("icmp sgt") || ir.contains("sgt"));
}

#[test]
fn test_ir_closure_passed_to_function() {
    // Test that a closure can be passed to a function and called.
    let ir = ir_for(
        r#"
fn apply(f: i64, x: i64) -> i64 {
    f
}
fn main() {
    let double = |x: i64| x * 2;
    let result = apply(double(3), 0);
    println(result);
}
"#,
    );
    assert!(ir.contains("__closure_0"));
}

// ── Closure end-to-end execution tests ───────────────────────────

#[test]
fn test_e2e_closure_identity() {
    let out = run_program(
        r#"
fn main() {
    let f = |x: i64| x;
    println(f(42));
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "42");
    }
}

#[test]
fn test_e2e_closure_add_one() {
    let out = run_program(
        r#"
fn main() {
    let inc = |x: i64| x + 1;
    println(inc(7));
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "8");
    }
}

#[test]
fn test_e2e_closure_multiply() {
    let out = run_program(
        r#"
fn main() {
    let triple = |x: i64| x * 3;
    println(triple(4));
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "12");
    }
}

#[test]
fn test_e2e_closure_captures_outer() {
    let out = run_program(
        r#"
fn main() {
    let offset = 100;
    let add_offset = |x: i64| x + offset;
    println(add_offset(5));
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "105");
    }
}

#[test]
fn test_e2e_closure_multiple_captures() {
    let out = run_program(
        r#"
fn main() {
    let a = 3;
    let b = 7;
    let combine = |x: i64| x + a + b;
    println(combine(0));
    println(combine(10));
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["10", "20"]);
    }
}

#[test]
fn test_e2e_closure_two_params() {
    let out = run_program(
        r#"
fn main() {
    let add = |x: i64, y: i64| x + y;
    println(add(10, 32));
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "42");
    }
}

#[test]
fn test_e2e_closure_two_closures() {
    let out = run_program(
        r#"
fn main() {
    let double = |x: i64| x * 2;
    let add_one = |x: i64| x + 1;
    println(add_one(double(5)));
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "11");
    }
}

#[test]
fn test_e2e_closure_returning_closure_currying() {
    // B-2026-07-12-12 — a closure whose body is itself a closure (currying:
    // `let make = |n| |x| x + n`) failed codegen: the outer closure fn was
    // declared `-> i64` (the closure-return-type default) while its body
    // returned a `{ptr, ptr}` fat pointer → LLVM verifier reject. Fixed by
    // (1) inferring a closure body's return type as the fat-pointer type and
    // (2) heap-allocating the escaping inner env PER outer call so distinct
    // instances don't alias one reused stack env (`make(5)` and `make(10)`
    // must stay independent). interp == JIT == AOT.
    let single = run_program(
        r#"
fn main() {
    let make = |n: i64| |x: i64| x + n;
    let add5 = make(5);
    println(add5(10));
}
"#,
    );
    if let Some(out) = single {
        assert_eq!(out.trim(), "15");
    }
    // Multi-instance: the regression the prior investigation hit — a naive
    // return-type-only fix aliased the env and printed `20` for both.
    let multi = run_program(
        r#"
fn main() {
    let make = |n: i64| |x: i64| x + n;
    let add5 = make(5);
    let add10 = make(10);
    println(add5(10));
    println(add10(10));
}
"#,
    );
    if let Some(out) = multi {
        assert_eq!(out.trim(), "15\n20");
    }
}

#[test]
fn test_e2e_closure_captures_in_loop() {
    let out = run_program(
        r#"
fn main() {
    let step = 5;
    let advance = |x: i64| x + step;
    let mut n = 0;
    for _ in 0..4 {
        n = advance(n);
    }
    println(n);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "20");
    }
}

// Regression test for the 2026-05-29 closure-capturing-String hang
// surfaced by `tests/safety_design.rs::asan_closure_borrow_capture_no_escape`.
// Root cause was a missing `String.from(literal)` codegen branch — the
// call returned the `i64 0` placeholder, `s` was alloca'd as i64, the
// closure body GEP'd Vec layout from the i64 slot, and LLVM DCE'd
// main down to `printf(undef) + brk #0x1` (macOS parked the process
// at the brk, looking like an infinite loop). Fixed in
// `src/codegen/assoc_call.rs` by the explicit String.from passthrough
// branch next to String.new. The test pins the closure-with-String-
// capture shape so the gap doesn't reopen.
#[test]
fn test_e2e_closure_captures_string_calls_len() {
    let out = run_program(
        r#"
fn main() {
    let s = String.from("hello");
    let len_plus = |extra: i64| s.len() + extra;
    println(len_plus(5));
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "10");
    }
}

// ── Disjoint closure capture: per-path env layout (slice 4) ─

// The tests below exercise line 353 phase-5 checklist
// "Disjoint closure capture" slice 4: when the ownership pass
// supplies per-path capture modes, `compile_closure` lays the env
// struct out with one slot per captured `CapturePath` instead of one
// slot per captured root binding. `run_program` is ownership-loaded
// (so the per-path layout is the default-exercised one); the IR
// inspections still need `ir_for_with_ownership` — the plain
// `ir_for` passes `None` for ownership and falls through to the
// per-name layout.

#[test]
fn test_e2e_disjoint_field_capture_returns_leaf_value() {
    // Headline slice-4 case: closure captures a single field of a
    // struct. The env's slot for `p.x` is sized to the leaf type
    // (i64), not the whole struct, and the body stitches the leaf
    // back into a fresh `p` alloca so the body's `p.x` read walks
    // through the normal FieldAccess path.
    let out = run_program_with_ownership(
        r#"
struct Point { x: i64, y: i64 }
fn main() {
    let p = Point { x: 7, y: 11 };
    let read_x = || p.x;
    println(read_x());
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "7");
    }
}

#[test]
fn test_e2e_disjoint_two_closures_over_sibling_fields() {
    // Spec test from the line-353 entry: "two closures over different
    // fields of the same struct compile and run". Each closure gets
    // its own per-path env with one i64 leaf slot; the outer code
    // calls both and sums the results.
    let out = run_program_with_ownership(
        r#"
struct Point { x: i64, y: i64 }
fn main() {
    let p = Point { x: 7, y: 11 };
    let read_x = || p.x;
    let read_y = || p.y;
    println(read_x() + read_y());
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "18");
    }
}

#[test]
fn test_e2e_disjoint_field_capture_with_outer_sibling_access() {
    // Spec test: "outer-scope access to u.history after a closure
    // captured u.name is permitted". The ownership pass (slice 3)
    // accepts this; codegen (slice 4) emits a per-path env that
    // captures just `p.x`, leaving `p.y` accessible in the outer
    // scope. Verifies the full pipeline composes.
    let out = run_program_with_ownership(
        r#"
struct Point { x: i64, y: i64 }
fn main() {
    let p = Point { x: 7, y: 11 };
    let read_x = || p.x;
    let saved_y = p.y;
    println(read_x() + saved_y);
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "18");
    }
}

#[test]
fn test_e2e_disjoint_capture_two_fields_under_one_root() {
    // A single closure that captures two disjoint sub-paths under
    // the same root. Without slice 4 this collapsed into a whole-
    // root capture; with slice 4 the env carries two i64 slots
    // (one for `p.x`, one for `p.y`) and the body stitches both
    // into a fresh `p` alloca.
    let out = run_program_with_ownership(
        r#"
struct Point { x: i64, y: i64 }
fn main() {
    let p = Point { x: 7, y: 11 };
    let sum = || p.x + p.y;
    println(sum());
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "18");
    }
}

#[test]
fn test_e2e_disjoint_capture_nested_field_path() {
    // Multi-segment projection (`o.a.v`). The path resolver walks
    // both struct steps via `struct_field_names` lookups; the
    // capture-site GEP chain has length 2; the body stitches the
    // leaf back into the matching nested position of a fresh `o`
    // alloca.
    let out = run_program_with_ownership(
        r#"
struct Inner { v: i64 }
struct Outer { a: Inner, b: Inner }
fn main() {
    let o = Outer { a: Inner { v: 3 }, b: Inner { v: 5 } };
    let f = || o.a.v;
    let g = || o.b.v;
    println(f() + g());
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "8");
    }
}

#[test]
fn test_e2e_method_call_on_captured_root_uses_whole_root_layout() {
    // Slice 1's path scanner commits a whole-root capture when it
    // hits a stopping construct (method call on the captured
    // binding). Slice 4 honours that — the env carries one slot of
    // the full struct type, not per-path. End-to-end check: the
    // method call inside the closure reads through the captured
    // whole-root alloca and returns the right value.
    let out = run_program_with_ownership(
        r#"
struct Point { x: i64, y: i64 }
impl Point { fn doubled_x(self) -> i64 { self.x + self.x } }
fn main() {
    let p = Point { x: 7, y: 11 };
    let f = || p.doubled_x();
    println(f());
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "14");
    }
}

#[test]
fn test_ir_disjoint_field_capture_env_slot_is_leaf_type() {
    // IR pin: the synthesized closure body's env-struct load has a
    // single-i64 element type, not the `Point` struct type. This is
    // the wire-format change slice 4 introduces — without it the
    // env carries the whole root (3-i64 word for `{x, y, padding}`
    // depending on alignment) which forces extra copies at both
    // capture and unpack.
    let ir = ir_for_with_ownership(
        r#"
struct Point { x: i64, y: i64 }
fn main() {
    let p = Point { x: 7, y: 11 };
    let read_x = || p.x;
    let _ = read_x();
}
"#,
    );
    // Env load inside the synthesized closure body is typed `{ i64 }`,
    // not `{ i64, i64 }` (the whole-Point shape). The closure body
    // also contains a `cap.gep` GEP for the stitching write into the
    // fresh `p` alloca.
    assert!(
        ir.contains("load { i64 }"),
        "expected env load typed `{{ i64 }}` (single leaf slot) in:\n{}",
        ir
    );
    assert!(
        ir.contains("cap.gep"),
        "expected stitching GEP `cap.gep.<n>` in the closure body in:\n{}",
        ir
    );
}

#[test]
fn test_ir_method_call_on_captured_root_forces_whole_root_env() {
    // Companion to the e2e test above: at the IR level, the env
    // struct for a closure whose body method-calls a captured root
    // carries the whole Point struct (not a per-path layout) — the
    // slice-1 path scanner committed `(p, [])` because method calls
    // are stopping constructs, and slice 4 honoured that.
    let ir = ir_for_with_ownership(
        r#"
struct Point { x: i64, y: i64 }
impl Point { fn doubled_x(self) -> i64 { self.x + self.x } }
fn main() {
    let p = Point { x: 7, y: 11 };
    let f = || p.doubled_x();
    let _ = f();
}
"#,
    );
    // Whole-root capture means the env's single slot is the full
    // Point struct (two i64 words), so the body's env load is typed
    // `{ { i64, i64 } }` — the outer braces are the env struct, the
    // inner are the captured Point.
    assert!(
        ir.contains("load { { i64, i64 } }"),
        "expected env load typed `{{ {{ i64, i64 }} }}` (whole-root Point) in:\n{}",
        ir
    );
}

#[test]
fn test_e2e_disjoint_capture_preserves_uncaptured_fields_after_call() {
    // Stress test: the per-path env carries only `p.x`, leaves
    // `p.y` unpopulated in the closure body's stitched alloca. The
    // closure body only reads `p.x` (ownership-checked), so the
    // undef `p.y` is never touched. After the call returns, the
    // outer scope's `p.y` is still its original value. Pins that
    // outer-scope state is not perturbed by the per-path stitching.
    let out = run_program_with_ownership(
        r#"
struct Point { x: i64, y: i64 }
fn main() {
    let p = Point { x: 7, y: 11 };
    let f = || p.x;
    let after_call = f();
    println(after_call);
    println(p.y);
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["7", "11"]);
    }
}

/// B-2026-08-10-1 — a NESTED indexed store releases the element it
/// overwrites, as the single-index store already did.
///
/// `compile_nested_vec_vec_index_store` ended in a bare `build_store`, so
/// `d[0][0] = f"zz"` orphaned the old String's buffer while the
/// single-index `r[0] = f"zz"` freed it. Both paths now go through one
/// helper, which is the point — the leak existed because the nested path
/// never grew a copy of a discipline the other has carried since
/// B-2026-06-19-7, and a second copy would have drifted the same way.
///
/// This test asserts VALUES, not memory: the leak itself is invisible in
/// output and is gated by the ASAN fixture (whose memory half needs the
/// `-O0` leg, since at the default level the orphaned allocation is dead
/// and LLVM deletes it). What these cases catch is the release going
/// WRONG rather than missing — freeing a buffer that is still live shows
/// up here as a wrong or empty string.
///
/// Case 2 is the struct-FIELD base, which only reaches this path because
/// B-2026-08-09-21 routed it here. Case 3 is the self-alias `d[0][0] =
/// d[0][0]`, the shape where a release-before-store would free the very
/// buffer being read. Case 4 is a scalar leaf, which must take the
/// no-release path untouched.
/// B-2026-08-10-4 — `split_at_mut` (design.md "`split_at_mut` — disjoint
/// mutable partition"), which was specified in full and implemented
/// nowhere. Both halves are `mut Slice[T]` views over the receiver's own
/// buffer, so a write through either lands in the caller's collection.
///
/// Both receivers the spec names are pinned — `Vec[T]` and `mut Slice[T]`
/// — because they take different paths to the base pointer, and the Slice
/// one is where the interpreter twin got it wrong: an aliasing bug there
/// is INVISIBLE except by writing through a half and reading back through
/// the original, since the returned lengths are correct either way.
///
/// The halves are passed to a function rather than written as
/// `parts.0[0] = x`. That spelling is a separate PRE-EXISTING codegen gap
/// — index-assign through a tuple field whose type is a Slice — which
/// reproduces without `split_at_mut` via `(v.as_slice_mut(), 0)` and is
/// filed on its own row.
/// B-2026-08-10-5 — indexing a SLICE-typed tuple field, read and write.
///
/// `t.0[i]` was refused by codegen whenever the field's type was a Slice
/// ("Index operator applied to non-array type" on a read, "Index
/// assignment target must be a variable" on a store), while the identical
/// spelling built fine with a Vec-typed field and `--interp` handled both.
///
/// The cause was one gate: the typechecker recorded the element type into
/// `temp_recv_elem_types` only for `Type::Named{Vec|VecDeque}`, and a
/// `Slice[T]` is `Type::Slice`, so codegen's tuple-index arms never fired
/// and the expression fell to the generic tail.
///
/// Case 2 is why this mattered enough to fix rather than document:
/// `split_at_mut` returns a tuple of slices, so writing through a half by
/// index — the obvious use of a mutable partition — hit this on every
/// program. Case 3 is the Vec-typed field, which must keep working: the
/// arms now choose the container shape from the tuple's LLVM field type,
/// and getting that backwards would read a `cap` word off a 16-byte slice
/// slot or miss one on a 24-byte Vec slot.
/// B-2026-08-10-13 — a `sort_by` / `sort_by_key` closure parameter keeps
/// its element type, so the body can call methods and index it.
///
/// `v.sort_by(|x, y| x.len().cmp(y.len()))` over a `Vec[Vec[i64]]` or
/// `Vec[String]` failed codegen with a self-identified dispatch
/// fall-through ("no handler for method 'len' on variable 'x'"), while the
/// interpreter ran it. The comparator params were registered with
/// `record_var_type_name` only — enough for `compile_field_access`, which
/// needs a struct name, and not for method dispatch or index lowering,
/// which resolve through `vec_elem_types` / `var_elem_type_exprs`.
///
/// That is why the corpus never hit it: every earlier `sort_by` compared
/// TUPLE FIELDS (`|a, b| a.0.cmp(b.0)`), and tuple-field access needs no
/// type lookup at all. Case 4 keeps that shape as the control — it also
/// takes a different code path entirely (the mono sort, gated to
/// all-int-field elements), so it proves the fix did not disturb the path
/// that already worked.
///
/// Case 3 is index-in-comparator and cases 5/6 are `sort_by_key`, the
/// sibling method: it had the identical gap and is fixed with it rather
/// than left for the next kata to rediscover.
/// B-2026-08-10-16 — an explicit `return` inside a sort comparator body
/// produces the comparator's result instead of a real function return.
///
/// The body is INLINED into the thunk (or, on the mono path, straight into
/// the sort function), so the fn-level return machinery was wrong twice:
/// it emitted `ret <Ordering>` where the enclosing LLVM function returns
/// `i64` or `void`, and it drained the WHOLE cleanup stack including the
/// CALLER's frames, referencing the caller's allocas from inside the sort
/// function. Module verification reported both — "Found return instr that
/// returns non-void in Function of void return type" and "Instruction does
/// not dominate all uses".
///
/// Case 1 is the mono path (all-int element) and case 2 the thunk path
/// (container element); they are separate emitters and the verifier
/// failure differs between them, so one does not cover the other.
///
/// Cases 3 and 4 are the shapes that already built — implicit block tail
/// and if-expression tail. They matter because the fix routes ALL bodies
/// through the retarget now: if the join mishandled a body that never
/// returns early, these would break while the `return` cases passed.
/// B-2026-08-10-17 — kata #254's own comparator, end to end.
///
/// A lexicographic comparator is the shape that needs an EARLY return from
/// inside a loop: compare element-by-element, exit at the first
/// difference. It could not be written before — the `return`s were checked
/// against the enclosing fn's return type — and could not be BUILT before
/// B-2026-08-10-16, which retargeted the explicit return in a comparator.
///
/// It is pinned here rather than only in the typechecker because the two
/// fixes are only jointly sufficient: -17 makes it typecheck, -16 makes it
/// codegen. This is the case that stayed broken while three successive
/// minimised repros were each fixed, so it is worth an end-to-end test of
/// its own.
/// B-2026-08-10-18 — an explicit `return` inside a fused-iterator closure
/// produces THAT ELEMENT's value and lets the loop continue.
///
/// The emitters splice the closure body into a synthesized loop compiled in
/// the ENCLOSING function, so a `return` there returned from that function.
/// `fold` did not fail at all: it exited `main` on the first element with
/// status 0, silently truncating the program; the other five left a `ret`
/// mid-block and failed LLVM verification.
///
/// Case 1 is the semantic crux and the reason the obvious fix is wrong.
/// Retargeting the `return` to a merge AFTER the loop would compile and
/// look plausible, but it would EXIT the iteration — only one element would
/// be produced. Returning early on exactly one element and asserting the
/// other three are still mapped is what distinguishes "this element's
/// value" from "stop here".
///
/// Case 2 does the same for `fold`, where the distinction is visible in the
/// accumulator: skipping the `3` must still sum 1 + 2 + 4.
#[test]
fn test_e2e_explicit_return_in_a_fused_iter_closure() {
    assert_eq!(
        run_program(
            "fn main() {\n\
                     let v: Vec[i64] = [1i64, 2i64, 3i64, 4i64];\n\
                     let d: Vec[i64] = v.iter()\n\
                         .map(|n| { if n == 2i64 { return 99i64; } n })\n\
                         .collect();\n\
                     let s: i64 = v.iter()\n\
                         .fold(0i64, |acc, n| { if n == 3i64 { return acc; } acc + n });\n\
                     let e: Vec[i64] = v.iter()\n\
                         .filter(|n| { if n == 1i64 { return true; } return n > 3i64; })\n\
                         .collect();\n\
                     println(f\"{d[0]} {d[1]} {d[2]} {d[3]} | {s} | {e[0]} {e[1]}\");\n\
                 }"
        )
        .as_deref(),
        Some("1 99 3 4 | 7 | 1 4\n")
    );
    // The remaining terminals, each with a plain explicit return.
    assert_eq!(
        run_program(
            "fn main() {\n\
                     let v: Vec[i64] = [1i64, 2i64, 3i64];\n\
                     let a: bool = v.iter().any(|n| { return n > 2i64; });\n\
                     let b: bool = v.iter().all(|n| { return n > 0i64; });\n\
                     let mut w: Vec[i64] = v;\n\
                     w.retain(|n| { return n > 1i64; });\n\
                     println(f\"{a} {b} {w.len()}\");\n\
                 }"
        )
        .as_deref(),
        Some("true true 2\n")
    );
    // `fold` again, standalone: this is the arm that used to exit `main`,
    // so the assertion is as much that the LATER println runs at all.
    assert_eq!(
        run_program(
            "fn main() {\n\
                     println(f\"before\");\n\
                     let v: Vec[i64] = [1i64, 2i64, 3i64];\n\
                     let s: i64 = v.iter().fold(0i64, |acc, n| { return acc + n; });\n\
                     println(f\"after {s}\");\n\
                 }"
        )
        .as_deref(),
        Some("before\nafter 6\n")
    );
}

#[test]
fn test_e2e_sort_by_closure_param_keeps_its_element_type() {
    // 1. The row's repro — Vec[Vec[i64]] by length.
    assert_eq!(
        run_program(
            "fn main() {\n\
                     let mut v: Vec[Vec[i64]] = Vec.new();\n\
                     let mut a: Vec[i64] = Vec.new(); a.push(1i64); a.push(2i64); a.push(3i64);\n\
                     let mut b: Vec[i64] = Vec.new(); b.push(9i64);\n\
                     let mut c: Vec[i64] = Vec.new(); c.push(4i64); c.push(5i64);\n\
                     v.push(a); v.push(b); v.push(c);\n\
                     v.sort_by(|x, y| x.len().cmp(y.len()));\n\
                     println(f\"{v[0].len()} {v[1].len()} {v[2].len()}\");\n\
                 }"
        )
        .as_deref(),
        Some("1 2 3\n")
    );
    // 2. Vec[String] by length — a different container element.
    assert_eq!(
        run_program(
            "fn main() {\n\
                     let mut v: Vec[String] = Vec.new();\n\
                     v.push(f\"ccc\"); v.push(f\"a\"); v.push(f\"bb\");\n\
                     v.sort_by(|x, y| x.len().cmp(y.len()));\n\
                     println(f\"{v[0]} {v[1]} {v[2]}\");\n\
                 }"
        )
        .as_deref(),
        Some("a bb ccc\n")
    );
    // 3. INDEX inside the comparator, the other lowering that needs the type.
    assert_eq!(
        run_program(
            "fn main() {\n\
                     let mut v: Vec[Vec[i64]] = Vec.new();\n\
                     let mut a: Vec[i64] = Vec.new(); a.push(5i64);\n\
                     let mut b: Vec[i64] = Vec.new(); b.push(2i64);\n\
                     v.push(a); v.push(b);\n\
                     v.sort_by(|x, y| x[0].cmp(y[0]));\n\
                     println(f\"{v[0][0]} {v[1][0]}\");\n\
                 }"
        )
        .as_deref(),
        Some("2 5\n")
    );
    // 4. CONTROL — tuple fields, which already built and route through the
    //    all-int mono sort rather than the thunk this fix touches.
    assert_eq!(
        run_program(
            "fn main() {\n\
                     let mut v: Vec[(i64, i64)] = Vec.new();\n\
                     v.push((3i64, 1i64)); v.push((1i64, 2i64));\n\
                     v.sort_by(|a, b| a.0.cmp(b.0));\n\
                     println(f\"{v[0].0} {v[1].0}\");\n\
                 }"
        )
        .as_deref(),
        Some("1 3\n")
    );
    // 5/6. `sort_by_key`, the sibling with the same gap.
    assert_eq!(
        run_program(
            "fn main() {\n\
                     let mut v: Vec[String] = Vec.new();\n\
                     v.push(f\"ccc\"); v.push(f\"a\"); v.push(f\"bb\");\n\
                     v.sort_by_key(|x| x.len());\n\
                     println(f\"{v[0]} {v[1]} {v[2]}\");\n\
                 }"
        )
        .as_deref(),
        Some("a bb ccc\n")
    );
    assert_eq!(
        run_program(
            "fn main() {\n\
                     let mut v: Vec[Vec[i64]] = Vec.new();\n\
                     let mut a: Vec[i64] = Vec.new(); a.push(1i64); a.push(2i64);\n\
                     let mut b: Vec[i64] = Vec.new(); b.push(9i64);\n\
                     v.push(a); v.push(b);\n\
                     v.sort_by_key(|x| x.len());\n\
                     println(f\"{v[0].len()} {v[1].len()}\");\n\
                 }"
        )
        .as_deref(),
        Some("1 2\n")
    );
}

#[test]
fn test_e2e_auto_par_captures_indexed_access_base() {
    // `refs_in_expr` was missing an `ExprKind::Index` arm — so
    // `nums[j]` inside a par-branch body didn't walk into `nums`,
    // and `nums` was missed from the capture set. The branch fn
    // then ran with `nums` absent from `self.variables`, panicking
    // at `compile_slice_index`'s `get_data_ptr(name).unwrap()`.
    // Repro shape: function with a Slice param, a Vec/Map
    // declaration (forms an independent par-group with the
    // length binding), and a later block that indexes the slice.
    let out = run_program(
        r#"
fn min_jumps(nums: Slice[i64]) -> i64 {
    let n = nums.len();
    let mut visited: Vec[bool] = Vec.new();
    let mut bucket: Map[i64, Vec[i64]] = Map.new();
    for _ in 0..n { visited.push(false); }
    visited[0] = true;
    let mut sum = 0i64;
    let mut i = 0i64;
    while i < n {
        sum = sum + nums[i];
        i = i + 1;
    }
    let _ = bucket.len();
    sum
}
fn main() {
    let a: Array[i64, 3] = [1, 2, 3];
    println(min_jumps(a));
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "6");
    }
}

#[test]
fn test_ir_nonpar_output_uses_lean_fwrite_not_capture_chokepoint() {
    // B-2026-06-15-2: 1a401c7b routed EVERY console write through the
    // runtime `karac_runtime_write_console` chokepoint, linking the
    // OutputCapture machinery (segs realloc + replay + Drop) into every
    // output-bearing binary — incl. KARAC_AUTO_PAR=0 seq-twins that can
    // never install a capture — a ~17 KiB lean-floor regression. Writes now
    // route through the internal `__karac_write_console` wrapper whose body
    // is defined at finalization: a NON-parallel program (no `karac_par_run`
    // / `karac_par_reduce` site) gets a lean direct `fwrite`, so the runtime
    // chokepoint + capture machinery AOT `-dead_strip`. Assert the wrapper
    // exists, calls `fwrite`, and does NOT call the chokepoint (a `declare`
    // for it may still appear — only a `call` would anchor the machinery).
    let ir = ir_for(
        r#"
fn main() {
    println("hello");
}
"#,
    );
    assert!(
        ir.contains("@__karac_write_console"),
        "console writes must route through the internal wrapper; ir:\n{ir}"
    );
    assert!(
        ir.contains("@fwrite"),
        "a non-parallel program's wrapper must call the lean fwrite; ir:\n{ir}"
    );
    assert!(
        !ir.contains("call void @karac_runtime_write_console"),
        "a non-parallel binary must NOT call the capture chokepoint — that \
             would anchor the OutputCapture machinery (the 1a401c7b lean-floor \
             regression); ir:\n{ir}"
    );
}

#[test]
fn test_e2e_map_entry_or_insert_with_vacant_invokes_closure() {
    // Vacant key → or_insert_with fires the closure to produce default.
    let out = run_program(
        r#"
fn main() {
    let mut m: Map[i64, i64] = Map.new();
    m.entry(1_i64).or_insert_with(|| 17_i64);
    let v = m.get(1_i64);
    match v {
        Some(x) => println(x),
        None => println(0_i64),
    }
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "17");
    }
}

#[test]
fn test_e2e_map_entry_or_insert_with_occupied_skips_closure() {
    // Occupied key → closure does NOT run; map unchanged.
    let out = run_program(
        r#"
fn main() {
    let mut m: Map[i64, i64] = Map.new();
    m.insert(2_i64, 5_i64);
    m.entry(2_i64).or_insert_with(|| 999_i64);
    let v = m.get(2_i64);
    match v {
        Some(x) => println(x),
        None => println(0_i64),
    }
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "5");
    }
}

// ── Slice B follow-up (2026-05-09): fn-pointer-as-free-fn-arg + ──
//                       Server.serve(handler) dispatch
//
// Sub-step (b): free-fn-name-as-value codegen path.
// Sub-step (c): `Server.serve(handler)` dispatcher arm.
// Sub-step (d): closure-as-handler-arg structured rejection.

/// Sub-step (b) pin — `let f = target;` lowers without the
/// "Undefined variable 'target'" diagnostic that fired before this
/// slice. Uses the free-fn name as a value; v1 doesn't track a
/// fn-pointer type for direct calls through the binding, so the
/// test stays at the "binds and compiles" assertion.
#[test]
fn test_free_fn_as_value_emits_fn_ptr() {
    let src = r#"
fn target() -> i64 { 42 }

fn main() {
    let _f = target;
    println(target());
}
"#;
    let mut parsed = karac::parse(src);
    assert!(
        parsed.errors.is_empty(),
        "parse errors: {:?}",
        parsed.errors
    );
    let resolved = karac::resolve(&parsed.program);
    let typed = karac::typecheck(&parsed.program, &resolved);
    karac::lower(&mut parsed.program, &typed);
    let ir = compile_to_ir(&parsed.program, None, None);
    assert!(
        ir.is_ok(),
        "expected free-fn-name-as-value to compile cleanly; got: {:?}",
        ir.err()
    );
}

/// Sub-step (d) pin — passing a closure to `Server.serve(...)` is
/// rejected with the structured `E_CLOSURE_AS_FN_PTR_NOT_YET`
/// diagnostic. Defense-in-depth at the codegen layer; the closure
/// `{ fn_ptr, env_ptr }` ABI doesn't match the FFI extern's bare-
/// pointer parameter slot.
#[test]
fn test_server_serve_rejects_closure_handler() {
    let src = r#"
fn main() {
    let _result = Server.serve("127.0.0.1:0", |req| Response { status: 200, body: "{}" });
}
"#;
    let mut parsed = karac::parse(src);
    assert!(
        parsed.errors.is_empty(),
        "parse errors: {:?}",
        parsed.errors
    );
    let resolved = karac::resolve(&parsed.program);
    let typed = karac::typecheck(&parsed.program, &resolved);
    karac::lower(&mut parsed.program, &typed);
    let err = compile_to_ir(&parsed.program, None, None)
        .expect_err("expected closure-as-handler to be rejected")
        .message;
    assert!(
        err.contains("E_CLOSURE_AS_FN_PTR_NOT_YET"),
        "expected diagnostic to carry E_CLOSURE_AS_FN_PTR_NOT_YET; got: {}",
        err
    );
}

#[test]
fn test_state_struct_type_unions_multi_yield_captures() {
    // Two sequential yields, second one sees a binding introduced
    // after the first. The layout = source-order union `[a, b]`,
    // both `Vec[i64]` — state struct = `{ i32, {ptr,i64,i64},
    // {ptr,i64,i64} }`. Pins that the union shape lowers correctly,
    // not just single-yield single-field cases.
    let ir = ir_for_with_state_struct_layouts(
        "effect resource Network;
             pub fn fetch() with sends(Network) receives(Network) {}
             fn driver(a: Vec[i64]) {
                 fetch();
                 let b: Vec[i64] = a;
                 fetch();
             }",
    );
    let line = ir
        .lines()
        .find(|l| l.starts_with("%kara.state.driver = type {"))
        .unwrap_or_else(|| panic!("no driver state struct type def:\n{ir}"));
    // Two Vec-shaped fields in the type definition line.
    let vec_shape_count =
        line.matches("{ ptr, i64, i64 }").count() + line.matches("{ptr, i64, i64}").count();
    assert_eq!(
        vec_shape_count, 2,
        "expected two inline Vec layouts in unioned state struct: {line}"
    );
}

// ── Phase 6 line 26 slice 8a: captured-locals reload prologue ──────
//
// Each state arm emits a uniform reload prologue: for every captured
// local in the slice-4 layout, GEP into the state-struct field at
// `idx+1` (skipping the tag at field 0), load the value, alloca a
// slot, and store the loaded value into the slot. Bodies still
// terminate with the Pending stub; slice 8b's body-splitting walks
// these allocas for the actual user-code resume.

#[test]
fn test_poll_fn_reload_prologue_emits_gep_load_alloca_store_per_captured_local() {
    // A function with one captured local (`items: Vec[i64]`) has
    // one GEP+load+alloca+store quadruple per state arm. The GEP
    // targets field 1 (skipping the i32 tag at field 0); the load
    // reads the inline Vec layout (`{ ptr, i64, i64 }`); the alloca
    // reserves a slot of the same type; the store deposits the
    // reloaded value.
    let ir = ir_for_with_state_struct_layouts(
        "effect resource Network;
             pub fn fetch() with sends(Network) receives(Network) {}
             fn driver(items: Vec[i64]) { fetch(); }",
    );
    let body = extract_fn_ir(&ir, "__kara_poll_driver");
    // GEP into state-struct field 1 (captured-local `items` slot).
    // Match the inbounds GEP shape with the `, i32 0, i32 1` indices
    // that step from struct-base to field 1.
    assert!(
        body.contains("getelementptr inbounds %kara.state.driver, ptr %0, i32 0, i32 1"),
        "reload prologue must GEP into state struct field 1 for `items`:\n{body}"
    );
    // Load the inline Vec shape from the GEP'd field pointer.
    assert!(
        body.contains("load { ptr, i64, i64 }") || body.contains("load {ptr, i64, i64}"),
        "reload prologue must load the inline Vec layout for `items`:\n{body}"
    );
    // Alloca a slot for the reloaded local.
    assert!(
        body.contains("alloca { ptr, i64, i64 }") || body.contains("alloca {ptr, i64, i64}"),
        "reload prologue must alloca a slot for the reloaded `items`:\n{body}"
    );
    // The store transfers the loaded value into the alloca'd slot.
    // LLVM renders this as `store { ptr, i64, i64 } %items.reload, ptr %items.slot`.
    assert!(
        body.contains("store { ptr, i64, i64 }") || body.contains("store {ptr, i64, i64}"),
        "reload prologue must store the loaded value into the slot:\n{body}"
    );
}

#[test]
fn test_poll_fn_reload_prologue_empty_for_no_captured_locals() {
    // A function with no captured locals (no params, no in-scope
    // lets at the yield point) has an empty layout — the reload
    // loop iterates zero times, so each state arm has just the
    // unconditional `ret i8 0`. No GEPs into captured-local fields.
    let ir = ir_for_with_state_struct_layouts(
        "effect resource Network;
             pub fn fetch() with sends(Network) receives(Network) {}
             fn driver() { fetch(); }",
    );
    let body = extract_fn_ir(&ir, "__kara_poll_driver");
    // Only the tag GEP (field 0) is present; no field-1+ GEPs.
    assert!(
        !body.contains("ptr %0, i32 0, i32 1"),
        "no-capture function must not GEP into captured-local fields:\n{body}"
    );
    // Tag GEP still exists at field 0.
    assert!(
        body.contains("ptr %0, i32 0, i32 0"),
        "tag GEP at field 0 must still be present:\n{body}"
    );
}

// ── Phase 6 line 26 slice 8p: assignment statements inside arm bodies ─
//
// `name = value` assignments where `name` is already in `current_names`
// (a captured local OR an arm-local let) store the recognised value
// into the binding's existing slot — no new alloca. Composes with
// slice 8n writeback: assigning to a captured local in arm 0 makes
// the new value land in the state-struct field at yield, so arm 1's
// reload sees the updated value.

#[test]
fn test_body_splitting_8p_assigns_literal_to_captured_local() {
    // `fn driver(n: i64) with sends(Network) { n = 99; fetch(); }`
    // — assign before yield; writeback in slice 8n sees the new
    // value.
    let ir = ir_for_with_state_struct_layouts(
        "effect resource Network;
             pub fn fetch() with sends(Network) receives(Network) {}
             fn driver(n: i64) with sends(Network) receives(Network) {
                 n = 99;
                 fetch();
             }",
    );
    let body = extract_fn_ir(&ir, "__kara_poll_driver");
    assert!(
        body.contains("store i64 99, ptr %n.slot"),
        "assignment n = 99 must store into existing n.slot:\n{body}"
    );
    // Slice 8n writeback should now load n.slot (post-99) and
    // store into the state-struct field before the yield.
    let assign_pos = body
        .find("store i64 99, ptr %n.slot")
        .expect("assignment store missing");
    let writeback_pos = body
        .find("%n.writeback = load i64, ptr %n.slot")
        .expect("writeback load missing");
    assert!(
        assign_pos < writeback_pos,
        "assignment must precede the writeback load:\n{body}"
    );
}

#[test]
fn test_body_splitting_8p_assigns_captured_to_captured() {
    // `fn driver(a: i64, b: i64) with sends(Network) { b = a; fetch(); }`
    // — assign one captured local from another. Both slots are
    // already in slot_map from slice 8a; the assignment loads from
    // `a.slot` and stores into `b.slot`.
    let ir = ir_for_with_state_struct_layouts(
        "effect resource Network;
             pub fn fetch() with sends(Network) receives(Network) {}
             fn driver(a: i64, b: i64) with sends(Network) receives(Network) {
                 b = a;
                 fetch();
             }",
    );
    let body = extract_fn_ir(&ir, "__kara_poll_driver");
    assert!(
        body.contains("%a.assign_rhs = load i64, ptr %a.slot"),
        "RHS read must load from a.slot via .assign_rhs:\n{body}"
    );
    assert!(
        body.contains("store i64 %a.assign_rhs, ptr %b.slot"),
        "LHS write must store into b.slot:\n{body}"
    );
}

// ── Phase 6 line 26 slice 8r: compound-assign in arm bodies ───────────
//
// `name OP= value` compound-assignment desugars in the body-splitting
// walker to `Assign { name, value: Binary { op, lhs: Slot(name), rhs:
// <recognised> } }`, so the existing slice-8p Assign emission +
// slice-8q Binary materialisation handle the codegen unchanged.
// Walker supports the five arithmetic CompoundOps (`+=` / `-=` /
// `*=` / `/=` / `%=`); bitwise / shift compound ops (`&=` / `|=` /
// `^=` / `<<=` / `>>=`) silently drop pending the same widening on
// the `Binary` recognition side.

#[test]
fn test_body_splitting_8r_compound_add_assign_captured_local() {
    // `n += 1;` before yield — desugars to `n = n + 1`. The slot
    // store carries the binary result; slice 8n's writeback then
    // transfers the post-arm value to the state struct.
    let ir = ir_for_with_state_struct_layouts(
        "effect resource Network;
             pub fn fetch() with sends(Network) receives(Network) {}
             fn driver(n: i64) with sends(Network) receives(Network) {
                 n += 1;
                 fetch();
             }",
    );
    let body = extract_fn_ir(&ir, "__kara_poll_driver");
    assert!(
        body.contains("%n.assign_rhs = load i64, ptr %n.slot"),
        "compound-assign lhs must load n.slot via .assign_rhs:\n{body}"
    );
    assert!(
        body.contains("%binop.assign_rhs = add i64 %n.assign_rhs, 1"),
        "compound-assign must emit `add i64 %n.assign_rhs, 1`:\n{body}"
    );
    assert!(
        body.contains("store i64 %binop.assign_rhs, ptr %n.slot"),
        "compound-assign result must be stored back into n.slot:\n{body}"
    );
    // Writeback observes the post-op value.
    let assign_pos = body
        .find("store i64 %binop.assign_rhs, ptr %n.slot")
        .expect("compound-assign store missing");
    let writeback_pos = body
        .find("%n.writeback = load i64, ptr %n.slot")
        .expect("writeback load missing");
    assert!(
        assign_pos < writeback_pos,
        "compound-assign must precede the writeback load:\n{body}"
    );
}

#[test]
fn test_body_splitting_8s_let_string_captured_alloca_uses_string_type() {
    // `let s = name` where `name: String` is a captured local —
    // String is an inline `{ ptr, i64, i64 }` shape (same as Vec).
    // Slice 8s makes the slot alloca match.
    let ir = ir_for_with_state_struct_layouts(
        "effect resource Network;
             pub fn fetch() with sends(Network) receives(Network) {}
             fn driver(name: String) with sends(Network) receives(Network) {
                 fetch();
                 let s = name;
             }",
    );
    let body = extract_fn_ir(&ir, "__kara_poll_driver");
    assert!(
        body.contains("%s.slot = alloca { ptr, i64, i64 }")
            || body.contains("%s.slot = alloca {ptr, i64, i64}"),
        "let s = name must alloca a String-shaped slot:\n{body}"
    );
    assert!(
        !body.contains("%s.slot = alloca i64"),
        "s.slot must NOT be an i64 alloca:\n{body}"
    );
}

#[test]
fn test_body_splitting_8s_let_shared_struct_captured_alloca_uses_ptr() {
    // `let r = h` where `h: Hub` is a captured shared struct —
    // shared structs collapse to a pointer-sized handle. Slice 8s
    // makes the slot alloca a `ptr`, not i64.
    let ir = ir_for_with_state_struct_layouts(
        "effect resource Network;
             pub fn fetch() with sends(Network) receives(Network) {}
             shared struct Hub { count: i64 }
             fn driver(h: Hub) with sends(Network) receives(Network) {
                 fetch();
                 let r = h;
             }",
    );
    let body = extract_fn_ir(&ir, "__kara_poll_driver");
    assert!(
        body.contains("%r.slot = alloca ptr"),
        "let r = h must alloca a ptr slot for shared-struct handle:\n{body}"
    );
    assert!(
        !body.contains("%r.slot = alloca i64"),
        "r.slot must NOT be an i64 alloca:\n{body}"
    );
}

#[test]
fn test_terminal_return_8o_uses_captured_local_final_expr() {
    // `fn driver(n: i64) -> i64 ... { fetch(); n }` — `n` is a
    // captured local; the terminal arm loads it from the slot and
    // stores into the terminal field.
    let ir = ir_for_with_state_struct_layouts(
        "effect resource Network;
             pub fn fetch() with sends(Network) receives(Network) {}
             fn driver(n: i64) -> i64 with sends(Network) receives(Network) { fetch(); n }",
    );
    let body = extract_fn_ir(&ir, "__kara_poll_driver");
    assert!(
        body.contains("%n.return = load i64, ptr %n.slot"),
        "terminal arm must load captured local n via .return:\n{body}"
    );
    assert!(
        body.contains("store i64 %n.return, ptr %kara.return.field_ptr"),
        "terminal arm must store slot-loaded n into terminal field:\n{body}"
    );
}

#[test]
fn test_terminal_return_8ah_uses_call_with_captured_arg() {
    // `fn driver(n: i64) -> i64 ... { fetch(); ident(n) }` — `n`
    // is a captured local; slice 8ah materialises it via the
    // per-arm slot map and threads the loaded value as the
    // synchronous call's arg.
    let ir = ir_for_with_state_struct_layouts(
        "effect resource Network;
             pub fn fetch() with sends(Network) receives(Network) {}
             fn ident(x: i64) -> i64 { x }
             fn driver(n: i64) -> i64 with sends(Network) receives(Network) { fetch(); ident(n) }",
    );
    let body = extract_fn_ir(&ir, "__kara_poll_driver");
    assert!(
        body.contains("%n.return = load i64, ptr %n.slot"),
        "terminal arm must load n.slot via .return:\n{body}"
    );
    assert!(
        body.contains("%ident.return = call i64 @ident(i64 %n.return)"),
        "terminal arm must emit call passing the slot-loaded n:\n{body}"
    );
    assert!(
        body.contains("store i64 %ident.return, ptr %kara.return.field_ptr"),
        "terminal arm must store call result into terminal field:\n{body}"
    );
}

// ── Phase 6 line 26 slice 8n: cross-yield captured-local writeback ────
//
// Before each non-terminal arm's tag-store + Pending return, the
// poll-fn now writes each captured-local's current slot value back
// into its state-struct field. This makes slice 8m's arm-local lets
// (which can shadow captured-local slot pointers) actually survive
// across yields — the next arm's reload prologue reads the post-arm-
// body value.

#[test]
fn test_body_splitting_8n_writes_back_captured_local_before_yield() {
    // `fn driver(n: i64) with sends(Network) { fetch(); }` — `n` is
    // a captured local, no user mutation; the writeback is a value-
    // equivalent no-op but still appears in IR as a load+GEP+store
    // before the tag-store + Pending return.
    let ir = ir_for_with_state_struct_layouts(
        "effect resource Network;
             pub fn fetch() with sends(Network) receives(Network) {}
             fn driver(n: i64) with sends(Network) receives(Network) {
                 fetch();
             }",
    );
    let body = extract_fn_ir(&ir, "__kara_poll_driver");
    assert!(
        body.contains("%n.writeback = load i64, ptr %n.slot"),
        "state_0 must load n.slot for writeback:\n{body}"
    );
    assert!(
            body.contains("%n.writeback_field_ptr = getelementptr inbounds %kara.state.driver, ptr %0, i32 0, i32 1"),
            "writeback must GEP into state-struct field 1 for n:\n{body}"
        );
    // Writeback must precede the tag-store + Pending return.
    let writeback_store_pos = body
        .find("store i64 %n.writeback, ptr %n.writeback_field_ptr")
        .expect("writeback store missing");
    let tag_store_pos = body
        .find("store i32 1, ptr %state_0.next_tag_ptr")
        .expect("tag-store missing");
    assert!(
        writeback_store_pos < tag_store_pos,
        "writeback must precede tag-store:\n{body}"
    );
}

// ── Phase 6 line 26 slice 8u: state-struct destructor helper ──────────
//
// For each network-boundary function whose state-struct holds at
// least one heap-bearing captured-local field, codegen emits an
// `define internal void @__kara_state_drop_<fn_key>(ptr %state)`
// helper that walks the captured-local fields and frees any
// heap-bearing ones (`cap > 0 ? free(data)` for Vec/String/VecDeque,
// `rc_dec` against the slot's loaded handle for shared struct
// fields). The destructor is the unified unwind primitive the
// future `?`-Err-propagation and cooperative-cancel use sites
// both invoke; slice 8u lands the primitive only.
//
// The state struct's own heap allocation is the *caller's*
// responsibility to `free` after invoking the destructor — matches
// the constructor's caller-allocates / caller-frees discipline.
// Skipped entirely when no captured-local field is heap-bearing
// (an empty destructor would be dead IR).

#[test]
fn test_state_destructor_emitted_for_state_struct_with_vec_captured_local() {
    // A `Vec[i64]` captured local triggers destructor emission —
    // the field's `cap > 0 ? free(data)` pattern is the v1
    // heap-bearing arm.
    let ir = ir_for_with_state_struct_layouts(
        "effect resource Network;
             pub fn fetch() with sends(Network) receives(Network) {}
             fn driver(items: Vec[i64]) { fetch(); }",
    );
    let define_line = ir
        .lines()
        .find(|l| l.contains("@__kara_state_drop_driver"))
        .unwrap_or_else(|| panic!("expected @__kara_state_drop_driver in IR:\n{ir}"));
    assert!(
        define_line.contains("define"),
        "destructor should be defined: {define_line}"
    );
    assert!(
        define_line.contains("internal"),
        "destructor should have internal linkage: {define_line}"
    );
    assert!(
        define_line.contains("void @__kara_state_drop_driver(ptr"),
        "destructor signature must be `void @__kara_state_drop_driver(ptr ...)`: {define_line}"
    );
}

#[test]
fn test_e2e_let_bound_range_captures_bounds_at_binding() {
    // The discriminator between the two ways to fix B-2026-08-17-29.
    // Re-substituting the bound EXPRESSIONS at the loop would iterate
    // `10..6` here — empty — while capturing the bounds at the `let`
    // iterates `2..6`. design.md line 2616 types a range as a first-class
    // `Range[T]` value, so 14 is the answer, and it is what `--interp`
    // produces. A regression to expression-substitution prints 0, not an
    // error, so this assertion is the only thing that would catch it.
    //
    // The second half pins the scope contract: an inner `let r` shadows
    // for its block and the outer binding is intact afterwards (the
    // capture table rides `snapshot_var_env`/`restore_var_env`).
    let output = run_program(
        "fn main() {\n\
                 let mut a = 2;\n\
                 let r = a..6;\n\
                 a = 10;\n\
                 let mut n = 0;\n\
                 for i in r { n = n + i; }\n\
                 println(n);\n\
                 let s = 0..5;\n\
                 let mut inner = 0;\n\
                 {\n\
                     let s = 0..2;\n\
                     for i in s { inner = inner + 1; }\n\
                 }\n\
                 let mut outer = 0;\n\
                 for i in s { outer = outer + 1; }\n\
                 println(inner);\n\
                 println(outer);\n\
             }",
    )
    .expect("compile + run failed");
    assert_eq!(output, "14\n2\n5\n");
}

#[test]
fn test_e2e_vec_retain_scalar_heap_and_capture() {
    // B-2026-07-15-19: `Vec[T].retain(|x| pred)` had no AOT codegen lowering
    // (typechecked + ran under the interpreter since B-2026-07-15-16, but
    // `karac build` loud-bailed "no handler for method 'retain'"). Now lowered
    // as an in-place compaction with a write cursor, the inline predicate
    // evaluated per element, and drop-glue on the filtered-out elements.
    // Covers a scalar Vec (compaction only), a heap Vec[String] (filtered
    // elements must be freed — verified leak-clean in the sibling LSan test),
    // and a predicate that CAPTURES an outer variable (the param binds by
    // shadowing so the capture stays visible).
    let output = run_program(
        "fn main() {\n\
                 let mut a: Vec[i64] = Vec.new();\n\
                 a.push(1); a.push(2); a.push(3); a.push(4); a.push(5); a.push(6);\n\
                 a.retain(|x| x % 2 == 0);\n\
                 println(a.len());\n\
                 println(a[0]); println(a[1]); println(a[2]);\n\
                 let mut s: Vec[String] = Vec.new();\n\
                 s.push(\"a\"); s.push(\"bbb\"); s.push(\"cc\"); s.push(\"ddddd\");\n\
                 s.retain(|w| w.len() >= 3);\n\
                 println(s.len());\n\
                 println(s[0]); println(s[1]);\n\
                 let mut n: Vec[i64] = Vec.new();\n\
                 n.push(10); n.push(5); n.push(20); n.push(3);\n\
                 let threshold: i64 = 6;\n\
                 n.retain(|x| x > threshold);\n\
                 println(n.len());\n\
                 println(n[0]); println(n[1]);\n\
             }",
    )
    .expect("compile + run failed");
    // scalar: [2,4,6]; heap: [\"bbb\",\"ddddd\"]; capture>6: [10,20].
    assert_eq!(output, "3\n2\n4\n6\n2\nbbb\nddddd\n2\n10\n20\n");
}

/// B-2026-06-07-1 regression: `tg.spawn(closure)` in a function that
/// also declares a `Vec` (or any heap collection) — which makes the
/// function eligible for statement-level auto-parallelization — must
/// compile and run. The `g` binding escapes into auto-par's
/// return-slot list; `infer_let_binding_llvm_type` sizes that slot
/// from the `TaskGroup` type annotation via `llvm_type_for_name`,
/// which used to hit the `i64` fall-through default (TaskGroup isn't
/// in `struct_types` — baked stdlib defs aren't loaded into codegen).
/// The reconstructed slot was then a bare `i64`, so `tg.spawn(...)`'s
/// receiver load read an `IntValue` where the dispatcher does
/// `into_struct_value()` on the `{ i64 }` TaskGroup shape → ICE
/// ("Found IntValue ... but expected the StructValue variant").
/// Fixed by giving `TaskGroup`/`TaskHandle` an explicit `{ i64 }`
/// arm in `llvm_type_for_name` (mirrors the TCP/TLS baked-struct
/// arms). Fire-and-forget (no `.join()`) per the bug repro — the
/// group's scope-exit drop waits for the child.
#[test]
fn test_e2e_taskgroup_spawn_aggregate_capture_under_auto_par() {
    let out = run_program_capturing(
        r#"
fn consume(v: Vec[i64]) -> i64 { v.len() }
fn main() {
    let data: Vec[i64] = Vec.new();
    let mut g: TaskGroup = TaskGroup.new();
    g.spawn(|| consume(data));
    println("ok");
}
"#,
    );
    if let Some(c) = out {
        assert_eq!(c.stdout.trim(), "ok");
    }
}

#[test]
fn test_e2e_vec_sort_by_key_with_captured_variable() {
    // Key closure captures an outer-scope variable (`offset`). Exercises
    // the env-struct + outer-stack-alloca capture path inside the bridge
    // thunk — subtracting a constant from each element preserves the
    // ordering, so the result mirrors `|x| x` ascending.
    let out = run_program(
        r#"
fn main() {
    let offset = 100i64;
    let mut v: Vec[i64] = Vec.new();
    v.push(5); v.push(2); v.push(8); v.push(1);
    v.sort_by_key(|x| x - offset);
    for x in v.iter() { println(x); }
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["1", "2", "5", "8"]);
    }
}

#[test]
fn test_e2e_vec_sort_by_key_closure_typed_local() {
    // Closure-typed local callee (`let k = |x| x; v.sort_by_key(k);`)
    // routes through `emit_sort_by_key_closure_thunk`: indirect call
    // through the spilled fat pointer's `{fn_ptr, env_ptr}`.
    let out = run_program(
        r#"
fn main() {
    let k = |x: i64| x;
    let mut v: Vec[i64] = Vec.new();
    v.push(30); v.push(10); v.push(20);
    v.sort_by_key(k);
    for x in v.iter() { println(x); }
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["10", "20", "30"]);
    }
}

#[test]
fn test_e2e_vec_sort_by_key_closure_typed_local_struct_field_body() {
    // Closure-typed local with a struct element and a field-access
    // body (`|s: Score| s.v`). Pins the fix to compile_closure's
    // param binding step that registers the param's Kāra type name
    // in var_type_names — without it, `s.v` inside the precompiled
    // closure body silently lowered to `i64 0` (field_index_for
    // returned None), turning sort_by_key into a no-op. Surfaced via
    // the 2026-05-29 kata-15 non-inline-callee probe.
    let out = run_program(
        r#"
struct Score { v: i64 }
fn main() {
    let k = |s: Score| s.v;
    let mut v: Vec[Score] = Vec.new();
    v.push(Score { v: 30 });
    v.push(Score { v: 10 });
    v.push(Score { v: 20 });
    v.sort_by_key(k);
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
fn test_e2e_vec_sort_by_closure_typed_local() {
    // sort_by counterpart of the sort_by_key closure-typed-local test.
    // Routes through `emit_sort_by_thunk` (existing helper) via the
    // new Identifier-dispatch in the `"sort_by"` arm. Replaces the
    // broken `pending_closure_fn_type.take()` protocol that errored
    // "closure missing fn_type" for any non-inline callee.
    let out = run_program(
        r#"
fn main() {
    let cmp = |a: i64, b: i64| a.cmp(b);
    let mut v: Vec[i64] = Vec.new();
    v.push(30); v.push(10); v.push(20);
    v.sort_by(cmp);
    for x in v.iter() { println(x); }
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["10", "20", "30"]);
    }
}

#[test]
fn test_e2e_vec_sort_by_mono_i64_fallback_on_captures() {
    // Slice 6.1 gate boundary — closure that captures an outer
    // variable falls through to the existing thunk path (the mono
    // emitter explicitly rejects captures in v1; the
    // `collect_closure_free_vars(...).is_empty()` check at the
    // dispatch site is the chokepoint). Sorts by distance² from a
    // captured pivot. If the gate misclassified and routed to the
    // mono fn (which discards captures), the output would either be
    // the ascending raw values OR a crash, neither matching the
    // pivot-distance ordering this asserts.
    let out = run_program(
        r#"
fn main() {
    let pivot: i64 = 5i64;
    let mut v: Vec[i64] = Vec.new();
    v.push(1); v.push(10); v.push(4); v.push(9);
    v.sort_by(|a, b| ((a - pivot) * (a - pivot)).cmp((b - pivot) * (b - pivot)));
    for x in v.iter() { println(x); }
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        // Distance² from pivot=5: 1→16, 10→25, 4→1, 9→16 — sorted
        // ascending by distance: 4 (d²=1), then either 1 or 9 (both
        // d²=16; stable sort preserves input order so 1 before 9),
        // then 10 (d²=25). The runtime stable-sort gives 4, 1, 9, 10.
        assert_eq!(lines, vec!["4", "1", "9", "10"]);
    }
}

/// Case 1, the real point of the marker: the exported symbol is
/// callable from a foreign C translation unit. Emits the Kāra object
/// (no `main`), compiles a C `main` that declares + calls the export,
/// links the two, and runs. The body is pure arithmetic, so no Kāra
/// runtime archive is needed. Soft-skips if `cc`/`nm` are absent.
#[test]
fn extern_c_export_callable_from_c() {
    use karac::codegen::compile_to_object_with_options;
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);

    let src = "extern \"C\" fn kara_add_one(x: i32) -> i32 { x + 1 }\n";
    let mut parsed = karac::parse(src);
    assert!(parsed.errors.is_empty(), "parse: {:?}", parsed.errors);
    let resolved = karac::resolve(&parsed.program);
    let typed = karac::typecheck(&parsed.program, &resolved);
    karac::lower(&mut parsed.program, &typed);
    let ownership = karac::ownershipcheck(&parsed.program, &typed);
    super::common::assert_check_clean(&resolved, &typed, src);
    // Effects are the THIRD phase of the same gate, and were simply
    // absent — a test could pin behaviour for a program `karac build`
    // refuses (B-2026-08-19-5). Runs after `lower`, threaded with the
    // typechecker's tables, exactly as `Pipeline::run_all_checks` does.
    super::common::assert_effects_clean_for(&parsed.program, &typed, src);
    super::common::assert_ownership_clean(&ownership, src);

    let id = COUNTER.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    let kara_obj = format!("/tmp/karac_ffi_export_{pid}_{id}.o");
    let c_src = format!("/tmp/karac_ffi_main_{pid}_{id}.c");
    let exe = format!("/tmp/karac_ffi_exe_{pid}_{id}");
    let cleanup = || {
        let _ = std::fs::remove_file(&kara_obj);
        let _ = std::fs::remove_file(&c_src);
        let _ = std::fs::remove_file(&exe);
    };

    compile_to_object_with_options(
        &parsed.program,
        &kara_obj,
        Some(&ownership),
        None,
        None,
        None,
    )
    .expect("codegen failed for extern \"C\" export");

    // The export must appear as a *defined* external symbol under its
    // bare C name (un-mangled). On Mach-O the symbol carries a leading
    // underscore; the substring check tolerates both.
    if let Ok(nm) = std::process::Command::new("nm").arg(&kara_obj).output() {
        let s = String::from_utf8_lossy(&nm.stdout);
        assert!(
            s.lines()
                .any(|l| l.contains("kara_add_one") && l.contains(" T ")),
            "expected defined external symbol `kara_add_one`; nm:\n{s}"
        );
    }

    std::fs::write(
        &c_src,
        "extern int kara_add_one(int);\n\
             int main(void) { return kara_add_one(41) == 42 ? 0 : 7; }\n",
    )
    .unwrap();

    let cc = match std::process::Command::new("cc")
        .args([c_src.as_str(), kara_obj.as_str(), "-o", exe.as_str()])
        .output()
    {
        Ok(o) => o,
        Err(_) => {
            cleanup();
            eprintln!("extern-c C-interop: skipped (no `cc`)");
            return;
        }
    };
    if !cc.status.success() {
        cleanup();
        eprintln!(
            "extern-c C-interop: link skipped:\n{}",
            String::from_utf8_lossy(&cc.stderr)
        );
        return;
    }

    let run = std::process::Command::new(&exe)
        .output()
        .expect("run linked C+Kāra exe");
    cleanup();
    assert!(
        run.status.success(),
        "C harness calling the Kāra export returned nonzero: {:?}",
        run.status
    );
}

#[test]
fn e2e_closure_struct_return_via_fn_param() {
    // B-2026-07-19: a closure passed to a `Fn(..) -> S` param whose body
    // returns an aggregate via a METHOD CALL (`|q| q.twice()`) or produces a
    // tuple mis-declared its LLVM return type as `i64` (the structural
    // heuristic can't see a method's returned surface type), so codegen died
    // with "Function return type does not match operand type of return inst".
    // Struct-literal bodies already worked; this generalizes the fix to any
    // aggregate return. Unblocks std.autograd's `Tape.grad` closures
    // (`|x| x.mul(x)` returns Var). Covers: method-call struct return,
    // struct-literal return (regression), and a tuple return.
    if let Some(out) = run_program(
        r#"
struct P { a: i64, b: i64 }
impl P { fn twice(ref self) -> P { P { a: self.a * 2, b: self.b * 2 } } }
fn ap(f: Fn(P) -> P, p: P) -> P { f(p) }
fn ap_lit(f: Fn(P) -> P, p: P) -> P { f(p) }
fn ap_tup(f: Fn(i64) -> (i64, i64), x: i64) -> (i64, i64) { f(x) }
fn main() {
    let r = ap(|q| q.twice(), P { a: 5, b: 6 });
    println(r.a);
    println(r.b);
    let s = ap_lit(|q| P { a: q.a + 1, b: q.b + 2 }, P { a: 10, b: 20 });
    println(s.a);
    println(s.b);
    let t = ap_tup(|n| (n + 1, n * 2), 5);
    println(t.0);
    println(t.1);
}
"#,
    ) {
        assert_eq!(out, "10\n12\n11\n22\n6\n10\n");
    }
}

#[test]
fn test_e2e_unannotated_string_closure_param_abi() {
    // B-2026-07-02-12: an un-annotated closure passed to a
    // `Fn(String) -> String` param compiled as `(ptr, i64) -> i64` (the
    // i64 fallback) while call sites dispatched through the declared-Fn
    // ABI — the String's pointer word printed as an integer, silently.
    // The typechecker now records the closure literal's resolved Fn type
    // at its span; codegen types un-annotated params from it (and
    // registers String/Vec params in the semantic side-tables so
    // f-string interpolation formats them correctly). Exercises both the
    // non-generic and the monomorphized-generic routes, including a
    // String accumulator folded through a loop.
    let out = run_program(
        "fn cat(s: String, f: Fn(String) -> String) -> String {\n\
                 return f(s);\n\
             }\n\
             fn fold3[A](xs: Vec[i64], init: A, f: Fn(A, i64) -> A) -> A {\n\
                 let mut acc = init;\n\
                 for x in xs {\n\
                     acc = f(acc, x);\n\
                 }\n\
                 return acc;\n\
             }\n\
             fn main() {\n\
                 println(cat(\"ab\", |a| f\"{a}!\"));\n\
                 println(fold3(vec![1, 2, 3], 0, |a, x| a + x));\n\
                 println(fold3(vec![1, 2, 3], \"\", |a, x| f\"{a}{x}\"));\n\
             }\n",
    );
    if let Some(out) = out {
        assert_eq!(out, "ab!\n6\n123\n");
    }
}

/// B-2026-08-15-22 — the predicate factory the typecheck fix unblocked has
/// to RUN correctly, not merely be admitted.
///
/// A typecheck relaxation that let through a program the backend
/// miscompiles would be worse than the false positive it removed. The
/// closure is invoked REPEATEDLY on purpose: repeatability is exactly what
/// the `Fn` slot promises and what the old `OnceFn` demotion denied, so a
/// capture that were really consumed would surface as a wrong answer or a
/// crash on the second call. The captured needle is also read after the
/// calls, to show it was never taken.
///
/// ONLY `contains` IS COMPILED HERE, and the omission is measured rather
/// than stylistic. `starts_with` / `ends_with` / `Map.contains_key` /
/// `Set.contains` inside a closure body all die in codegen with "Function
/// return type does not match operand type of return inst" — the closure's
/// return is typed `i64` against the predicate's `i1`. That is PRE-EXISTING
/// and reachable with no capture at all (`|s| s.starts_with("ab")` fails
/// identically on the parent), so it is not this fix's doing; it is filed as
/// B-2026-08-15-25. Admitting them at the type level is still correct — the
/// interpreter runs every one of them — and the typechecker tests cover the
/// full matrix.
#[test]
fn test_e2e_closure_factory_over_a_read_only_string_predicate() {
    assert_eq!(
        run_program(
            "fn forbids(bad: String) -> Fn(ref String) -> bool { |s| not s.contains(bad) }\n\
                 fn has_ref(v: Vec[String], s: ref String) -> bool { return v.contains(s); }\n\
                 fn main() {\n\
                     let f = forbids(\"xy\");\n\
                     let a = \"abxyc\";\n\
                     let b = \"hello\";\n\
                     println(f\"{f(a)} {f(b)} {f(a)}\");\n\
                     let mut v: Vec[String] = Vec.new();\n\
                     v.push(\"one\");\n\
                     let n = \"one\";\n\
                     let r = has_ref(v, n);\n\
                     println(f\"{r} {n.len()}\");\n\
                 }\n"
        ),
        Some("false true false\ntrue 3\n".to_string())
    );
}

/// B-2026-08-15-25 and B-2026-08-15-28 — a closure whose body is a builtin
/// predicate, and a closure taking a handle-backed builtin by `ref`.
///
/// Both are detectors: on the parent this program does not build at all
/// (six "Function return type does not match operand type of return inst"
/// failures), and the shapes that DID build there read garbage.
///
/// -25: `infer_closure_return_type` falls back to `i64` when it cannot
/// recover a body's surface type, and the override that repairs that from
/// the typechecker-recorded `Fn(T) -> R` only fired for STRUCT returns. A
/// `bool` is `i1` and a `u8` is `i8` — both ints — so both kept the `i64`
/// signature and the body's `ret` mismatched it. `vecat` is in here for
/// exactly that reason: it returns `u8`, not `bool`, so it pins the
/// generalization rather than a bool special case.
///
/// -28: a `Map`/`Set` value IS a pointer, and the closure ABI passes it
/// directly, so registering a `ref Map` parameter as a borrow added a second
/// load and the body called the runtime with the map's first word as a
/// pointer. `msize`/`ssize` are the pins that would catch it silently —
/// `|m| m.len()` returned 529 for a two-entry map on the parent rather than
/// crashing, so a fixture that only checked "does it build" would miss it.
///
/// Every closure is invoked more than once, and the containers are re-read
/// at the end, so a handle corrupted by the old double-deref cannot pass.
#[test]
fn test_e2e_closure_over_builtin_predicates_and_handle_refs() {
    assert_eq!(
            run_program(
                "struct P { x: i64, y: i64 }\n\
                 fn starts(p: String) -> Fn(ref String) -> bool { |s| s.starts_with(p) }\n\
                 fn ends(p: String) -> Fn(ref String) -> bool { |s| s.ends_with(p) }\n\
                 fn forbids(b: String) -> Fn(ref String) -> bool { |s| not s.contains(b) }\n\
                 fn keyed(k: String) -> Fn(ref Map[String, i64]) -> bool { |m| m.contains_key(k) }\n\
                 fn member(e: String) -> Fn(ref Set[String]) -> bool { |st| st.contains(e) }\n\
                 fn msize() -> Fn(ref Map[String, i64]) -> i64 { |m| m.len() }\n\
                 fn ssize() -> Fn(ref Set[String]) -> i64 { |st| st.len() }\n\
                 fn nocap() -> Fn(ref String) -> bool { |s| s.starts_with(\"ab\") }\n\
                 fn vecat() -> Fn(ref Vec[u8], i64) -> u8 { |w, i| w[i] }\n\
                 fn slen() -> Fn(ref String) -> i64 { |s| s.len() }\n\
                 fn psum() -> Fn(ref P) -> i64 { |p| p.x + p.y }\n\
                 fn vlen() -> Fn(ref Vec[String]) -> i64 { |v| v.len() }\n\
                 fn main() {\n\
                     let a = \"abxyc\";\n\
                     let b = \"hello\";\n\
                     let g1 = starts(\"ab\");  println(f\"{g1(a)} {g1(b)} {g1(a)}\");\n\
                     let g2 = ends(\"lo\");    println(f\"{g2(a)} {g2(b)}\");\n\
                     let g3 = forbids(\"xy\"); println(f\"{g3(a)} {g3(b)}\");\n\
                     let g4 = nocap();       println(f\"{g4(a)} {g4(b)}\");\n\
                     let mut m: Map[String, i64] = Map.new();\n\
                     let _ = m.insert(\"k\", 1);\n\
                     let _ = m.insert(\"j\", 2);\n\
                     let g5 = keyed(\"k\");\n\
                     let g6 = keyed(\"q\");\n\
                     println(f\"{g5(m)} {g6(m)} {g5(m)}\");\n\
                     let g7 = msize();\n\
                     println(f\"{g7(m)}\");\n\
                     let mut st: Set[String] = Set.new();\n\
                     let _ = st.insert(\"z\");\n\
                     let g8 = member(\"z\");\n\
                     let g9 = member(\"q\");\n\
                     println(f\"{g8(st)} {g9(st)}\");\n\
                     let g10 = ssize();\n\
                     println(f\"{g10(st)}\");\n\
                     let v: Vec[u8] = [7u8, 8u8, 9u8];\n\
                     println(f\"{vecat()(v, 1)}\");\n\
                     let s2 = \"abcd\";\n\
                     println(f\"{slen()(s2)}\");\n\
                     let p = P { x: 3, y: 4 };\n\
                     println(f\"{psum()(p)}\");\n\
                     let mut vs: Vec[String] = Vec.new();\n\
                     vs.push(\"a\"); vs.push(\"b\");\n\
                     println(f\"{vlen()(vs)}\");\n\
                     println(f\"{m.len()} {st.len()}\");\n\
                 }\n"
            ),
            Some(
                "true false true\nfalse true\nfalse true\ntrue false\n\
                 true false true\n2\ntrue false\n1\n8\n4\n7\n2\n2 1\n"
                    .to_string()
            )
        );
}

/// B-2026-08-17-26 — design.md § Pipe Operator prescribes a closure RHS as
/// the escape hatch from its `_` restrictions, and all three prescribed
/// forms were rejected at typecheck. They are transcribed verbatim here
/// and pinned against the direct spelling they stand for, because the
/// point of the workaround is that it RUNS, not merely that it checks.
#[test]
fn a_closure_pipe_stage_compiles_to_the_call_it_stands_for() {
    let prelude = "fn f(a: i64, b: i64) -> i64 { return a + b; }\n\
                       fn g(x: i64) -> i64 { return x * 2; }\n\
                       fn tag(s: String) -> String { return \"[\" + s + \"]\"; }\n";
    // (piped-through-a-closure spelling, the equivalent direct call)
    for (piped, direct) in [
        // design.md: "let d = data |> g; d |> |d| f(d, extra)"
        ("(5 |> g) |> |d| f(d, 1)", "f(g(5), 1)"),
        // design.md: "data |> |d| f(g(d), extra)"
        ("5 |> |d| f(g(d), 1)", "f(g(5), 1)"),
        // design.md: "wrap in a closure — data |> |d| f(d, d)"
        ("5 |> |d| f(d, d)", "f(5, 5)"),
        // the un-annotated param resolved from a HEAP argument
        ("\"x\" |> |s| tag(s)", "tag(\"x\")"),
    ] {
        let src = |e: &str| format!("{prelude}fn main() {{ println({e}); }}\n");
        let Some(got) = run_program(&src(piped)) else {
            return;
        };
        let want = run_program(&src(direct)).expect("direct-call control must build");
        assert_eq!(got, want, "`{piped}` must run as `{direct}` does");
    }
}
