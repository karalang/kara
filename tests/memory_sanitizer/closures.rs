//! closures, captures, heap-env bindings, fn pointers -- fixtures for `tests/memory_sanitizer.rs`.
//!
//! Split out of `tests/memory_sanitizer.rs` on 2026-09-21. The TEST TARGET is
//! unchanged: this file is a module of that target, so
//! `cargo test --features llvm --test memory_sanitizer` still runs everything
//! and CI needs no edit. Run this area alone with:
//!
//!     cargo test --features llvm --test memory_sanitizer closures::
//!
//! New fixtures about closures, captures, heap-env bindings, fn pointers belong in this file.

use super::*;

/// B-2026-08-28-38 — a by-value aggregate argument to a CLOSURE is owned
/// exactly once.
///
/// The body half was a run-vs-build divergence: a closure's by-value STRUCT
/// param ran zero `Drop` bodies compiled against the interpreter's one,
/// because `compile_closure_call` never registered the caller-side owner
/// that the free-function path gets from `track_inline_owned_aggregate_arg`.
/// This fixture is the half that decides whether the repair is safe, since
/// adding an owner is the double-free direction.
///
/// `tuple-arg` is the row that constrains it. A tuple argument ALREADY had
/// an owner, so registering unconditionally gave it two — measured as body
/// counts going 1 -> 2, and 2 -> 4 for a two-dropper tuple, on both
/// compiled backends. At a heap-carrying element that is a double free
/// rather than a doubled println, which is what this fixture would catch and
/// the behavioural twin would not.
#[test]
fn asan_closure_by_value_aggregate_arg_is_owned_once() {
    const H: &str = "struct R { id: i64, name: String }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"drop {self.id} {self.name}\") } }\n";
    // The row's shape: a struct argument, previously owned by nobody.
    assert_clean_asan_run(
        &format!(
            "{H}fn main() {{ let f = |r: R| {{ r.id }};\n\
             \x20            println(f\"{{f(R {{ id: 41, name: f\"n{{41}}\" }})}}\"); }}\n"
        ),
        // ORDER CORRECTED by B-2026-08-29-55 -- argument temp dies at call
        // return, before the enclosing `println`; matches `--interp`.
        &["drop 41 n41", "41"],
        "struct-arg",
    );
    // The same through a destructure in the body.
    assert_clean_asan_run(
            &format!("{H}struct W {{ r: R, n: i64 }}\n\
             fn main() {{ let f = |w: W| {{ let W {{ r, n }} = w; r.id + n }};\n\
             \x20            println(f\"{{f(W {{ r: R {{ id: 41, name: f\"n{{41}}\" }}, n: 1 }})}}\"); }}\n"),
            // ORDER CORRECTED by B-2026-08-29-55 -- see the sibling above.
            &["drop 41 n41", "42"],
            "struct-arg-destructured",
        );
    // CONSTRAINT — a tuple argument already had an owner. Registering a
    // second one doubles the drop, which at a heap element is a double free.
    assert_clean_asan_run(
        &format!(
            "{H}fn main() {{ let f = |p: (R, i64)| {{ let (r, n) = p; r.id + n }};\n\
             \x20            println(f\"{{f((R {{ id: 41, name: f\"n{{41}}\" }}, 1))}}\"); }}\n"
        ),
        // The tuple argument's body fires BEFORE the print while the struct
        // argument's fires after — the two are owned by different registrars
        // firing at different points (the B-2026-08-28-19 ordering family).
        // Counts are what this row is about; the order is recorded as
        // measured rather than assumed.
        &["drop 41 n41", "42"],
        "tuple-arg",
    );
    // CONTROL — the free-function spelling, correct all along.
    assert_clean_asan_run(
        &format!(
            "{H}fn take(r: R) -> i64 {{ r.id }}\n\
             fn main() {{ println(f\"{{take(R {{ id: 41, name: f\"n{{41}}\" }})}}\"); }}\n"
        ),
        // ORDER CORRECTED by B-2026-08-29-55 -- see the siblings above; the
        // free-function spelling was diverging from `--interp` too.
        &["drop 41 n41", "41"],
        "free-fn-control",
    );
}

#[test]
fn asan_closure_nested_consume_frees_fresh_arg_no_leak() {
    // B-2026-07-15-9: an indirect closure call passing a FRESH owned heap
    // temp (`String.from(..)`) whose owned param is consumed inside the
    // closure body — the caller must free the temp exactly as the direct-
    // call path does. The nested `wrap(wrap(s))` shape is where the closure
    // param survives O2 (the single-`wrap` shape was masked by dead-malloc
    // elision), so every iteration leaked the input String without the
    // caller-side `materialize_owned_temp` now emitted at the closure call
    // site. 30 iterations so any per-call leak accumulates past noise.
    assert_clean_asan_run(
        r#"
fn wrap(s: String) -> String { "[" + s + "]" }
fn main() {
    let f = |s: String| wrap(wrap(s));
    let mut i: i64 = 0i64;
    let mut total: i64 = 0i64;
    while i < 30i64 {
        let r = f(String.from("payload-string"));
        total = total + r.len();
        i = i + 1;
    }
    println(total.to_string());
}
"#,
        // wrap(wrap("payload-string")) = "[[payload-string]]" = 18 chars; 30*18 = 540
        &["540"],
        "closure_nested_consume_frees_fresh_arg_no_leak",
    );
}

#[test]
fn asan_closure_identity_returns_param_no_double_free() {
    // B-2026-07-15-9 double-free guard: with the caller now freeing the
    // fresh arg it passes to a closure, a closure that RETURNS its owned
    // heap param (`|s| s`, `|s| { s }`) must deep-copy the param on return —
    // otherwise the caller's arg-free and the result-binding's free hit the
    // same buffer (double-free / ASan abort). Both the bare-tail and
    // block-tail identity shapes, looped.
    assert_clean_asan_run(
        r#"
fn main() {
    let id1 = |s: String| s;
    let id2 = |s: String| { s };
    let mut i: i64 = 0i64;
    let mut total: i64 = 0i64;
    while i < 30i64 {
        let a = id1(String.from("first-payload"));
        let b = id2(String.from("second-payload"));
        total = total + a.len() + b.len();
        i = i + 1;
    }
    println(total.to_string());
}
"#,
        // 13 + 14 = 27 per iter; 30 * 27 = 810
        &["810"],
        "closure_identity_returns_param_no_double_free",
    );
}

#[test]
fn asan_closure_vec_arg_nested_consume_no_leak() {
    // B-2026-07-15-9 Vec sibling: the fresh-owned-heap-arg cleanup at the
    // closure call site covers `Vec` (`{ptr,len,cap}`), not only `String`.
    // A fresh `Vec.filled(..)` passed into a closure whose body consumes it
    // (`dup(dup(v))`, a by-value Vec param) must be freed by the caller.
    assert_clean_asan_run(
        r#"
fn dup(v: Vec[i64]) -> Vec[i64] {
    let mut out: Vec[i64] = Vec.new();
    out.push(v.len());
    out
}
fn main() {
    let f = |v: Vec[i64]| dup(dup(v));
    let mut i: i64 = 0i64;
    let mut total: i64 = 0i64;
    while i < 30i64 {
        let r = f(Vec.filled(5, 9));
        total = total + r[0];
        i = i + 1;
    }
    println(total.to_string());
}
"#,
        // dup(dup(filled(5))) → [1]; r[0] = 1; 30 * 1 = 30
        &["30"],
        "closure_vec_arg_nested_consume_no_leak",
    );
}

#[test]
fn asan_curry_closure_vec_store_no_leak() {
    // B-2026-07-12-12 — a curried closure (`let make = |n| |x| x + n`)
    // heap-allocates a reference-counted env box per outer call. When those
    // closures are stored in a `Vec[Fn]` that persists, the env boxes must
    // be freed when the Vec drops. This is un-elidable (the boxes escape
    // into a heap Vec, so LLVM's malloc-to-stack promotion can't remove
    // them): pre-fix it leaked one 16-byte box per iteration (1000 = 16 KB
    // definitely-lost under valgrind). The fix routes the curry call through
    // the SAME `is_heap_env_producing_call` predicate as a named heap-env
    // fn, so the Vec-owner slice frees each element's env on drop. 200 boxes
    // built + stored + dropped, so any per-iteration leak accumulates past
    // noise for LSan (Linux CI).
    assert_clean_asan_run(
        r#"
fn main() {
    let make = |n: i64| |x: i64| x + n;
    let mut fs: Vec[Fn(i64) -> i64] = Vec.new();
    let mut i: i64 = 0;
    while i < 200 {
        fs.push(make(i));
        i = i + 1;
    }
    println(f"{fs[100](0)}");
}
"#,
        &["100"],
        "asan_curry_closure_vec_store_no_leak",
    );
}

