//! par blocks, spawn, tasks, channels, atomics, pools -- fixtures for `tests/memory_sanitizer.rs`.
//!
//! Split out of `tests/memory_sanitizer.rs` on 2026-09-21. The TEST TARGET is
//! unchanged: this file is a module of that target, so
//! `cargo test --features llvm --test memory_sanitizer` still runs everything
//! and CI needs no edit. Run this area alone with:
//!
//!     cargo test --features llvm --test memory_sanitizer concurrency::
//!
//! New fixtures about par blocks, spawn, tasks, channels, atomics, pools belong in this file.

use super::*;

#[test]
fn asan_par_field_bodies_walk_does_not_read_freed_buffer() {
    assert_clean_asan_run_min_allocs_auto_par(
            "struct R { id: i64, tag: String }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\"); } }\n\
             struct Box3 { xs: Vec[R] }\n\
             fn build(k: i64) -> Box3 {\n\
             \x20   let mut v: Vec[R] = Vec.new();\n\
             \x20   v.push(R { id: k, tag: f\"t\" });\n\
             \x20   let bx: Box3 = Box3 { xs: v };\n\
             \x20   return bx\n\
             }\n\
             fn mk_v() -> Vec[String] { let mut n: Vec[String] = Vec.new(); n.push(f\"aa\"); return n }\n\
             fn main() {\n\
             \x20   let a: Box3 = build(10);\n\
             \x20   println(f\"a{a.xs[0].id}\");\n\
             \x20   let b: Vec[String] = mk_v();\n\
             \x20   println(f\"b{b.len()}\");\n\
             \x20   println(f\"done\");\n\
             }\n",
            &["a10", "dR10", "b1", "done"],
            "b1-par-field-bodies-after-free",
            1,
        );
    // Case 2 — `a` read in the block's FINAL EXPRESSION, so its endpoint
    // pins to `scope_exit` and the bodies walk still meets the struct's
    // memory drop in the scope-exit LIFO drain. This is the case that
    // actually guards B-2026-09-02-1 now; see this test's doc.
    assert_clean_asan_run_min_allocs_auto_par(
            "struct R { id: i64, tag: String }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\"); } }\n\
             struct Box3 { xs: Vec[R] }\n\
             fn build(k: i64) -> Box3 {\n\
             \x20   let mut v: Vec[R] = Vec.new();\n\
             \x20   v.push(R { id: k, tag: f\"t\" });\n\
             \x20   let bx: Box3 = Box3 { xs: v };\n\
             \x20   return bx\n\
             }\n\
             fn mk_v() -> Vec[String] { let mut n: Vec[String] = Vec.new(); n.push(f\"aa\"); return n }\n\
             fn main() {\n\
             \x20   let a: Box3 = build(10);\n\
             \x20   println(f\"a{a.xs[0].id}\");\n\
             \x20   let b: Vec[String] = mk_v();\n\
             \x20   println(f\"b{b.len()}\");\n\
             \x20   println(f\"done{a.xs[0].id}\")\n\
             }\n",
            &["a10", "b1", "done10", "dR10"],
            "b1-par-field-bodies-drain-order",
            1,
        );
}

/// B-2026-08-08-16 — the flip's own guard. Every other fixture in this file
/// now compiles with auto-par, but a fixture that quietly stops being
/// parallelized still passes while covering nothing, and the same is true
/// of the whole suite if the analysis ever returns empty. This asserts the
/// harness's pipeline actually produces parallel groups, so "999 green with
/// auto-par on" keeps meaning what it says.
///
/// Deliberately checks the ANALYSIS rather than the emitted object: it is
/// the thing the harness threads into codegen, it needs no linker or
/// symbol-table reading, and it fails for the one reason worth failing for.
#[test]
fn asan_harness_actually_parallelizes() {
    let src = "fn work(n: i64) -> i64 { let mut t = 0; for i in 0..n { t = t + i; } t }\n\
                   fn main() {\n\
                       let a = work(1000);\n\
                       let b = work(2000);\n\
                       println(a + b);\n\
                   }\n";
    let mut parsed = karac::parse(src);
    assert!(parsed.errors.is_empty(), "parse: {:?}", parsed.errors);
    karac::desugar_program(&mut parsed.program);
    let resolved = karac::resolve(&parsed.program);
    let typed = karac::typecheck(&parsed.program, &resolved);
    karac::lower(&mut parsed.program, &typed);
    let effects = karac::effectcheck(&parsed.program);
    let analysis = karac::concurrency_analyze_typed(&parsed.program, &effects, Some(&typed));
    let groups: usize = analysis
        .function_decisions
        .values()
        .map(|f| f.parallel_groups.len())
        .sum();
    assert!(
        groups > 0,
        "the ASAN harness threads this analysis into codegen; with zero \
             parallel groups every fixture in this file silently reverts to \
             covering sequential codegen only — the exact hole B-2026-08-08-16 \
             closed"
    );
}

// ── Heap-closure-env epic Slice 1 (B-2026-06-22-2) ───────────
// A returned capturing closure gets a reference-counted HEAP environment
// (`emit_rc_alloc { i64 refcount, env }`); the owning `let f = make(..)`
// binding frees it via `FreeClosureEnv` at scope exit. This asserts the RC
// env is freed exactly once — no leak (LSan) and no use-after-free /
// double-free (ASAN) — for the supported call shape, including a binding
// called multiple times.

#[test]
fn asan_oncelock_local_binding_freed_no_leak() {
    // A local `OnceLock[i64]` binding's scope-exit `FreeOnceHandle` must
    // reclaim the runtime cell (control block + sealed value buffer) — a
    // missed `karac_runtime_once_free` leaks one cell + buffer per
    // iteration (LSan on Linux CI catches it). 40 fresh cells, each
    // set+get, so any per-iteration leak accumulates well past noise. Also
    // pins the double-set `AlreadySetError` path (a second `set` allocates
    // nothing, so it cannot leak, but exercising it guards the Err arm).
    assert_clean_asan_run(
        r#"
fn main() {
    let mut i: i64 = 0i64;
    let mut sum: i64 = 0i64;
    while i < 40i64 {
        let cell: OnceLock[i64] = OnceLock.new();
        match cell.set(i) { Ok(_) => {}, Err(_) => {}, }
        match cell.set(i) { Ok(_) => {}, Err(_) => {}, }
        match cell.get() { Some(v) => { sum = sum + v; }, None => {}, }
        i = i + 1;
    }
    println(sum.to_string());
}
"#,
        // sum 0..39 = 780
        &["780"],
        "oncelock_local_binding_freed_no_leak",
    );
}

#[test]
fn asan_oncelock_string_set_get_no_leak() {
    // B-2026-07-12-2 heap-`T` ungate (gap 1, success-path element leak): a
    // heap-owning `OnceLock[String]` `set(v)` moves `v`'s buffer into the
    // cell; the scope-exit `FreeOnceHandle` must run the ELEMENT drop on the
    // sealed value (the `String` char buffer) before `once_free`, else every
    // iteration leaks the buffer. 40 fresh cells, each a single `set` + a
    // `get` read-back.
    assert_clean_asan_run(
        r#"
fn main() {
    let mut i: i64 = 0i64;
    let mut total: i64 = 0i64;
    while i < 40i64 {
        let cell: OnceLock[String] = OnceLock.new();
        match cell.set("hello".to_string()) { Ok(_) => {}, Err(_) => {}, }
        match cell.get() { Some(v) => { total = total + v.len(); }, None => {}, }
        i = i + 1;
    }
    println(total.to_string());
}
"#,
        // 40 * len("hello") = 200
        &["200"],
        "oncelock_string_set_get_no_leak",
    );
}

#[test]
fn asan_oncelock_string_double_set_discard_no_leak() {
    // B-2026-07-12-2 heap-`T` ungate (gap 2, rejected-value discard leak): a
    // second `set` on a filled cell returns `Err(AlreadySetError { rejected:
    // v2 })` carrying `v2`'s `String` buffer; a `match ... { Err(_) => {} }`
    // that discards it must free that buffer (the source `set`-result temp is
    // side-effecting, so it survives DCE and genuinely leaks). 40 cells, each
    // a winning `set` + a losing discarded `set` + a `get`.
    assert_clean_asan_run(
        r#"
fn main() {
    let mut i: i64 = 0i64;
    let mut total: i64 = 0i64;
    while i < 40i64 {
        let cell: OnceLock[String] = OnceLock.new();
        match cell.set("first".to_string()) { Ok(_) => {}, Err(_) => {}, }
        match cell.set("second".to_string()) { Ok(_) => {}, Err(_) => {}, }
        match cell.get() { Some(v) => { total = total + v.len(); }, None => {}, }
        i = i + 1;
    }
    println(total.to_string());
}
"#,
        // first wins → get is "first" (5); 40 * 5 = 200
        &["200"],
        "oncelock_string_double_set_discard_no_leak",
    );
}

#[test]
fn asan_oncelock_string_reject_recover_no_leak_or_double_free() {
    // B-2026-07-12-2 heap-`T` ungate (recover path — the double-free guard):
    // a losing `set`'s `Err(e) => use(e.rejected)` MOVES the rejected `String`
    // out and consumes it (`println`). The consuming-arm suppressor must zero
    // the source so the rejected value is freed EXACTLY once — a missed
    // suppression double-frees, a missed free (when NOT recovered) leaks. 40
    // iterations recover-and-print.
    assert_clean_asan_run(
        r#"
fn main() {
    let mut i: i64 = 0i64;
    let mut n: i64 = 0i64;
    while i < 40i64 {
        let cell: OnceLock[String] = OnceLock.new();
        match cell.set("first".to_string()) { Ok(_) => {}, Err(_) => {}, }
        match cell.set("second".to_string()) {
            Ok(_) => {}
            Err(e) => { n = n + e.rejected.len(); }
        }
        i = i + 1;
    }
    println(n.to_string());
}
"#,
        // 40 * len("second") = 240
        &["240"],
        "oncelock_string_reject_recover_no_leak_or_double_free",
    );
}

#[test]
fn asan_oncelock_string_reject_recover_consume_no_double_free() {
    // B-2026-07-12-2 heap-`T` recover-CONSUME: `Err(e) => { let s =
    // e.rejected; ... }` MOVES the rejected `String` out into `s`, which
    // owns + frees it. The materialized source's `FreeInlineResultPayload`
    // must be SUPPRESSED on this consuming arm (else double-free with `s`);
    // the borrow-only skip must NOT fire here (a field IS moved out). The
    // read variant (`e.rejected.len()`) is the sibling test above.
    assert_clean_asan_run(
        r#"
fn main() {
    let mut i: i64 = 0i64;
    let mut n: i64 = 0i64;
    while i < 40i64 {
        let cell: OnceLock[String] = OnceLock.new();
        match cell.set("first".to_string()) { Ok(_) => {}, Err(_) => {}, }
        match cell.set("second".to_string()) {
            Ok(_) => {}
            Err(e) => { let s = e.rejected; n = n + s.len(); }
        }
        i = i + 1;
    }
    println(n.to_string());
}
"#,
        // 40 * len("second") = 240
        &["240"],
        "oncelock_string_reject_recover_consume_no_double_free",
    );
}

#[test]
fn asan_oncelock_single_field_struct_no_leak() {
    // B-2026-07-12-2 heap-`T` ungate — a single-heap-field WRAPPER struct
    // `T` (`Holder { val: String }`, exactly 3 words, fits): the only
    // heap-bearing struct shape that clears the `wide` (>3-word) gate, since
    // a `String`/`Vec` field is itself 3 words so any 2-field struct is >=4.
    // The cell's element drop (gap 1) + the discarded rejected value (gap 2)
    // both drive through `emit_struct_drop_synthesis_mono` / the transparent
    // single-field wrapper `inline_heap_payload_elem`.
    assert_clean_asan_run(
        r#"
struct Holder { val: String }
fn main() {
    let mut i: i64 = 0i64;
    let mut total: i64 = 0i64;
    while i < 40i64 {
        let cell: OnceLock[Holder] = OnceLock.new();
        match cell.set(Holder { val: "hi".to_string() }) { Ok(_) => {}, Err(_) => {}, }
        match cell.set(Holder { val: "second".to_string() }) { Ok(_) => {}, Err(_) => {}, }
        match cell.get() { Some(h) => { total = total + h.val.len(); }, None => {}, }
        i = i + 1;
    }
    println(total.to_string());
}
"#,
        // first wins → get "hi" (2); 40 * 2 = 80
        &["80"],
        "oncelock_single_field_struct_no_leak",
    );
}

#[test]
fn asan_vec_clear_under_autopar_no_double_free() {
    // B-2026-07-14-17: `Vec.clear()` was invisible to the auto-parallelizer's
    // write-dependency gate (unseeded in the effectchecker builtin table —
    // the residual B-2026-07-02-8 flagged). A SECOND Vec in `main` gives the
    // auto-parallelizer an independent group; `a.clear()` (which frees the
    // buffer + drops the String elements + resets the header) then raced its
    // sibling read, so the scope-exit cleanup freed the already-freed buffer
    // — a DOUBLE-FREE at -O2 (ASan abort), and a stale non-zero `len` read at
    // -O0. Seeding `Vec.clear` `allocates(Heap)` serializes it. Looped over
    // heap-owning elements so any per-iteration imbalance accumulates for
    // ASan/LSan; the second Vec `b` keeps the racing group present.
    assert_clean_asan_run(
        r#"
fn main() {
    let mut i: i64 = 0i64;
    while i < 3i64 {
        let mut a: Vec[String] = Vec.new();
        a.push(f"a-{i}-padding-padding");
        a.push(f"b-{i}-padding-padding");
        a.clear();
        println(a.len());
        let mut b: Vec[String] = Vec.new();
        b.push(f"c-{i}-padding-padding");
        println(b.len());
        println(b[0]);
        i = i + 1;
    }
}
"#,
        &[
            "0",
            "1",
            "c-0-padding-padding",
            "0",
            "1",
            "c-1-padding-padding",
            "0",
            "1",
            "c-2-padding-padding",
        ],
        "asan_vec_clear_under_autopar_no_double_free",
    );
}

#[test]
fn asan_oncelock_vec_set_get_no_leak() {
    // B-2026-07-12-2 heap-`T` ungate — `OnceLock[Vec[i64]]` (Vec is also a
    // 3-word `{ptr,len,cap}` fitting `T`): the moved-in Vec buffer must be
    // freed by the cell's element drop.
    assert_clean_asan_run(
        r#"
fn main() {
    let mut i: i64 = 0i64;
    let mut total: i64 = 0i64;
    while i < 40i64 {
        let cell: OnceLock[Vec[i64]] = OnceLock.new();
        let mut v: Vec[i64] = Vec.new();
        v.push(1i64);
        v.push(2i64);
        match cell.set(v) { Ok(_) => {}, Err(_) => {}, }
        match cell.get() { Some(g) => { total = total + g.len(); }, None => {}, }
        i = i + 1;
    }
    println(total.to_string());
}
"#,
        // 40 * 2 = 80
        &["80"],
        "oncelock_vec_set_get_no_leak",
    );
}

#[test]
fn asan_oncelock_wide_allscalar_no_leak() {
    // B-2026-07-12-2 gap 3 — a WIDE all-scalar element (`Wide { a,b,c,d }`,
    // 4 words > the 3-word `Option`/`Result` inline area). `get` heap-boxes
    // the borrow (box-only free) and `set`'s `Err` payload boxes past the
    // 5-word `Result` area; the double-set discard + the get borrow must
    // both stay leak-free.
    assert_clean_asan_run(
        r#"
struct Wide { a: i64, b: i64, c: i64, d: i64 }
fn main() {
    let mut i: i64 = 0i64;
    let mut total: i64 = 0i64;
    while i < 40i64 {
        let cell: OnceLock[Wide] = OnceLock.new();
        match cell.set(Wide { a: 1i64, b: 2i64, c: 3i64, d: 4i64 }) { Ok(_) => {}, Err(_) => {}, }
        match cell.set(Wide { a: 9i64, b: 9i64, c: 9i64, d: 9i64 }) { Ok(_) => {}, Err(_) => {}, }
        match cell.get() { Some(w) => { total = total + w.a + w.b + w.c + w.d; }, None => {}, }
        i = i + 1;
    }
    println(total.to_string());
}
"#,
        // first wins → 1+2+3+4 = 10; 40 * 10 = 400
        &["400"],
        "oncelock_wide_allscalar_no_leak",
    );
}

#[test]
fn asan_oncelock_wide_heap_struct_no_leak() {
    // B-2026-07-12-2 gap 3 — a WIDE struct-with-heap element (`Rec { id:
    // i64, name: String }`, 4 words with a heap field). `get`'s boxed
    // borrow-copy aliases the cell's `String` (box-only free leaves the
    // cell's elem-drop sole owner); the DISCARDED second-`set` rejected
    // value's inner `String` is freed by the `FreeInlineResultPayload`
    // struct-drop arm (the multi-field struct the overlay can't handle).
    assert_clean_asan_run(
        r#"
struct Rec { id: i64, name: String }
fn main() {
    let mut i: i64 = 0i64;
    let mut total: i64 = 0i64;
    while i < 40i64 {
        let cell: OnceLock[Rec] = OnceLock.new();
        match cell.set(Rec { id: 7i64, name: "first".to_string() }) { Ok(_) => {}, Err(_) => {}, }
        match cell.set(Rec { id: 8i64, name: "second".to_string() }) { Ok(_) => {}, Err(_) => {}, }
        match cell.get() { Some(r) => { total = total + r.id + r.name.len(); }, None => {}, }
        i = i + 1;
    }
    println(total.to_string());
}
"#,
        // first wins → 7 + len("first")=5 → 12; 40 * 12 = 480
        &["480"],
        "oncelock_wide_heap_struct_no_leak",
    );
}

#[test]
fn asan_oncelock_wide_heap_struct_reject_recover_no_leak() {
    // B-2026-07-12-2 gap 3 — recover the rejected WIDE struct value out of
    // the `Err` arm (`Err(e) => let r: Rec = e.rejected`). The move-out
    // binding `r` owns the recovered struct (its scope-exit drop frees the
    // inner `String`); the consuming arm zeros the whole payload area so the
    // discard struct-drop skips (no double-free). Chained field READ recovery
    // works too; a `let`-bind avoids the deferred chained-method-receiver.
    assert_clean_asan_run(
        r#"
struct Rec { id: i64, name: String }
fn main() {
    let mut i: i64 = 0i64;
    let mut total: i64 = 0i64;
    while i < 40i64 {
        let cell: OnceLock[Rec] = OnceLock.new();
        match cell.set(Rec { id: 1i64, name: "first".to_string() }) { Ok(_) => {}, Err(_) => {}, }
        match cell.set(Rec { id: 2i64, name: "second".to_string() }) {
            Ok(_) => {},
            Err(e) => { let r: Rec = e.rejected; total = total + r.id + r.name.len(); },
        }
        i = i + 1;
    }
    println(total.to_string());
}
"#,
        // rejected: 2 + len("second")=6 → 8; 40 * 8 = 320
        &["320"],
        "oncelock_wide_heap_struct_reject_recover_no_leak",
    );
}

#[test]
fn asan_oncelock_wide_vec_field_struct_no_leak() {
    // B-2026-07-12-2 gap 3 — a WIDE struct whose heap field is a `Vec`
    // (`Bag { tag: i64, items: Vec[i64] }`, 4 words). Exercises the
    // struct-drop's recursive `Vec` buffer free through set/get.
    assert_clean_asan_run(
        r#"
struct Bag { tag: i64, items: Vec[i64] }
fn main() {
    let mut i: i64 = 0i64;
    let mut total: i64 = 0i64;
    while i < 40i64 {
        let cell: OnceLock[Bag] = OnceLock.new();
        let mut v: Vec[i64] = Vec.new();
        v.push(10i64);
        v.push(20i64);
        match cell.set(Bag { tag: 3i64, items: v }) { Ok(_) => {}, Err(_) => {}, }
        match cell.get() { Some(b) => { total = total + b.tag + b.items.len(); }, None => {}, }
        i = i + 1;
    }
    println(total.to_string());
}
"#,
        // (3 + 2) * 40 = 200
        &["200"],
        "oncelock_wide_vec_field_struct_no_leak",
    );
}

#[test]
fn asan_oncelock_get_or_init_heapfree_aggregate_no_leak() {
    // B-2026-07-12-2 follow-on: `get_or_init` with a heap-FREE aggregate `T`
    // (`Point { x, y }`). No element heap to leak, but the per-iteration
    // once-handle + closure env must be reclaimed cleanly across the loop.
    assert_clean_asan_run(
        r#"
struct Point { x: i64, y: i64 }
fn main() {
    let mut i: i64 = 0i64;
    let mut total: i64 = 0i64;
    while i < 40i64 {
        let cell: OnceLock[Point] = OnceLock.new();
        let p = cell.get_or_init(|| Point { x: 3i64, y: 4i64 });
        total = total + p.x + p.y;
        i = i + 1;
    }
    println(total.to_string());
}
"#,
        // 7 * 40 = 280
        &["280"],
        "oncelock_get_or_init_heapfree_aggregate_no_leak",
    );
}

#[test]
fn asan_oncelock_get_or_init_string_no_leak() {
    // B-2026-07-12-2 follow-on (closed): `get_or_init` with a HEAP `T`
    // (`String`). The returned value is a borrowed cap-0 view — the
    // binding's scope-exit free must no-op (no double-free against the
    // cell's `FreeOnceHandle` elem-drop) and the sealed String must be
    // freed exactly once per cell. The second `get_or_init` call takes the
    // already-set path (its closure value is never built). Loops so LSan
    // catches a per-iteration leak of either the sealed value or a
    // spuriously-built second closure value.
    assert_clean_asan_run(
        r#"
fn main() {
    let mut i: i64 = 0i64;
    let mut total: i64 = 0i64;
    while i < 40i64 {
        let cell: OnceLock[String] = OnceLock.new();
        let a = cell.get_or_init(|| "alpha".to_string());
        let b = cell.get_or_init(|| "unused".to_string());
        total = total + a.len() + b.len();
        i = i + 1;
    }
    println(total.to_string());
}
"#,
        // (5 + 5) * 40 = 400
        &["400"],
        "oncelock_get_or_init_string_no_leak",
    );
}

#[test]
fn asan_oncelock_get_or_init_heap_struct_no_leak() {
    // Heap-owning STRUCT `T` (`Config { name: String, port: i64 }`) — the
    // wide struct-with-heap shape. The borrowed view's nested String cap is
    // zeroed (recursive `zero_heap_caps_in_value`), so the binding's
    // struct-drop no-ops on the field; the cell's elem-drop is the single
    // owner of the sealed struct's heap.
    assert_clean_asan_run(
        r#"
struct Config { name: String, port: i64 }
fn main() {
    let mut i: i64 = 0i64;
    let mut total: i64 = 0i64;
    while i < 40i64 {
        let cell: OnceLock[Config] = OnceLock.new();
        let cfg = cell.get_or_init(|| Config { name: "service".to_string(), port: 8080i64 });
        total = total + cfg.port + cfg.name.len();
        i = i + 1;
    }
    println(total.to_string());
}
"#,
        // (8080 + 7) * 40 = 323480
        &["323480"],
        "oncelock_get_or_init_heap_struct_no_leak",
    );
}

#[test]
fn asan_oncelock_get_or_init_vec_no_leak() {
    // Heap `Vec[i64]` element `T` via `OnceCell` — same borrowed-view
    // contract as the String twin; also reads through the view (`get`) to
    // prove the aliased buffer is live until cell teardown.
    assert_clean_asan_run(
        r#"
fn main() {
    let mut i: i64 = 0i64;
    let mut total: i64 = 0i64;
    while i < 40i64 {
        let cell: OnceCell[Vec[i64]] = OnceCell.new();
        let xs = cell.get_or_init(|| Vec[10i64, 20i64, 30i64]);
        match xs.get(1i64) { Some(n) => { total = total + n + xs.len(); }, None => {} }
        i = i + 1;
    }
    println(total.to_string());
}
"#,
        // (20 + 3) * 40 = 920
        &["920"],
        "oncelock_get_or_init_vec_no_leak",
    );
}

#[test]
fn asan_process_spawn_pipe_roundtrip_no_leak() {
    // `std.process` codegen (phase-8 P1) — the full chained-builder
    // spawn → take-stream → read_to_string → wait roundtrip, looped so
    // LSan catches a per-iteration leak of: the chained `Command` temps
    // (each builder link constructs a fresh Command whose `cmd_args` /
    // `cmd_env` Vecs must drop), the `read_to_string` Ok String (owned
    // buffer adopted from the runtime), or the IoError payloads. The
    // spawn-error arm exercises the `IoError.Other`-free unit-variant
    // Err path (`NotFound` carries no heap).
    assert_clean_asan_run(
        r#"
fn main() {
    let mut i: i64 = 0i64;
    let mut total: i64 = 0i64;
    while i < 8i64 {
        let cmd = Command.new("echo").arg("asan-pipe").env("KARA_ASAN", "1").stdout(Stdio.Piped);
        match cmd.spawn() {
            Ok(child) => {
                match child.stdout() {
                    Some(o) => {
                        match o.read_to_string() {
                            Ok(text) => { total = total + text.len(); }
                            Err(e) => {}
                        }
                    }
                    None => {}
                }
                match child.wait() {
                    Ok(st) => { total = total + st.code; }
                    Err(e) => {}
                }
            }
            Err(e) => {}
        }
        let bad = Command.new("definitely-not-a-binary-kara");
        match bad.spawn() {
            Ok(c) => {}
            Err(e) => { total = total + 1i64; }
        }
        i = i + 1;
    }
    println(total.to_string());
}
"#,
        // ("asan-pipe\n".len() == 10, code 0, +1 bad) * 8 = 88
        &["88"],
        "process_spawn_pipe_roundtrip_no_leak",
    );
}

#[test]
fn asan_vec_extend_from_slice_disjoint_source_no_panic() {
    // Disjoint src/dst — guard must NOT fire even when the grow
    // path runs. dst cap=2, push one element so grow is required
    // mid-extend. Counterpart to the rejection test above.
    assert_clean_asan_run(
        r#"
fn main() {
    let src: Vec[i64] = Vec.filled(4, 5);
    let mut dst: Vec[i64] = Vec.with_capacity(2);
    dst.push(1);
    dst.extend_from_slice(src);
    println(dst.len());
}
"#,
        &["5"],
        "vec_extend_from_slice_disjoint_source_no_panic",
    );
}

// ── Atomic-RC across par {}: refcount race detection ─────────
// The `arc_values` subset of RC bindings crosses `par {}` thread
// boundaries. With non-atomic load+add+store the refcount races
// when both branches run concurrent inc/dec on the same heap block;
// with atomic-RC (`atomicrmw add` / `atomicrmw sub`, `SeqCst`) the
// increment is race-free. ASAN's standard run does not detect data
// races on its own, but it *will* catch the secondary symptoms:
// a UAF when the racing dec drops below zero and one branch tries
// to free a still-live heap block, or a double-free when both
// branches independently free. Pre-slice (substep 2 missing) this
// test would manifest one of those errors under load; with the
// atomic path it stays clean.

#[test]
fn asan_par_block_arc_promoted_no_double_free() {
    assert_clean_asan_run_with_ownership(
        r#"
shared struct Counter { val: i64 }
fn use_c(c: Counter) -> i64 { c.val }
fn main() {
    let cond: bool = false;
    let c = Counter { val: 7 };
    let d = c;
    if cond { use_c(d); }
    par {
        println(use_c(d));
        println(use_c(d));
    }
}
"#,
        "par_block_arc_promoted_no_double_free",
    );
}

/// B-2026-07-11-3: branch bindings that OWN HEAP (String) escape the
/// `par {}` block into the enclosing scope and are consumed AFTER the
/// block (no tail expression) — the join hoist. Each branch buffer now
/// transfers into a parent return slot and is dropped at the enclosing
/// scope's end like any other `let`, exactly once. Asserts no leak
/// (LSan) and no use-after-free / double-free (ASAN) on the escaped
/// heap owners — the class that broadening the codegen slot set to
/// every branch binding could have regressed.
#[test]
fn asan_par_block_heap_bindings_escape_no_double_free() {
    assert_clean_asan_run_with_ownership(
        r#"
fn label(n: i64) -> String { f"v{n}" }
fn main() {
    par {
        let sa = label(1);
        let sb = label(2);
    }
    println(sa);
    println(sb);
}
"#,
        "par_block_heap_bindings_escape_no_double_free",
    );
}

/// B-2026-08-01-33 mechanism 3, stage 2 — a RECURSIVE traversal of a
/// `shared` graph through a NON-COUNTING (`frozen`) handle, run from two
/// `par` branches over one shared root.
///
/// This is the surface the stage-2 relaxation opened, and it is the one
/// worth an ASAN/LSan gate: the branches walk the same 255-node tree
/// concurrently while codegen emits NO retain/release for any place
/// projected off the frozen root. That is the point — a projection is a
/// deref, so there is no refcount for two branches to race — but it also
/// means nothing is counting, so a hole in the escape check would show up
/// here as a use-after-free rather than as a wrong answer.
///
/// NOT VACUOUS, and deliberately so (B-2026-08-04-17): the seed is
/// `env.args().len()` (opaque, 1 under both the harness and a bare run),
/// every node's tag is an f-string built from it at runtime, and `main`
/// reads those bytes back with `contains` after the traversal. So the ~510
/// heap blocks the tree costs are real allocations the optimizer cannot
/// fold away, and the floor below pins that.
///
/// Expected output is computed independently rather than read off a run:
/// 255 nodes x (val 1 + tag length 19) = 5100 per traversal, x2 branches
/// = 10200, +1 for the `contains` check = 10201.
#[test]
fn asan_frozen_recursive_traversal_across_par_branches_no_leak() {
    assert_clean_asan_run_min_allocs(
        r#"
shared struct Node { tag: String, val: i64, kids: Vec[Node] }

fn build(depth: i64, seed: i64) -> Node {
    if depth <= 0 {
        return Node { tag: f"leaf-{seed}-runtime-heap", val: seed, kids: [] };
    }
    let a = build(depth - 1, seed);
    let b = build(depth - 1, seed);
    return Node { tag: f"node-{seed}-runtime-heap", val: seed, kids: [a, b] };
}

fn sum(n: frozen Node) -> i64 {
    let mut s: i64 = n.val + n.tag.len();
    let mut i: i64 = 0;
    while i < n.kids.len() {
        s = s + sum(n.kids[i]);
        i = i + 1;
    }
    return s;
}

fn driver(root: frozen Node) -> i64 {
    let (a, b) = par {
        let a = sum(root);
        let b = sum(root);
        (a, b)
    };
    return a + b;
}

fn main() {
    let seed: i64 = env.args().len();
    let root = build(7, seed);
    let total = driver(root);
    let mut w: i64 = 0;
    if root.tag.contains("runtime-heap") { w = 1; }
    println(total + w);
}
"#,
        &["10201"],
        "frozen_recursive_traversal_across_par_branches_no_leak",
        // 255 nodes, each costing a tag String plus (for the 127 internal
        // ones) a kids buffer. The floor sits far above the 3 of a
        // folded-away run while leaving room for allocator variation.
        200,
    );
}

/// B-2026-08-01-33 mechanism 3, **stage 2.5** — the same 255-node
/// two-branch traversal as the stage-2 fixture above, but written so that
/// every child is BOUND to a local before it is recursed into.
///
/// This is the shape stage 2 refused and stage 2.5 admits, and it is the
/// one that most needs the gate. A binding used to take a retain and a
/// scope-exit release; codegen now emits NEITHER, so the alias points at an
/// object whose count it never touched. If the escape check has a hole, or
/// if the skip fires on a binding whose root is not really the caller's,
/// the symptom here is a use-after-free (ASAN) or a leak (LSan on Linux CI)
/// — not a wrong answer, which is why the arithmetic tests cannot cover it.
/// The 255-node tree bounds the alias count: 254 aliases per traversal, x2
/// branches, all live concurrently.
///
/// NOT VACUOUS, on the same three legs as its stage-2 sibling
/// (B-2026-08-04-17): the seed is `env.args().len()`, every tag is an
/// f-string built from it at runtime, and `main` reads those bytes back
/// with `contains` after the traversal.
///
/// The expected output is computed independently rather than read off a
/// run: 255 nodes x (val 1 + tag length 19) = 5100 per traversal, x2
/// branches = 10200, +1 for the `contains` check = 10201 — the same total
/// as the sibling, since binding the child changes how the walk is spelled
/// and nothing about what it sums.
#[test]
fn asan_frozen_alias_traversal_across_par_branches_no_leak() {
    assert_clean_asan_run_min_allocs(
        r#"
shared struct Node { tag: String, val: i64, kids: Vec[Node] }

fn build(depth: i64, seed: i64) -> Node {
    if depth <= 0 {
        return Node { tag: f"leaf-{seed}-runtime-heap", val: seed, kids: [] };
    }
    let a = build(depth - 1, seed);
    let b = build(depth - 1, seed);
    return Node { tag: f"node-{seed}-runtime-heap", val: seed, kids: [a, b] };
}

fn sum(n: frozen Node) -> i64 {
    let mut s: i64 = n.val + n.tag.len();
    let mut i: i64 = 0;
    while i < n.kids.len() {
        let k = n.kids[i];
        s = s + sum(k);
        i = i + 1;
    }
    return s;
}

fn driver(root: frozen Node) -> i64 {
    let (a, b) = par {
        let a = sum(root);
        let b = sum(root);
        (a, b)
    };
    return a + b;
}

fn main() {
    let seed: i64 = env.args().len();
    let root = build(7, seed);
    let total = driver(root);
    let mut w: i64 = 0;
    if root.tag.contains("runtime-heap") { w = 1; }
    println(total + w);
}
"#,
        &["10201"],
        "frozen_alias_traversal_across_par_branches_no_leak",
        // Same tree as the sibling: 255 tag Strings plus 127 kids buffers.
        200,
    );
}

/// B-2026-08-01-33 mechanism 3, **stage 2.6** — the same 255-node
/// two-branch traversal as the two fixtures above, spelled with `for`.
///
/// Worth its own fixture rather than folding into the stage-2.5 one: the
/// loop variable is bound by a DIFFERENT codegen path from a `let`, and
/// that path takes no retain and registers no scope-exit cleanup — which
/// is exactly why stage 2.6 needed no codegen change and exactly why a
/// hole in it would show as a use-after-free (ASAN) or a leak (LSan on
/// Linux CI) rather than as a wrong answer.
///
/// NOT VACUOUS, on the same three legs as its siblings (B-2026-08-04-17):
/// the seed is `env.args().len()`, every tag is an f-string built from it
/// at runtime, and `main` reads those bytes back with `contains`.
///
/// Expected output computed independently, and identical to the siblings'
/// because the spelling changes nothing about what is summed: 255 nodes x
/// (val 1 + tag length 19) = 5100 per traversal, x2 branches = 10200, +1
/// for the `contains` check = 10201.
#[test]
fn asan_frozen_for_traversal_across_par_branches_no_leak() {
    assert_clean_asan_run_min_allocs(
        r#"
shared struct Node { tag: String, val: i64, kids: Vec[Node] }

fn build(depth: i64, seed: i64) -> Node {
    if depth <= 0 {
        return Node { tag: f"leaf-{seed}-runtime-heap", val: seed, kids: [] };
    }
    let a = build(depth - 1, seed);
    let b = build(depth - 1, seed);
    return Node { tag: f"node-{seed}-runtime-heap", val: seed, kids: [a, b] };
}

fn sum(n: frozen Node) -> i64 {
    let mut s: i64 = n.val + n.tag.len();
    for k in n.kids {
        s = s + sum(k);
    }
    return s;
}

fn driver(root: frozen Node) -> i64 {
    let (a, b) = par {
        let a = sum(root);
        let b = sum(root);
        (a, b)
    };
    return a + b;
}

fn main() {
    let seed: i64 = env.args().len();
    let root = build(7, seed);
    let total = driver(root);
    let mut w: i64 = 0;
    if root.tag.contains("runtime-heap") { w = 1; }
    println(total + w);
}
"#,
        &["10201"],
        "frozen_for_traversal_across_par_branches_no_leak",
        // Same tree as the siblings: 255 tag Strings plus 127 kids buffers.
        200,
    );
}

/// B-2026-08-01-33 mechanism 3, **stage 2.7** — the same 255-node
/// two-branch traversal as the three fixtures above, written as a method
/// with a `frozen self` receiver that recurses by calling itself.
///
/// The distinct risk this covers is the RECEIVER. `frozen self` lowers to
/// `ref self`, so the emitted code is identical to a `ref self` method's,
/// and the whole safety argument rests on the ownership pass having seen
/// `self` — which it very nearly did not, since `self` is its own
/// `ExprKind` rather than an identifier. A regression there would leave a
/// non-counting receiver escaping into somewhere it outlives, which shows
/// up here as a use-after-free (ASAN) or a leak (LSan on Linux CI), not as
/// a wrong answer.
///
/// NOT VACUOUS, on the same three legs as its siblings (B-2026-08-04-17),
/// and the expected output is computed independently and identically —
/// 255 nodes x (val 1 + tag length 19) = 5100 per traversal, x2 branches
/// = 10200, +1 for the `contains` check = 10201 — because the spelling
/// changes nothing about what is summed.
#[test]
fn asan_frozen_self_oo_traversal_across_par_branches_no_leak() {
    assert_clean_asan_run_min_allocs(
        r#"
shared struct Node { tag: String, val: i64, kids: Vec[Node] }

impl Node {
    fn total(frozen self) -> i64 {
        let mut s: i64 = self.val + self.tag.len();
        for k in self.kids {
            s = s + k.total();
        }
        return s;
    }
}

fn build(depth: i64, seed: i64) -> Node {
    if depth <= 0 {
        return Node { tag: f"leaf-{seed}-runtime-heap", val: seed, kids: [] };
    }
    let a = build(depth - 1, seed);
    let b = build(depth - 1, seed);
    return Node { tag: f"node-{seed}-runtime-heap", val: seed, kids: [a, b] };
}

fn driver(root: frozen Node) -> i64 {
    let (a, b) = par {
        let a = root.total();
        let b = root.total();
        (a, b)
    };
    return a + b;
}

fn main() {
    let seed: i64 = env.args().len();
    let root = build(7, seed);
    let total = driver(root);
    let mut w: i64 = 0;
    if root.tag.contains("runtime-heap") { w = 1; }
    println(total + w);
}
"#,
        &["10201"],
        "frozen_self_oo_traversal_across_par_branches_no_leak",
        // Same tree as the siblings: 255 tag Strings plus 127 kids buffers.
        200,
    );
}

/// B-2026-08-01-33 mechanism 3, **stage 3** — the two-branch traversal
/// shared from a `freeze`d LOCAL, with no `frozen` parameter introducing
/// the root.
///
/// The distinct risk is the OWNER. In every fixture above, the owner whose
/// refcount the frozen handle skips is the CALLER's value, alive for the
/// whole call by construction. Here it is `root`, a local in the same
/// frame with its own scope-exit release — so if the ownership rule that
/// stops `root` being consumed while `g` is live ever weakens, the frozen
/// handle dangles and this fixture reports a use-after-free (ASAN) or a
/// leak (LSan on Linux CI) rather than a wrong answer.
///
/// NOT VACUOUS on the same three legs as its siblings (B-2026-08-04-17),
/// and the expected output is computed independently and identically —
/// 255 nodes x (val 1 + tag length 19) = 5100 per traversal, x2 branches
/// = 10200, +1 for the `contains` check = 10201.
#[test]
fn asan_freeze_statement_shares_a_local_across_par_branches_no_leak() {
    assert_clean_asan_run_min_allocs(
        r#"
shared struct Node { tag: String, val: i64, kids: Vec[Node] }

fn build(depth: i64, seed: i64) -> Node {
    if depth <= 0 {
        return Node { tag: f"leaf-{seed}-runtime-heap", val: seed, kids: [] };
    }
    let a = build(depth - 1, seed);
    let b = build(depth - 1, seed);
    return Node { tag: f"node-{seed}-runtime-heap", val: seed, kids: [a, b] };
}

fn sum(n: frozen Node) -> i64 {
    let mut s: i64 = n.val + n.tag.len();
    for k in n.kids { s = s + sum(k); }
    return s;
}

fn main() {
    let seed: i64 = env.args().len();
    let root = build(7, seed);
    // Read the tag bytes back BEFORE the freeze. `contains` is not one of the
    // two container queries a frozen place permits, and after the `freeze`
    // the source is restricted too — which is the rule working, not a
    // limitation of the fixture. Statement order is what the walk keys on.
    let mut w: i64 = 0;
    if root.tag.contains("runtime-heap") { w = 1; }
    let g = freeze root;
    let (a, b) = par {
        let a = sum(g);
        let b = sum(g);
        (a, b)
    };
    println(a + b + w);
}
"#,
        &["10201"],
        "freeze_statement_shares_a_local_across_par_branches_no_leak",
        // Same tree as the siblings: 255 tag Strings plus 127 kids buffers.
        200,
    );
}

/// B-2026-08-01-33 stage 3b step 1 — the same share, on a type the freeze
/// site REFUSED until this slice.
///
/// `Node` carries `mut visits`, so it is not deeply immutable and E0512's
/// type-level stand-in rejected it outright. The per-instance uniqueness
/// proof replaces that stand-in for a `freeze` STATEMENT: `root` is named
/// nowhere but its own `let`, so no other handle exists to write through,
/// and the freeze is admitted.
///
/// What the fixture reads is the point. `tag` and `kids` are IMMUTABLE
/// fields, and `visits` — the `mut` one — is still unreachable through the
/// frozen handle, because the projection guard (stage 3b step 3) is
/// deliberately untouched. So this pins the increment exactly: a
/// `mut`-bearing `shared` value can now be shared across branches for reads
/// of its immutable parts, and no further.
///
/// The traversal is INLINE rather than in a callee, and that is the
/// remaining limitation rather than a fixture convenience: a `frozen Node`
/// PARAMETER of a `mut`-bearing type is still refused, since a parameter's
/// instance belongs to a caller this check cannot see. #133 needs that, and
/// needs step 3 as well.
#[test]
fn asan_freeze_statement_shares_a_mut_bearing_local_across_par_branches_no_leak() {
    assert_clean_asan_run_min_allocs(
        r#"
shared struct Node { tag: String, val: i64, kids: Vec[Node], mut visits: i64 }

fn build(depth: i64, seed: i64) -> Node {
    if depth <= 0 {
        return Node { tag: f"leaf-{seed}-runtime-heap", val: seed, kids: [], visits: 0 };
    }
    let a = build(depth - 1, seed);
    let b = build(depth - 1, seed);
    return Node { tag: f"node-{seed}-runtime-heap", val: seed, kids: [a, b], visits: 0 };
}

fn main() {
    let seed: i64 = env.args().len();
    // NOTHING may name `root` between its `let` and the `freeze`, or the
    // uniqueness proof fails and E0512 refuses the type again. That is the rule
    // working: the sibling fixture above reads its source before the freeze
    // precisely because its type IS deeply immutable and needs no proof.
    let root = build(7, seed);
    let g = freeze root;
    let (a, b) = par {
        let a = g.val + g.tag.len() + g.kids.len();
        let b = g.val + g.tag.len() + g.kids.len();
        (a, b)
    };
    println(a + b);
}
"#,
        &["44"],
        "freeze_statement_shares_a_mut_bearing_local_across_par_branches",
        // 255 tag Strings plus the 127 interior kids buffers.
        300,
    );
}

/// B-2026-08-01-33 stage 3b step 3 — **LeetCode #133**, the program this
/// whole entry was opened for, running across four `par` branches.
///
/// It needs all three steps at once and each is visible in the source:
/// the `freeze` STATEMENT (step 1's uniqueness proof — nothing may name
/// `root` between its `let` and the freeze), the `frozen` PARAMETER on a
/// `mut`-bearing type (step 2, admitted from the caller's freeze), and the
/// READ of the `mut` `neighbors` field inside the callee (step 3).
///
/// The clone half is the part that makes it #133 rather than a traversal:
/// each branch BUILDS fresh nodes and writes THEIR `mut neighbors`. Those
/// writes are to local instances, not to anything rooted at the frozen
/// place, which is exactly the distinction the ledger said a type-level
/// check could not make and an instance-level one could.
///
/// The write-channel enumeration that keeps this sound is pinned in
/// tests/ownership.rs (`frozen_mut_field_is_readable_but_never_writable`),
/// including the `mut`-marked-argument hole this step opened and closed.
#[test]
fn asan_kata133_clone_graph_frozen_across_par_branches_no_leak() {
    assert_clean_asan_run_min_allocs(
        r#"
shared struct Node { val: i64, mut neighbors: Vec[Node] }

fn clone_graph(n: frozen Node) -> i64 {
    let mut acc: i64 = n.val;
    let mut fresh = Node { val: n.val, neighbors: [] };
    for k in n.neighbors {
        acc = acc + clone_graph(k);
        fresh.neighbors.push(Node { val: k.val, neighbors: [] });
    }
    return acc + fresh.neighbors.len();
}

fn sum_clones(root: frozen Node, count: i64) -> i64 {
    let mut s: i64 = 0;
    let mut i: i64 = 0;
    while i < count { s = s + clone_graph(root); i = i + 1; }
    return s;
}

fn build(depth: i64, seed: i64) -> Node {
    if depth <= 0 { return Node { val: seed, neighbors: [] }; }
    let a = build(depth - 1, seed);
    let b = build(depth - 1, seed);
    return Node { val: seed, neighbors: [a, b] };
}

fn main() {
    let seed: i64 = env.args().len();
    let root = build(6, seed);
    let g = freeze root;
    let (s1, s2, s3, s4) = par {
        let s1 = sum_clones(g, 7);
        let s2 = sum_clones(g, 7);
        let s3 = sum_clones(g, 7);
        let s4 = sum_clones(g, 7);
        (s1, s2, s3, s4)
    };
    println(s1 + s2 + s3 + s4);
}
"#,
        &["7084"],
        "kata133_clone_graph_frozen_across_par_branches",
        // The 63-node source graph plus a fresh clone per visited node,
        // 7 rounds x 4 branches.
        1000,
    );
}