#[test]
fn asan_b04_2_destructuring_closure_heap_collect_no_leak() {
    // B-2026-07-04-2 sub-part 2 (destructuring half): a tuple-destructuring
    // `map` param over a HEAP element — `enumerate().map(|(i, s)| s)` over
    // `Vec[String]`. The fix desugars it to `|__dp| { let i = __dp.0; let s =
    // __dp.1; s }`: the index i64 is bound and dropped, the String field is
    // MOVED out of the `(i64, String)` tuple and pushed. Neither a leak (the
    // tuple's non-returned field) nor a double-free (the moved String) may
    // result. `.iter()` borrows, so the source survives. 40× with ≥45-byte
    // payloads for LSan reachability; reads a collected element each round.
    assert_clean_asan_run(
        r#"
fn main() {
    let mut round: i64 = 0i64;
    while round < 40i64 {
        let w: Vec[String] = Vec[
            "destructure-map-payload-alpha-aaaaaaaaaaaaaaaa".to_string(),
            "destructure-map-payload-bravo-bbbbbbbbbbbbbbbb".to_string(),
            "destructure-map-payload-charlie-cccccccccccccc".to_string()
        ];
        let kept: Vec[String] = w.iter().enumerate().map(|(i, s)| s).collect();
        let k1: String = kept[1i64].clone();
        println(f"{kept.len()} {w.len()} {k1}");
        round = round + 1i64;
    }
}
"#,
        ["3 3 destructure-map-payload-bravo-bbbbbbbbbbbbbbbb"]
            .repeat(40)
            .as_slice(),
        "asan_b04_2_destructuring_closure_heap_collect_no_leak",
    );
}

#[test]
fn asan_heap_env_closure_freed_no_leak() {
    assert_clean_asan_run(
        r#"
fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }
fn main() {
    let f = make(21i64);
    println(f"{f(21i64)}");
}
"#,
        &["42"],
        "asan_heap_env_closure_freed_no_leak",
    );
}

#[test]
fn asan_heap_env_closure_multi_call_freed_no_leak() {
    assert_clean_asan_run(
        r#"
fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }
fn main() {
    let f = make(20i64);
    let a = f(1i64);
    let b = f(2i64);
    println(f"{a + b}");
}
"#,
        &["43"],
        "asan_heap_env_closure_multi_call_freed_no_leak",
    );
}

#[test]
fn asan_closure_mut_ref_capture_no_leak() {
    // B-2026-07-11-23 — a non-escaping stored closure that MUTATES a captured
    // local captures it BY REFERENCE (a `ptr` to the outer slot in the env).
    // The body writes through the pointer to the real binding, so the write
    // lands on the outer `c` (yielding 7 over `f(3); f(4)`) and the by-ref
    // env carries no separate heap allocation to leak — a wrong (value-copy)
    // capture would silently drop the write. Also mixes in a read-only (by-
    // value) capture `k` to exercise the mixed per-name env layout.
    assert_clean_asan_run(
        r#"
fn main() {
    let mut c: i64 = 0;
    let k: i64 = 100;
    let f = |x: i64| { c = c + x + k; };
    f(3i64);
    f(4i64);
    println(f"{c}");
}
"#,
        &["207"],
        "asan_closure_mut_ref_capture_no_leak",
    );
}

/// Shared-ownership inc-on-copy (B-2026-06-22-2): copying a heap-env closure
/// binding (`let g = f`, plus a copy-of-a-copy `let h = g`) shares ONE RC env
/// box across all owners — the copy increments the refcount and each owner's
/// `FreeClosureEnv` decrements, so the box is freed EXACTLY once. Asserts no
/// leak (LSan) and no use-after-free / double-free (ASAN). Without the
/// inc-on-copy the box would be under-counted and freed early (UAF) by the
/// first owner's scope exit while later owners still alias it.
#[test]
fn asan_heap_env_closure_copy_freed_no_leak() {
    assert_clean_asan_run(
        r#"
fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }
fn main() {
    let f = make(10i64);
    let g = f;
    let h = g;
    println(f"{f(1i64) + g(2i64) + h(3i64)}");
}
"#,
        &["36"],
        "asan_heap_env_closure_copy_freed_no_leak",
    );
}

/// Slice 2 (B-2026-06-22-2): an escaping closure that captures a heap
/// String/Vec value. The env OWNS the buffer (freed by the per-closure
/// env-drop fn at RC-zero). A captured owned PARAM shallow-aliases the
/// caller's buffer, so it is DEEP-COPIED into the env (caller keeps its own,
/// env owns an independent copy); a captured LOCAL is moved (the source
/// binding's cap is zeroed so it does not double-free). All strings exceed
/// the SSO inline limit so the heap path is exercised. Asserts no leak (LSan) + no
/// double-free / UAF (ASAN) — without the env-drop the buffer leaks; without
/// the param deep-copy the caller and env both free it (double-free, which
/// glibc detects for Vec and silently corrupts for String).
#[test]
fn asan_heap_env_string_param_capture_no_leak() {
    assert_clean_asan_run(
        r#"
fn make(p: String) -> Fn(i64) -> i64 { |n| p.len() + n }
fn main() {
    let name = String.from("a heap-backed string well beyond the sso inline limit");
    let f = make(name);
    println(f"{f(0i64)}");
}
"#,
        &["53"],
        "asan_heap_env_string_param_capture_no_leak",
    );
}

#[test]
fn asan_heap_env_vec_param_capture_no_leak() {
    assert_clean_asan_run(
        r#"
fn make(v: Vec[i64]) -> Fn(i64) -> i64 { |n| v.len() + n }
fn main() {
    let mut xs = Vec.new();
    xs.push(1i64);
    xs.push(2i64);
    xs.push(3i64);
    let f = make(xs);
    println(f"{f(10i64)}");
}
"#,
        &["13"],
        "asan_heap_env_vec_param_capture_no_leak",
    );
}

#[test]
fn asan_heap_env_local_string_capture_no_leak() {
    assert_clean_asan_run(
        r#"
fn make() -> Fn(i64) -> i64 {
    let s = String.from("a heap-backed string well beyond the sso inline limit");
    |n| s.len() + n
}
fn main() { let f = make(); println(f"{f(0i64)}"); }
"#,
        &["53"],
        "asan_heap_env_local_string_capture_no_leak",
    );
}

#[test]
fn asan_heap_env_local_vec_capture_no_leak() {
    assert_clean_asan_run(
        r#"
fn make() -> Fn(i64) -> i64 {
    let mut v = Vec.new();
    v.push(5i64);
    v.push(6i64);
    |n| v.len() + n
}
fn main() { let f = make(); println(f"{f(1i64)}"); }
"#,
        &["3"],
        "asan_heap_env_local_vec_capture_no_leak",
    );
}

/// The heap-capture env box is RC-shared across a copy (`let g = f`); the
/// captured buffer is freed EXACTLY once when the last owner drops.
#[test]
fn asan_heap_env_string_capture_copied_no_double_free() {
    assert_clean_asan_run(
        r#"
fn make(p: String) -> Fn(i64) -> i64 { |n| p.len() + n }
fn main() {
    let f = make(String.from("a heap-backed string well beyond the sso inline limit"));
    let g = f;
    println(f"{f(0i64) + g(0i64)}");
}
"#,
        &["106"],
        "asan_heap_env_string_capture_copied_no_double_free",
    );
}

/// A mixed env (POD `base` + heap `String`): the env-drop frees ONLY the
/// String field's buffer, leaving the POD word untouched.
#[test]
fn asan_heap_env_mixed_pod_string_capture_no_leak() {
    assert_clean_asan_run(
        r#"
fn make(p: String, base: i64) -> Fn(i64) -> i64 { |n| p.len() + base + n }
fn main() {
    let f = make(String.from("a heap-backed string well beyond the sso inline limit"), 100i64);
    println(f"{f(5i64)}");
}
"#,
        &["158"],
        "asan_heap_env_mixed_pod_string_capture_no_leak",
    );
}

/// Return-again move-out (B-2026-06-22-2): a relay RE-RETURNS a bound
/// heap-env closure (explicit `return f`, bare-identifier tail, relay-of-a-
/// relay, and copy-then-return). The RC env box MOVES OUT of each relay to
/// its caller — the source binding's `FreeClosureEnv` is neutralized on the
/// returning path, so the box is freed EXACTLY once at the final owner's
/// scope exit. Asserts no leak (LSan) and no use-after-free / double-free
/// (ASAN). Without the move-out, the relay's scope exit would free the box
/// the caller still holds (UAF), or double-free across copy-then-return.
#[test]
fn asan_heap_env_closure_returned_again_freed_no_leak() {
    assert_clean_asan_run(
        r#"
fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }
fn relay(k: i64) -> Fn(i64) -> i64 { let f = make(k); return f; }
fn relay_tail(k: i64) -> Fn(i64) -> i64 { let f = make(k); f }
fn relay2(k: i64) -> Fn(i64) -> i64 { let g = relay_tail(k); g }
fn relay_copy(k: i64) -> Fn(i64) -> i64 { let f = make(k); let g = f; g }
fn main() {
    let a = relay(10i64);
    let b = relay_tail(20i64);
    let c = relay2(30i64);
    let d = relay_copy(40i64);
    println(f"{a(1i64) + b(2i64) + c(3i64) + d(4i64)}");
}
"#,
        &["110"],
        "asan_heap_env_closure_returned_again_freed_no_leak",
    );
}

/// Store-in-struct slice (B-2026-06-22-2): a fresh heap-env closure stored in
/// a struct literal field (`let h = H { f: make(k) }`) is RC-dropped
/// per-instance via a `FreeClosureEnv` on that field at the struct local's
/// scope exit — freed EXACTLY once. Covers a closure-only struct and a struct
/// with a sibling data field. Asserts no leak (LSan) and no use-after-free /
/// double-free (ASAN). Without the instance field drop the env would leak;
/// with a (wrong) type-driven drop a sibling stack-env closure would crash —
/// neither happens here.
#[test]
fn asan_heap_env_stored_in_struct_field_freed_no_leak() {
    assert_clean_asan_run(
        r#"
struct H { f: Fn(i64) -> i64 }
struct G { f: Fn(i64) -> i64, n: i64 }
fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }
fn main() {
    let h = H { f: make(21i64) };
    let g = G { f: make(20i64), n: 2i64 };
    println(f"{(h.f)(21i64) + (g.f)(20i64) + g.n}");
}
"#,
        &["84"],
        "asan_heap_env_stored_in_struct_field_freed_no_leak",
    );
}

/// Binding-source slice (B-2026-06-22-2): a heap-env BINDING stored into a
/// struct field (`let h = H { f: f }`) is co-owned — the store bumps the shared
/// RC env's refcount, so both the source binding's scope-exit drop AND the
/// field's instance drop fire and the box is freed EXACTLY once. Covers
/// co-ownership with the source still used after the store, a store through a
/// COPY of the binding (`let g = f; H { f: g }`), and the composite where the
/// source is MOVED OUT (tail return) after being stored — the field drop decs,
/// the caller frees the last ref. Without the store inc the box would be
/// double-freed (ASAN); without the field drop it would leak (LSan).
#[test]
fn asan_heap_env_binding_stored_in_struct_field_freed_no_leak() {
    assert_clean_asan_run(
        r#"
struct H { f: Fn(i64) -> i64 }
struct G { f: Fn(i64) -> i64, n: i64 }
fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }
fn relay(k: i64) -> Fn(i64) -> i64 { let f = make(k); let h = H { f: f }; f }
fn main() {
    let f = make(10i64);
    let h = H { f: f };
    let a = f(1i64);
    let b = (h.f)(2i64);
    let g = make(5i64);
    let g2 = g;
    let gg = G { f: g2, n: 3i64 };
    let c = g(4i64);
    let d = (gg.f)(6i64);
    let e = gg.n;
    let r = relay(30i64);
    let k = r(7i64);
    println(f"{a + b + c + d + e + k}");
}
"#,
        &["83"],
        "asan_heap_env_binding_stored_in_struct_field_freed_no_leak",
    );
}

/// Aggregate-escape slice (B-2026-06-22-2): a function may RETURN a struct that
/// OWNS a heap-env closure field. The env box MOVES OUT inside the struct — the
/// callee neutralizes the owner's field env slot on the returning path (so its
/// `FreeClosureEnv` no-ops), and the caller's `let r = build(..)` binding
/// registers an instance `FreeClosureEnv` on each owned field and frees it once.
/// Covers an explicit return, a bare-tail return, a sibling data field, a
/// binding-source field (store inc → rc 2, then move-out decs to 1 at callee
/// scope exit), and a relay-of-aggregate (fixpoint). Each env freed EXACTLY
/// once across the move boundary — without the move-out the callee would free
/// the box the caller holds (UAF); without the caller field drop it would leak.
#[test]
fn asan_heap_env_aggregate_returned_freed_no_leak() {
    assert_clean_asan_run(
        r#"
struct H { f: Fn(i64) -> i64 }
struct G { f: Fn(i64) -> i64, n: i64 }
fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }
fn build(k: i64) -> H { let h = H { f: make(k) }; return h; }
fn build_tail(k: i64) -> H { let h = H { f: make(k) }; h }
fn build_data(k: i64) -> G { let g = G { f: make(k), n: 3i64 }; g }
fn build_binding(k: i64) -> H { let f = make(k); let h = H { f: f }; return h; }
fn relay(k: i64) -> H { let r = build(k); return r; }
fn main() {
    let a = build(10i64);
    let b = build_tail(20i64);
    let c = build_data(30i64);
    let d = build_binding(40i64);
    let e = relay(5i64);
    println(f"{(a.f)(1i64) + (b.f)(2i64) + (c.f)(3i64) + c.n + (d.f)(4i64) + (e.f)(6i64)}");
}
"#,
        &["124"],
        "asan_heap_env_aggregate_returned_freed_no_leak",
    );
}

/// Tuple-store slice (B-2026-06-22-2): a heap-env closure stored in a tuple
/// element is RC-dropped per-instance via a `FreeClosureEnv` on that element.
/// Covers a FRESH-call element with a sibling data element, a BINDING source
/// (store inc → rc 2, source still used, both drop → one free), and two
/// closures in one tuple. Each env freed EXACTLY once at scope exit — without
/// the element drop they leak (LSan); with the binding store missing the inc it
/// double-frees (ASAN).
#[test]
fn asan_heap_env_stored_in_tuple_freed_no_leak() {
    assert_clean_asan_run(
        r#"
fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }
fn main() {
    let t = (make(10i64), 2i64);
    let f = make(20i64);
    let u = (f, 3i64);
    let v = (make(5i64), make(7i64));
    println(f"{(t.0)(1i64) + t.1 + f(0i64) + (u.0)(1i64) + u.1 + (v.0)(1i64) + (v.1)(2i64)}");
}
"#,
        &["72"],
        "asan_heap_env_stored_in_tuple_freed_no_leak",
    );
}

/// Array-store slice (B-2026-06-22-2): a heap-env closure stored in a
/// fixed-size array element is RC-dropped per-instance via a `FreeClosureEnv`
/// on that element GEP. Covers a FRESH single-element array, a multi-element
/// array (two closures, each called through), and a BINDING source (store inc →
/// rc 2, source still used, both drop → one free). Each env freed EXACTLY once
/// at scope exit — without the element drop they leak (LSan); with the binding
/// store missing the inc it double-frees (ASAN).
#[test]
fn asan_heap_env_stored_in_array_freed_no_leak() {
    assert_clean_asan_run(
        r#"
fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }
fn main() {
    let a: Array[Fn(i64) -> i64, 1] = [make(10i64)];
    let b: Array[Fn(i64) -> i64, 2] = [make(5i64), make(7i64)];
    let f = make(20i64);
    let c: Array[Fn(i64) -> i64, 1] = [f];
    println(f"{(a[0])(1i64) + (b[0])(1i64) + (b[1])(2i64) + f(0i64) + (c[0])(1i64)}");
}
"#,
        &["67"],
        "asan_heap_env_stored_in_array_freed_no_leak",
    );
}

/// Vec-store slice (B-2026-06-22-2): heap-env closures pushed into a `Vec[Fn]`
/// are RC-dropped by a DYNAMIC `0..len` drop loop at the Vec's scope exit.
/// Covers a LOOP of fresh pushes (the dynamic-length case — three element envs
/// freed by the loop) and a BINDING push (push inc → rc 2, source `f` still
/// used, both the source's `FreeClosureEnv` and the Vec drop loop decrement →
/// one free). Without the drop loop the element envs leak (LSan); with the
/// binding push missing the inc it double-frees (ASAN). Also exercises the
/// auto-par bail (`let f = make(..)` no longer parallelized with `Vec.new()`).
#[test]
fn asan_heap_env_stored_in_vec_freed_no_leak() {
    assert_clean_asan_run(
        r#"
fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }
fn main() {
    let mut v: Vec[Fn(i64) -> i64] = Vec.new();
    let mut i = 0i64;
    while i < 3i64 { v.push(make(i)); i = i + 1i64; }
    let f = make(20i64);
    let mut w: Vec[Fn(i64) -> i64] = Vec.new();
    w.push(f);
    let mut acc = 0i64;
    let mut j = 0i64;
    while j < v.len() { acc = acc + (v[j])(10i64); j = j + 1i64; }
    acc = acc + f(0i64) + (w[0])(1i64);
    println(f"{acc}");
}
"#,
        &["74"],
        "asan_heap_env_stored_in_vec_freed_no_leak",
    );
}