/// B-2026-08-01-33 stage 3c (B-2026-08-07-23) — the ITERATIVE traversal, on
/// the shape that motivated the whole entry: a per-branch `Vec` worklist
/// holding non-counting handles into one shared graph.
///
/// This is the leak/UAF gate for a suppression PAIR, and the pair is why
/// the sanitizer is the right instrument rather than a traffic count:
///
/// * if the push retain were skipped but the element drain kept, the
///   container would release counts it never took and ASAN would report a
///   use-after-free on the second branch's read;
/// * if the drain were skipped but the retain kept, every element would
///   leak one ref and the nodes would never be freed — invisible to ASAN
///   on macOS and caught only by LSan, which is why the authoritative
///   answer is the Linux `memory-sanitizer` job.
///
/// The graph is deep enough (63 nodes) and the rounds many enough that a
/// single stranded ref per element is far above the `min_allocs` floor.
/// Each branch suffixes its own bindings because the `par` capture gate is
/// name-keyed and two branches declaring `let n = …` of a `shared` type
/// read as one binding reachable from both.
#[test]
fn asan_frozen_vec_worklist_traversal_across_par_branches_no_leak() {
    assert_clean_asan_run_min_allocs(
        r#"
shared struct Node { val: i64, mut kids: Vec[Node] }

fn build(depth: i64, seed: i64) -> Node {
    if depth <= 0 { return Node { val: seed, kids: [] }; }
    let a = build(depth - 1, seed);
    let b = build(depth - 1, seed);
    return Node { val: seed, kids: [a, b] };
}

fn main() {
    let seed: i64 = env.args().len();
    let root = build(5, seed);
    let g = freeze root;
    let (x, y) = par {
        let x = {
            let mut rounds: i64 = 0;
            let mut acc: i64 = 0;
            while rounds < 5 {
                let mut worka: Vec[Node] = Vec.new();
                worka.push(g);
                let mut ia: i64 = 0;
                while ia < worka.len() {
                    let na = worka[ia as u64];
                    acc = acc + na.val;
                    for ka in na.kids { worka.push(ka); }
                    ia = ia + 1;
                }
                rounds = rounds + 1;
            }
            acc
        };
        let y = {
            let mut roundsb: i64 = 0;
            let mut accb: i64 = 0;
            while roundsb < 5 {
                let mut workb: Vec[Node] = Vec.new();
                workb.push(g);
                let mut ib: i64 = 0;
                while ib < workb.len() {
                    let nb = workb[ib as u64];
                    accb = accb + nb.val;
                    for kb in nb.kids { workb.push(kb); }
                    ib = ib + 1;
                }
                roundsb = roundsb + 1;
            }
            accb
        };
        (x, y)
    };
    println(x + y);
}
"#,
        // 63 nodes, each val==1, 5 rounds, 2 branches.
        &["630"],
        "frozen_vec_worklist_traversal_across_par_branches",
        // The 63-node source graph plus each round's worklist buffer.
        100,
    );
}

/// B-2026-08-08-2 — LeetCode #133 Clone Graph, the kata that motivated the
/// whole `frozen` entry, finally running its BFS across `par` branches.
///
/// Every branch clones the SAME frozen source graph into its own private
/// structures and checksums the result against the source's. Three
/// container roles appear at once and they are deliberately not alike,
/// because conflating them is what made this row's original diagnosis
/// wrong:
///
/// * `queue: Vec[Node]` — holds FROZEN SOURCE handles. Non-counting, no
///   per-element drop (stage 3c).
/// * `visited: Map[i64, Node]` — holds the CLONES, which are ordinary
///   owned nodes actively mutated through (`curr_clone.neighbors.push`).
///   It needs nothing from `frozen` and must keep every count it takes.
/// * `work` / `seen` in the checksum — an ordinary traversal of the
///   finished clone.
///
/// So the leak surface is a MIXTURE, and that is the point: the frozen
/// container's drop must be skipped while the clone map's must not. A fix
/// that suppressed too broadly would leak the entire cloned graph in every
/// branch, which at this size is far above the `min_allocs` floor.
///
/// THE GRAPH IS ACYCLIC, and that is a limit of the gate rather than of the
/// feature. LeetCode #133's real inputs have cycles, and a cyclic `shared
/// struct` graph leaks under Kara's non-atomic RC today with NO `frozen`
/// and no `par` involved — measured at 288 bytes / 8 allocations for a
/// 4-node cycle built and checksummed by itself, and unchanged by cloning
/// it. Filed separately as B-2026-08-08-4. Using a cyclic graph here would
/// make this test red for that unrelated reason and blind it to the leak it
/// exists to catch.
///
/// The checksums are asserted EQUAL to the source's, not merely non-zero.
/// A traffic count cannot see a wrong answer, and a clone that silently
/// shared the source's nodes instead of copying them would still sum
/// correctly if only the values were checked — so the checksum walks
/// neighbours too.
#[test]
fn asan_kata133_clone_graph_bfs_across_par_branches_no_leak() {
    assert_clean_asan_run_min_allocs(
        r#"
shared struct Node { val: i64, mut neighbors: Vec[Node] }

fn clone_bfs(g: frozen Node) -> Node {
    let mut visited: Map[i64, Node] = Map.new();
    let mut queue: Vec[Node] = Vec.new();
    let root_clone = Node { val: g.val, neighbors: Vec.new() };
    let _ = visited.insert(g.val, root_clone);
    queue.push(g);
    let mut qi: i64 = 0;
    while qi < queue.len() {
        let curr = queue[qi as u64];
        qi = qi + 1;
        let curr_clone = visited.get(curr.val).unwrap();
        for nb in curr.neighbors {
            if let None = visited.get(nb.val) {
                let fresh = Node { val: nb.val, neighbors: Vec.new() };
                let _ = visited.insert(nb.val, fresh);
                queue.push(nb);
            }
            let nb_clone = visited.get(nb.val).unwrap();
            curr_clone.neighbors.push(nb_clone);
        }
    }
    return root_clone;
}

fn checksum(root: Node) -> i64 {
    let mut seen: Map[i64, i64] = Map.new();
    let mut work: Vec[Node] = Vec.new();
    work.push(root);
    let _ = seen.insert(root.val, 1);
    let mut i: i64 = 0;
    let mut total: i64 = 0;
    while i < work.len() {
        let n = work[i as u64];
        i = i + 1;
        total = total + n.val * 100;
        for nb in n.neighbors {
            total = total + nb.val;
            if let None = seen.get(nb.val) { let _ = seen.insert(nb.val, 1); work.push(nb); }
        }
    }
    return total;
}

fn build(seed: i64) -> Node {
    let a = Node { val: seed, neighbors: Vec.new() };
    let b = Node { val: seed + 1, neighbors: Vec.new() };
    let c = Node { val: seed + 2, neighbors: Vec.new() };
    let d = Node { val: seed + 3, neighbors: Vec.new() };
    b.neighbors.push(c);
    a.neighbors.push(b); a.neighbors.push(d);
    return a;
}

fn clone_and_sum(g: frozen Node, rounds: i64) -> i64 {
    let mut i: i64 = 0;
    let mut last: i64 = 0;
    while i < rounds { last = checksum(clone_bfs(g)); i = i + 1; }
    return last;
}

fn main() {
    let seed: i64 = env.args().len();
    // Built twice on purpose: naming `root` before the `freeze` would break
    // the uniqueness precondition the freeze site checks, and the expected
    // checksum is a property of the SHAPE, not of this particular instance.
    let expected = checksum(build(seed));
    let root = build(seed);
    let g = freeze root;
    let (x, y) = par {
        let x = clone_and_sum(g, 6);
        let y = clone_and_sum(g, 6);
        (x, y)
    };
    if x == expected { if y == expected { println("match"); } else { println("y"); } }
    else { println("x"); }
}
"#,
        &["match"],
        "kata133_clone_graph_bfs_across_par_branches",
        // 4 source nodes, plus a 4-node clone graph per round, 6 rounds x
        // 2 branches, plus each round's visited map and worklist buffers.
        200,
    );
}

/// B-2026-08-18-39 — a HEAP-BOXED enum payload published as an auto-par
/// RETURN SLOT is freed exactly once, by the JOINING SCOPE.
///
/// An `Option[T]` whose `T` outgrows the enum's 3-word inline area spills
/// behind a `malloc`'d box, and the return slot carries only that pointer.
/// `BoxedEnumDrop` was matched by neither the sentinel scan nor the
/// `SlotOwnership` transfer loop, so the branch freed the box it had just
/// published and the parent's read was a use-after-free.
///
/// THIS IS THE LEAK-SIDE GUARD ON THE FIX, not on the original defect. The
/// repair is a TRANSFER — remove the branch's action, re-register the
/// equivalent against the parent's alloca — and the way to get that half
/// right and still be wrong is the one the `SharedElided` row documents:
/// suppress in the branch, adopt in nobody, and the box leaks instead of
/// being read after free. Measured: with the transfer reduced to bare
/// suppression this fixture reports 32 leaked bytes. The use-after-free
/// half is pinned by output value in tests/par_codegen.rs.
///
/// THE CONSUMER READS, IT DOES NOT MOVE, and that is a deliberate
/// restriction rather than an accident of drafting. Moving the joined
/// binding into a by-value call (`tail(hit)`) still leaks — the move-out
/// sentinel zeroes the parent's slot so the adopted drop no-ops, and a
/// by-value enum parameter does not adopt the box. That hole is older than
/// this row and independent of it: sequentially the same program is clean
/// only because the box never escapes one function and LLVM deletes the
/// allocation outright, so nothing was ever freeing it there either. It is
/// filed as B-2026-08-18-48 and is NOT covered here.
///
/// BUILT IN A HELPER AND THE STACK THEN CHURNED, for the reason the
/// B-2026-08-08-15 fixture below spells out: built inline in `main`, the
/// parent's alloca and returns-struct are stack slots still holding the
/// pointer at exit, so LSan's conservative scan calls an orphaned box
/// reachable and reports nothing.
///
/// Vacuity guard lives in tests/codegen.rs
/// (`par_slot_boxed_option_fixture_actually_parallelizes`): a `match` on
/// these bindings makes the analyzer decline to fork at all, which would
/// empty this fixture silently — the first draft did exactly that and
/// passed against the broken compiler. Keep the two bodies in sync.
#[test]
fn asan_par_slot_boxed_option_payload_freed_once() {
    assert_clean_asan_run_min_allocs_auto_par(
        r#"
struct W { f0: i64, f1: i64, f2: i64, f3: i64 }

fn get(v: ref Vec[W], k: i64) -> Option[W] {
    let mut i = 0i64;
    while i < v.len() { if v[i].f0 == k { return Some(v[i]); } i = i + 1i64; }
    return None;
}

fn build(seed: i64) -> i64 {
    let mut ns: Vec[W] = Vec.new();
    ns.push(W { f0: seed, f1: seed + 1i64, f2: seed + 2i64, f3: seed + 3i64 });
    // Two independent Option-returning calls: the group the analyzer forks,
    // and the shape whose slots carry box pointers.
    let hit = get(ns, seed);
    let miss = get(ns, seed + 99i64);
    // A READING consumer — `?.` projects out of the box without moving the
    // binding, so the parent stays the owner and its adopted drop is what has
    // to fire. See the doc comment on why a moving consumer is excluded.
    let a = hit?.f3;
    let b = miss?.f3;
    return match a { Some(x) => x, None => -1i64 } + match b { Some(x) => x, None => -1i64 };
}

// Overwrites the dead frame the published box pointers lived in, so LSan
// cannot mistake stack residue for reachability. See the doc comment.
fn churn(n: i64) -> i64 {
    if n <= 0 { return 0; }
    let pad: i64 = n * 3;
    return pad + churn(n - 1);
}

fn main() {
    let seed: i64 = env.args().len();
    let k = build(seed);
    let noise = churn(2000);
    println(k + noise - noise);
}
"#,
        &["3"],
        "par_slot_boxed_option_payload",
        // The Vec's element buffer plus the boxed payload.
        2,
    );
}

/// B-2026-08-08-15 — an RC-bearing `shared struct` published as an auto-par
/// RETURN SLOT is released exactly once, by the JOINING SCOPE.
///
/// The branch that constructs the value writes it into the parent's return
/// struct and then must not release it; the parent must. `SlotOwnership`
/// transfers eight handle/payload cleanup kinds across that boundary and
/// carried no RC variant, so the release was suppressed in the branch
/// (`nullify_local`) and adopted by nobody.
///
/// THIS IS THE LEAK FACE. Its twin below is the same defect presenting as a
/// use-after-free; read them together, because which one a program gets is
/// decided by an implementation detail (whether the branch's queued release
/// was of a shape the sentinel scan recognised), not by anything the author
/// wrote.
///
/// THE GRAPH IS BUILT IN A HELPER AND THE STACK IS THEN CHURNED, for the
/// same reason `asan_vec_of_weak_back_edges_reclaims_a_cycle_no_leak` above
/// does it, and it is what makes this fixture mean anything. Built inline in
/// `main`, BOTH of these tests PASS against the broken compiler — measured,
/// not assumed: the parent's own alloca and its returns-struct are stack
/// slots still holding the published pointer at exit, so LSan's
/// conservative scan calls the orphaned box reachable and reports nothing.
/// A recursion that overwrites the dead frame is what exposes it. (valgrind
/// on the inline shape does report it, which is how the divergence surfaced
/// — do not take a green ASAN run on an inline shape as evidence here.)
///
/// Vacuity guard lives in tests/codegen.rs
/// (`par_slot_shared_struct_fixtures_actually_parallelize`): if the
/// analyzer stops splitting this shape, the transfer never happens and this
/// fixture silently covers nothing. Keep the two bodies in sync.
#[test]
fn asan_par_slot_shared_struct_into_container_released_once() {
    assert_clean_asan_run_min_allocs_auto_par(
        r#"
shared struct P { v: i64 }
shared struct Q { a: i64, b: i64, c: i64 }

// B-2026-09-03-40 — `mix` exists to keep the p/q initializers ABOVE the
// par_run band gate's dispatch threshold. Before that row the band formed
// even for three bare struct literals, because the gate's visibility floor
// was inverted and never declined a cheap group; it now declines one whose
// work is fully visible and under 500 units, which is these three
// initializers exactly. Without this the band never forms and the fixture
// goes vacuous — `par_slot_shared_struct_fixtures_actually_parallelize`
// (tests/codegen.rs) is the guard that says so, and it compiles this source
// verbatim, so EDIT BOTH.
//
// It does not perturb what is being tested: `mix` is pure and heap-free, the
// two boxes and the Vec buffer are unchanged (min-allocs still 3), and the
// printed answer is unchanged because it is `w.len() + q.a - q.a`.
fn mix(seed: i64) -> i64 {
    let mut s: i64 = seed;
    let mut i: i64 = 0;
    while i < 8 { s = s + i * 3; i = i + 1; }
    return s;
}

fn build(seed: i64) -> i64 {
    let p: P = P { v: mix(seed) };
    let q: Q = Q { a: mix(seed), b: mix(seed), c: mix(seed) };
    let mut w: Vec[P] = Vec.new();
    w.push(p);
    return w.len() + q.a - q.a;
}

// Overwrites the dead frame the published handles lived in, so LSan cannot
// mistake stack residue for reachability. See the doc comment.
fn churn(n: i64) -> i64 {
    if n <= 0 { return 0; }
    let pad: i64 = n * 3;
    return pad + churn(n - 1);
}

fn main() {
    let seed: i64 = env.args().len();
    let k = build(seed);
    let noise = churn(2000);
    println(k + noise - noise);
}
"#,
        &["1"],
        "par_slot_shared_struct_into_container",
        // 2 struct boxes + the Vec's element buffer.
        3,
    );
}

/// B-2026-08-08-15, THE USE-AFTER-FREE FACE — a published slot value that
/// the joining scope merely READS.
///
/// Same missing transfer as its twin above, opposite symptom. Here the
/// branch's queued release was a `FreeSharedElided` — the direct `free` the
/// RC-elision analysis collapses `RcDec` to once it proves the count can
/// never exceed 1 — which the branch-side sentinel scan did not match at
/// all. So the branch FREED the box it had just published, and the parent's
/// `p.v` read it back: `Invalid read of size 8` in `main` against a block
/// freed in `__par_branch_0_0`.
///
/// IT PRINTED THE RIGHT ANSWER while doing so — the freed block had not been
/// reused — which is exactly why this shape was first mistaken for a
/// passing control. Asserting stdout alone would NOT catch this; the
/// sanitizer is the whole test.
///
/// Helper + churn for the same reason as its twin above — see that doc
/// comment before editing either body.
///
/// HONESTY NOTE — THIS FIXTURE IS NOT STASH-PROVEN RED, and you should not
/// treat it as the guard for the use-after-free. Against the pre-fix
/// compiler it PASSES here: ASAN does not flag this access in this harness,
/// for reasons not run to ground. valgrind does, at BOTH opt levels —
///
///     Invalid read of size 8 at main
///     Address is 8 bytes inside a block of size 16 free'd
///       by __par_branch_0_0
///
/// so the defect is real and measured; only the detector is blind. It is
/// kept because it must stay green (a regression that reintroduces the
/// branch-side free may well become ASAN-visible in a slightly different
/// shape, and the stdout assertion pins the answer), but the fixture that
/// actually FAILS against the broken compiler is its container twin above.
/// If you are changing this code path, re-verify with valgrind, not this.
#[test]
fn asan_par_slot_shared_struct_read_only_join_no_use_after_free() {
    assert_clean_asan_run_min_allocs_auto_par(
        r#"
shared struct P { v: i64 }
shared struct Q { a: i64, b: i64, c: i64 }

// B-2026-09-03-40 — `mix` exists to keep the p/q initializers ABOVE the
// par_run band gate's dispatch threshold. Before that row the band formed
// even for three bare struct literals, because the gate's visibility floor
// was inverted and never declined a cheap group; it now declines one whose
// work is fully visible and under 500 units, which is these three
// initializers exactly. Without this the band never forms and the fixture
// goes vacuous — `par_slot_shared_struct_fixtures_actually_parallelize`
// (tests/codegen.rs) is the guard that says so, and it compiles this source
// verbatim, so EDIT BOTH.
//
// It does not perturb what is being tested: `mix` is pure and heap-free, the
// two boxes and the Vec buffer are unchanged (min-allocs still 3), and the
// printed answer is unchanged because it is `w.len() + q.a - q.a`.
fn mix(seed: i64) -> i64 {
    let mut s: i64 = seed;
    let mut i: i64 = 0;
    while i < 8 { s = s + i * 3; i = i + 1; }
    return s;
}

fn build(seed: i64) -> i64 {
    let p: P = P { v: mix(seed) };
    let q: Q = Q { a: mix(seed), b: mix(seed), c: mix(seed) };
    let mut w: Vec[i64] = Vec.new();
    w.push(p.v);
    return w.len() + q.a - q.a;
}

fn churn(n: i64) -> i64 {
    if n <= 0 { return 0; }
    let pad: i64 = n * 3;
    return pad + churn(n - 1);
}

fn main() {
    let seed: i64 = env.args().len();
    let k = build(seed);
    let noise = churn(2000);
    println(k + noise - noise);
}
"#,
        &["1"],
        "par_slot_shared_struct_read_only_join",
        // 2 struct boxes + the scalar Vec's buffer.
        3,
    );
}

/// B-2026-08-01-33 stage 3b step 2 — the shape the whole entry exists for,
/// on a `mut`-bearing type: a RECURSIVE traversal IN A CALLEE, shared
/// read-only across two `par` branches.
///
/// The sibling above keeps its traversal inline because a `frozen Node`
/// PARAMETER of a `mut`-bearing type was refused. Step 2 admits it — the
/// parameter's guarantee comes from the caller's `freeze`, not from its own
/// type — and this is what that buys. It is #133's structure exactly;
/// #133 itself additionally READS its `mut` field, which the projection
/// guard still refuses (step 3).
///
/// The load-bearing control lives in tests/ownership.rs
/// (`freeze_statement_relaxes_e0512_only_for_a_uniquely_bound_source`): an
/// UNFROZEN root reaching a branch must still be refused at the par
/// capture, because that gate is the entire reason admitting the parameter
/// is safe. Read the two together.
#[test]
fn asan_frozen_param_traversal_of_a_mut_bearing_graph_across_par_branches_no_leak() {
    assert_clean_asan_run_min_allocs(
        r#"
shared struct Node { tag: String, val: i64, kids: Vec[Node], mut visits: i64 }

fn build(depth: i64, seed: i64) -> Node {
    if depth <= 0 {
        return Node { tag: f"leaf-{seed}-runtime-heap", val: seed, kids: [], visits: 0 };
    }
    let a = build(depth - 1, seed);
    let b = build(depth - 1, seed);
    return Node { tag: f"node-{seed}-runtime-heap", val: seed, kids: [a, b], visits: 0 };
}

// A `frozen` parameter of a `mut`-bearing type: refused outright before step 2.
fn sum(n: frozen Node) -> i64 {
    let mut s: i64 = n.val + n.tag.len();
    for k in n.kids { s = s + sum(k); }
    return s;
}

fn main() {
    let seed: i64 = env.args().len();
    let root = build(7, seed);
    let g = freeze root;
    let (a, b) = par {
        let a = sum(g);
        let b = sum(g);
        (a, b)
    };
    println(a + b);
}
"#,
        // 255 nodes x (val 1 + tag len 19) x 2 branches.
        &["10200"],
        "frozen_param_traversal_of_a_mut_bearing_graph",
        // 255 tag Strings plus the 127 interior kids buffers.
        300,
    );
}