/// Container-escape slice (B-2026-06-22-2): a function returns a TUPLE / ARRAY
/// owning heap-env closure elements; the callee moves the env boxes out (its
/// return neutralizes the owner's element env slots) and the caller's binding
/// adopts a per-element `FreeClosureEnv`. Covers a fresh-element tuple escape, a
/// fresh-element array escape, and a BINDING-element tuple escape (store inc →
/// callee source drop + caller adopted drop = one free). Each env freed EXACTLY
/// once — without the caller-adopt it leaks (LSan); without the callee neutralize
/// it double-frees (ASAN).
#[test]
fn asan_heap_env_container_escape_freed_no_leak() {
    assert_clean_asan_run(
        r#"
fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }
fn build_t(k: i64) -> (Fn(i64) -> i64, i64) { let t = (make(k), 1i64); t }
fn build_a(k: i64) -> Array[Fn(i64) -> i64, 1] { let a: Array[Fn(i64) -> i64, 1] = [make(k)]; a }
fn build_bf(k: i64) -> (Fn(i64) -> i64, i64) { let f = make(k); let t = (f, 2i64); t }
fn main() {
    let r = build_t(10i64);
    let s = build_a(20i64);
    let u = build_bf(30i64);
    println(f"{(r.0)(1i64) + r.1 + (s[0])(2i64) + (u.0)(0i64) + u.1}");
}
"#,
        &["66"],
        "asan_heap_env_container_escape_freed_no_leak",
    );
}

/// Vec-escape slice (B-2026-06-22-2): a function returns a closure-owning
/// `Vec[Fn]`; the callee moves the BUFFER out (its tail-return cap-zero
/// suppresses its own dynamic drop loop) and the caller's binding adopts that
/// `0..len` drop loop. Covers a loop-built escape (3 element envs adopted), a
/// BINDING-push escape (store inc → callee source drop + caller loop drop = one
/// free), and a RELAY (the buffer flows callee→relay→caller, freed once at the
/// outermost binding). Without the caller-adopt the envs leak (LSan); if the
/// callee's loop weren't cap-zero suppressed it double-frees (ASAN).
#[test]
fn asan_heap_env_vec_escape_freed_no_leak() {
    assert_clean_asan_run(
        r#"
fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }
fn build(n: i64) -> Vec[Fn(i64) -> i64] {
    let mut v: Vec[Fn(i64) -> i64] = Vec.new();
    let mut i = 0i64;
    while i < n { v.push(make(i)); i = i + 1i64; }
    v
}
fn build_bf(k: i64) -> Vec[Fn(i64) -> i64] {
    let f = make(k);
    let mut v: Vec[Fn(i64) -> i64] = Vec.new();
    v.push(f);
    v
}
fn relay(n: i64) -> Vec[Fn(i64) -> i64] { let q = build(n); q }
fn main() {
    let r = build(3i64);
    let w = build_bf(20i64);
    let z = relay(2i64);
    let mut acc = 0i64;
    let mut j = 0i64;
    while j < r.len() { acc = acc + (r[j])(10i64); j = j + 1i64; }
    acc = acc + (w[0])(2i64);
    acc = acc + (z[0])(5i64) + (z[1])(6i64);
    println(f"{acc}");
}
"#,
        &["67"],
        "asan_heap_env_vec_escape_freed_no_leak",
    );
}

/// By-value arg-pass slice (B-2026-06-22-2): a heap-env closure BINDING passed
/// BY VALUE to a borrows-only callee (one that only CALLS it) is a pure
/// BORROW — the callee never frees the shared RC env, and the CALLER retains
/// sole ownership and RC-drops it EXACTLY once at scope exit (no inc, no
/// move-out). Covers the bare binding (`apply(f, ..)`), a borrow inside a
/// loop (`sumcalls`), a copy passed by value (`apply(g, ..)`), a borrows-only
/// aggregate builder (`build2` — its own returned env is a SECOND, distinct
/// box freed once), and continued use of `f` after the borrow. Asserts no
/// leak (LSan) and no use-after-free / double-free (ASAN). Without the
/// borrow-only treatment the callee would either free the caller's box early
/// (UAF) or the caller would free it twice; an erroneous inc would leak it.
#[test]
fn asan_heap_env_arg_pass_borrow_freed_no_leak() {
    assert_clean_asan_run(
        r#"
struct H { f: Fn(i64) -> i64 }
fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }
fn apply(g: Fn(i64) -> i64, x: i64) -> i64 { g(x) }
fn sumcalls(g: Fn(i64) -> i64, n: i64) -> i64 {
    let mut s = 0i64;
    let mut i = 0i64;
    while i < n { s = s + g(i); i = i + 1i64; }
    s
}
fn build2(g: Fn(i64) -> i64) -> H { let local = make(7i64); let h = H { f: local }; let _u = g(0i64); h }
fn main() {
    let f = make(10i64);
    let g = f;
    let a = apply(f, 5i64);
    let b = sumcalls(g, 4i64);
    let r = build2(f);
    let c = (r.f)(3i64);
    let d = f(100i64);
    println(f"{a + b + c + d}");
}
"#,
        &["181"],
        "asan_heap_env_arg_pass_borrow_freed_no_leak",
    );
}

/// Owner by-value arg-pass slice (B-2026-06-22-2): a heap-env STRUCT OWNER
/// (`let a = H { f: make(k), g: make(k) }`) passed BY VALUE to a borrows-only
/// callee (one that only CALLS the owner's closure fields via `(h.f)(x)`) is a
/// pure BORROW — the callee never frees the shared RC envs (a param gets no
/// Fn-field `FreeClosureEnv`), and the CALLER retains sole ownership and
/// RC-drops each env EXACTLY once at scope exit (no inc, no move-out — a call
/// arg is not a return move-out, so the owner's env slots are not neutralized).
/// The owner stays usable after the call (`(a.f)(1) + (a.g)(1)`). Without the
/// borrow-only treatment the callee would free the caller's boxes early (UAF)
/// or the owner would free them twice; an erroneous inc would leak them.
#[test]
fn asan_heap_env_struct_owner_arg_pass_borrow_freed_no_leak() {
    assert_clean_asan_run(
        r#"
struct H { f: Fn(i64) -> i64, g: Fn(i64) -> i64 }
fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }
fn use_it(h: H) -> i64 { (h.f)(1i64) + (h.g)(2i64) }
fn main() {
    let a = H { f: make(10i64), g: make(20i64) };
    let r = use_it(a);
    println(f"{r + (a.f)(1i64) + (a.g)(1i64)}");
}
"#,
        &["65"],
        "asan_heap_env_struct_owner_arg_pass_borrow_freed_no_leak",
    );
}

/// Owner by-value arg-pass with a sibling HEAP String field: the struct owner is
/// passed by value to a borrows-only callee. The `Fn` env is borrowed (caller
/// frees once), and the sibling `String` is handled by the normal owned-struct
/// arg-pass copy semantics — the caller's owner stays valid and readable after
/// the call (`a.name`), each heap allocation freed exactly once. Guards against
/// a String double-free (if the param drop and the caller's owner drop both
/// freed a shared buffer) or a leak.
#[test]
fn asan_heap_env_struct_owner_arg_pass_string_sibling_freed_no_leak() {
    assert_clean_asan_run(
        r#"
struct H { f: Fn(i64) -> i64, name: String }
fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }
fn use_it(h: H) -> i64 { (h.f)(1i64) }
fn main() {
    let a = H { f: make(10i64), name: "an independently long heap string payload here" };
    let r = use_it(a);
    println(f"{r + (a.f)(2i64)}");
    println(a.name);
}
"#,
        &["23", "an independently long heap string payload here"],
        "asan_heap_env_struct_owner_arg_pass_string_sibling_freed_no_leak",
    );
}

/// Container owner arg-pass slice (B-2026-06-22-2): a heap-env TUPLE owner with
/// TWO closure elements passed BY VALUE to a borrows-only callee. The callee
/// receives a shallow copy of the tuple aliasing the SAME RC env boxes, calls
/// both elements, and frees neither (a param gets no per-element
/// `FreeClosureEnv`); the caller retains sole ownership and RC-drops each env
/// EXACTLY once at scope exit (no inc, no move-out). The owner stays usable
/// after the call. `(t.0)(1)+(t.1)(2)=33` in the callee, then `(a.0)(1)+(a.1)(1)
/// =32` after → `65`. Without the borrow treatment the callee would free the
/// caller's boxes early (UAF) or both would free them (double-free); a stray inc
/// would leak them.
#[test]
fn asan_heap_env_tuple_owner_arg_pass_borrow_freed_no_leak() {
    assert_clean_asan_run(
        r#"
fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }
fn use_t(t: (Fn(i64) -> i64, Fn(i64) -> i64)) -> i64 { (t.0)(1i64) + (t.1)(2i64) }
fn main() {
    let a = (make(10i64), make(20i64));
    let r = use_t(a);
    println(f"{r + (a.0)(1i64) + (a.1)(1i64)}");
}
"#,
        &["65"],
        "asan_heap_env_tuple_owner_arg_pass_borrow_freed_no_leak",
    );
}

/// The ARRAY twin: a two-element `Array[Fn,2]` owner borrowed (both elements
/// called via the `[0, idx]` GEP), reused after the call. Each shared env freed
/// exactly once. `33` in the callee, `32` after → `65`.
#[test]
fn asan_heap_env_array_owner_arg_pass_borrow_freed_no_leak() {
    assert_clean_asan_run(
        r#"
fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }
fn use_a(a: Array[Fn(i64) -> i64, 2]) -> i64 { (a[0])(1i64) + (a[1])(2i64) }
fn main() {
    let a: Array[Fn(i64) -> i64, 2] = [make(10i64), make(20i64)];
    let r = use_a(a);
    println(f"{r + (a[0])(1i64) + (a[1])(1i64)}");
}
"#,
        &["65"],
        "asan_heap_env_array_owner_arg_pass_borrow_freed_no_leak",
    );
}

/// The `Vec[Fn]` owner arg-pass — the CRITICAL case. By-value arg-pass passes
/// the Vec header by value WITHOUT zeroing the caller's cap (unlike `let w = v`,
/// a move), so it is a BORROW: the callee reads through the shared buffer and
/// frees nothing; the caller's dynamic per-element drop loop frees every element
/// env (and the buffer) EXACTLY once. Multi-element to exercise that loop, and
/// the owner is reused after the call (`(v[0])/(v[1])` still valid — the cap was
/// not zeroed). `33` in the callee, `32` after → `65`. Were arg-pass a move, the
/// callee would adopt and free the buffer while the caller's loop freed it too
/// (double-free), or the element envs would leak.
#[test]
fn asan_heap_env_vec_owner_arg_pass_borrow_freed_no_leak() {
    assert_clean_asan_run(
        r#"
fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }
fn use_v(v: Vec[Fn(i64) -> i64]) -> i64 { (v[0])(1i64) + (v[1])(2i64) }
fn main() {
    let mut v: Vec[Fn(i64) -> i64] = Vec.new();
    v.push(make(10i64));
    v.push(make(20i64));
    let r = use_v(v);
    println(f"{r + (v[0])(1i64) + (v[1])(1i64)}");
}
"#,
        &["65"],
        "asan_heap_env_vec_owner_arg_pass_borrow_freed_no_leak",
    );
}

/// A TUPLE owner with a sibling HEAP String element passed by value to a
/// borrows-only callee. The `Fn` env is borrowed (caller frees once) and the
/// String element rides along by the normal owned arg-pass copy — the caller's
/// owner stays valid and readable after the call (`a.1`), each heap allocation
/// freed exactly once. Guards against a String double-free or a leak alongside
/// the borrowed closure env. `r=(t.0)(1)=11`, `r+(a.0)(2)=23`, then `a.1`.
#[test]
fn asan_heap_env_tuple_owner_arg_pass_string_sibling_freed_no_leak() {
    assert_clean_asan_run(
        r#"
fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }
fn use_t(t: (Fn(i64) -> i64, String)) -> i64 { (t.0)(1i64) }
fn main() {
    let a = (make(10i64), "an independently long heap string payload here");
    let r = use_t(a);
    println(f"{r + (a.0)(2i64)}");
    println(a.1);
}
"#,
        &["23", "an independently long heap string payload here"],
        "asan_heap_env_tuple_owner_arg_pass_string_sibling_freed_no_leak",
    );
}

/// Reassignment slice (B-2026-06-22-2): `g = f` where both are heap-env closure
/// bindings is a COPY — the reassignment drops `g`'s OLD env, incs the SHARED
/// env `f` holds, and stores it, so `g` and `f` co-own one box freed EXACTLY
/// once while `g`'s original env is freed once at the reassignment. Both `g`
/// and `f` are used after (`g(1) + f(1)`), confirming the source stays a live
/// co-owner. Without the drop-old the original `g` env leaks; without the inc
/// the shared box is freed twice (the source's and `g`'s scope-exit drops).
#[test]
fn asan_heap_env_binding_reassign_copy_no_leak() {
    assert_clean_asan_run(
        r#"
fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }
fn main() {
    let f = make(10i64);
    let mut g = make(20i64);
    g = f;
    println(f"{g(1i64) + f(1i64)}");
}
"#,
        &["22"],
        "asan_heap_env_binding_reassign_copy_no_leak",
    );
}

/// `g = make(j)` is a MOVE to a fresh env: the reassignment drops `g`'s old env
/// (freed once) and `g` becomes the sole owner of the fresh one (freed once at
/// scope exit). Without the drop-old the original env leaks. `g(5) = 35`.
#[test]
fn asan_heap_env_binding_reassign_to_fresh_no_leak() {
    assert_clean_asan_run(
        r#"
fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }
fn main() {
    let mut g = make(20i64);
    g = make(30i64);
    println(f"{g(5i64)}");
}
"#,
        &["35"],
        "asan_heap_env_binding_reassign_to_fresh_no_leak",
    );
}

/// The strongest leak case: reassigning in a LOOP. Each of the 50 iterations
/// drops the prior env before storing the next, so every intermediate env box
/// is freed once — without the per-assignment drop-old, 50 unreachable env
/// boxes would leak (LSan-caught). The final `make(50*10)`; `g(5) = 505`.
#[test]
fn asan_heap_env_binding_reassign_in_loop_no_leak() {
    assert_clean_asan_run(
        r#"
fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }
fn main() {
    let mut g = make(0i64);
    let mut i = 1i64;
    while i <= 50i64 {
        g = make(i * 10i64);
        i = i + 1;
    }
    println(f"{g(5i64)}");
}
"#,
        &["505"],
        "asan_heap_env_binding_reassign_in_loop_no_leak",
    );
}

/// FIELD reassignment slice (B-2026-06-22-2): `r.f = g` where `r` is a heap-env
/// struct owner and `g` a heap-env binding is a COPY — the reassignment drops
/// `r.f`'s OLD env, incs the SHARED env `g` holds, and stores it into the field
/// slot, so `r.f` and `g` co-own one box freed EXACTLY once while `r.f`'s
/// original env is freed once at the reassignment. Both `r.f` and `g` are used
/// after, confirming the source stays a live co-owner. Without the drop-old the
/// original field env leaks; without the inc the shared box is freed twice (the
/// field's and `g`'s scope-exit drops). A POD sibling field rides along.
#[test]
fn asan_heap_env_struct_field_reassign_copy_no_leak() {
    assert_clean_asan_run(
        r#"
struct H { f: Fn(i64) -> i64, n: i64 }
fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }
fn main() {
    let g = make(100i64);
    let mut h = H { f: make(10i64), n: 7i64 };
    h.f = g;
    println(f"{(h.f)(1i64) + g(1i64) + h.n}");
}
"#,
        &["209"],
        "asan_heap_env_struct_field_reassign_copy_no_leak",
    );
}

/// `r.f = make(j)` is a MOVE to a fresh field env: the reassignment drops
/// `r.f`'s old env (freed once) and the field becomes the sole owner of the
/// fresh one (freed once at scope exit). Without the drop-old the original
/// field env leaks. `(h.f)(5) = 25`.
#[test]
fn asan_heap_env_struct_field_reassign_to_fresh_no_leak() {
    assert_clean_asan_run(
        r#"
struct H { f: Fn(i64) -> i64 }
fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }
fn main() {
    let mut h = H { f: make(10i64) };
    h.f = make(20i64);
    println(f"{(h.f)(5i64)}");
}
"#,
        &["25"],
        "asan_heap_env_struct_field_reassign_to_fresh_no_leak",
    );
}

/// The strongest field-reassign leak case: reassigning a field in a LOOP. Each
/// of the 50 iterations drops the prior field env before storing the next, so
/// every intermediate env box is freed once — without the per-assignment
/// drop-old, 50 unreachable env boxes would leak (LSan-caught). A two-closure-
/// field owner: only `f` is reassigned, `g` stays the original (freed once).
/// Last `f` is `make(500)`; `(h.f)(5) + (h.g)(0) = 505 + 20 = 525`.
#[test]
fn asan_heap_env_struct_field_reassign_in_loop_no_leak() {
    assert_clean_asan_run(
        r#"
struct H { f: Fn(i64) -> i64, g: Fn(i64) -> i64 }
fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }
fn main() {
    let mut h = H { f: make(0i64), g: make(20i64) };
    let mut i = 1i64;
    while i <= 50i64 {
        h.f = make(i * 10i64);
        i = i + 1;
    }
    println(f"{(h.f)(5i64) + (h.g)(0i64)}");
}
"#,
        &["525"],
        "asan_heap_env_struct_field_reassign_in_loop_no_leak",
    );
}