#[test]
fn test_auto_par_returns_no_use_after_move_no_double_drop() {
    // Each `read_*` builds a fresh `Vec[i64]` of three elements;
    // the parent sums the four `.len()` values and prints `12`.
    // Disjoint resources (`R0`..`R3`) make the four reads eligible
    // for auto-par grouping; the typed return value forces the
    // slot mechanism to fire (slice 2 would have dropped the
    // group via the use-outside gate).
    assert_clean_asan_run_with_concurrency(
        r#"
effect resource R0;
effect resource R1;
effect resource R2;
effect resource R3;

fn make_v0() -> Vec[i64] reads(R0) {
    let mut v: Vec[i64] = Vec.new();
    v.push(10_i64);
    v.push(20_i64);
    v.push(30_i64);
    v
}
fn make_v1() -> Vec[i64] reads(R1) {
    let mut v: Vec[i64] = Vec.new();
    v.push(11_i64);
    v.push(21_i64);
    v.push(31_i64);
    v
}
fn make_v2() -> Vec[i64] reads(R2) {
    let mut v: Vec[i64] = Vec.new();
    v.push(12_i64);
    v.push(22_i64);
    v.push(32_i64);
    v
}
fn make_v3() -> Vec[i64] reads(R3) {
    let mut v: Vec[i64] = Vec.new();
    v.push(13_i64);
    v.push(23_i64);
    v.push(33_i64);
    v
}

fn main() {
    let v0 = make_v0();
    let v1 = make_v1();
    let v2 = make_v2();
    let v3 = make_v3();
    println(v0.len() + v1.len() + v2.len() + v3.len());
}
"#,
        &["12"],
        "auto_par_returns_no_use_after_move_no_double_drop",
    );
}

// ── Auto-par scope cleanup ────────────────────────────────────
//
// Pre-fix the auto-par codegen path (`emit_par_branch_fn`) didn't
// push a root cleanup frame at branch entry, so every
// `track_vec_var` / `track_map_var` / `track_rc_var` call inside the
// branch silently failed to queue (their bodies are `if let Some(frame)
// = self.scope_cleanup_actions.last_mut()`). The branch's accumulated
// cleanup queue was also discarded on normal completion — only the
// cancel-path called `emit_scope_cleanup`. Result: every branch-local
// heap allocation leaked at branch exit, and any class-(ii) slot
// binding's heap buffer leaked at the parent function's scope-exit
// (parent didn't `track_vec_var` the slot's loaded alloca).
//
// The kata-6 (zigzag) bench at K = 10,000 measured ~474 MiB peak RSS
// from this leak. The fix:
//   1. par_blocks.rs: push a fresh cleanup frame at branch entry;
//      call `emit_scope_cleanup` before the branch's normal-completion
//      `ret void`, with cap-zero suppression on slot-source allocas
//      to prevent the slot's heap buffer from being freed twice
//      (branch + parent).
//   2. stmts.rs: re-enable `track_vec_var` on the parent's slot
//      alloca so the buffer is freed at parent scope-exit.
//
// These tests exercise the shapes that surfaced the leak in the
// 2026-05-17 kata-6 bench investigation; without the fix they
// produced LeakSanitizer reports of ~10 MiB+ accumulated leak per
// run.

#[test]
fn asan_auto_par_function_local_vec_freed_on_branch_exit() {
    // Bare Vec[i64] allocated inside a function called from a
    // 10-iter loop. Auto-par groups the let-stmts inside `build`,
    // dispatching the Vec allocation into a branch — without the
    // fix, the branch's track_vec_var no-ops and the slot's
    // parent-side alloca isn't tracked either; ~10 KB leak per
    // call. With the fix, the parent's `track_vec_var` runs at
    // function exit and frees the heap data.
    assert_clean_asan_run(
        r#"
fn build(n: i64) -> i64 {
    let mut v: Vec[i64] = Vec.new();
    let mut i = 0i64;
    while i < n {
        v.push(i);
        i = i + 1;
    }
    v.len()
}
fn main() {
    let mut sum = 0i64;
    let mut k = 0i64;
    while k < 10 {
        sum = sum + build(100);
        k = k + 1;
    }
    println(sum);
}
"#,
        &["1000"],
        "auto_par_function_local_vec_freed_on_branch_exit",
    );
}

/// B-2026-07-31-40 — a Drop-valued `Map` introduced INSIDE an auto-par
/// parallel group keeps working end-to-end across the multi-action
/// slot-ownership transfer. NOTE this ASAN test guards the
/// DOUBLE-FREE/UAF side only (a wrong transfer that re-registered the
/// handle free twice, or dropped the branch-side suppression, aborts
/// here): the pre-fix LEAK is NOT LSan-visible under this harness —
/// unoptimized code plus parked pool threads leave stale copies of the
/// handle pointer inside scanned stack regions, so LSan classifies the
/// lost blocks as reachable. The leak side is pinned by the
/// optimizer-independent IR assertion in `tests/par_codegen.rs`
/// (`test_ir_auto_par_ingroup_dropval_map_transfers_handle_free`),
/// which was proven failing pre-fix.
#[test]
fn asan_auto_par_ingroup_dropval_map_freed() {
    let mut expected: Vec<&str> = Vec::new();
    for _ in 0..10 {
        expected.push("drop 7");
        expected.push("drop 8");
    }
    expected.push("65");
    assert_clean_asan_run(
        r#"
struct Res { id: i64 }
impl Drop for Res {
    fn drop(mut ref self) {
        println(f"drop {self.id}")
    }
}
fn work(n: i64) -> i64 {
    let a = n + 1;
    let mut m: Map[i64, Res] = Map.new();
    let _ = m.insert(1, Res { id: 7 });
    let _ = m.insert(1, Res { id: 8 });
    a + m.len()
}
fn main() {
    let mut k = 0i64;
    let mut sum = 0i64;
    while k < 10 {
        sum = sum + work(k);
        k = k + 1;
    }
    println(f"{sum}");
}
"#,
        &expected,
        "auto_par_ingroup_dropval_map_freed",
    );
}

#[test]
fn asan_auto_par_vec_of_vec_freed_on_branch_exit() {
    // Vec[Vec[char]] built inside a function — the kata-6 zigzag
    // shape. Each call's per-row inner Vecs and outer Vec
    // allocate; without the fix all of these leak. The recursive-
    // drop fast path inside `FreeVecBuffer` handles the inner
    // Vec[char] buffers when the outer Vec drops; the fix routes
    // through that path correctly when the outer Vec is registered
    // via the parent-side `track_vec_var`.
    assert_clean_asan_run(
        r#"
fn build(n: i64) -> i64 {
    let mut rows: Vec[Vec[char]] = Vec.new();
    let mut r = 0i64;
    while r < 4 {
        let row: Vec[char] = Vec.new();
        rows.push(row);
        r = r + 1;
    }
    let mut i = 0i64;
    while i < n {
        rows[i % 4].push('A');
        i = i + 1;
    }
    rows[0].len()
}
fn main() {
    let mut sum = 0i64;
    let mut k = 0i64;
    while k < 10 {
        sum = sum + build(100);
        k = k + 1;
    }
    println(sum);
}
"#,
        &["250"],
        "auto_par_vec_of_vec_freed_on_branch_exit",
    );
}

// ── B-2026-07-30-1: iteration-local `shared` under reduction fan-out ──
//
// THE load-bearing memory test for the `iter_local` precision pass.
// Before it, the B-2026-07-16-6 type gate declined any reduction whose
// body touched a `shared`-typed expression, so this shape never
// reached a worker thread at all. It does now, which puts a
// NON-ATOMIC refcount header inside a parallel region for the first
// time — the exact hazard B-2026-07-16-6 documents.
//
// The precision claim is that each iteration's list is allocated and
// dropped entirely within one worker, so no two threads ever touch
// one header. That claim is only worth the paper it is written on if
// a sanitizer agrees, and the failure mode if the escape analysis is
// wrong is precisely what LSan/ASAN catch: a lost rc-dec (leak) or a
// lost rc-inc (use-after-free / double-free).
//
// Geometry: 60_000 iterations × a 12-node `Option[Node]` chain built
// head-first and folded back down. The trip count clears
// `REDUCE_DISPATCH_THRESHOLD_UNITS` so the lowering actually fires
// rather than falling back to sequential codegen and passing
// vacuously. The `while cur.is_some()` walk avoids `break`, which
// `block_has_early_exit` rejects for unrelated reasons.
#[test]
fn asan_auto_par_reduction_iteration_local_shared_list_no_leak_no_uaf() {
    assert_clean_asan_run_with_concurrency(
        r#"
shared struct Node {
    val: i64,
    next: Option[Node],
}

fn main() {
    let mut sum = 0;
    let mut k = 0;
    while k < 60000 {
        let mut head: Option[Node] = None;
        let mut j = 0;
        while j < 12 {
            head = Some(Node { val: k + j, next: head });
            j = j + 1;
        }
        let mut acc = 0;
        let mut cur = head;
        while cur.is_some() {
            let n = cur.unwrap();
            acc = acc + n.val;
            cur = n.next;
        }
        sum = sum + acc;
        k = k + 1;
    }
    println(sum);
}
"#,
        &["21603600000"],
        "auto_par_reduction_iteration_local_shared_list_no_leak_no_uaf",
    );
}

#[test]
fn asan_auto_par_vec_char_return_freed_on_caller_scope_exit() {
    // Function returns a Vec[char] consumed by the caller. The
    // class-(ii) slot machinery moves the branch's local Vec to a
    // parent-side alloca; with the fix that parent alloca is
    // `track_vec_var`-registered so the buffer is freed when the
    // surrounding function returns.
    assert_clean_asan_run(
        r#"
fn build_chars(n: i64) -> Vec[char] {
    let mut out: Vec[char] = Vec.new();
    let mut i = 0i64;
    while i < n {
        out.push('X');
        i = i + 1;
    }
    out
}
fn main() {
    let mut sum = 0i64;
    let mut k = 0i64;
    while k < 10 {
        let v = build_chars(100);
        sum = sum + v.len();
        k = k + 1;
    }
    println(sum);
}
"#,
        &["1000"],
        "auto_par_vec_char_return_freed_on_caller_scope_exit",
    );
}

#[test]
fn asan_auto_par_shared_struct_option_return_slot() {
    // A par group with two effectful stmts where one returns
    // `Option[shared T]` consumed in the parent scope. Pre-fix
    // (2026-05-17), the branch's `emit_scope_cleanup` ran the
    // queued `RcDecOption` on the slot-source local, dropping the
    // head Node's refcount to 0 → freed. The parent's load from
    // the return slot then yielded a dangling pointer; the
    // kata 2 add-two-numbers bench manifested as `node.val = 0`
    // (allocator-zeroed memory). Fix added RcDec/RcDecOption
    // suppression to the par-branch slot-source loop (analog to
    // the existing Vec `cap=0` suppression).
    //
    // The test geometry mirrors the kata 2 reduction: `make_vec`
    // returns `Vec[i64]`, `from_arr` returns `Option[Node]` where
    // `Node` is a `shared struct`. The analyzer parallelizes
    // `let b = make_vec(...)` and `let l1 = from_arr(...)` (both
    // effectful, independent vars, no effect-resource conflict).
    // The body prints `node.val` for the surviving head node —
    // 7 if the RC transfer worked, 0 (or ASAN error) if not.
    // Inline match on `l1` rather than passing through a helper fn
    // taking `Option[shared T]` by value — kept as-is for historical
    // continuity; the helper-fn shape that previously hung is now
    // covered separately by
    // `asan_option_shared_chain_through_helper_fn` above.
    assert_clean_asan_run(
        r#"
shared struct Node {
    val: i64,
    mut next: Option[Node],
}

fn make_vec(n: u64) -> Vec[i64] {
    let mut v: Vec[i64] = Vec.new();
    let mut i = 0u64;
    while i < n {
        v.push(7);
        i = i + 1u64;
    }
    v
}

fn from_arr(arr: Slice[i64]) -> Option[Node] {
    let n = arr.len();
    if n == 0 {
        return None;
    }
    let head = Node { val: arr[0], next: None };
    let mut tail = head;
    for i in 1..n {
        let node = Node { val: arr[i], next: None };
        tail.next = Some(node);
        tail = node;
    }
    Some(head)
}

fn main() {
    let a = make_vec(5u64);
    let b = make_vec(5u64);
    let l1 = from_arr(a.as_slice());

    match l1 {
        Some(n) => println(n.val),
        None => println(-1i64),
    }
    println(b.len());
}
"#,
        &["7", "5"],
        "auto_par_shared_struct_option_return_slot",
    );
}

/// B-2026-08-26-26 — a fallible `try_*` companion consumed with `?` must be
/// SEEDED receiver-mutating, or the auto-parallelizer hoists it into a
/// worker function and the `?` early-return cannot compile there.
///
/// Found while landing `try_resize`/`try_append` (B-2026-08-26-22) but NOT
/// caused by them: verified on clean HEAD, with `try_push` — a companion
/// that has shipped for some time. The seed key every instance companion
/// shared, `__builtin_try_alloc`, matches neither `key == method` nor
/// `key.ends_with(".try_push")`, so
/// `method_effects_imply_receiver_mutation` read every one of them as
/// non-mutating. The analyzer then moved the call into a parallel worker,
/// which returns void, and the `?` failure path's `ret {i64, i64}` failed
/// module verification. `karac build` refused a program `--interp` runs
/// correctly.
///
/// THE SECOND `Vec` IS LOAD-BEARING, not scenery: it is what gives the
/// analyzer an independent group to hoist into. Without it the same
/// `try_push` calls compile fine, which is why the shipped feature went
/// this long without tripping over it.
///
/// AND THIS FIXTURE HAS TO LIVE HERE, for the same reason its
/// B-2026-08-25-20 neighbour below does: `run_program_capturing` passes
/// `None` for the concurrency analysis, so auto-par never runs under the
/// codegen E2E harness and no fixture there can reach this at all.
#[test]
fn asan_fallible_companions_are_seeded_against_the_auto_parallelizer() {
    assert_clean_asan_run(
        r#"
fn build() -> Result[i64, AllocError] {
    let mut v: Vec[i64] = Vec.new();
    v.try_push(1)?;
    v.try_push(2)?;
    v.try_reserve(16)?;
    v.try_resize(4, 9)?;
    let mut spare: Vec[i64] = Vec.new();
    spare.push(7);
    v.try_append(spare)?;
    let mut s: String = String.new();
    s.try_push_str("hi")?;
    let mut other: Vec[i64] = Vec.new();
    other.push(99);
    println(f"{v.len()} {v[3]} {v[4]} {other[0]} [{s}]");
    return Ok(v.len());
}
fn main() {
    match build() {
        Ok(n) => { println(f"ok {n}"); }
        Err(e) => { println("alloc failed"); }
    }
}
"#,
        &["5 9 7 99 [hi]", "ok 5"],
        "fallible_companions_seeded_against_auto_par",
    );
}

/// B-2026-08-25-20 — `Vec.resize` / `Vec.append` must be SEEDED
/// receiver-mutating in `effectchecker.rs`, or the auto-parallelizer races
/// them.
///
/// THIS FIXTURE LIVES HERE, NOT IN `tests/codegen.rs`, AND THAT IS THE
/// WHOLE REASON IT EXISTS. `run_program_capturing` passes `None` for the
/// concurrency analysis, so auto-par never runs under the codegen E2E
/// harness and NO fixture there can catch this class — the first draft of
/// this test was written in that file and passed against the unseeded
/// compiler. This harness passes `Some(&analysis)`, so it sees what
/// `karac build` sees.
///
/// The failure is not hypothetical and its shape is delicate: an
/// `a.append(b)` LATER in the body made an EARLIER `v[4]` read on an
/// UNRELATED Vec panic with `vec index out of bounds` at -O2, because
/// `method_effects_imply_receiver_mutation` saw `append` as read-only and
/// the analyzer co-grouped the statements around a stale branch-capture
/// `{ptr,len,cap}` header. Five index reads in one f-string is what makes
/// the group big enough to hoist — the trimmed-down version does not
/// reproduce, so do not simplify this program.
#[test]
fn asan_vec_resize_and_append_are_seeded_against_the_auto_parallelizer() {
    assert_clean_asan_run(
        r#"
fn main() {
    let mut v: Vec[i64] = Vec.new();
    v.push(1); v.push(2); v.push(3);
    v.resize(5, 9);
    println(f"grow: len {v.len()} [{v[0]},{v[1]},{v[2]},{v[3]},{v[4]}]");
    let mut a: Vec[i64] = Vec.new();
    a.push(10);
    let mut b: Vec[i64] = Vec.new();
    b.push(30);
    a.append(b);
    println(f"append: {a.len()} {a[0]} {a[1]}");
    let mut p: Vec[String] = Vec.new();
    p.push("one");
    let mut q: Vec[String] = Vec.new();
    q.push("two");
    p.append(q);
    p.resize(4, "pad");
    println(f"heap: {p.len()} {p[0]} {p[1]} {p[2]} {p[3]}");
}
"#,
        &[
            "grow: len 5 [1,2,3,9,9]",
            "append: 2 10 30",
            "heap: 4 one two pad pad",
        ],
        "vec_resize_and_append_seeded_against_auto_par",
    );
}

// ── Auto-par slot-ownership transfer (2026-06-05) ─────────────

/// The Map-handle slot-publication UAF: auto-par groups
/// `String.add` + `Map.new()`, the Map-producing branch writes the
/// handle into the parent's return slot, and pre-fix ALSO ran its
/// queued `FreeMapHandle` at branch end — the parent's `m.insert`
/// then operated on freed memory (SIGSEGV in release, UAF under
/// ASAN). Threads the full pipeline (ownership + concurrency) so
/// the auto-par lowering actually fires — the default harness's
/// `None, None` compile never reaches this code path.
#[test]
fn asan_auto_par_map_slot_published_handle_clean() {
    let label = "auto_par_map_slot_published_handle";
    if !asan_available() {
        eprintln!("[{label}] ASAN unavailable on this host — skipping");
        return;
    }
    let Some((stdout, status)) = run_under_asan_with_full_pipeline(
        r#"
fn main() {
    let name = "ka" + "ra";
    let mut m: Map[String, i64] = Map.new();
    m.insert("a", 1);
    m.insert("b", 2);
    let b = m.get("b");
    match b {
        Some(val) => println(val),
        None => println(0),
    }
    println(name);
}
"#,
        label,
    ) else {
        eprintln!("[{label}] setup failed — skipping");
        return;
    };
    assert!(
        status.success(),
        "[{label}] ASAN reported a memory error (exit code {:?}) — \
             look for heap-use-after-free on the slot-published Map handle",
        status.code()
    );
    assert_eq!(
        stdout.trim().lines().collect::<Vec<_>>(),
        vec!["2", "kara"],
        "[{label}] unexpected stdout (ASAN passed, output mismatched)"
    );
}

/// B-2026-07-22-9: a bare owned-heap USER-ENUM binding produced in a
/// par branch and then MOVED double-freed. Auto-par grouped the
/// String-payload `describe(Node.Ident(..))` sibling with the
/// `let a = mk_nums()` Vec-payload producer; the producer branch wrote
/// the `Node` back to the parent's return slot (a header bit-copy of
/// its `Nums(Vec)` payload), and the later `let c = a` move + scope-exit
/// drop then freed the Vec buffer twice. The move-hazard classifier only
/// tracked `String`/`Vec`/`Option`/`Result` bindings, never a user enum
/// that transitively owns heap, so neither the consumer guard
/// (B-2026-07-16-19) nor the new producer guard fired and the producer
/// stayed parallelized. Fixed by classifying a heap-owning user
/// enum/struct as a move-hazard AND de-parallelizing the producer of a
/// hazard consumed-by-move later. Interp was always correct; JIT + AOT
/// aborted. Loops 200× so any per-iteration double-free aborts under
/// ASAN and any leak accumulates for LSan. Threads the full pipeline so
/// auto-par actually fires (the `None` harness leaves it dormant).
#[test]
fn asan_auto_par_moved_heap_enum_producer_no_double_free() {
    let label = "auto_par_moved_heap_enum_producer";
    if !asan_available() {
        eprintln!("[{label}] ASAN unavailable on this host — skipping");
        return;
    }
    let Some((stdout, status)) = run_under_asan_with_full_pipeline(
        r#"
enum Node { Empty, Ident(String), Nums(Vec[i64]) }
fn describe(n: ref Node) -> String {
    match n {
        Ident(name) => { return "id ".to_string() + name; }
        Nums(v) => { return "nums ".to_string() + v.len().to_string(); }
        Empty => {}
    }
    return "empty".to_string();
}
fn mk_nums() -> Node {
    let mut v: Vec[i64] = Vec.new();
    v.push(3);
    return Node.Nums(v);
}
fn main() {
    let mut total = 0i64;
    let mut i = 0i64;
    while i < 200 {
        let d1 = describe(Node.Ident("foo".to_string()));
        total = total + d1.len();
        let a = mk_nums();
        let d2 = describe(a);
        total = total + d2.len();
        let c = a;
        let d3 = describe(c);
        total = total + d3.len();
        i = i + 1;
    }
    println(total);
}
"#,
        label,
    ) else {
        eprintln!("[{label}] setup failed — skipping");
        return;
    };
    assert!(
        status.success(),
        "[{label}] ASAN reported a memory error (exit code {:?}) — \
             the moved heap-enum par producer double-freed its Vec payload",
        status.code()
    );
    // d1="id foo"(6) + d2="nums 1"(6) + d3="nums 1"(6) = 18 per iter × 200.
    assert_eq!(
        stdout.trim(),
        "3600",
        "[{label}] unexpected stdout (ASAN passed, output mismatched)"
    );
}

/// A3a leak regression: two independent allocating calls (each builds a
/// fresh Vec) now AUTO-parallelize — `(Allocates,Allocates)` is no longer a
/// conflict. Each Vec is built in its own par branch, published to the
/// parent's slot, then MOVED into `sum` which owns and frees it at scope
/// exit. A wrongly-skipped or doubled free on the grouped branch's owned
/// buffer is exactly the leak/double-free class this gate exists for.
/// Threads the full pipeline (ownership + concurrency) so auto-par actually
/// fires — the default `None, None` harness leaves the grouping dead. Each
/// Vec carries 8 i64s (64 bytes), above the LSan reachability threshold, and
/// both are consumed (not live at exit), so a missing free is a detectable
/// non-reachable leak rather than one LSan masks.
#[test]
fn asan_par_ref_string_arg_network_call_no_double_free() {
    // A2b-2 variable-arg groundwork: a network call whose param is `ref
    // String` BORROWS its argument (no move), so lifting it into a par
    // branch must NOT drop the branch's view of the parent's owned `String`
    // — the parent stays the unique owner and frees each once. Uses an
    // explicit `par {}` (same capture machinery auto-par reuses) to prove
    // the borrow-capture is double-free-clean BEFORE the predicate is
    // relaxed to admit ref-param args to auto-par. Loop so any double-free
    // accumulates under ASan.
    assert_clean_asan_run(
        r#"
fn fetch(u: ref String) -> i64 with reads(Network) suspends { return u.len(); }
fn main() {
    let mut i: i64 = 0i64;
    while i < 3i64 {
        let a = "aaaaaaaaaaaaaaaaaaaa";
        let b = "bbbbbbbbbbbbbbbbbbbb";
        let r = par {
            let x = fetch(a);
            let y = fetch(b);
            x + y
        };
        println(r);
        i = i + 1;
    }
}
"#,
        &["40", "40", "40"],
        "asan_par_ref_string_arg_network_call_no_double_free",
    );
}

#[test]
fn asan_a2b2_autopar_ref_param_arg_no_double_free() {
    // A2b-2 variable-arg (end-to-end): two `reads(Network) suspends` calls
    // whose `ref String` param BORROWS an owned parent binding are now
    // grouped by AUTO-PAR (no explicit `par {}`) — the relaxed
    // `is_safe_network_fanout` admits identifier args at borrow positions.
    // The parent stays the unique owner of each `String`; the borrow into
    // the branch must not double-free. Pins the full admission path
    // (analysis groups -> codegen fans out), LSan/ASan-clean. Companion to
    // the explicit-`par {}` `asan_par_ref_string_arg_network_call_no_double_free`.
    assert_clean_asan_run(
        r#"
fn fetch(u: ref String) -> i64 with reads(Network) suspends { return u.len(); }
fn main() {
    let a = "aaaaaaaaaaaaaaaaaaaa";
    let b = "bbbbbbbbbbbbbbbbbbbb";
    let x = fetch(a);
    let y = fetch(b);
    println(x);
    println(y);
}
"#,
        &["20", "20"],
        "asan_a2b2_autopar_ref_param_arg_no_double_free",
    );
}

#[test]
fn asan_auto_par_network_effect_owned_heap_fanout_clean() {
    // A2b-2: two independent `reads(Network) suspends` fns returning owned
    // heap (String) are now grouped by auto-par via the arg-safe
    // network-fanout exemption (`is_safe_network_fanout`) and fanned out
    // through the return-slot move-only path. Each String must be freed
    // exactly once — the parent is the unique drop owner after the branch
    // bit-copies through its slot and the branch's own cleanup is
    // discarded. Distinct from `asan_auto_par_allocating_calls_clean`
    // (grouped via `allocates`): this exercises the `reads(Network)`-driven
    // admission end-to-end, so a leak/double-free surfaces under LSan/ASan.
    assert_clean_asan_run(
        r#"
fn fetch_a() -> String with reads(Network) suspends { return "aaaaaaaaaaaaaaaaaaaa"; }
fn fetch_b() -> String with reads(Network) suspends { return "bbbbbbbbbbbbbbbbbbbb"; }
fn main() {
    let x = fetch_a();
    let y = fetch_b();
    println(x);
    println(y);
}
"#,
        &["aaaaaaaaaaaaaaaaaaaa", "bbbbbbbbbbbbbbbbbbbb"],
        "asan_auto_par_network_effect_owned_heap_fanout_clean",
    );
}

#[test]
fn asan_a2b2_ephemeral_send_recv_owned_param_fanout_clean() {
    // A2b-2 Phase 1: two *ephemeral* network calls that `sends(Network)`
    // AND `receives(Network)` — the real `http_get` shape — with an OWNED
    // `String` param fed a literal arg. Before Phase 1 the send/recv
    // `Network` conflict kept this pair serial, so the fanned-out codegen
    // path was never reached for the send/recv shape; Phase 1's ephemeral
    // relaxation now groups it. Memory-safety proof: each coroutine takes
    // ownership of the moved-in `String` (a heap value materialized from
    // the literal) and returns it, so the value must be freed EXACTLY once
    // across the fork/join — it flows param → return-slot bit-copy → parent
    // (sole drop owner), with the branch's own cleanup discarded. A literal
    // arg names no parent binding, so there is no caller-side drop to
    // double-cancel (the coroutine-owned-param hazard cannot fire). A
    // double-free or leak surfaces under LSan/ASan. Companion to the
    // `reads(Network)` variant `asan_auto_par_network_effect_owned_heap_fanout_clean`,
    // which could not exercise the send/recv-conflict path.
    assert_clean_asan_run(
        r#"
fn get_a(u: String) -> String with sends(Network) receives(Network) { return u; }
fn get_b(u: String) -> String with sends(Network) receives(Network) { return u; }
fn main() {
    let x = get_a("aaaaaaaaaaaaaaaaaaaa");
    let y = get_b("bbbbbbbbbbbbbbbbbbbb");
    println(x);
    println(y);
}
"#,
        &["aaaaaaaaaaaaaaaaaaaa", "bbbbbbbbbbbbbbbbbbbb"],
        "asan_a2b2_ephemeral_send_recv_owned_param_fanout_clean",
    );
}

#[test]
fn asan_auto_par_allocating_calls_clean() {
    let label = "auto_par_allocating_calls";
    if !asan_available() {
        eprintln!("[{label}] ASAN unavailable on this host — skipping");
        return;
    }
    let Some((stdout, status)) = run_under_asan_with_full_pipeline(
        r#"
fn make(seed: i64) -> Vec[i64] {
    let mut v: Vec[i64] = Vec.new();
    let mut i = 0;
    while i < 8 {
        v.push(seed + i);
        i = i + 1;
    }
    return v;
}
fn sum(xs: Vec[i64]) -> i64 {
    let mut t = 0;
    let mut i = 0;
    while i < 8 {
        t = t + xs[i];
        i = i + 1;
    }
    return t;
}
fn main() {
    let a = make(100);
    let b = make(200);
    println(sum(a) + sum(b));
}
"#,
        label,
    ) else {
        eprintln!("[{label}] setup failed — skipping");
        return;
    };
    assert!(
        status.success(),
        "[{label}] ASAN reported a memory error (exit code {:?}) — \
             look for a LeakSanitizer report or double-free on a grouped \
             branch's owned Vec buffer",
        status.code()
    );
    // make(100)=sum 100..107=828; make(200)=sum 200..207=1628; total 2456.
    assert_eq!(
        stdout.trim().lines().collect::<Vec<_>>(),
        vec!["2456"],
        "[{label}] unexpected stdout (ASAN passed, output mismatched)"
    );
}

/// B-2026-07-03-32: the Column-handle slot-publication UAF. Auto-par
/// groups the `print(hd(av))` read and the `let c = Column.from_vec(…)`
/// producer into sibling par branches. The producing branch writes the
/// column control-block pointer into the parent's return slot, then
/// pre-fix ALSO ran its queued `FreeColumn` at branch end — freeing the
/// three buffers (data / null-bitmap / control) it had just published.
/// The parent's `c.len()` after the join then read a dangling control
/// block: `0` under `karac build` (correct `4` under `karac run` and
/// `KARAC_AUTO_PAR=0`), or an out-of-bounds panic on the first element
/// access — a SILENT wrong-output miscompile, the worst class. The fix
/// transfers `FreeColumn` (and its `DataFrame`/`Tensor` siblings) from
/// the branch to the parent via `SlotOwnership`, exactly like the
/// Map/Struct/SoA handles already were. Threads the full pipeline
/// (ownership + concurrency) so auto-par actually fires — the default
/// `None, None` harness leaves the grouping dead. The 4-element i64
/// column is 32 data bytes + a bitmap + a control block; a double-free
/// on the published control block trips ASAN, a skipped parent free is a
/// LeakSanitizer report on Linux.
#[test]
#[ignore = "B-2026-08-05-35 sweep: does not compile — codegen \"Index operator applied to non-array type\". Silently SKIPPED (reported ok) until the harness learned to fail on a codegen error; ignored so the gap is visible rather than green."]
fn asan_b32_auto_par_column_slot_published_handle_clean() {
    let label = "auto_par_column_slot_published_handle";
    if !asan_available() {
        eprintln!("[{label}] ASAN unavailable on this host — skipping");
        return;
    }
    let Some((stdout, status)) = run_under_asan_with_full_pipeline(
        r#"
fn hd(v: Vec[i64]) -> i64 { v[0] }
fn main() {
    let av: Vec[i64] = [4, 2, 7, 1];
    println(hd(av));
    let c: Column[i64] = Column.from_vec([5, 9, 3, 1]);
    println(c.len());
    println(c.iter_valid()[3]);
}
"#,
        label,
    ) else {
        eprintln!("[{label}] setup failed — skipping");
        return;
    };
    assert!(
        status.success(),
        "[{label}] ASAN reported a memory error (exit code {:?}) — \
             look for heap-use-after-free / double-free on the slot-published \
             Column control block, or a LeakSanitizer report on a skipped \
             parent free",
        status.code()
    );
    // hd([4,2,7,1])=4; from_vec([5,9,3,1]).len()=4; iter_valid()[3]=1.
    assert_eq!(
        stdout.trim().lines().collect::<Vec<_>>(),
        vec!["4", "4", "1"],
        "[{label}] unexpected stdout — a `0` for len (or a panic) is the \
             dangling-control-block miscompile this gate exists for"
    );
}

#[test]
fn asan_channel_send_recv_heap_payload_no_double_free() {
    // B-2026-07-13-16. `tx.send(v)` for an owned heap payload (`Vec`/`String`)
    // memcpy'd the value's `{ptr,len,cap}` header into the type-erased queue
    // but never neutralized the SOURCE binding's scope-exit free — so the
    // source `v` freed the buffer AND the `recv`'d binding freed the same
    // (aliased) buffer: `free(): double free detected in tcache 2` under
    // JIT/native (the interpreter moves the value into the channel, so it was
    // correct). A string LITERAL source stayed clean by luck (`cap == 0`).
    // `send` is now a MOVE — the source's Vec/String/Map/fstr cleanup is
    // suppressed, so the queue is the sole owner until `recv` transfers to the
    // receiver. This churns balanced send→recv→use of both a `Vec[i64]` and an
    // owned `String` (100 iters each): a missed suppression double-frees
    // (ASAN), a stray extra owner leaks (LSan).
    assert_clean_asan_run(
        r#"
fn main() {
    let mut i: i64 = 0;
    let mut total: i64 = 0;
    while i < 100 {
        let (vtx, vrx): (Sender[Vec[i64]], Receiver[Vec[i64]]) = Channel.new();
        let mut v: Vec[i64] = Vec.new();
        v.push(i);
        v.push(i + 1);
        vtx.send(v);
        let gv = vrx.recv();
        total = total + gv.len();
        let (stx, srx): (Sender[String], Receiver[String]) = Channel.new();
        let s = f"msg-{i}";
        stx.send(s);
        let gs = srx.recv();
        total = total + gs.len();
        i = i + 1;
    }
    println(total.to_string());
}
"#,
        // Vec len 2 × 100 = 200; String "msg-{i}" lengths: i=0..9 → 5 (×10=50),
        // i=10..99 → 6 (×90=540) = 590. Total = 790.
        &["790"],
        "channel_send_recv_heap_payload_no_double_free",
    );
}

#[test]
fn asan_channel_try_send_heap_payload_both_arms_no_double_free() {
    // B-2026-08-22-21. `try_send` has an ownership shape `send` does not:
    // the value leaves the caller on BOTH paths, but to two different
    // owners — into the queue when the send lands, into the `SendError`
    // payload handed back to the program when it does not. So the source's
    // scope-exit free must be suppressed either way (it is, via the same
    // suppressor set `send` uses), and exactly one owner must free.
    //
    // The pair is UNANNOTATED on purpose, and not only to exercise the
    // element-size pinning fix this row also made: an explicit
    // `(Sender[String], Receiver[String])` on `Channel.bounded(cap)` is
    // REJECTED today (`found '(Sender[?T0], Receiver[?T0])'`), which
    // B-2026-08-22-21 recorded as annotation-fighting-inference rather than
    // a gap in `bounded`.
    //
    // The FULL arm is the one worth an ASAN fixture: it is the path where
    // the value comes back out of the call, and getting it wrong is a
    // double free (source frees + the `SendError` binding frees the same
    // aliased buffer) rather than a wrong printed value. Both arms churn
    // 100 iterations on a capacity-1 channel so each iteration lands one
    // `Ok` and one `Full` with owned `String` payloads.
    assert_clean_asan_run(
        r#"
fn main() {
    let mut i: i64 = 0;
    let mut total: i64 = 0;
    while i < 100 {
        let (tx, rx) = Channel.bounded(1);
        let first = f"first-{i}";
        match tx.try_send(first) {
            Ok(u) => { total = total + 1; }
            Err(SendError.Full(v)) => { total = total + v.len(); }
            Err(SendError.Closed(v)) => { total = total + v.len(); }
        }
        let second = f"second-{i}";
        match tx.try_send(second) {
            Ok(u) => { total = total + 1; }
            Err(SendError.Full(v)) => { total = total + v.len(); }
            Err(SendError.Closed(v)) => { total = total + v.len(); }
        }
        let got = rx.recv();
        total = total + got.len();
        i = i + 1;
    }
    println(total.to_string());
}
"#,
        // Per iteration: first `try_send` lands (+1); second is rejected
        // Full, so `v` is "second-{i}" (len 8 for i=0..9, 9 for i=10..99);
        // `recv` drains "first-{i}" (len 7 / 8).
        // i=0..9   : 10 * (1 + 8 + 7)  = 160
        // i=10..99 : 90 * (1 + 9 + 8)  = 1620
        &["1780"],
        "channel_try_send_heap_payload_both_arms_no_double_free",
    );

    // A payload WIDER than the 3-word String/Vec floor: a four-`i64`
    // struct is 4 words. This pins the `SendError` payload area being
    // sized from the program's channel element types rather than fixed —
    // with a fixed floor the `Full` arm heap-boxes this struct into the
    // payload, and that box has no owner (the seeded layout's drop kind
    // for a bare `T` is `None`, and unlike a user generic enum there is no
    // monomorph to give it a concrete one). Measured on the 6-word
    // two-`String` version before the sizing landed: 4800 bytes leaked in
    // 100 objects, one box per rejected send.
    //
    // The payload here owns NO heap of its own, which is deliberate: the
    // struct-with-heap case leaks its own `String` fields on the reject
    // path for a separate reason the sizing does not touch, and that is
    // filed as its own row rather than folded in here. A plain `send` of
    // either struct was clean throughout, so neither is the transport.
    assert_clean_asan_run(
        r#"
struct Quad { a: i64, b: i64, c: i64, d: i64 }
fn main() {
    let mut i: i64 = 0;
    let mut total: i64 = 0;
    while i < 100 {
        let (tx, rx) = Channel.bounded(1);
        let x = Quad { a: i, b: i, c: i, d: i };
        let r1 = tx.try_send(x);
        let y = Quad { a: i, b: i, c: i, d: 7 };
        match tx.try_send(y) {
            Ok(u) => { total = total + 1; }
            Err(SendError.Full(v)) => { total = total + v.d; }
            Err(SendError.Closed(v)) => { total = total + v.a; }
        }
        let g = rx.recv();
        total = total + g.a;
        i = i + 1;
    }
    println(total.to_string());
}
"#,
        // Rejected `Quad.d` is 7 each (700); `recv` drains `a` = i (4950).
        &["5650"],
        "channel_try_send_wide_scalar_struct_payload_no_leak",
    );

    // B-2026-08-22-23 — A STRUCT PAYLOAD THAT OWNS HEAP, which is the case
    // where the reject path's ownership differs from `send`'s.
    //
    // Who owns a rejected value depends on the payload kind, and both
    // answers are the language's existing convention rather than anything
    // channel-specific: for a `String`/`Vec` the ARM BINDING takes it, so
    // the source must be disarmed on every path; for a struct the SOURCE
    // BINDING keeps it and the arm binding takes none. `send` never had to
    // choose — it always moves into the queue and has no reject path — so a
    // copy of its unconditional suppression disarms a struct source on a
    // path where nothing replaced it.
    //
    // Measured before the fix: 1480 bytes leaked in 200 objects, the two
    // `String` field buffers of each rejected `Big`. Measured while getting
    // it wrong in the other direction (disarming on neither path): a
    // double free on the `String` and `Vec` cases above. Both directions
    // are pinned here, so a future edit that collapses the split back into
    // one unconditional block fails on one case or the other.
    assert_clean_asan_run(
        r#"
struct Big { a: String, b: String }
fn main() {
    let mut i: i64 = 0;
    let mut total: i64 = 0;
    while i < 100 {
        let (tx, rx) = Channel.bounded(1);
        let x = Big { a: f"one-{i}", b: f"two-{i}" };
        let r0 = tx.try_send(x);
        let y = Big { a: f"three-{i}", b: f"four-{i}" };
        match tx.try_send(y) {
            Ok(u) => { total = total + 1; }
            Err(SendError.Full(v)) => { total = total + v.a.len() + v.b.len(); }
            Err(SendError.Closed(v)) => { total = total + v.a.len(); }
        }
        let g = rx.recv();
        total = total + g.a.len();
        i = i + 1;
    }
    println(total.to_string());
}
"#,
        // Rejected Big gives back "three-{i}" + "four-{i}"; recv drains
        // "one-{i}". i=0..9: (7+6)+5 = 18 each -> 180.
        // i=10..99: (8+7)+6 = 21 each -> 1890. Total 2070.
        &["2070"],
        "channel_try_send_struct_with_heap_payload_no_leak",
    );

    // The `Vec` payload, as the third kind and the other side of the
    // split: like `String` its arm binding owns the rejected value, so it
    // must be disarmed unconditionally. It is here because it is the case
    // that regressed to a double free when the struct fix was first applied
    // to every payload kind at once.
    assert_clean_asan_run(
        r#"
fn main() {
    let mut i: i64 = 0;
    let mut total: i64 = 0;
    while i < 100 {
        let (tx, rx) = Channel.bounded(1);
        let mut a: Vec[i64] = Vec.new();
        a.push(i);
        let r0 = tx.try_send(a);
        let mut b: Vec[i64] = Vec.new();
        b.push(i);
        b.push(i);
        match tx.try_send(b) {
            Ok(u) => { total = total + 1; }
            Err(SendError.Full(v)) => { total = total + v.len(); }
            Err(SendError.Closed(v)) => { total = total + v.len(); }
        }
        let g = rx.recv();
        total = total + g.len();
        i = i + 1;
    }
    println(total.to_string());
}
"#,
        // Rejected Vec has len 2, drained Vec has len 1 -> 3 per iter.
        &["300"],
        "channel_try_send_vec_payload_no_double_free",
    );

    // The `Closed` arm, with a struct-with-heap payload. Reachable only
    // since B-2026-08-22-24 gave the interpreter per-end liveness and put
    // the compiled receiver check back, so this ownership split had never
    // been measured on it — and it is the arm where a rejected value is
    // MOST likely to be dropped on the floor, because the channel it came
    // back from is dead.
    //
    // It rides the same reject-path rule as `Full`: the source binding
    // keeps ownership, so the disarm must not fire here either. That falls
    // out of placing the struct disarm on the Ok branch rather than on
    // "not Full", which is why the split is written the way it is.
    assert_clean_asan_run(
        r#"
struct Big { a: String, b: String }
fn orphan(seed: Big) -> Sender[Big] {
    let (tx, rx) = Channel.new();
    let t2 = tx.clone();
    t2.send(seed);
    return tx;
}
fn main() {
    let mut i: i64 = 0;
    let mut total: i64 = 0;
    while i < 100 {
        let s = orphan(Big { a: f"seed-{i}", b: f"sd-{i}" });
        let y = Big { a: f"three-{i}", b: f"four-{i}" };
        match s.try_send(y) {
            Ok(u) => { total = total + 1; }
            Err(SendError.Full(v)) => { total = total + v.a.len(); }
            Err(SendError.Closed(v)) => { total = total + v.a.len() + v.b.len(); }
        }
        i = i + 1;
    }
    println(total.to_string());
}
"#,
        // Every send is Closed (the receiver died with `orphan`'s frame),
        // giving back "three-{i}" + "four-{i}".
        // i=0..9: 7+6 = 13 each -> 130. i=10..99: 8+7 = 15 -> 1350. Total 1480.
        &["1480"],
        "channel_try_send_closed_arm_struct_payload_no_leak",
    );
}

/// DataFrame `column_names` / `select` heap lifecycle (phase-11 Arrow
/// Q6 codegen, slice 2c): `column_names` mallocs a fresh `Vec[String]`
/// whose elements are independent name copies (freed by the Vec's own
/// drop, never the frame's name buffers); `select` mallocs a fresh
/// frame holding column copies (its own `FreeDataFrame` drop). The
/// `cols` literal arg is freed by the caller's owned-temp drop (freeing
/// it in `select` would double-free). A missing free leaks (Linux
/// detect_leaks); a double free is caught everywhere.
#[test]
fn asan_dataframe_column_names_select_clean() {
    let label = "dataframe_names_select";
    if !asan_available() {
        eprintln!("[{label}] ASAN unavailable on this host — skipping");
        return;
    }
    let Some((stdout, status)) = run_under_asan(
        r#"
fn main() {
    let mut df: DataFrame = DataFrame.new();
    df.insert("a", Column.from_vec([1, 2]));
    df.insert("b", Column.from_vec([3, 4]));
    let names: Vec[String] = df.column_names();
    println(names.len());
    let sub: DataFrame = df.select(["b", "a"]);
    println(sub.width());
    let col: Column[i64] = sub.column("b");
    println(col.len());
    println(df.width());
}
"#,
        label,
    ) else {
        eprintln!("[{label}] setup failed — skipping");
        return;
    };
    assert!(
        status.success(),
        "[{label}] ASAN reported a memory error (exit code {:?}) — \
             check column_names Vec[String] drop + select fresh-frame drop",
        status.code()
    );
    assert_eq!(
        stdout.trim().lines().collect::<Vec<_>>(),
        vec!["2", "2", "2", "2"],
        "[{label}] unexpected stdout (ASAN passed, output mismatched)"
    );
}

#[test]
fn asan_channel_send_recv_clone_single_free() {
    // Phase 6 "Channel AOT codegen lowering": the refcount Drop
    // (`CleanupAction::DropChannelEnd`) must reclaim the channel exactly
    // once. `Channel.new()` mints refcount 2 (the destructured `tx`/`rx`),
    // `tx.clone()` increments to 3, and the three scope-exit drops bring
    // it to 0 — a single free of the `KaracChannel`. A miscount would
    // surface here as an ASAN double-free (over-drop) or, on Linux CI's
    // LeakSanitizer, a leak (under-drop). Run through the full pipeline
    // (concurrency on) so the `stmt_has_channel_op` auto-par exclusion is
    // exercised — without it the `send`/`recv` fan into branch workers and
    // the channel-end allocas land in a captured scope, which would also
    // trip ASAN. String payloads exercise the multi-word transfer too.
    assert_clean_asan_run_with_concurrency(
        r#"
fn main() {
    let (tx, rx): (Sender[String], Receiver[String]) = Channel.new();
    tx.send("first");
    let tx2 = tx.clone();
    tx2.send("second");
    println(rx.recv());
    println(rx.recv());
    match rx.try_recv() {
        Some(v) => println(v),
        None => println("drained"),
    }
}
"#,
        &["first", "second", "drained"],
        "channel_send_recv_clone_single_free",
    );
}