/// VEC ELEMENT reassignment slice (B-2026-06-22-2), the final form: `v[i] = g`
/// where `v` is a heap-env `Vec[Fn]` owner and `g` a heap-env binding is a COPY
/// — the reassignment drops `v[i]`'s OLD env, incs the SHARED env `g` holds, and
/// stores it into the element slot, so `v[i]` and `g` co-own one box freed
/// EXACTLY once (the Vec's refcount-aware drop loop decs it, `g`'s scope-exit
/// drop decs it) while `v[i]`'s original env is freed once at the reassignment.
/// Both `v[i]` and `g` are used after. A second element rides along untouched.
/// Without the drop-old the original element env leaks; without the inc the
/// shared box is freed twice.
#[test]
fn asan_heap_env_vec_element_reassign_copy_no_leak() {
    assert_clean_asan_run(
        r#"
fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }
fn main() {
    let g = make(100i64);
    let mut v: Vec[Fn(i64) -> i64] = Vec.new();
    v.push(make(10i64));
    v.push(make(20i64));
    v[0i64] = g;
    println(f"{(v[0i64])(1i64) + g(1i64) + (v[1i64])(2i64)}");
}
"#,
        &["224"],
        "asan_heap_env_vec_element_reassign_copy_no_leak",
    );
}

/// `v[i] = make(j)` is a MOVE to a fresh element env: the reassignment drops
/// `v[i]`'s old env (freed once) and the element becomes the sole owner of the
/// fresh one (freed once by the drop loop). Without the drop-old the original
/// element env leaks. `(v[0])(5) = 25`.
#[test]
fn asan_heap_env_vec_element_reassign_to_fresh_no_leak() {
    assert_clean_asan_run(
        r#"
fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }
fn main() {
    let mut v: Vec[Fn(i64) -> i64] = Vec.new();
    v.push(make(10i64));
    v[0i64] = make(20i64);
    println(f"{(v[0i64])(5i64)}");
}
"#,
        &["25"],
        "asan_heap_env_vec_element_reassign_to_fresh_no_leak",
    );
}

/// The strongest Vec-element leak case: reassigning over a DYNAMIC index in a
/// LOOP. Each of the 50 iterations drops the prior element env before storing
/// the next, across all elements, so every intermediate env box is freed once —
/// without the per-assignment drop-old, the overwritten env boxes would leak
/// (LSan-caught). Sum over the final pass: `(v[k])(0) = k*10` for k in 0..5 plus
/// the loop's last writes — the program prints the final-state sum `100`.
#[test]
fn asan_heap_env_vec_element_reassign_in_loop_no_leak() {
    assert_clean_asan_run(
        r#"
fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }
fn main() {
    let mut v: Vec[Fn(i64) -> i64] = Vec.new();
    v.push(make(0i64));
    v.push(make(0i64));
    v.push(make(0i64));
    v.push(make(0i64));
    v.push(make(0i64));
    let mut i = 0i64;
    while i < 50i64 {
        let mut k = 0i64;
        while k < 5i64 {
            v[k] = make(k * 10i64);
            k = k + 1i64;
        }
        i = i + 1;
    }
    println(f"{(v[0i64])(0i64) + (v[1i64])(0i64) + (v[2i64])(0i64) + (v[3i64])(0i64) + (v[4i64])(0i64)}");
}
"#,
        &["100"],
        "asan_heap_env_vec_element_reassign_in_loop_no_leak",
    );
}

/// Owner-copy slice (B-2026-06-22-2): `let s = a` where `a` is a heap-env
/// STRUCT owner. The struct copy shallow-copies the `Fn` field so `s` aliases
/// `a`'s SAME RC env box; the copy INCs the shared env and registers `s`'s own
/// `FreeClosureEnv`, so each owner RC-drops once and the box is freed EXACTLY
/// once (COPY semantics — `a` stays live). Covers a 3-owner copy chain
/// (`a`→`s`→`t`, rc reaches 3, three balanced drops), a sibling HEAP String
/// field (DEEP-copied to independent buffers, composing with the env inc), and
/// owner-copy-then-ESCAPE (`build` returns the copy `s` — move-out + caller
/// adopt). Asserts no leak (LSan) and no use-after-free / double-free (ASAN).
/// Without the inc the first owner's drop would free the box the others still
/// alias (UAF / double-free); a stray extra inc would leak it.
#[test]
fn asan_heap_env_owner_copy_freed_no_leak() {
    assert_clean_asan_run(
        r#"
struct H { f: Fn(i64) -> i64, name: String }
fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }
fn build(k: i64) -> H { let a = H { f: make(k), name: "an independently heap-copied payload" }; let s = a; s }
fn main() {
    let a = H { f: make(10i64), name: "another sufficiently long heap string here" };
    let s = a;
    let t = s;
    let r = build(20i64);
    println(f"{(a.f)(1i64) + (s.f)(1i64) + (t.f)(1i64) + (r.f)(2i64)}");
}
"#,
        &["55"],
        "asan_heap_env_owner_copy_freed_no_leak",
    );
}

/// Owner-copy slice (B-2026-06-22-2), TUPLE: `let s = t` where `t` is a heap-env
/// tuple owner. The tuple copy shallow-copies the inline `Fn` fat pointer so
/// `s`'s element aliases `t`'s SAME RC env box; the copy INCs the shared env and
/// registers `s`'s own per-element `FreeClosureEnv`, so each owner RC-drops once
/// and the box is freed EXACTLY once (COPY semantics — `t` stays live). Covers a
/// 3-owner copy chain (`a`→`s`→`t`, rc reaches 3, three balanced drops) and
/// owner-copy-then-ESCAPE (`build` returns the copy `s` — move-out neutralizes
/// `s`, the caller adopts). Without the inc the first owner's drop would free the
/// box the others still alias (UAF / double-free); a stray extra inc would leak.
#[test]
fn asan_heap_env_tuple_owner_copy_freed_no_leak() {
    assert_clean_asan_run(
        r#"
fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }
fn build(k: i64) -> (Fn(i64) -> i64, i64) { let a = (make(k), 0i64); let s = a; s }
fn main() {
    let a = (make(10i64), 0i64);
    let s = a;
    let t = s;
    let r = build(20i64);
    println(f"{(a.0)(1i64) + (s.0)(1i64) + (t.0)(1i64) + (r.0)(2i64)}");
}
"#,
        &["55"],
        "asan_heap_env_tuple_owner_copy_freed_no_leak",
    );
}

/// The ARRAY twin: a fixed-size `Array[Fn,2]` owner copied (chain + escape).
/// Exercises the array element-GEP path (`[0, idx]`) across BOTH elements — each
/// env is inc'd and freed exactly once. `build` returns a 2-element array copy
/// `s` (multi-element move-out + caller adopt).
#[test]
fn asan_heap_env_array_owner_copy_freed_no_leak() {
    assert_clean_asan_run(
        r#"
fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }
fn build(k: i64) -> Array[Fn(i64) -> i64, 2] { let a: Array[Fn(i64) -> i64, 2] = [make(k), make(k + 5i64)]; let s = a; s }
fn main() {
    let a: Array[Fn(i64) -> i64, 2] = [make(10i64), make(20i64)];
    let s = a;
    let t = s;
    let r = build(30i64);
    println(f"{(a[0])(1i64) + (s[1])(1i64) + (t[0])(1i64) + (r[0])(2i64) + (r[1])(2i64)}");
}
"#,
        &["112"],
        "asan_heap_env_array_owner_copy_freed_no_leak",
    );
}

/// Owner-copy slice (B-2026-06-22-2), tuple with a HEAP String SIBLING: the
/// String's buffer is SHARED (the source's drop is suppressed via cap-zero, the
/// copy frees it exactly once) while the `Fn` element env is RC-inc'd. LSan/ASAN
/// confirm the string is freed exactly once and the env exactly once — no leak,
/// no double-free, no use-after-free (both owners read the shared buffer before
/// scope exit). Composes the closure-env inc with the pre-existing tuple-copy
/// heap-field move.
#[test]
fn asan_heap_env_tuple_owner_copy_string_sibling_freed_no_leak() {
    assert_clean_asan_run(
        r#"
fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }
fn main() {
    let a = (make(10i64), "an independently long heap string payload here");
    let s = a;
    println(f"{(a.0)(1i64) + (s.0)(2i64)}");
    println(s.1);
    println(a.1);
}
"#,
        &[
            "23",
            "an independently long heap string payload here",
            "an independently long heap string payload here",
        ],
        "asan_heap_env_tuple_owner_copy_string_sibling_freed_no_leak",
    );
}