#[test]
fn asan_channel_move_into_spawn_single_free() {
    // Move-across-spawn: `tx` is captured into the spawned closure and
    // consumed by `worker(tx)`. The channel `new` mints refcount 2 (`tx`
    // / `rx`); `main` drops both at scope exit (the moved-in `tx` param
    // isn't a `bind_pattern` binding, so the worker registers no second
    // drop), balancing to a single free AFTER `h.join()` guarantees the
    // worker's `send` already ran (no use-after-free on the worker side,
    // no double-free on `main`'s). Verified leak-balanced at runtime
    // (1 alloc / 1 free); ASAN guards the double-free / UAF edges.
    assert_clean_asan_run_with_concurrency(
        r#"
fn worker(tx: Sender[i64]) -> i64 {
    tx.send(42);
    0
}
fn main() {
    let (tx, rx): (Sender[i64], Receiver[i64]) = Channel.new();
    let h: TaskHandle[i64] = spawn(|| worker(tx));
    h.join();
    println(rx.recv());
}
"#,
        &["42"],
        "channel_move_into_spawn_single_free",
    );
}

#[test]
fn asan_channel_producer_consumer_close_single_free() {
    // Cross-task sender-drop: the producer (spawned task) sends 2 values
    // then finishes — its moved `Sender` is dropped BY THE TASK (the
    // wrapper), which both closes the channel (terminating the consumer's
    // blocking `recv` drain) and releases exactly one reference. The
    // parent's drop of the moved `Sender` is suppressed, so the channel is
    // freed exactly once (no double-free here, no leak on Linux LSan) —
    // and the program terminates rather than deadlocking.
    assert_clean_asan_run_with_concurrency(
        r#"
fn producer(tx: Sender[i64]) -> i64 {
    tx.send(10);
    tx.send(20);
    0
}
fn consume(rx: Receiver[i64]) -> i64 {
    let mut sum = 0;
    let mut go = true;
    while go {
        let v = rx.recv();
        if v == 0 { go = false; } else { sum = sum + v; }
    }
    sum
}
fn main() {
    let (tx, rx): (Sender[i64], Receiver[i64]) = Channel.new();
    let h: TaskHandle[i64] = spawn(|| producer(tx));
    println(consume(rx));
    h.join();
}
"#,
        &["30"],
        "channel_producer_consumer_close_single_free",
    );
}

#[test]
fn asan_channel_end_returned_from_fn_single_free() {
    // Regression for the channel-end MOVE-OUT-ON-RETURN double-drop
    // (recv-out-slot-read-race root cause): a factory `fn mk() ->
    // Receiver[T] { let (tx, rx) = Channel.new(); ...; rx }` returns the
    // `Receiver` as its tail expression — moving it into the caller's
    // binding. `bind_pattern` queues a `DropChannelEnd` for `rx` at the
    // destructure site; without move-out suppression at the return, that
    // drop fires at `mk`'s scope exit (decrementing the channel's `total`)
    // AND again when the caller's `r` goes out of scope — a double-drop
    // that frees the `KaracChannel` early. Here `mk` keeps a cloned sender
    // alive across the return (mirroring the host-async `pointer_moves()`
    // shape, where the host owns a clone), so the over-drop frees the
    // channel while a live sender reference still points at it: ASAN flags
    // the heap-use-after-free / double-free. With the fix, `mk` drops only
    // its local `tx`, the caller's `r` drops `rx` once, and `s` (the
    // surviving clone) drops last → exactly one free. Covers the tail-
    // expression return; a sibling `return rx;` form is exercised below.
    assert_clean_asan_run(
        r#"
fn mk() -> Receiver[i64] {
    let (tx, rx): (Sender[i64], Receiver[i64]) = Channel.new();
    let keep = tx.clone();
    keep.send(7);
    tx.send(9);
    rx
}
fn main() {
    let r = mk();
    println(r.recv());
    println(r.recv());
}
"#,
        &["7", "9"],
        "channel_end_returned_from_fn_single_free",
    );
}

#[test]
fn asan_channel_end_explicit_return_single_free() {
    // Sibling of `asan_channel_end_returned_from_fn_single_free` for the
    // explicit `return rx;` shape (vs the tail-expression form). Same
    // move-out-on-return double-drop class; the suppression lives in the
    // `ExprKind::Return` arm. `cond` is always true so `mk` always returns
    // `rx` (the moved-out end) — a single free with the fix, an ASAN
    // double-free without it.
    //
    // RETURNS THE RECEIVER, matching this comment and the sibling above.
    // It used to return the SENDER and then `s.send(22)` in `main`, on a
    // channel whose receiver had already died with `mk`'s frame — which
    // B-2026-08-22-24 makes panic, per design.md's "`send` panics if all
    // receivers are dropped". That send was incidental to the drop-
    // suppression this test exists for, and the fixture had drifted from
    // its own description; both sends now happen while `rx` is alive.
    assert_clean_asan_run(
        r#"
fn mk(cond: bool) -> Receiver[i64] {
    let (tx, rx): (Sender[i64], Receiver[i64]) = Channel.new();
    tx.send(11);
    tx.send(22);
    if cond {
        return rx;
    }
    rx
}
fn main() {
    let r = mk(true);
    println(r.recv());
    println(r.recv());
}
"#,
        &["11", "22"],
        "channel_end_explicit_return_single_free",
    );
}

#[test]
fn asan_channel_end_let_rebind_single_free() {
    // Regression for the channel-end LET-REBIND move double-drop — the
    // let-binding sibling of `asan_channel_end_returned_from_fn_single_free`
    // (which covers the tail-expression return) and
    // `asan_channel_end_explicit_return_single_free` (the `return rx;` form).
    // Here the `Receiver` is moved into a NEW `let` binding first
    // (`let keep = rx;`), then that rebind is returned. The destructure
    // queues a `DropChannelEnd` for `rx`; `bind_pattern` queues a SECOND for
    // `keep`. Without move-suppression at the let-rebind, BOTH fire — `rx`'s
    // at `mk`'s scope exit AND the caller's `r` at `main`'s — double-dropping
    // the channel's refcount and freeing the `KaracChannel` early while the
    // caller still reads from it: ASAN flags the heap-use-after-free /
    // double-free. With the fix the source `rx`'s `DropChannelEnd` is
    // suppressed, `keep` carries the single live drop (itself suppressed at
    // the return as the new owner moves to `main`), and `main`'s `r` frees
    // exactly once. The two buffered sends arrive in order → "7","9".
    assert_clean_asan_run(
        r#"
fn mk() -> Receiver[i64] {
    let (tx, rx): (Sender[i64], Receiver[i64]) = Channel.new();
    let keep = rx;
    tx.send(7);
    tx.send(9);
    keep
}
fn main() {
    let r = mk();
    println(r.recv());
    println(r.recv());
}
"#,
        &["7", "9"],
        "channel_end_let_rebind_single_free",
    );
}

#[test]
fn asan_channel_end_let_rebind_branch_buried_no_leak() {
    // Guards the BRANCH-BURIED corner of the let-rebind suppression: only
    // ONE arm rebinds the channel end (`if cond { let keep = rx; ... }`),
    // while the OTHER arm keeps using the source `rx`. A compile-time
    // retraction of `rx`'s `DropChannelEnd` (the terminal-site
    // `suppress_channel_drop_for_var`) would remove it unconditionally, so
    // the non-rebinding `else` path would never drop `rx` and leak the
    // `KaracChannel` (`total` stuck at 1) — caught by LeakSanitizer on Linux.
    // The branch-safe in-slot null sentinel
    // (`neutralize_moved_channel_end_slot`) only neutralizes `rx` on the path
    // that actually executes the move, so BOTH arms free exactly once: no
    // leak, no double-free. `cond` is exercised both ways from `main`.
    // (On macOS — no LSan — this asserts the no-double-free / no-UAF half;
    // the Linux LSan gate asserts the no-leak half.)
    assert_clean_asan_run(
        r#"
fn pick(cond: bool) -> i64 {
    let (tx, rx): (Sender[i64], Receiver[i64]) = Channel.new();
    tx.send(100);
    if cond {
        let keep = rx;
        keep.recv()
    } else {
        rx.recv()
    }
}
fn main() {
    let a = pick(true);
    let b = pick(false);
    println(a + b);
}
"#,
        &["200"],
        "channel_end_let_rebind_branch_buried_no_leak",
    );
}

#[test]
fn asan_discarded_taskgroup_spawn_loop_eager_reap_no_double_free() {
    // B-2026-06-17-2 — the canonical server shape `loop { tg.spawn(|| …) }`
    // discards each child's `TaskHandle`. Codegen now marks the discarded
    // handle detached (`karac_runtime_task_detach`), and the runtime
    // eager-reaps detached, completed children inside
    // `karac_runtime_taskgroup_register`'s sweep — bounding the group's
    // `children` Vec instead of leaking ~100 B/conn unbounded.
    //
    // This E2E drives the FULL path (codegen detach emission + register-time
    // sweep + scope-exit `join_and_free`) under ASAN/LSan. Its job is to
    // pin the UAF-prone hazard the spike flagged: the sweep and the
    // scope-exit join must never both free the same child (double-free), and
    // the sweep's terminal-peek must never free a still-running child (UAF).
    // The `join_and_free` barrier at the group's scope exit makes the run
    // deterministic — every child is reclaimed before exit, so Linux LSan
    // also confirms no leak. (The fails-before-fix leak *regression* lives in
    // the runtime unit test `taskgroup_register_reaps_detached_completed_
    // children`, which asserts the Vec stays bounded; an at-exit LSan check
    // can't catch the leak because `join_and_free` reclaims everything when
    // a finite scope exits.)
    assert_clean_asan_run_with_concurrency(
        r#"
fn work(n: i64) -> i64 {
    n + 1
}
fn main() {
    let mut tg = TaskGroup.new();
    let mut i = 0;
    while i < 2000 {
        let c = i;
        tg.spawn(|| work(c));
        i = i + 1;
    }
    println(0);
}
"#,
        &["0"],
        "discarded_taskgroup_spawn_loop_eager_reap_no_double_free",
    );
}

#[test]
fn asan_bounded_channel_scope_exit_single_free() {
    // `BoundedChannel.new` allocates a runtime queue; the `BoundedChannel`
    // Drop frees it (and any undrained payloads) exactly once at scope
    // exit. String elements exercise the heap-payload copy path (the
    // queue owns the byte blobs; the source String's own drop is
    // independent). ASAN proves: no leak (queue + undrained "world" blob
    // freed), no double-free (single-owner, no refcount).
    assert_clean_asan_run(
        r#"
fn main() {
    let bc: BoundedChannel[String] = BoundedChannel.new(2, OnFull.FailFast);
    match bc.send("hello") { Ok(_) => println(1), Err(_) => println(0), }
    match bc.send("world") { Ok(_) => println(1), Err(_) => println(0), }
    match bc.recv() { Some(s) => println(s), None => println("none"), }
    // "world" left undrained — its blob is freed by the channel's Drop.
}
"#,
        &["1", "1", "hello"],
        "bounded_channel_scope_exit_single_free",
    );
}

// ── Cross-task shared (aliased) heap capture — read-only ───────
//
// The companion to the move cases above: here ONE heap buffer is captured
// by MULTIPLE sibling tasks that only *read* it (the closures pass it to a
// `ref` param), and the parent keeps owning it. This is the canonical
// parallel-stencil fan-out — split a shared input grid into bands, one task
// per band — and the shape the Slipstream LBM dogfood (examples/slipstream)
// drove out. Before the capture-mode fix in `codegen/task_group.rs`, the
// spawn lowering treated EVERY capture as a move: each task re-registered a
// free of the shared buffer and the parent's free was suppressed, so N
// tasks freed the one buffer N times — a double-free / use-after-free that
// produced wrong sums and an allocator "failed to lock mutex" abort. The
// fix: a borrowed capture stays owned by the parent (freed once after the
// join barrier, the same `Copy`-capture rule a `par {}` branch uses), so
// the buffer is freed exactly once. This asserts BOTH value-correctness
// (the band sums total 4950 = sum 0..99 — a miscompiled shared read returns
// garbage) AND ASAN/LSan cleanliness (no double-free, no leak). The Vec is
// 100×i64 = 800 bytes, well past any allocator-freelist threshold, so a
// leak regression surfaces on LSan.
#[test]
fn asan_taskgroup_spawn_shared_vec_read_across_tasks_single_free() {
    assert_clean_asan_run(
        r#"
fn band_sum(data: ref Vec[i64], lo: i64, hi: i64) -> i64 {
    let mut acc = 0;
    let mut i = lo;
    while i < hi { acc = acc + data[i]; i = i + 1; }
    acc
}
fn main() with panics {
    let mut data: Vec[i64] = Vec.new();
    let mut i = 0;
    while i < 100 { data.push(i); i = i + 1; }
    let mut pool: TaskGroup = TaskGroup.new();
    let mut handles: Vec[TaskHandle[i64]] = Vec.new();
    let mut k = 0;
    while k < 4 {
        let lo = k * 25;
        let hi = lo + 25;
        handles.push(pool.spawn(|| band_sum(data, lo, hi)));
        k = k + 1;
    }
    let mut total = 0;
    for h in handles { total = total + h.join(); }
    if total == 4950 { println("total 4950"); } else { println("WRONG"); }
}
"#,
        &["total 4950"],
        "taskgroup_spawn_shared_vec_read_across_tasks",
    );
}

// String variant of the shared read-only capture: three tasks each borrow
// the same captured `String` (a `ref` param compare). Same double-free class
// as the Vec case (the `{data,len,cap}` header is shared), and the same
// single-free expectation. Payload is >= 40 bytes so an LSan leak regression
// clears the reachable-short-String freelist threshold
// (`lsan-reachability-short-string-leaks`).
#[test]
fn asan_taskgroup_spawn_shared_string_read_across_tasks_single_free() {
    assert_clean_asan_run(
        r#"
fn match_addr(addr: ref String) -> i64 {
    if addr == "relay-upstream-host-127.0.0.1-port-9000-ok" { 1 } else { 0 }
}
fn main() with panics {
    let addr: String = "relay-upstream-host-127.0.0.1-port-9000-ok";
    let mut pool: TaskGroup = TaskGroup.new();
    let mut handles: Vec[TaskHandle[i64]] = Vec.new();
    let mut k = 0;
    while k < 3 {
        handles.push(pool.spawn(|| match_addr(addr)));
        k = k + 1;
    }
    let mut hits = 0;
    for h in handles { hits = hits + h.join(); }
    if hits == 3 { println("hits 3"); } else { println("WRONG"); }
}
"#,
        &["hits 3"],
        "taskgroup_spawn_shared_string_read_across_tasks",
    );
}

// ── Relay per-request parse + atomic soak ─────────────────────
//
// High-volume leak stress for the exact allocation churn a Relay
// (`examples/relay`) connection handler runs per request: the
// request-line peek `Vec.from_slice(buf[0..n])` -> `String.from_utf8`
// -> `.split(' ')` -> `parts[i].clone()`, plus the shared-`Metrics`
// `Atomic[i64].fetch_add`/`.load`. Each of those is a heap allocation
// (Vec[u8], String, Vec[String] + per-part Strings, the cloned path),
// and every iteration must reclaim ALL of them — a single missing free
// in any drop path compounds 4000x and LSan (Linux CI / `scripts/
// lsan-local.sh`) flags it. This is the steady-state proxy loop in a
// bottle: no networking, just the parse/atomic allocators on repeat.
//
// Payloads are deliberately >= 40 bytes: LSan misses *reachable*
// short-String leaks (the small-string buffer can stay pinned by an
// allocator freelist), so a leak regression only surfaces with a
// payload past the small-string threshold — see the user-memory note
// `lsan-reachability-short-string-leaks`. The buffer plants two spaces
// so `split(' ')` yields a >= 40-byte middle token that `clone()`
// returns, keeping the leak-candidate object well past the threshold.
//
// The fd path (`connect_start`/`connect_finish` close-on-failure) is
// intentionally NOT soaked here: it needs a live reactor to drive the
// write-readiness park, which a bare ASAN `main` has no harness for.
// That path is covered by the relay E2E under a real reactor and the
// existing tcp/park ASAN cases.
#[test]
fn asan_relay_request_parse_atomic_soak_no_leak() {
    assert_clean_asan_run(
        r#"
par struct Counters {
    n: Atomic[i64],
}
fn request_path(bytes: Vec[u8]) -> String {
    match String.from_utf8(bytes) {
        Result.Ok(line) => {
            let parts = line.split(' ');
            if parts.len() >= 2 {
                return parts[1].clone();
            }
            return "/";
        }
        Result.Err(_) => {
            return "/";
        }
    }
}
fn main() {
    // 48 'A' (0x41, valid UTF-8); spaces at 3 and 44 split it into
    // ["AAA", <40-byte middle>, "AAA"] — parts[1] is the >= 36-byte
    // leak-candidate the LSan short-string blind spot needs.
    let mut buf: Array[u8, 48] = [65u8; 48];
    buf[3] = 32u8;
    buf[44] = 32u8;
    let counters = Counters { n: Atomic.new(0) };
    let mut i: i64 = 0;
    loop {
        if i >= 4000 { break; }
        let bytes: Vec[u8] = Vec.from_slice(buf[0..48]);
        let path = request_path(bytes);
        if path.len() >= 40 {
            let _ = counters.n.fetch_add(1, MemoryOrdering.Relaxed);
        }
        i = i + 1;
    }
    println(counters.n.load(MemoryOrdering.Relaxed));
}
"#,
        &["4000"],
        "relay_request_parse_atomic_soak",
    );
}

#[test]
fn asan_parvec_autopar_slot_keeps_elem_agg_drop() {
    // B-2026-07-02-4: the auto-par dispatch's parent-side re-track
    // used plain one-level `track_vec_var`, DOWNGRADING a
    // `Vec[Vec[String]]` slot's cleanup — every nested string leaked
    // whenever the statement shape parallelized (KARAC_AUTO_PAR=0 was
    // clean, which is how the class masqueraded as "index-read
    // leaks"). This program's independent-lets shape triggers
    // auto-par grouping; the re-track now mirrors the LET-site
    // agg/map/tensor element dispatch. NOTE: leak tests must include
    // auto-par-TRIGGERING shapes — loop-based builders serialize and
    // never covered this path.
    assert_clean_asan_run(
        r#"
fn main() {
    let mut outer: Vec[Vec[String]] = Vec.new();
    let mut a: Vec[String] = Vec.new();
    a.push(f"payload padded beyond thirty-six bytes {1}");
    outer.push(a);
    println(outer.len());
}
"#,
        &["1"],
        "parvec_autopar_slot_keeps_elem_agg_drop",
    );
}

#[test]
fn asan_parvec_autopar_for_loop_after_crossing() {
    // B-2026-07-02-4: the full-content variant — three rows crossing
    // the auto-par boundary then iterated (192 bytes leaked pre-fix).
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
    for row in outer {
        println(row.len());
    }
}
"#,
        &["2", "1", "0"],
        "parvec_autopar_for_loop_after_crossing",
    );
}

#[test]
fn asan_parvec_explicit_par_join_slots_freed() {
    // B-2026-07-02-4 explicit-par sibling: `par { let x = build(1);
    // let y = build(2); x.len() + y.len() }` — Step 6 bound the Vec
    // slots into the parent with NO cleanup at all (544 bytes/call
    // pre-fix). The same rich element dispatch now registers there.
    assert_clean_asan_run(
        r#"
fn build(n: i64) -> Vec[Vec[String]] {
    let mut outer: Vec[Vec[String]] = Vec.new();
    let mut a: Vec[String] = Vec.new();
    a.push(f"payload padded beyond thirty-six bytes {n}");
    outer.push(a);
    outer
}
fn main() {
    let total = par {
        let x = build(1);
        let y = build(2);
        x.len() + y.len()
    };
    println(total);
}
"#,
        &["2"],
        "parvec_explicit_par_join_slots_freed",
    );
}

#[test]
fn asan_channel_sent_unreceived_heap_payload_no_leak() {
    // B-2026-07-13-17: a heap payload SENT on a channel but never RECEIVED
    // has no owner to free it (send moves it into the queue; the source's
    // free is suppressed). The channel destructor now drains any still-queued
    // payloads through the element's drop fn. A Vec sent and never recv'd,
    // then the channel drops at scope exit — must be leak-clean.
    assert_clean_asan_run(
        r#"
fn main() {
    let (tx, rx): (Sender[Vec[i64]], Receiver[Vec[i64]]) = Channel.new();
    let mut v = Vec.new();
    v.push(1);
    v.push(2);
    tx.send(v);
    println("sent");
}
"#,
        &["sent"],
        "channel_sent_unreceived_heap_payload_no_leak",
    );
}

#[test]
fn asan_channel_balanced_send_recv_no_double_free() {
    // The dual guard: a RECEIVED payload is owned by its receiver binding and
    // must NOT also be freed by the channel destructor (the received blob was
    // dequeued, so it is not on the queue at drop). Balanced send→recv over a
    // Vec payload, plus a second sent-but-unreceived Vec that the destructor
    // drains — exercises both halves in one program, leak- and
    // double-free-clean.
    assert_clean_asan_run(
        r#"
fn main() {
    let (tx, rx): (Sender[Vec[i64]], Receiver[Vec[i64]]) = Channel.new();
    let mut a = Vec.new();
    a.push(1);
    let mut b = Vec.new();
    b.push(2);
    b.push(3);
    tx.send(a);
    tx.send(b);
    let got = rx.recv();
    println(f"got len: {got.len()}");
}
"#,
        &["got len: 1"],
        "channel_balanced_send_recv_no_double_free",
    );
}

#[test]
fn asan_auto_par_consuming_match_on_option_string_no_double_free() {
    // B-2026-07-16-19: a fn returning `Option[String]` built from a MOVED
    // Vec element (`Some(words[0])` after `split`), called twice in a main
    // whose statement mix auto-parallelizes. Pre-fix the `match r` stmt was
    // lifted into a par-branch worker: the worker's match moved the payload
    // out of its bit-copied env alloca and freed it, then the parent's
    // scope-exit `FreeInlineOptionPayload` freed the same buffer again
    // (JIT: "free(): double free"; native: valgrind Invalid free under
    // karac_par_run). The analyzer's move-hazard gate now keeps the
    // consuming match sequential.
    assert_clean_asan_run(
        r#"
fn first_word(s: String) -> Option[String] {
    let words = s.split(" ");
    if words.len() > 0 { Some(words[0]) } else { None }
}
fn main() {
    let r = first_word("hello world");
    match r {
        Some(w) => println(w),
        None => println("e"),
    };
    let e = first_word("");
    println(e.unwrap_or("none").len());
}
"#,
        &["hello", "0"],
        "auto_par_consuming_match_on_option_string_no_double_free",
    );
}

#[test]
fn asan_auto_par_published_option_string_slots_no_double_free_no_leak() {
    // B-2026-07-16-19 (publish leg): two Option[String]-returning calls
    // with literal args STILL auto-parallelize (nothing consumed — the
    // bindings are published through par return slots). Pre-fix each
    // branch freed the payload it had just published (the branch-exit
    // suppression loop knew Vec / RC / Map / .. shapes but not
    // `FreeInlineOptionPayload`), so the parent's `unwrap_or` consumed
    // freed memory and freed it again. Post-fix the branch tag-sentinels
    // its action and the parent re-registers the payload free against its
    // rebind alloca — exactly one owner. LSan (Linux CI) also guards the
    // no-leak half: a dropped re-registration would leak both payloads.
    assert_clean_asan_run(
        r#"
fn first_word(s: String) -> Option[String] {
    let words = s.split(" ");
    if words.len() > 0 { Some(words[0]) } else { None }
}
fn main() {
    let a = first_word("aaaaaaaaaaaaaaaaaaaaaaaaaaaaa bb");
    let b = first_word("ccccccccccccccccccccccccccccc dd");
    println(a.unwrap_or("x").len());
    println(b.unwrap_or("y").len());
}
"#,
        &["29", "29"],
        "auto_par_published_option_string_slots_no_double_free_no_leak",
    );
}

#[test]
fn asan_direct_vec_iterator_terminals_and_string_join_no_leak() {
    // B-2026-07-16-14: the direct-on-Vec iterator terminals — `v.sum()` /
    // `.product()` / `.max()` / `.min()` (lowering-desugared to the
    // `.iter()` chain; max/min via the synthetic-reduce codegen) — and the
    // Vec[String] `join(sep)` / `concat()` methods (karac_string_join
    // walks the element triples READ-ONLY; the vector keeps ownership,
    // the result is a fresh owned buffer freed once at scope exit).
    // Pre-fix all six passed `karac check` via the silent unknown-method
    // leniency and trapped on every backend. Edge coverage: empty vec
    // (sum 0 / max None / join "") and the positional join of an empty
    // first element ("|x" — the separator is structural, not
    // emptiness-gated). LSan guards the join/concat result + separator
    // temp ownership.
    //
    // B-2026-08-11-15 moved the float leg from `Vec[f64]` to `Vec[F64]`:
    // `max`/`min` on an IEEE float are now gated (the answer is
    // order-dependent with a NaN present), so this fixture keeps its
    // memory-safety coverage on the path users are actually directed to.
    // That is strictly MORE for ASAN than the old spelling, not less — the
    // element is a struct rather than a scalar, and the `unwrap_or`
    // default is now a constructed `F64` temporary whose ownership LSan
    // also has to see balanced.
    assert_clean_asan_run(
        r#"
fn main() {
    let v: Vec[i64] = [10, 20, 30, 40];
    println(v.sum());
    println(v.product());
    println(v.max().unwrap_or(0));
    println(v.min().unwrap_or(0));
    let e: Vec[i64] = [];
    println(e.sum());
    println(e.max().unwrap_or(-1));
    let s: Vec[String] = ["alpha-heap-sized-string-payload", "beta", "gamma"];
    println(s.join("-").len());
    println(s.concat().len());
    let tricky: Vec[String] = ["", "x"];
    println(tricky.join("|"));
    let none: Vec[String] = [];
    println(none.join(",").len());
    let fs: Vec[f64] = [1.5, 2.5, 0.5];
    let fw: Vec[F64] = fs.iter().map(F64.from).collect();
    let fmax: F64 = fw.max().unwrap_or(F64.from(0.0));
    let fmin: F64 = fw.min().unwrap_or(F64.from(0.0));
    println(fmax.value);
    println(fmin.value);
}
"#,
        &[
            "100", "240000", "40", "10", "0", "-1", "42", "40", "|x", "0", "2.5", "0.5",
        ],
        "direct_vec_iterator_terminals_and_string_join_no_leak",
    );
}

/// B-2026-07-30-11 (boxed-payload bodies) — the double-free-direction
/// gate for the arm-channel Drop routing over heap-BOXED Option
/// payloads: `while let / if let / match Some(r) = v.pop()` where the
/// payload struct exceeds the inline area. The box drop owns the
/// interior; the bodies-only walker must free nothing (the naive
/// wrapper routing crashed with `free(): double free` at all three
/// sites), and a borrow accessor's aliased payload must register no
/// owned track.
#[test]
fn asan_arm_channel_boxed_payload_freed_once() {
    assert_clean_asan_run(
        r#"
struct G { id: i64, s: String }
impl Drop for G {
    fn drop(mut ref self) {
        if self.id < 0i64 { println(self.id); }
    }
}
fn main() {
    let mut it = 0i64;
    while it < 200i64 {
        let mut v: Vec[G] = Vec.new();
        v.push(G { id: it, s: f"one-{it}" });
        v.push(G { id: it + 1i64, s: f"two-{it}" });
        while let Option.Some(r) = v.pop() {
            let _ = r.s.len();
        }
        let mut w: Vec[G] = Vec.new();
        w.push(G { id: it, s: f"three-{it}" });
        if let Option.Some(r) = w.pop() {
            let _ = r.s.len();
        }
        let mut u: Vec[G] = Vec.new();
        u.push(G { id: it, s: f"four-{it}" });
        match u.pop() {
            Option.Some(r) => { let _ = r.s.len(); }
            Option.None => {}
        }
        let mut m: Map[i64, G] = Map.new();
        m.insert(1i64, G { id: it, s: f"five-{it}" });
        if let Option.Some(r) = m.get(1i64) {
            let _ = r.s.len();
        }
        it = it + 1i64;
    }
    println("done");
}
"#,
        &["done"],
        "arm_channel_boxed_payload_freed_once",
    );
}

/// B-2026-07-31-43 — `len`/`is_empty` on a FRESH-TEMP `Vec` receiver
/// with heap-bearing elements frees the elements, not just the outer
/// buffer. The parser gives a MethodCall its receiver's span, so the
/// chain's scalar result span-clobbers the receiver's `Vec[T]` in
/// `expr_types` and the intercept's `owned_temp_drops` hint was always
/// absent — the drop-track degraded to an outer-buffer-only free and
/// every element String (`Env.args().len()`: the argv strings;
/// `mk(n).len()`: each runtime-built element) or struct-element heap
/// field leaked, once per evaluation — unbounded in the loop below.
/// Now the typechecker records the element type in the dedicated
/// `temp_recv_len_elem_types` table and the intercept walks the
/// elements (String via the `FreeVecBuffer` vec-struct recursion, user
/// structs via the per-element `__karac_drop_<S>`).
#[test]
fn asan_len_family_fresh_recv_elements_freed() {
    assert_clean_asan_run(
        r#"
struct Rec { id: i64, s: String }
fn mk(n: i64) -> Vec[String] {
    let mut v: Vec[String] = Vec.new();
    let mut i = 0i64;
    while i < n {
        v.push(f"item-{i}-payload");
        i = i + 1;
    }
    v
}
fn mk_recs(n: i64) -> Vec[Rec] {
    let mut v: Vec[Rec] = Vec.new();
    let mut i = 0i64;
    while i < n {
        v.push(Rec { id: i, s: f"rec-{i}-payload" });
        i = i + 1;
    }
    v
}
fn main() {
    let a = Env.args().len();
    let mut total = 0i64;
    let mut it = 0i64;
    while it < 200i64 {
        total = total + mk(3i64).len();
        if mk(2i64).is_empty() { total = total - 100i64; }
        total = total + mk_recs(2i64).len();
        it = it + 1i64;
    }
    println(f"{a >= 1i64} {total}");
}
"#,
        &["true 1000"],
        "len_family_fresh_recv_elements_freed",
    );
}

// ── Auto-par indexed-write fan-out (disjoint-writes lowering) ─────

#[test]
fn asan_disjoint_fanout_worker_with_per_iteration_heap_is_clean() {
    // The fan-out worker pushes a per-iteration cleanup frame so a
    // body-local `Vec` drops each iteration rather than accumulating for
    // the worker's whole chunk. Without that frame every iteration's buffer
    // survives to the join — a leak that scales with the chunk size and
    // that only LeakSanitizer (Linux CI, not macOS) reports.
    //
    // The digest read-back also proves the temporaries were still alive
    // when read: a frame drained one statement too early would be a
    // use-after-free here, which ASAN reports on every host.
    if !asan_available() {
        eprintln!("skipping: ASAN unavailable");
        return;
    }
    let src = concat!(
        "fn heavy(v: i64) -> i64 {\n",
        "    let mut acc: i64 = v % 1000003;\n",
        "    let mut t: i64 = 0;\n",
        "    while t < 200 { acc = (acc * 1103515245 + 12345) % 2147483647; t = t + 1; }\n",
        "    acc\n",
        "}\n",
        "fn kernel(h: i64, w: i64, out: mut Slice[i64]) {\n",
        "    for y in 0..h {\n",
        "        let mut tmp: Vec[i64] = Vec.new();\n",
        "        let mut x: i64 = 0;\n",
        "        while x < w { tmp.push(heavy(y * 11 + x)); x = x + 1; }\n",
        "        let mut j: i64 = 0;\n",
        "        while j < w { out[y * w + j] = tmp[j]; j = j + 1; }\n",
        "    }\n",
        "}\n",
        "fn main() {\n",
        "    let h: i64 = 64;\n",
        "    let w: i64 = 20;\n",
        "    let mut buf: Vec[i64] = Vec.filled(h * w, 0);\n",
        "    kernel(h, w, mut buf);\n",
        "    let mut d: i64 = 0;\n",
        "    let mut i: i64 = 0;\n",
        "    while i < h * w { d = (d * 131 + buf[i]) % 1000000007; i = i + 1; }\n",
        "    println(f\"{d}\");\n",
        "}\n",
    );
    let Some((out, status)) =
        run_under_asan_with_full_pipeline(src, "disjoint_fanout_per_iteration_heap")
    else {
        return;
    };
    assert!(
        status.success(),
        "ASAN reported a problem in a disjoint-write fan-out worker:\n{out}"
    );
}

/// B-2026-09-07-47 — AN RC-PROMOTED BINDING THAT CROSSES AN AUTO-PAR JOIN
/// LOSES ITS BOX, because the joined variable is typed by the `let` rather
/// than by what the branch actually put in it.
///
/// A par return slot's LLVM type comes from `infer_let_binding_llvm_type`,
/// which reads the `let` and answers with the binding's LOGICAL type. Two
/// physical-vs-logical divergences were already taught to it — an
/// SoA-laid-out `Vec[E]` is the 4-field SoA struct, and a handle-backed
/// builtin is one control-block pointer (B-2026-08-08-18). RC-FALLBACK
/// PROMOTION is a third and nothing taught it: the let site heap-boxes the
/// binding, so the local holds a `{i64 rc, T}` box HANDLE — a pointer —
/// while the slot stays typed `T`.
///
/// Every RC-fallback READ path survived that, because they all resolve
/// through `rc_fallback_heap_types` and touch offset 0 — exactly where the
/// branch's 8-byte handle store lands inside the wider field. The one path
/// that is not offset-based is the scope-exit `RcDec`, and it is gated on
/// `slot.ty.is_pointer_type()` (B-2026-07-12-6, which stops an `i64` shadow
/// of a shared binding being reinterpreted as a heap pointer). A
/// struct-typed slot fails that guard, so the dec falls back to the pointer
/// captured at REGISTRATION — for a par-transferred cleanup
/// (`register_one_slot_ownership`) the parent's ALLOCA — and decrements the
/// stack slot's first word as if it were the refcount. No crash, nothing
/// observable, box never freed.
///
/// Measured on the parent, x86_64 Linux, valgrind, three runs each: 58
/// allocs / 33 frees with `40 (direct) + 38 (indirect)` definitely lost,
/// against 58/35 and zero lost after the fix — the SAME allocations and
/// exactly two more frees, the box and its `String`. Cell 5 is the same
/// program BUILT with `KARAC_AUTO_PAR=0`, which is 25/25 clean on both
/// compilers: the sequential lane's binding is pointer-typed, so the guard
/// passes there and the identical `RcDec` frees the box. That is what makes
/// this a build-lane property rather than a run-time one.
///
/// CELLS 5 AND 6 ARE NOT "fan-out that handles the box correctly" — they
/// emit ZERO `__par_branch_*` functions, so they never reach the path at
/// all. B-2026-09-07-47's own row presents them as discriminating controls,
/// which overstates them; they are kept because a change that made either
/// shape start fanning out would newly expose it here.
///
/// TRIP COUNT IS IRRELEVANT (cells 1 / 3 / 4 at 3 / 0 / 5 trips all lose
/// the same 40 B): the box is minted at the `let`, not in the loop, and it
/// is the JOIN that strands it. A loop that never runs still loses it.
#[test]
fn asan_rc_promoted_binding_crossing_a_par_join_still_frees_its_box() {
    const PRE: &str = "struct P { a: String, b: i64 }\n\
             fn seed() -> i64 { env.args().len() }\n\
             fn payload() -> String { f\"payload-{seed()}-aaaaaaaaaaaaaaaaaaaaaaaaaaaa\" }\n\
             fn mkp(n: i64) -> P { return P { a: payload(), b: n }; }\n";

    // Cell 1 — the row's own shape: an owned-`self` WHOLE consume in a
    // running loop, with a second heap local to give the analyzer a second
    // group to fan out. 3 trips.
    assert_clean_asan_run_min_allocs_auto_par(
        &format!(
            "{PRE}impl P {{ fn take(self) -> i64 {{ return self.b; }} }}\n\
                 fn go() -> i64 {{ let t = mkp(9); let mut r = payload(); let mut i = 0i64;\n\
                 \x20 while i < 3i64 {{ t.take(); i = i + 1; }}\n\
                 \x20 return r.len(); }}\n\
                 fn main() {{ println(go()); }}\n"
        ),
        &["38"],
        "b47-whole-method-consume-3-trips",
        6,
    );

    // Cell 2 — a PROJECTING call argument off the same promoted binding.
    // A different consume spelling reaching the same join.
    assert_clean_asan_run_min_allocs_auto_par(
            &format!(
                "{PRE}fn takes(s: String) -> i64 {{ return s.len(); }}\n\
                 fn go() -> i64 {{ let t = mkp(9); let mut r = payload(); let mut i = 0i64; let mut acc = 0i64;\n\
                 \x20 while i < 3i64 {{ acc = acc + takes(t.a); i = i + 1; }}\n\
                 \x20 return r.len() + acc; }}\n\
                 fn main() {{ println(go()); }}\n"
            ),
            &["152"],
            "b47-projecting-argument",
            6,
        );

    // Cell 3 — ZERO trips. The loop body never executes and the same 40 B
    // goes missing, which is what says the box is stranded by the join and
    // not by the consume.
    assert_clean_asan_run_min_allocs_auto_par(
        &format!(
            "{PRE}impl P {{ fn take(self) -> i64 {{ return self.b; }} }}\n\
                 fn go() -> i64 {{ let t = mkp(9); let mut r = payload(); let mut i = 0i64;\n\
                 \x20 while i < 0i64 {{ t.take(); i = i + 1; }}\n\
                 \x20 return r.len(); }}\n\
                 fn main() {{ println(go()); }}\n"
        ),
        &["38"],
        "b47-zero-trip-loop",
        6,
    );

    // Cell 4 — 5 trips, the other direction from cell 3.
    assert_clean_asan_run_min_allocs_auto_par(
        &format!(
            "{PRE}impl P {{ fn take(self) -> i64 {{ return self.b; }} }}\n\
                 fn go() -> i64 {{ let t = mkp(9); let mut r = payload(); let mut i = 0i64;\n\
                 \x20 while i < 5i64 {{ t.take(); i = i + 1; }}\n\
                 \x20 return r.len(); }}\n\
                 fn main() {{ println(go()); }}\n"
        ),
        &["38"],
        "b47-five-trips",
        6,
    );

    // Cell 5 (CONTROL) — a FREE-FUNCTION whole consume. Emits no
    // `__par_branch_*` at all, so it never reaches the join; clean on both
    // compilers. See this test's doc on why that is weaker than the row
    // claims.
    assert_clean_asan_run_min_allocs_auto_par(
        &format!(
            "{PRE}fn takep(p: P) -> i64 {{ return p.b; }}\n\
                 fn go() -> i64 {{ let t = mkp(9); let mut r = payload(); let mut i = 0i64;\n\
                 \x20 while i < 3i64 {{ takep(t); i = i + 1; }}\n\
                 \x20 return r.len(); }}\n\
                 fn main() {{ println(go()); }}\n"
        ),
        &["38"],
        "b47-control-free-fn-consume-no-fanout",
        6,
    );

    // Cell 6 (CONTROL) — strip the second heap local. One group, nothing to
    // fan out, no join, no slot. Also zero `__par_branch_*`.
    assert_clean_asan_run_min_allocs_auto_par(
        &format!(
            "{PRE}impl P {{ fn take(self) -> i64 {{ return self.b; }} }}\n\
                 fn go() -> i64 {{ let t = mkp(9); let mut i = 0i64; let mut acc = 0i64;\n\
                 \x20 while i < 3i64 {{ acc = acc + t.take(); i = i + 1; }}\n\
                 \x20 return acc; }}\n\
                 fn main() {{ println(go()); }}\n"
        ),
        &["27"],
        "b47-control-single-heap-local-no-fanout",
        6,
    );

    // Cell 7 — an EXPLICIT statement-position `par` block, whose bindings
    // hoist into the surrounding scope (B-2026-07-11-3). This is the
    // sibling bind-back site (`compile_par_block` Step 6), reached without
    // an outer `let` to rebind the name over it.
    assert_clean_asan_run_min_allocs_auto_par(
        &format!(
            "{PRE}fn takep(p: P) -> i64 {{ return p.b; }}\n\
                 fn go() -> i64 {{\n\
                 \x20 par {{\n\
                 \x20   let t = mkp(9);\n\
                 \x20   let k = seed() + 41i64;\n\
                 \x20 }}\n\
                 \x20 let mut i = 0i64;\n\
                 \x20 while i < 3i64 {{ takep(t); i = i + 1; }}\n\
                 \x20 return k; }}\n\
                 fn main() {{ println(go()); }}\n"
        ),
        &["42"],
        "b47-explicit-statement-par-block",
        6,
    );
}