/// Owner-copy slice (B-2026-06-22-2), VEC: `let w = v` where `v` is a heap-env
/// `Vec[Fn]` owner is a MOVE (not a copy) — codegen zeroes `v`'s cap, which the
/// `cap > 0` guard in the `FreeVecBuffer` cleanup uses to skip v's WHOLE cleanup
/// (the dynamic per-element env-drop loop AND the buffer free), while `w`
/// registers its own loop. Each element env (and the buffer) is freed EXACTLY
/// once. Covers a multi-element buffer, a move chain (`v`→`w`→`x`, the cap
/// zeroed at each hop so only the final owner frees), and move-then-ESCAPE
/// (`build` returns the moved owner `w` — drop-loop relocation + caller adopt).
/// Without the cap-zero, both `v` and `w` would run the drop loop (double-free);
/// without `w`'s registration, the moved buffer would leak.
#[test]
fn asan_heap_env_vec_owner_move_freed_no_leak() {
    assert_clean_asan_run(
        r#"
fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }
fn build(k: i64) -> Vec[Fn(i64) -> i64] { let mut v: Vec[Fn(i64) -> i64] = Vec.new(); v.push(make(k)); v.push(make(k + 5i64)); let w = v; w }
fn main() {
    let mut v: Vec[Fn(i64) -> i64] = Vec.new();
    v.push(make(10i64));
    v.push(make(20i64));
    let w = v;
    let x = w;
    let r = build(30i64);
    println(f"{(x[0])(1i64) + (x[1])(1i64) + (r[0])(2i64) + (r[1])(2i64)}");
}
"#,
        &["101"],
        "asan_heap_env_vec_owner_move_freed_no_leak",
    );
}

// ── `collect_all_vec` with capturing closures ─────────────────
//
// The canonical fan-out shape: each closure captures an outer
// binding and wraps a named call (`|| fetch(a)`). The captured
// values live in stack env allocas in `main`'s frame and are read
// by the worker threads across the synchronous `karac_par_run`
// join — a use-after-free of the env (or of the freed input Vec
// buffer) would trip ASAN here.

#[test]
fn asan_collect_all_vec_capturing_closures_no_uaf() {
    assert_clean_asan_run(
        r#"
fn fetch(id: i64) -> Result[i64, String] {
    if id > 0 { Result.Ok(id * 10) } else { Result.Err(f"bad:{id}") }
}
fn main() {
    let a: i64 = 1;
    let b: i64 = -2;
    let c: i64 = 3;
    let fs: Vec[Fn() -> Result[i64, String]] = Vec[|| fetch(a), || fetch(b), || fetch(c)];
    let results: Vec[Result[i64, String]] = collect_all_vec(fs);
    for r in results {
        match r {
            Result.Ok(v) => { println(f"ok {v}"); }
            Result.Err(e) => { println(f"err {e}"); }
        }
    }
}
"#,
        &["ok 10", "err bad:-2", "ok 30"],
        "collect_all_vec_capturing",
    );
}

// ── Closures that RETURN a heap value (closure-heap-return-cleanup) ──
//
// A closure whose body is a block returning a heap binding
// (`|| { let s = mk(); s }`) used to free that binding via the
// block's *nested* scope cleanup BEFORE the tail-return suppression
// could fire — handing back a dangling pointer (use-after-free, and a
// double-free / SIGABRT for the String-from-call case). The fix
// compiles the closure's block body like a function body (raw
// `compile_block`, no nested scope) so the suppression zeroes the
// returned binding's `cap` before the closure's own scope cleanup
// runs. This run exercises String (via f-string and via call),
// direct-f-string, and Vec returns — a stale free of any would trip
// ASAN's double-free / heap-use-after-free.

#[test]
fn asan_closure_returns_heap_value_no_double_free() {
    assert_clean_asan_run(
        r#"
fn mk() -> String { f"made" }
fn compute() -> Vec[i64] { Vec[9, 8] }
fn main() {
    let f2: Fn() -> String = || { let s = f"hi"; s };
    let f4: Fn() -> String = || { let s: String = mk(); s };
    let f5: Fn() -> String = || f"direct";
    let f1: Fn() -> Vec[i64] = || { let v: Vec[i64] = Vec[1, 2, 3]; v };
    let f3: Fn() -> Vec[i64] = || { let w = compute(); w };
    let r2: String = f2();
    let r4: String = f4();
    let r5: String = f5();
    let r1: Vec[i64] = f1();
    let r3: Vec[i64] = f3();
    println(r2);
    println(r4);
    println(r5);
    println(r1.len());
    println(r3.len());
}
"#,
        &["hi", "made", "direct", "3", "2"],
        "closure_returns_heap_value",
    );
}

#[test]
fn asan_owned_struct_option_shared_field_captured_from_builder_no_uaf() {
    // #48 (phase-12 self-hosting): an owned (non-`shared`) struct with an
    // `Option[shared T]` field, built in a helper and returned, then read.
    // The non-shared struct-literal path didn't capture-inc the field's
    // inner RC handle (only the shared-struct path did), so the source
    // local's scope-exit `FreeInlineOptionPayload` dec freed the inner
    // `Expr` to refcount 0 before the caller read its tail — a
    // heap-use-after-free (and an under-count → eventual double-free). The
    // inner payload carries a ≥36-byte String so the freed-then-read access
    // lands on a real heap block ASAN flags (and LSan would flag the leak
    // if the count went the other way). Mirrors the codegen E2E
    // `test_e2e_owned_struct_option_shared_field_captured_from_builder`.
    assert_clean_asan_run(
        r#"
struct Span { line: i64, column: i64, offset: i64, length: i64 }
enum Stmt { Empty }
shared enum Expr { Str(String), Blk(Block), Error }
struct Block { stmts: Vec[Stmt], tail: Option[Expr], span: Span }
fn mk() -> Block {
    let s: Vec[Stmt] = [];
    let mut payload = String.new();
    payload.push_str("owned-struct-option-shared-field-uaf-payload");
    let e = Expr.Str(payload);
    let tail: Option[Expr] = Some(e);
    Block { stmts: s, tail: tail, span: Span { line: 0, column: 0, offset: 0, length: 5 } }
}
fn render_block(b: Block) -> String {
    let Block { stmts, tail, span } = b;
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
    let blk = mk();
    println(render_expr(Expr.Blk(blk)));
}
"#,
        &["owned-struct-option-shared-field-uaf-payload"],
        "owned_struct_option_shared_field_captured_from_builder_no_uaf",
    );
}

#[test]
fn asan_closure_returns_captured_heap_no_leak_no_double_free() {
    // B-2026-07-18-42: a closure capturing a whole heap String/Vec and
    // RETURNING it. The captured buffer is owned by the enclosing frame
    // (stack-env alias) or the RC env box (heap-env), so returning the alias
    // directly double-freed (receiver + owner). The fix deep-copies the
    // captured value at the body return, keeping the copy independent of the
    // owner's buffer. This exercises both a double-free (returning the alias)
    // and, on the LSan leg, a leak (over-copying without balancing the env
    // drop): the escaping closure is invoked TWICE so a per-call over-copy
    // would strand a buffer. Covers stack-env param, stack-env local, and an
    // escaping heap-env closure called multiple times.
    assert_clean_asan_run(
        r#"
fn dup(x: String) -> String { let g = || x; g() }
fn loc() -> String { let s = "L".to_string(); let g = || s; g() }
fn mk(x: String) -> Fn() -> String { || x }
fn main() {
    println(dup("P".to_string()));
    println(loc());
    let g = mk("E".to_string());
    println(g());
    println(g());
}
"#,
        &["P", "L", "E", "E"],
        "closure_returns_captured_heap",
    );
}

#[test]
fn asan_closure_captures_heap_struct_returned_clean() {
    // B-2026-07-18-46: a stack-env closure capturing a heap-bearing struct/
    // enum and returning it now deep-clones at the tail, so the returned
    // value owns independent buffers while the frame's owner drop frees the
    // source. Verify no double-free / leak (a missing clone double-frees; an
    // over-clone leaks) across a struct, a Vec-field struct, and an enum.
    assert_clean_asan_run(
        r#"
struct W { s: String }
struct Wv { v: Vec[i64] }
enum E { A(String) }
fn f(w: W) -> W { let g = || w; g() }
fn fv(w: Wv) -> Wv { let g = || w; g() }
fn fe(e: E) -> E { let g = || e; g() }
fn main() {
    println(f(W { s: "one".to_string() }).s);
    println(fv(Wv { v: [1, 2, 3] }).v.len());
    match fe(E.A("four".to_string())) { E.A(s) => println(s) }
}
"#,
        &["one", "3", "four"],
        "closure_captures_heap_struct_returned",
    );
}