/// B-2026-09-07-58 — AN RC-PROMOTED BINDING PUBLISHED THROUGH A
/// VALUE-PRODUCING `par` BLOCK'S JOIN STILL LOSES ITS BOX, because the
/// outer destructuring `let` rebinds the name over the pointer-typed entry
/// `compile_par_block`'s Step 6 installs.
///
/// The sibling of B-2026-09-07-47, reached by a path that fix does not
/// cover. That row re-typed the joined variable at the bind-back
/// (`joined_slot_var_type`), which is what makes the scope-exit `RcDec`'s
/// `slot.ty.is_pointer_type()` reload guard pass. For a VALUE-PRODUCING
/// `par` block the bind-back's entry does not survive: Step 6 binds `t`
/// pointer-typed and correctly, the join expression `(t, u)` loads each
/// struct out of its box to build the tuple, and the OUTER
/// `let (t, u) = …` destructure then rebinds both names to fresh
/// struct-typed allocas and purges their RC metadata. The transferred
/// `RcDec` reloads BY NAME at drain time, fails the pointer guard on the
/// destructure's binding, and falls back to the pointer captured at
/// registration — the parent's ALLOCA — which it decrements as if it were
/// the refcount.
///
/// THE MECHANISM WAS MEASURED, NOT INFERRED. An instrumented drain reads
/// `ptr_typed=false, rc_box=false` on the par spelling and
/// `ptr_typed=true, rc_box=true` on the byte-identical sequential control
/// (cell 5) — the two states the guard discriminates on, one per spelling.
///
/// Measured on the parent, x86_64 Linux, valgrind, archives current, on
/// these exact heap-free cells (a `{i64 rc, Q{i64,i64}}` box is 24 B):
///
/// ```text
///                       parent              with the fix
///   cell 1 (one box)    51/27, 24 B lost    51/28, zero
///   cell 2 (two boxes)  52/27, 48 B in 2    52/29, zero
///   cell 3 (reordered)  51/27, 24 B lost    51/28, zero
///   cell 4 (zero-trip)  counts VARY         counts VARY, zero (6 runs)
///   cell 5 (control)    53/30, zero         53/30, zero
/// ```
///
/// On cells 1, 2, 3 and 5: the SAME allocations either way, and exactly ONE
/// MORE FREE PER BOX — which is what says the fix releases a box that was
/// stranded, rather than changing what the program allocates.
///
/// CELL 4'S COUNTS ARE NOT A STABLE QUANTITY and are deliberately not
/// quoted: the zero-trip shape measures anywhere in 51/28 … 54/31 across
/// runs of ONE binary, always balanced. Its VERDICT is stable — 24 B lost
/// before, zero after on six consecutive runs, output `9` on every one — so
/// the cell discriminates, but a reader who took a single alloc count off
/// it and compared would be reading noise. (Cells 1, 2, 3 and 5 were
/// re-run three times each and do not move.)
///
/// IT LEAKS IN BOTH LANES, which is what separates it from B-2026-09-07-47
/// and is why the row was split out rather than folded in:
/// `KARAC_AUTO_PAR=0` disables AUTO-par only, and an explicit `par` block
/// still fans out. So that row's distinguishing test — clean when BUILT
/// with auto-par off — does not hold here, and cells 1 and 2 below are
/// asserted under BOTH lanes rather than one.
///
/// EVERY CELL'S BOXED STRUCT OWNS NO HEAP, deliberately. The row's own
/// `struct P { a: String, b: i64 }` cell also loses a 38 B `String` per
/// leaf, and that half is NOT this defect: the same 38 B goes missing from
/// a par-join destructure with no RC promotion anywhere in the program
/// (nothing consumed in a loop), while the identical destructure off a
/// plain CALL is clean — `finish_owned_tuple_destructure` gates leaf
/// cleanup on `expr_yields_fresh_owned_temp`, which admits `Call` and
/// `MethodCall` and not a `par` block. That is its own row. A heap-free
/// box makes the box the ONLY thing these cells can lose, so a regression
/// here can only be this defect.
///
/// THE FIX IS NOT IN THE PAR PATH. `pin_rc_dec_before_rebind` runs at
/// `bind_pattern`'s rebind choke point, capturing the handle while the
/// name still means the box and rewriting the pending action to carry it.
/// That is the general statement of the defect: `RcDec` resolves by NAME,
/// and its registered-pointer fallback is a stack slot for every site that
/// registers an alloca rather than a value. A destructuring `let` is
/// simply the first rebind found that reaches it.
#[test]
fn asan_rc_promoted_binding_through_a_par_join_tuple_still_frees_its_box() {
    const PRE: &str = "struct Q { b: i64, c: i64 }\n\
             fn seed() -> i64 { env.args().len() }\n\
             fn mkq(n: i64) -> Q { return Q { b: n, c: seed() }; }\n\
             impl Q { fn take(self) -> i64 { return self.b; } }\n";

    // Cell 1 — one promoted slot and a scalar sibling, so exactly one box
    // is in play. Both lanes.
    let one_box = format!(
        "{PRE}fn go() -> i64 {{\n\
             \x20 let (t, k) = par {{ let t = mkq(9); let k = seed() * 0i64; (t, k) }};\n\
             \x20 let mut i = 0i64;\n\
             \x20 while i < 3i64 {{ t.take(); i = i + 1; }}\n\
             \x20 return t.b + k; }}\n\
             fn main() {{ println(go()); }}\n"
    );
    assert_clean_asan_run_min_allocs_auto_par(&one_box, &["9"], "b58-one-box-autopar", 6);
    assert_clean_asan_run_min_allocs(&one_box, &["9"], "b58-one-box-noautopar", 6);

    // Cell 2 — TWO promoted bindings across one join. The parent loses one
    // box PER promoted slot, so a fix that pins only the first slot's
    // action fails here and passes cell 1. Both lanes.
    let two_boxes = format!(
        "{PRE}fn go() -> i64 {{\n\
             \x20 let (t, u) = par {{ let t = mkq(9); let u = mkq(10); (t, u) }};\n\
             \x20 let mut i = 0i64;\n\
             \x20 while i < 3i64 {{ t.take(); u.take(); i = i + 1; }}\n\
             \x20 return t.b + u.b; }}\n\
             fn main() {{ println(go()); }}\n"
    );
    assert_clean_asan_run_min_allocs_auto_par(&two_boxes, &["19"], "b58-two-boxes-autopar", 6);
    assert_clean_asan_run_min_allocs(&two_boxes, &["19"], "b58-two-boxes-noautopar", 6);

    // Cell 3 — the promoted binding SECOND in the tuple. The defect is the
    // rebind, not the element index, so reordering must not change it.
    assert_clean_asan_run_min_allocs_auto_par(
        &format!(
            "{PRE}fn go() -> i64 {{\n\
                 \x20 let (k, t) = par {{ let k = seed() * 0i64; let t = mkq(9); (k, t) }};\n\
                 \x20 let mut i = 0i64;\n\
                 \x20 while i < 3i64 {{ t.take(); i = i + 1; }}\n\
                 \x20 return t.b + k; }}\n\
                 fn main() {{ println(go()); }}\n"
        ),
        &["9"],
        "b58-promoted-slot-second",
        6,
    );

    // Cell 4 — ZERO trips. The box is minted at the branch's `let`, not in
    // the loop, and it is the JOIN that strands it; a loop that never runs
    // loses the same box. (Same discriminator as B-2026-09-07-47's cell 3,
    // and it holds for the same reason.)
    assert_clean_asan_run_min_allocs_auto_par(
        &format!(
            "{PRE}fn go() -> i64 {{\n\
                 \x20 let (t, k) = par {{ let t = mkq(9); let k = seed() * 0i64; (t, k) }};\n\
                 \x20 let mut i = 0i64;\n\
                 \x20 while i < 0i64 {{ t.take(); i = i + 1; }}\n\
                 \x20 return t.b + k; }}\n\
                 fn main() {{ println(go()); }}\n"
        ),
        &["9"],
        "b58-zero-trip-loop",
        6,
    );

    // Cell 5 — the SEQUENTIAL control: cell 2 with the `par` block spelled
    // as two plain `let`s. It was clean before this fix and is clean after,
    // so it proves nothing about the fix on its own — it is here because it
    // is the ORACLE the fix restores parity with, and because a change that
    // broke the reload path (the half that keeps this shape clean) would
    // show up here and nowhere else in this test.
    assert_clean_asan_run_min_allocs_auto_par(
        &format!(
            "{PRE}fn go() -> i64 {{\n\
                 \x20 let t = mkq(9); let u = mkq(10);\n\
                 \x20 let mut i = 0i64;\n\
                 \x20 while i < 3i64 {{ t.take(); u.take(); i = i + 1; }}\n\
                 \x20 return t.b + u.b; }}\n\
                 fn main() {{ println(go()); }}\n"
        ),
        &["19"],
        "b58-sequential-control",
        6,
    );
}

/// B-2026-08-14-27 — a borrow-elided index read must not acquire a cleanup
/// when it crosses a par join.
///
/// `let first = m[0]` over a `ref Vec[Vec[i64]]` copies the row header
/// verbatim, CAP INCLUDED, and owns nothing: the caller's container still
/// owns that buffer. The sequential let path has always skipped its
/// `track_vec_*` registration for exactly this shape. The par bind-back had
/// no equivalent test and re-registered a cleanup for every Vec-shaped slot,
/// so the parent freed the row and the container's element drop freed it
/// again.
///
/// The `let rows = m.len()` line is not decoration. Three independent
/// bindings is what makes the auto-parallelizer split this preamble into
/// branches at all; with two the group never forms and the same program is
/// clean, which is why the shape reads as unrelated to concurrency. The
/// second `report` call is the 1x1 case whose 8-byte row is the smallest
/// block glibc will hand back into a live allocation.
#[test]
fn asan_borrow_elided_index_read_across_a_par_join() {
    assert_clean_asan_run(
        r#"
fn spiral(m: ref Vec[Vec[i64]]) -> Vec[i64] {
    let mut out: Vec[i64] = Vec.new();
    let rows = m.len();
    let first = m[0].clone();
    let cols = first.len();
    let mut r = 0i64;
    while r < rows {
        let mut c = 0i64;
        while c < cols {
            out.push(m[r][c]);
            c = c + 1i64;
        }
        r = r + 1i64;
    }
    out
}

fn report(grid: Vec[Vec[i64]]) {
    let m = grid;
    let order = spiral(m);
    let mut line: String = "";
    let mut k = 0i64;
    while k < order.len() {
        line.push_str(f"{order[k]} ");
        k = k + 1i64;
    }
    println(line);
}

fn main() {
    report([[1, 2, 3], [4, 5, 6], [7, 8, 9]]);
    report([[1]]);
    println("end");
}
"#,
        &["1 2 3 4 5 6 7 8 9 ", "1 ", "end"],
        "borrow_elided_index_across_par_join",
    );
}

/// B-2026-08-14-28 — an `Option[shared]` CLUSTER root published from a par
/// branch must transfer its free-walk to the parent, not run it.
///
/// A `let l = build_list()` returning a linked chain queues a
/// `FreeClusterWalkOption` that follows the `next` links and frees every
/// node. The branch's publish-time suppression scan handled `RcDec`,
/// `RcDecOption` and the inline payload drains but had no arm for the
/// cluster walk, so the branch freed the WHOLE LIST it had just handed to
/// the parent. `leetcode/1-100/2-add-two-numbers/iterative.kara` SEGFAULTed
/// on the first `to_string`; linked with ASAN — a different heap layout —
/// the same binary printed six lines of garbage digits instead, which is
/// what reading freed nodes looks like once the allocator has reused them.
///
/// TRANSFERRED rather than sentinel-suppressed, and this test is what
/// distinguishes the two. Suppression alone stops the branch from freeing
/// what the parent reads and passes any output comparison — while leaking
/// 400 bytes across the kata's six lists, where the same program built with
/// auto-par off is clean. Only LeakSanitizer separates those two fixes.
///
/// Two lists, both let-bound before the call, is the minimum: with one the
/// group never forms, and passing them inline instead of binding them keeps
/// the whole thing sequential.
#[test]
fn asan_shared_cluster_published_from_a_par_branch() {
    assert_clean_asan_run(
        r#"
shared struct ListNode {
    val: i64,
    mut next: Option[ListNode],
}

fn consume(l1: Option[ListNode], l2: Option[ListNode]) -> Option[ListNode] {
    let dummy = ListNode { val: 0, next: None };
    let mut tail = dummy;
    let mut a = l1;
    let mut b = l2;
    loop {
        let mut done = true;
        if let Some(n) = a { a = n.next; done = false; }
        if let Some(n) = b { b = n.next; done = false; }
        if done { break; }
        let node = ListNode { val: 7, next: None };
        tail.next = Some(node);
        tail = node;
    }
    dummy.next
}

fn from_array(arr: Slice[i64]) -> Option[ListNode] {
    let n = arr.len();
    if n == 0 { return None; }
    let head = ListNode { val: arr[0], next: None };
    let mut tail = head;
    for i in 1..n {
        let node = ListNode { val: arr[i], next: None };
        tail.next = Some(node);
        tail = node;
    }
    Some(head)
}

fn total(list: Option[ListNode]) -> i64 {
    let mut c = 0i64;
    let mut cur = list;
    loop {
        match cur {
            Some(n) => { c = c + n.val; cur = n.next; }
            None => break,
        }
    }
    c
}

fn report(a: Slice[i64], b: Slice[i64]) {
    let l1 = from_array(a);
    let l2 = from_array(b);
    let out = consume(l1, l2);
    println(f"{total(out)}");
}

fn main() {
    let a1: Array[i64, 3] = [2, 4, 3];
    let b1: Array[i64, 3] = [5, 6, 4];
    report(a1, b1);
    let a2: Array[i64, 1] = [1];
    let b2: Array[i64, 2] = [9, 9];
    report(a2, b2);
    println("end");
}
"#,
        &["21", "14", "end"],
        "shared_cluster_published_from_par_branch",
    );
}

/// B-2026-08-15-13's memory half — the direction its fix could go wrong.
///
/// That row was a BUILD failure, so its own detector is an E2E test. What
/// belongs here is the consequence of fixing it: the joined binding now
/// carries a struct type name it did not have before, and a `var_type_names`
/// entry is what arms struct-shaped drops. If the parent lane's new
/// registration were to schedule a drop the par branch already owns, a
/// program that merely failed to compile would start double-freeing instead
/// — strictly worse. The `String` field is what makes that observable: a
/// second drop of `Entry` frees its buffer twice.
///
/// Loop-driven so a per-iteration imbalance accumulates for LSan rather than
/// showing up once at exit.
///
/// The `shared struct` arm of the E2E sibling is deliberately NOT here. It
/// strands one 32-byte RC box — a leak that reproduces identically under
/// `KARAC_AUTO_PAR=0`, where the slot machinery this fix touches never runs,
/// so it is not this fix's doing. It is only newly REACHABLE, because the
/// program could not be compiled at all before. Filed separately
/// (B-2026-08-15-14); pinning it here would quarantine an unrelated defect
/// inside this row's fixture.
#[test]
fn asan_autopar_joined_vec_element_struct_binding_no_double_free() {
    assert_clean_asan_run(
        r#"
#[derive(Clone)]
struct Entry { service: String, weight: i64 }
fn agg(entries: Vec[Entry]) -> String {
    let mut index: Map[String, usize] = Map.new();
    let e = entries[0].clone();
    return e.service;
}
fn agg_two(entries: Vec[Entry]) -> String {
    let mut index: Map[String, usize] = Map.new();
    let a = entries[0].clone();
    let b = entries[1].clone();
    return a.service + "-" + b.service;
}
fn agg_nested(rows: Vec[Vec[Entry]]) -> String {
    let mut index: Map[String, usize] = Map.new();
    let inner = rows[0].clone();
    let e = inner[1].clone();
    return e.service;
}
fn main() {
    let mut es: Vec[Entry] = Vec.new();
    es.push(Entry { service: "alphaalphaalpha", weight: 3 });
    es.push(Entry { service: "betabetabeta", weight: 5 });
    let mut k = 0;
    let mut t = 0;
    while k < 20 {
        t = t + agg(es.clone()).len();
        t = t + agg_two(es.clone()).len();
        let mut rows: Vec[Vec[Entry]] = Vec.new();
        rows.push(es.clone());
        t = t + agg_nested(rows).len();
        k = k + 1;
    }
    println(t);
    println(es[1].service);
    println("done");
}
"#,
        &["1100", "betabetabeta", "done"],
        "asan_autopar_joined_vec_element_struct_binding_no_double_free",
    );
}

#[test]
fn asan_par_readonly_two_branch_vec_i64() {
    assert_clean_asan_run(
        r#"
fn sum_first_half(data: ref Vec[i64]) -> i64 {
    let mut total = 0i64;
    let mut i = 0;
    while i < data.len() / 2 {
        total = total + data[i];
        i = i + 1;
    }
    total
}
fn sum_second_half(data: ref Vec[i64]) -> i64 {
    let mut total = 0i64;
    let mut i = data.len() / 2;
    while i < data.len() {
        total = total + data[i];
        i = i + 1;
    }
    total
}
fn process_in_parallel(data: Vec[i64]) -> (i64, i64) {
    par {
        let a = sum_first_half(data);
        let b = sum_second_half(data);
        (a, b)
    }
}
fn main() {
    let mut data: Vec[i64] = Vec.new();
    let mut i = 0i64;
    while i < 1000i64 {
        data.push(i);
        i = i + 1;
    }
    let (a, b) = process_in_parallel(data);
    println(a);
    println(b);
}
"#,
        &["124750", "374750"],
        "asan_par_readonly_two_branch_vec_i64",
    );
}

#[test]
fn asan_par_readonly_two_branch_vec_of_heap_structs() {
    // Heap-payload elements (String fields) — the branches traverse the
    // same element blocks concurrently through `ref` params; the parent
    // remains the sole owner of buffer AND elements after the join.
    assert_clean_asan_run(
        r#"
struct Doc { title: String, words: i64 }
fn count_long(docs: ref Vec[Doc], lo: i64, hi: i64) -> i64 {
    let mut n = 0i64;
    let mut i = lo;
    while i < hi {
        if docs[i as usize].title.len() > 4 {
            n = n + 1i64;
        }
        i = i + 1;
    }
    n
}
fn scan(docs: ref Vec[Doc]) -> (i64, i64) {
    let half = (docs.len() as i64) / 2i64;
    let total = docs.len() as i64;
    par {
        let a = count_long(docs, 0i64, half);
        let b = count_long(docs, half, total);
        (a, b)
    }
}
fn main() {
    let mut docs: Vec[Doc] = Vec.new();
    let mut i = 0i64;
    while i < 40i64 {
        if i % 2i64 == 0i64 {
            docs.push(Doc { title: "longtitle", words: i });
        } else {
            docs.push(Doc { title: "abc", words: i });
        }
        i = i + 1;
    }
    let (a, b) = scan(docs);
    println(a);
    println(b);
}
"#,
        &["10", "10"],
        "asan_par_readonly_two_branch_vec_of_heap_structs",
    );
}

/// B-2026-08-29-66 under ASAN/LSan — the memory half of the auto-par
/// branch-publish handshake, which the ordering assertions in
/// `tests/par_codegen.rs` can only see indirectly (a garbage id printed
/// off freed memory). Both shapes go through
/// `assert_clean_asan_run_min_allocs_auto_par` because the defect exists
/// ONLY with the auto-parallelizer on: `KARAC_AUTO_PAR=0` was correct and
/// valgrind-clean throughout, which is what the row's "differs between
/// auto-par on and off" got backwards.
///
/// `published` is the row's own repro — a struct wrapping a `Vec` of
/// `Drop` elements, lifted into a group and read afterwards, whose
/// field-bodies walk landed after the `StructDrop` that frees `xs`.
/// `branch-local` is the unpublished sibling: the binding is never read
/// again, so every statement joins the group, nothing is published, and
/// the walk fell to the branch's own scope-exit drain instead.
#[test]
fn asan_auto_par_struct_wrapping_vec_of_drop_elems_is_memory_balanced() {
    const D: &str = "struct R { id: i64, tag: String }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\") } }\n\
             struct Box3 { xs: Vec[R] }\n\
             fn build(k: i64) -> Box3 {\n\
             \x20  let mut v: Vec[R] = Vec.new();\n\
             \x20  v.push(R { id: k, tag: f\"t\" });\n\
             \x20  let bx: Box3 = Box3 { xs: v };\n\
             \x20  println(\"mid\");\n\
             \x20  return bx\n\
             }\n";
    for (label, body, want) in [
        (
            "published",
            format!(
                "{D}fn main() {{ println(\"lead\");\n\
                     \x20            let a: Box3 = build(14);\n\
                     \x20            println(f\"v{{a.xs[0].id}}\"); println(\"post\"); }}\n"
            ),
            vec!["lead", "mid", "v14", "dR14", "post"],
        ),
        (
            "branch-local",
            format!(
                "{D}fn main() {{ println(\"lead\");\n\
                     \x20            let a: Box3 = build(9);\n\
                     \x20            println(\"post\"); }}\n"
            ),
            vec!["lead", "mid", "dR9", "post"],
        ),
    ] {
        assert_clean_asan_run_min_allocs_auto_par(&body, &want, label, 1);
    }
}

/// B-2026-09-08-8 — A VALUE-PRODUCING `par` BLOCK IS NOT A FRESH OWNED TEMP
/// AT THE DESTRUCTURE, so every heap-bearing leaf of
/// `let (t, k) = par { … (t, k) }` is left with no owner.
///
/// `finish_owned_tuple_destructure` hands its leaves scope-exit cleanup
/// only when the RHS is a fresh owned temp, and `expr_yields_fresh_owned_temp`
/// admits exactly `Call | MethodCall`. A `par` block is neither, and it is
/// not a PLACE either, so the destructure reached neither branch and nothing
/// freed the leaves.
///
/// Measured on the parent (x86_64 Linux, valgrind, archives current), with
/// NO RC-fallback promotion anywhere in the program — so B-2026-09-07-58's
/// box is not involved — against the identical destructure off a plain call:
///
/// ```text
///                              parent              with the fix
///   par join, one heap leaf    56/32, 38 B lost    59/36, zero
///   par join, two heap leaves  2 x 38 B lost       56/33, zero
///   plain call (control)       24/24, zero         24/24, zero
/// ```
///
/// BOTH LANES, because `KARAC_AUTO_PAR=0` disables AUTO-par only and an
/// explicit `par` block still fans out — the same reason B-2026-09-07-58's
/// cells are asserted under both.
///
/// THE RC-PROMOTED SPELLING IS CELL 4, and it is here because the row filing
/// this named it as a trap to MEASURE rather than reason about: when the
/// leaf is promoted, B-2026-09-07-58's pin makes the parent free the box,
/// and that box carries a value-drop fn that frees the same `String`.
/// Registering a leaf cleanup beside it would be a double free — the shape
/// B-2026-09-07-30's history records for the sibling unification. Measured
/// clean in both lanes: zero lost, no invalid free.
///
/// ONE SHAPE IS DELIBERATELY LEFT LEAKING and is NOT asserted here, because
/// this harness has no leak-EXPECTING helper and a clean-run assertion on it
/// would be false. A join tail that hands out an OUTER binding
/// (`par { let k = …; (outer, k) }`) is declined by the admission and still
/// loses its 38 B, measured. That is B-2026-08-29-27's fail-closed
/// discipline rather than an oversight: an outer binding stays readable past
/// the join, so handing its storage to a leaf would turn a bounded leak into
/// a use-after-free at every gate that frees at the use site. Its OUTPUT is
/// pinned in the `codegen.rs` twin; the leak is recorded on B-2026-09-08-8
/// so that widening the admission has to confront that argument first.
#[test]
fn asan_par_join_tuple_destructure_owns_its_heap_leaves() {
    const PRE: &str = "struct P { a: String, b: i64 }\n\
             fn seed() -> i64 { env.args().len() }\n\
             fn payload() -> String { f\"payload-{seed()}-aaaaaaaaaaaaaaaaaaaaaaaaaaaa\" }\n\
             fn mkp(n: i64) -> P { return P { a: payload(), b: n }; }\n";
    // 1 — the row's cell A: one heap-bearing leaf through the join.
    let one_leaf = format!(
        "{PRE}fn go() -> i64 {{\n\
             \x20 let (t, k) = par {{ let t = mkp(9); let k = payload().len(); (t, k) }};\n\
             \x20 return t.a.len() + k; }}\n\
             fn main() {{ println(go()); }}\n"
    );
    assert_clean_asan_run_no_auto_par(&one_leaf, &["76"], "b8-one-leaf-noautopar");
    assert_clean_asan_run_min_allocs_auto_par(&one_leaf, &["76"], "b8-one-leaf-autopar", 6);
    // 2 — two heap-bearing leaves: the leak scales per leaf.
    let two_leaves = format!(
        "{PRE}fn go() -> i64 {{\n\
             \x20 let (t, u) = par {{ let t = mkp(9); let u = mkp(10); (t, u) }};\n\
             \x20 return t.a.len() + u.a.len(); }}\n\
             fn main() {{ println(go()); }}\n"
    );
    assert_clean_asan_run_no_auto_par(&two_leaves, &["76"], "b8-two-leaves-noautopar");
    assert_clean_asan_run_min_allocs_auto_par(&two_leaves, &["76"], "b8-two-leaves-autopar", 6);
    // 3 — CONTROL: the identical destructure off a plain call, clean before
    // and after. A failure here means the probe, not the par path.
    let plain_call = format!(
        "{PRE}fn pair() -> (P, i64) {{ return (mkp(9), payload().len()); }}\n\
             fn go() -> i64 {{ let (t, k) = pair(); return t.a.len() + k; }}\n\
             fn main() {{ println(go()); }}\n"
    );
    assert_clean_asan_run_no_auto_par(&plain_call, &["76"], "b8-plain-call-control");
    // 4 — the RC-PROMOTED leaf: the double-free trap, measured not reasoned.
    let promoted = format!(
        "{PRE}fn take(p: P) -> i64 {{ return p.a.len(); }}\n\
             fn go() -> i64 {{\n\
             \x20 let (t, k) = par {{ let t = mkp(9); let k = payload().len(); (t, k) }};\n\
             \x20 let mut n = 0i64; let mut i = 0i64;\n\
             \x20 while i < 3i64 {{ n = n + take(t); i = i + 1; }}\n\
             \x20 return n + k; }}\n\
             fn main() {{ println(go()); }}\n"
    );
    assert_clean_asan_run_no_auto_par(&promoted, &["152"], "b8-promoted-noautopar");
    assert_clean_asan_run_min_allocs_auto_par(&promoted, &["152"], "b8-promoted-autopar", 6);
}