// A heap value (`String`) moved into a `tg.spawn` closure INSIDE A LOOP:
// the per-iteration `let addr = base.clone()` is freed at loop-body scope
// exit, but the spawned task now owns that buffer (the env got a bitwise
// copy of the `{data,len,cap}` header). Ownership must transfer cleanly
// from parent to task — exactly once across the two of them:
//
//   1. The parent's per-iteration `FreeVecBuffer` is suppressed (the
//      original B-2026-06-18-8 half): a non-suppressed parent frees the
//      buffer the task still reads. A single non-loop spawn masked it (the
//      `TaskGroup` join precedes the parent free); the loop drains each
//      iteration's frame first → ASAN use-after-free.
//   2. The task wrapper must then free whatever the body does not itself
//      consume (the completing half). The handler here only *reads* the
//      captured string (`addr: String` is inferred `ref`), so nothing in
//      the body owns it — without a wrapper-side free the buffer leaks once
//      per spawn. macOS ASAN has no LeakSanitizer, so the suppress-only fix
//      looked green locally while leaking under the Linux/LSan gate
//      (`scripts/lsan-local.sh`); `lower_spawn_shared` now re-registers the
//      parent's `FreeVecBuffer` against the wrapper-local binding to close
//      it. The move-into-callee sibling below guards the no-double-free
//      half of that same transfer.
//
// The handler body compares its captured string to the known content (a
// buffer read — poisoned-memory access if freed under ASAN) and stays
// silent unless it mismatches, so a regression shows as an ASAN
// use-after-free / leak / a `CORRUPT` line. This is the canonical
// `loop { let s = …; tg.spawn(|| use(s)) }` server-handler shape — exactly
// `examples/relay/relay.kara`'s round-robin accept loop.
#[test]
fn asan_taskgroup_spawn_heap_capture_in_loop_coro_no_uaf() {
    assert_clean_asan_run(
        r#"
fn check(addr: String) {
    sleep_ms(5);
    if addr == "relay-upstream-127.0.0.1-9000" {
    } else {
        println("CORRUPT");
    }
}
fn main() {
    let base = "relay-upstream-127.0.0.1-9000";
    let mut tg: TaskGroup = TaskGroup.new();
    let mut i: i64 = 0;
    loop {
        let addr = base.clone();
        i = i + 1;
        tg.spawn(|| check(addr));
        if i >= 6 { break; }
    }
    println("ok");
}
"#,
        &["ok"],
        "taskgroup_spawn_heap_capture_in_loop_coro",
    );
}

// Non-coroutine sibling of the above: the handler does not suspend, so it
// lowers through the run-to-completion spawn path rather than the coro
// park path. Same double-ownership hole, same fix (the `FreeVecBuffer`
// suppression is shared by both paths in `lower_spawn_shared`).
#[test]
fn asan_taskgroup_spawn_heap_capture_in_loop_noncoro_no_uaf() {
    assert_clean_asan_run(
        r#"
fn check(addr: String) {
    if addr == "relay-upstream-127.0.0.1-9000" {
    } else {
        println("CORRUPT");
    }
}
fn main() {
    let base = "relay-upstream-127.0.0.1-9000";
    let mut tg: TaskGroup = TaskGroup.new();
    let mut i: i64 = 0;
    loop {
        let addr = base.clone();
        i = i + 1;
        tg.spawn(|| check(addr));
        if i >= 6 { break; }
    }
    println("ok");
}
"#,
        &["ok"],
        "taskgroup_spawn_heap_capture_in_loop_noncoro",
    );
}

// Companion to the two loop-capture cases above: there the spawned body
// only *borrows* the captured `String` (`check(addr)` — a `ref` param), so
// the task wrapper is the sole owner and must free it. Here the body
// *moves* the capture into a consuming callee (`sink` pushes it into a
// local `Vec`, taking ownership), so `sink`'s own scope-exit drop frees the
// buffer. The wrapper's transferred `FreeVecBuffer` (the same ownership
// hand-off the loop tests exercise) MUST then be a no-op — the move into
// `sink` zeros the capture's `cap`, so the `cap > 0` drain guard skips it.
// If the wrapper freed regardless, this is a double-free (ASAN: `attempting
// double-free`); if neither freed, a leak (LSan). Exactly one free is the
// pass. Guards the move-suppression half of the B-2026-06-18-8 follow-up.
#[test]
fn asan_taskgroup_spawn_heap_capture_moved_into_callee_single_free() {
    assert_clean_asan_run(
        r#"
fn sink(addr: String) {
    let mut held: Vec[String] = Vec.new();
    held.push(addr);
    if held[0] == "relay-upstream-127.0.0.1-9000" {
    } else {
        println("CORRUPT");
    }
}
fn main() {
    let base = "relay-upstream-127.0.0.1-9000";
    let mut tg: TaskGroup = TaskGroup.new();
    let mut i: i64 = 0;
    loop {
        let addr = base.clone();
        i = i + 1;
        tg.spawn(|| sink(addr));
        if i >= 4 { break; }
    }
    println("ok");
}
"#,
        &["ok"],
        "taskgroup_spawn_heap_capture_moved_into_callee",
    );
}

#[test]
fn asan_closure_mut_captured_collection_no_leak() {
    // B-2026-07-15-13: a closure mutating a captured collection via a
    // mutating method (`acc.push`, `buf.push_str`, `m.insert`) now captures
    // the receiver by mut-ref and writes through. The env stores a POINTER
    // to the outer slot (not a value), so the closure's push/realloc updates
    // the outer `{ptr,len,cap}` in place — verify no leak (the by-value copy
    // that leaked pre-fix left the closure's grown buffer unreferenced) and
    // no double-free (the outer binding stays the sole owner; the env holds
    // a pointer, excluded from the env-drop's buffer free).
    // Vec MULTI-call (accumulation) is check-clean; String / Map use a
    // SINGLE mutating call to stay clear of the separate multi-call
    // push_str/insert ownership over-strictness (B-2026-07-15-14).
    assert_clean_asan_run(
        r#"
fn main() {
    let mut acc: Vec[i64] = Vec.new();
    let mut push = |x: i64| { acc.push(x); };
    let mut i: i64 = 0;
    while i < 20 {
        push(i);
        i = i + 1;
    }
    println(acc.len());
    let mut buf: String = "";
    let mut append = |s: String| { buf.push_str(s); };
    append("alpha beta gamma delta epsilon zeta well past inline");
    println(buf.len());
    let mut m: Map[String, i64] = Map.new();
    let mut record = |k: String, v: i64| { m.insert(k, v); };
    record("one", 1);
    println(m.len());
}
"#,
        &["20", "52", "1"],
        "closure_mut_captured_collection_no_leak",
    );
}

/// B-2026-07-31-16: closure-scoped `return` retargeting must stay
/// leak/double-free clean on both edges — the retargeted return drains
/// the heap body-local (`pad`) then the provider frame, and the moved-out
/// String returned THROUGH the closure (`heap`) must be owned exactly
/// once by the receiving binding. 200 iterations of both dynamic paths
/// plus the String-through-closure shape.
#[test]
fn asan_wp_closure_return_frees_body_local_and_moves_result() {
    assert_clean_asan_run(
        r#"
trait Counter { fn get(ref self) -> i64; }
effect resource Ctr: Counter;
struct InMem { n: i64 }
impl Counter for InMem { fn get(ref self) -> i64 { self.n } }
fn read() -> i64 with reads(Ctr) { Ctr.get() }
fn local(n: i64) -> i64 with reads(Ctr) {
    let x = with_provider[Ctr](InMem { n: n }, || {
        let pad = f"pad-{read()}";
        if read() == 9i64 { return pad.len() * 100i64; }
        read()
    });
    x + 1i64
}
fn heap() -> String with reads(Ctr) {
    with_provider[Ctr](InMem { n: 42i64 }, || { return f"v-{read()}"; })
}
fn main() with reads(Ctr) {
    let mut acc = 0i64;
    let mut i = 0i64;
    while i < 200i64 {
        acc = acc + local(9i64) + local(4i64) + heap().len();
        i = i + 1;
    }
    println(acc);
}
"#,
        // (501 + 5 + 4) x 200 = 102000.
        &["102000"],
        "wp_closure_return_frees_body_local_and_moves_result",
    );
}

/// B-2026-07-31-19: closure-scoped `?` — the fail edge's bounded drain
/// must free the heap body-local (`pad`) and the Err's String payload
/// must be owned exactly once through the merge slot. Both dynamic
/// paths, 200 iterations.
#[test]
fn asan_wp_closure_question_fail_edge_frees_body_local() {
    assert_clean_asan_run(
        r#"
trait Counter { fn get(ref self) -> i64; }
effect resource Ctr: Counter;
struct InMem { n: i64 }
impl Counter for InMem { fn get(ref self) -> i64 { self.n } }
fn read() -> i64 with reads(Ctr) { Ctr.get() }
fn might_fail(x: i64) -> Result[i64, String] {
    if x > 5i64 { return Err(f"big-{x}"); }
    Ok(x)
}
fn probe(n: i64) -> i64 with reads(Ctr) {
    let r = with_provider[Ctr](InMem { n: n }, || {
        let pad = f"pad-{read()}";
        let v = might_fail(read())?;
        Ok(v + pad.len())
    });
    match r {
        Ok(v) => v,
        Err(e) => e.len(),
    }
}
fn main() with reads(Ctr) {
    let mut acc = 0i64;
    let mut i = 0i64;
    while i < 200i64 {
        acc = acc + probe(3i64) + probe(9i64);
        i = i + 1;
    }
    println(acc);
}
"#,
        // probe(3) = 3 + 5 = 8; probe(9) = "big-9".len() = 5; x 200 = 2600.
        &["2600"],
        "wp_closure_question_fail_edge_frees_body_local",
    );
}
