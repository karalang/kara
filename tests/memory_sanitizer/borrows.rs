//! ref and mut ref params, borrows, escape analysis, elision -- fixtures for `tests/memory_sanitizer.rs`.
//!
//! Split out of `tests/memory_sanitizer.rs` on 2026-09-21. The TEST TARGET is
//! unchanged: this file is a module of that target, so
//! `cargo test --features llvm --test memory_sanitizer` still runs everything
//! and CI needs no edit. Run this area alone with:
//!
//!     cargo test --features llvm --test memory_sanitizer borrows::
//!
//! New fixtures about ref and mut ref params, borrows, escape analysis, elision belong in this file.

use super::*;

/// B-2026-09-06-25 — the interpreter-only row's program under ASAN + LSan
/// on the compiled side: every materialization of a view bound off a
/// borrow-projection scrutinee (arm value, `return`, whole field, by-value
/// argument, under `ref h` / `mut ref h` / `ref self` / `mut ref self`)
/// is a real copy with exactly one owner of its `String` / `Vec` buffers,
/// and the caller's original keeps its own.
#[test]
fn asan_borrow_projection_view_materialized_is_a_real_copy() {
    assert_clean_asan_run(
            "struct R { id: i64, tag: String, xs: Vec[i64] }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"  dR{self.id}\") } }\n\
             fn mk(i: i64) -> R { return R { id: i, tag: f\"t{i}\", xs: [i] } }\n\
             enum E { A(R), B }\n\
             impl Drop for E { fn drop(mut ref self) { println(\"  dE\") } }\n\
             struct H { e: E }\n\
             struct W { r: R }\n\
             fn consume(x: R) -> i64 { return x.id }\n\
             \n\
             #[allow(partial_move_of_drop_enum)]\n\
             fn p_out(h: ref H) -> R { let r2 = match h.e { E.A(r) => r, E.B => mk(0) }; return r2; }\n\
             #[allow(partial_move_of_drop_enum)]\n\
             fn p_out_mut(h: mut ref H) -> R { match h.e { E.A(r) => { return r; } E.B => { return mk(0); } } }\n\
             fn p_field(w: ref W) -> R { return w.r; }\n\
             fn p_consume(h: ref H) -> i64 { match h.e { E.A(r) => { return consume(r); } E.B => { return 0; } } }\n\
             fn p_read(h: ref H) -> i64 { match h.e { E.A(r) => { return r.id; } E.B => { return 0; } } }\n\
             impl H {\n\
             \x20   fn m_consume(ref self) -> i64 { match self.e { E.A(r) => { return consume(r); } E.B => { return 0; } } }\n\
             \x20   fn m_consume_mut(mut ref self) -> i64 { match self.e { E.A(r) => { return consume(r); } E.B => { return 0; } } }\n\
             \x20   fn m_consume_iflet(ref self) -> i64 { if let E.A(r) = self.e { return consume(r); } return 0; }\n\
             #[allow(partial_move_of_drop_enum)]\n\
             \x20   fn m_let(ref self) -> i64 { match self.e { E.A(r) => { let m = r; return consume(m); } E.B => { return 0; } } }\n\
             #[allow(partial_move_of_drop_enum)]\n\
             \x20   fn m_out(ref self) -> R { let r2 = match self.e { E.A(r) => r, E.B => mk(0) }; return r2; }\n\
             \x20   fn m_read(ref self) -> i64 { match self.e { E.A(r) => { return r.id; } E.B => { return 0; } } }\n\
             \x20   fn m_mixed(ref self, k: bool) -> i64 { match self.e { E.A(r) if k => { return consume(r); } E.A(r) => { return r.id; } E.B => { return 0; } } }\n\
             }\n\
             \n\
             fn main() {\n\
             \x20   println(\"p_out\"); let a1 = H { e: E.A(mk(1)) }; let r1 = p_out(a1); println(f\"  got{r1.id}\");\n\
             \x20   println(\"p_out_mut\"); let mut a2 = H { e: E.A(mk(2)) }; let r2 = p_out_mut(mut a2); println(f\"  got{r2.id}\");\n\
             \x20   println(\"p_field\"); let w3 = W { r: mk(3) }; let r3 = p_field(w3); println(f\"  got{r3.id}\");\n\
             \x20   println(\"p_consume\"); let a4 = H { e: E.A(mk(4)) }; let x4 = p_consume(a4); println(f\"  got{x4}\");\n\
             \x20   println(\"p_read\"); let a5 = H { e: E.A(mk(5)) }; let x5 = p_read(a5); println(f\"  got{x5}\");\n\
             \x20   println(\"m_consume\"); let a6 = H { e: E.A(mk(6)) }; let x6 = a6.m_consume(); println(f\"  got{x6}\");\n\
             \x20   println(\"m_consume_mut\"); let mut a7 = H { e: E.A(mk(7)) }; let x7 = a7.m_consume_mut(); println(f\"  got{x7}\");\n\
             \x20   println(\"m_consume_iflet\"); let a8 = H { e: E.A(mk(8)) }; let x8 = a8.m_consume_iflet(); println(f\"  got{x8}\");\n\
             \x20   println(\"m_let\"); let a9 = H { e: E.A(mk(9)) }; let x9 = a9.m_let(); println(f\"  got{x9}\");\n\
             \x20   println(\"m_out\"); let a10 = H { e: E.A(mk(10)) }; let r10 = a10.m_out(); println(f\"  got{r10.id}\");\n\
             \x20   println(\"m_read\"); let a11 = H { e: E.A(mk(11)) }; let x11 = a11.m_read(); println(f\"  got{x11}\");\n\
             \x20   println(\"m_mixed/taken\"); let a12 = H { e: E.A(mk(12)) }; let x12 = a12.m_mixed(true); println(f\"  got{x12}\");\n\
             \x20   println(\"m_mixed/read\"); let a13 = H { e: E.A(mk(13)) }; let x13 = a13.m_mixed(false); println(f\"  got{x13}\");\n\
             \x20   println(\"end\");\n\
             }\n",
            &[
                "p_out",
                "  dE",
                "  dR1",
                "  got1",
                "  dR1",
                "p_out_mut",
                "  dE",
                "  dR2",
                "  got2",
                "  dR2",
                "p_field",
                "  dR3",
                "  got3",
                "  dR3",
                "p_consume",
                "  dR4",
                "  dE",
                "  dR4",
                "  got4",
                "p_read",
                "  dE",
                "  dR5",
                "  got5",
                "m_consume",
                "  dR6",
                "  dE",
                "  dR6",
                "  got6",
                "m_consume_mut",
                "  dR7",
                "  dE",
                "  dR7",
                "  got7",
                "m_consume_iflet",
                "  dR8",
                "  dE",
                "  dR8",
                "  got8",
                "m_let",
                "  dR9",
                "  dE",
                "  dR9",
                "  got9",
                "m_out",
                "  dE",
                "  dR10",
                "  got10",
                "  dR10",
                "m_read",
                "  dE",
                "  dR11",
                "  got11",
                "m_mixed/taken",
                "  dR12",
                "  dE",
                "  dR12",
                "  got12",
                "m_mixed/read",
                "  dR13",
                "  dE",
                "  dR13",
                "  got13",
                "end"
            ],
            "borrow_projection_view_materialized",
        );
}

/// B-2026-09-15-4 — the MEMORY half of a `-> ref (T, U)` method result used
/// in a value position.
///
/// `m.get(h.peek())` over `Map[(String, String), i64]` read the borrow's
/// `ptr` as the `{ptr,len,cap}` pair itself and hashed whatever it found:
/// SIGSEGV, with 2461 valgrind errors from 4 contexts ("Use of
/// uninitialised value of size 8", then "Invalid read of size 8", then
/// signal 11). The output half is
/// `test_e2e_ref_tuple_return_used_in_a_value_position` in
/// `tests/codegen.rs`; both are kept because the two ways this shape
/// failed were a crash and a silently wrong answer, and the scalar cell
/// (which cannot crash) is invisible to a sanitizer while the heap cell is
/// invisible to an output assertion.
///
/// `free:` is the free-function spelling, which was already correct — it is
/// here as the oracle the method arm was made to match, so a regression
/// that reaches only one of the two is still caught.
#[test]
fn asan_ref_tuple_return_in_a_value_position_is_balanced() {
    assert_clean_asan_run(
        r#"
struct Hold { pr: (String, String) }
impl Hold { fn peek(ref self) -> ref (String, String) { return self.pr; } }
fn peekf(h: ref Hold) -> ref (String, String) { return h.pr; }

fn main() {
    let mut i: i64 = 0;
    while i < 2 {
        let h = Hold { pr: (f"b154-left-aaaaaaaaaaaaaaaa-{i}", f"b154-right-bbbbbbbbbbbbbbbb-{i}") };

        let mut m: Map[(String, String), i64] = Map.new();
        m.insert((f"b154-left-aaaaaaaaaaaaaaaa-{i}", f"b154-right-bbbbbbbbbbbbbbbb-{i}"), 7);
        match m.get(h.peek()) {
            Some(v) => { println(f"mapheap:{v}"); }
            None => { println("mapheap:missing"); }
        }

        println(f"proj:{h.peek().0.len()}");
        println(f"free:{peekf(h).1.len()}");

        let p: ref (String, String) = h.peek();
        println(f"bound:{p.0.len()}");

        i = i + 1;
    }
}
"#,
        &[
            "mapheap:7",
            "proj:28",
            "free:29",
            "bound:28",
            "mapheap:7",
            "proj:28",
            "free:29",
            "bound:28",
        ],
        "asan_ref_tuple_return_in_a_value_position_is_balanced",
    );
}

#[test]
fn asan_ref_self_field_return_no_double_free() {
    // Returning a heap FIELD through a BORROWED receiver (`fn name(ref self)
    // -> String { self.n }`). The borrow does not own the field, so the
    // returned value must be a deep CLONE — an alias would be freed twice
    // (the caller drops the receiver, freeing the field, AND drops the
    // returned value). The receiver is USED AFTER the call each iteration
    // (`x.n`/`x.tags` read), so a move would corrupt it; the clone leaves
    // the field intact. Covers a String field and a `Vec[i64]` field via
    // `ref self` / `mut ref self`; looped so any per-iteration imbalance
    // accumulates (leak on LSan, double-free / UAF on ASan).
    assert_clean_asan_run(
        r#"
struct Person { n: String, tags: Vec[i64] }
impl Person {
    fn name(ref self) -> String { self.n }
    fn steal_tags(mut ref self) -> Vec[i64] { self.tags }
}
fn main() {
    let mut i: i64 = 0i64;
    while i < 3i64 {
        let mut p = Person { n: f"name-{i}-padded-padded", tags: [i, i + 1i64, i + 2i64] };
        let nm = p.name();
        println(nm);
        println(p.n);
        let tg = p.steal_tags();
        println(f"{tg.len()}");
        println(f"{p.tags.len()}");
        i = i + 1;
    }
}
"#,
        &[
            "name-0-padded-padded",
            "name-0-padded-padded",
            "3",
            "3",
            "name-1-padded-padded",
            "name-1-padded-padded",
            "3",
            "3",
            "name-2-padded-padded",
            "name-2-padded-padded",
            "3",
            "3",
        ],
        "ref_self_field_return_no_double_free",
    );
}

#[test]
fn asan_freshtemp_user_method_ref_self_no_double_free() {
    // Slice 3j: a `ref self` user method on a fresh-temp struct receiver
    // (`make_counter().m_get()`), looped. The struct owns a `Vec[String]`
    // field; the temp materializes into `__urecv_tmp` and — because `self` is
    // borrowed — is drop-tracked so its field Vec + Strings free once via
    // `__karac_drop_struct_Counter` at scope exit. The method reads a field
    // String through `self.items.get(0)` (an `Option[ref String]` borrow, not
    // consumed). Hazards: (1) the borrowed temp must be freed exactly once —
    // the method borrows, so the caller owns it (macOS ASAN would catch a
    // double-free against any spurious second drop); (2) the field Strings
    // must be freed by the struct drop, else they leak (Linux LSan). ≥36-byte
    // field strings defeat LSan short-string reachability; the outer loop
    // re-materializes the temp each pass.
    assert_clean_asan_run(
        r#"
struct Counter { items: Vec[String], base: i64 }
impl Counter {
    fn m_get(ref self) -> i64 {
        match self.items.get(0) {
            Some(s) => return self.base + s.len(),
            None => return self.base,
        };
    }
}
fn make_counter() -> Counter {
    let mut c = Counter { items: Vec.new(), base: 100_i64 };
    c.items.push("first field string padded beyond thirty-six bytes ok");
    c.items.push("second field string padded beyond thirty-six byte");
    return c;
}
fn main() {
    let mut p = 0;
    while p < 3 {
        println(make_counter().m_get());
        p = p + 1;
    };
}
"#,
        &["152", "152", "152"],
        "freshtemp_user_method_ref_self_no_double_free",
    );
}

#[test]
fn asan_ref_param_field_let_move_no_leak_no_double_free() {
    // B-2026-07-21-11 memory leg: the let-move deep copy. Double-free
    // half: the copy's heap freed exactly once by the binding, never
    // again by the caller's struct drop. Leak half: every clone the
    // helper mints (struct, enum, String, Vec[String], Option payload)
    // must drain through the binding's normal owned tracking — a
    // mis-registered clone leaks once per call under LSan. Loop so any
    // per-iteration imbalance accumulates.
    assert_clean_asan_run(
        r#"
enum Tok { Plus, Ident(String) }
struct Pt { s: String, x: i64 }
struct Holder { inner: Pt, tok: Tok, name: String, strs: Vec[String], opt: Option[String], n: i64 }
fn f_struct(h: ref Holder) -> i64 {
    let p = h.inner;
    return p.s.len() + p.x;
}
fn f_enum(h: ref Holder) -> i64 {
    let t = h.tok;
    match t {
        Ident(nm) => { return nm.len(); }
        Plus => { return 0; }
    }
    return -1;
}
fn f_str(h: ref Holder) -> i64 {
    let s = h.name;
    return s.len();
}
fn f_vstr(h: ref Holder) -> i64 {
    let v = h.strs;
    return (v[0] + v[1]).len();
}
fn f_opt(h: ref Holder) -> i64 {
    let o = h.opt;
    match o {
        Some(s) => { return s.len(); }
        None => { return 0; }
    }
    return -1;
}
fn main() {
    let mut i: i64 = 0;
    let mut acc: i64 = 0;
    while i < 40 {
        let a = Holder { inner: Pt { s: "ld".to_string(), x: 6 }, tok: Tok.Ident("mv".to_string()), name: "st".to_string(), strs: ["ab", "cd"], opt: Some("xy".to_string()), n: 1 };
        acc = acc + f_struct(a) + f_enum(a) + f_str(a) + f_vstr(a) + f_opt(a);
        acc = acc + f_struct(a);
        i = i + 1;
    }
    println(acc);
}
"#,
        &["1040"],
        "ref_param_field_let_move",
    );
}

#[test]
fn asan_ref_param_tuple_field_consume_no_leak_no_double_free() {
    // B-2026-07-21-10 memory leg: the ref-chain tuple clone. Double-free
    // half: a consumed String element freed exactly once by its binding.
    // Leak half: a `_` element must be freed exactly once by the clone's
    // tuple StructDrop (a lost registration leaks one element per call
    // under LSan). Loop so any per-iteration imbalance accumulates.
    assert_clean_asan_run(
        r#"
struct Holder { pair: (String, i64), two: (String, String), n: i64 }
fn render(h: ref Holder) -> i64 {
    match h.pair {
        (s, x) => { return ("t:".to_string() + s).len() + x; }
    }
    return -1;
}
fn left_only(h: ref Holder) -> i64 {
    match h.two {
        (a, _) => { return a.len(); }
    }
    return -1;
}
fn main() {
    let mut i: i64 = 0;
    let mut acc: i64 = 0;
    while i < 40 {
        let a = Holder { pair: ("tp".to_string(), 4), two: ("aa".to_string(), "bravo".to_string()), n: 1 };
        acc = acc + render(a) + render(a) + left_only(a);
        i = i + 1;
    }
    println(acc);
}
"#,
        &["720"],
        "ref_param_tuple_field_consume",
    );
}

/// B-2026-08-05-39 — the MEMORY half of the `mut ref` aggregate
/// reassignment fix, which the value assertion cannot see.
///
/// Storing the new value through the borrow makes the program print the
/// right answer; it also ORPHANS whatever the caller's storage held, once
/// per call. Mutation-checked: with the reclamation disabled this exact
/// program still prints 41400 while valgrind reports 200 definitely-lost
/// blocks per shape (13,780 bytes). So the codegen twin passes either way
/// and this is the only gate on that half.
///
/// The f-string arm is the other direction and was found by running this:
/// `x = f"…"` stages its bytes in an accumulator slot whose own free is
/// armed, and the caller's storage then points at the same buffer —
/// leaving both armed aborted with `free(): double free detected in
/// tcache 2`. Every other RHS shape was clean, so the arm is specific.
///
/// NOT VACUOUS (B-2026-08-04-17): opaque `env.args().len()` seed, every
/// payload's CONTENT built from it at runtime, byte-level reads via
/// `contains`, and 200 iterations so nothing unrolls away.
#[test]
fn asan_mut_ref_aggregate_param_reassignment_no_leak() {
    assert_clean_asan_run_min_allocs(
        r#"struct Q { s: String, v: Vec[i64] }

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
    let n: i64 = env.args().len();
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
        i = i + 1;
    }
    println(acc);
}
"#,
        &["41400"],
        "mut_ref_aggregate_param_reassignment_no_leak",
        // 200 iterations x (5 replaced payloads + their displaced
        // originals) is well over a thousand allocations; the floor sits
        // far above the 3 of a folded-away run.
        400,
    );
}

#[test]
fn asan_for_loop_borrow_then_rebind_let_no_leak() {
    // Shadow guard: after the loop, a `let w = <fresh owned>` reusing the
    // loop var name must NOT be defensive-copied (the `let` clears stale
    // for_loop_borrow_vars membership) — else the copy + source-suppress
    // would orphan the fresh String (LSan-only leak; ≥36-byte payload).
    assert_clean_asan_run(
        r#"
fn main() {
    let mut words: Vec[String] = Vec.new();
    words.push("loopelem-aaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string());
    let mut sink: Vec[String] = Vec.new();
    for w in words {
        sink.push(w);
    }
    let mut w = String.new();
    w.push_str("rebound-owned-bbbbbbbbbbbbbbbbbbbbbbbb");
    sink.push(w);
    println(sink.len());
}
"#,
        &["2"],
        "for_loop_borrow_then_rebind_let_no_leak",
    );
}

#[test]
fn asan_ref_arg_repeated_calls_no_compound_leak() {
    // Calling `show_len(make())` in a loop. Each iteration's
    // materialized temp is in the same call-arg scope; without
    // proper cleanup, allocations would either pile up (leak
    // arm) or be freed against the wrong cap (double-free arm).
    // 8 iterations is small but enough that any per-iteration
    // imbalance would surface as a deterministic crash under
    // ASAN's quarantine.
    assert_clean_asan_run(
        r#"
fn make() -> String {
    let a = "hi ";
    let b = "there";
    return a + b;
}

fn show_len(s: ref String) {
    println(s.len());
}

fn main() {
    let mut i = 0;
    while i < 8 {
        show_len(make());
        i = i + 1;
    }
}
"#,
        &["8", "8", "8", "8", "8", "8", "8", "8"],
        "ref_arg_repeated_calls_no_compound_leak",
    );
}

#[test]
fn asan_soa_mut_ref_fill_borrow_no_leak_or_double_free() {
    // Per-layout monomorphization slice 4 (multi-buffer WRITE, by mut ref):
    // a differently-named SoA buffer (`entities`, `layout entities`) is
    // FILLED through a shared `fill(buf: mut ref Vec[Entity])` helper that
    // pushes. The push reallocs each group buffer and writes the new
    // pointers / len / cap back through the deref'd caller-struct pointer
    // (`ref_params`). Ownership is BORROW: the mono must NOT queue a
    // `FreeSoaGroups` for the `mut ref` param — only `main`'s `entities`
    // binding owns the buffers and frees both groups once at scope exit.
    // Get it wrong and it's a double-free (callee + caller both free → ASAN)
    // or a leak (the realloc'd group buffers from a prior iteration never
    // freed → LSan). Looped 20× — each iteration builds a fresh `entities`,
    // fills it via mut-ref, reads it, drops it — to amplify either fault.
    // ≥36 bytes of live payload per element across the groups so a reachable
    // leak isn't masked by LSan's short-allocation blind spot.
    assert_clean_asan_run(
        r#"
struct Entity { x: f64, y: f64, hp: i64 }
layout entities: Vec[Entity] {
    group physics { x, y }
    group combat { hp }
}
fn fill(buf: mut ref Vec[Entity]) {
    buf.push(Entity { x: 1.0, y: 2.0, hp: 100 });
    buf.push(Entity { x: 3.0, y: 4.0, hp: 200 });
    buf.push(Entity { x: 5.0, y: 6.0, hp: 300 });
}
fn main() {
    let mut sum = 0;
    let mut k = 0;
    while k < 20 {
        let mut entities: Vec[Entity] = Vec.new();
        fill(mut entities);
        let mut i = 0;
        while i < entities.len() {
            let e = entities[i];
            sum = sum + e.hp;
            i = i + 1;
        }
        k = k + 1;
    }
    println(sum);
}
"#,
        &["12000"],
        "soa_mut_ref_fill_borrow",
    );
}

#[test]
fn asan_borrowed_param_walks_repeat() {
    // Phase C2a under ASAN: two long-lived chains walked by a
    // borrowing adder 200 times. The borrow contract is balanced
    // per call (caller arg-site head inc / callee exit RcDecOption)
    // while ALL walk traffic is count-free — an unbalanced cursor
    // (a stray alias-acquire inc, a counted advance, an over-eager
    // family cleanup) frees a reused chain mid-loop (ASAN UAF) or
    // leaks per call (LeakSanitizer / RSS). Exact total pins the
    // arithmetic: 200*15 + 9 + 15 = 3024.
    assert_clean_asan_run(
        r#"
shared struct ListNode { val: i64, mut next: Option[ListNode] }
fn from_three(a: i64, b: i64, c: i64) -> Option[ListNode] {
    let dummy = ListNode { val: 0, next: None };
    let mut tail = dummy;
    let mut i = 0;
    while i < 3 {
        let mut v = a;
        if i == 1 { v = b; }
        if i == 2 { v = c; }
        let node = ListNode { val: v, next: None };
        tail.next = Some(node);
        tail = node;
        i = i + 1;
    }
    dummy.next
}
fn add_two_numbers(l1: Option[ListNode], l2: Option[ListNode]) -> Option[ListNode] {
    let dummy = ListNode { val: 0, next: None };
    let mut tail = dummy;
    let mut a = l1;
    let mut b = l2;
    let mut carry: i64 = 0;
    loop {
        let mut s: i64 = carry;
        let mut done = true;
        if let Some(n) = a {
            s = s + n.val;
            a = n.next;
            done = false;
        }
        if let Some(n) = b {
            s = s + n.val;
            b = n.next;
            done = false;
        }
        if done and s == 0 {
            break;
        }
        let node = ListNode { val: s % 10, next: None };
        tail.next = Some(node);
        tail = node;
        carry = s / 10;
    }
    dummy.next
}
fn sum_chain(head: Option[ListNode]) -> i64 {
    let mut sum = 0;
    let mut cur = head;
    while cur.is_some() {
        let x = cur.unwrap();
        sum = sum + x.val;
        cur = x.next;
    }
    sum
}
fn main() {
    let l1 = from_three(2, 4, 3);
    let l2 = from_three(5, 6, 4);
    let mut total = 0;
    let mut iter = 0;
    while iter < 200 {
        let r = add_two_numbers(l1, l2);
        total = total + sum_chain(r);
        iter = iter + 1;
    }
    total = total + sum_chain(l1) + sum_chain(l2);
    println(total);
}
"#,
        &["3024"],
        "borrowed_param_walks_repeat",
    );
}

#[test]
fn asan_borrow_local_read_methods_no_double_free() {
    // B-2026-06-07-5 residue: read-only methods beyond len/is_empty on a
    // borrow-LOCAL now route through `compile_vec_method` (the receiver is
    // registered in `vec_elem_types`). Reading through the borrow must NOT
    // free the source's heap buffer — only the source frees it, once, at
    // scope exit. The String source is a heap concat (not a static
    // literal) and the Vec is heap, so a stray free of the borrow would
    // double-free (ASAN abort) and an early free would leave the trailing
    // `s.len()`/`xs.len()` reading freed memory.
    let label = "borrow_local_read_methods";
    if !asan_available() {
        eprintln!("[{label}] ASAN unavailable on this host — skipping");
        return;
    }
    let Some((stdout, status)) = run_under_asan(
        r#"
fn sid(s: ref String) -> ref String { s }
fn vid(v: ref Vec[i64]) -> ref Vec[i64] { v }
fn main() {
    let s: String = "hello " + "world";
    let n = sid(s);
    println(n.starts_with("hello"));
    let xs: Vec[i64] = [10, 20, 30];
    let m = vid(xs);
    match m.get(1) { Some(x) => println(x), None => println(0 - 1) }
    match m.last() { Some(x) => println(x), None => println(0 - 1) }
    println(s.len());
    println(xs.len());
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
             a read method on a borrow-local must not free the source's buffer",
        status.code()
    );
    assert_eq!(
        stdout.trim().lines().collect::<Vec<_>>(),
        vec!["true", "20", "30", "11", "3"],
        "[{label}] unexpected stdout (ASAN passed, output mismatched)"
    );
}

// ── Raw-pointer deref load/store (B-2026-06-11-3) ─────────────
//
// `unsafe { *p }` on a `*const T` / `*mut T` now emits a real `load`
// of the pointee (it previously yielded the address), and `*p = val`
// stores through the pointer (it previously clobbered the pointer
// variable's own alloca). Both addresses point into a live stack-owned
// `Array[u8, N]`, so a mis-emitted load/store (reading or writing the
// wrong address) would trip ASAN with a stack-buffer over/underflow.
// The loaded/stored value is a scalar `u8`, so there is no heap
// ownership to double-free; this guards the addressing, not a free.

#[test]
fn asan_raw_ptr_deref_load_no_bad_access() {
    assert_clean_asan_run(
        r#"
fn main() {
    let a: Array[u8, 3] = [65u8, 66u8, 67u8];
    let p = a.as_ptr();
    // Safety: `p` addresses element 0 of the live owned array.
    let b: u8 = unsafe { *p };
    println(b);
}
"#,
        &["65"],
        "raw_ptr_deref_load_no_bad_access",
    );
}

#[test]
fn asan_raw_ptr_deref_store_no_bad_access() {
    assert_clean_asan_run(
        r#"
fn main() {
    let mut a: Array[u8, 3] = [65u8, 66u8, 67u8];
    let p = a.as_mut_ptr();
    // Safety: `p` addresses element 0 of the live mutable owned array.
    unsafe { *p = 90u8; }
    let b: u8 = unsafe { *p };
    println(b);
    println(a[0]);
}
"#,
        &["90", "90"],
        "raw_ptr_deref_store_no_bad_access",
    );
}

#[test]
fn asan_statsref_reuse_and_fresh_temp_no_leak_no_double_free() {
    // B-2026-07-01-10: `Stats.*` params are now `ref Slice[f64]` and
    // the baked modes reach the ownership pass — reusing one dataset
    // across calls must neither leak nor double-free (the borrow arg
    // is NOT freed by the callee; a FRESH temp arg still frees via the
    // ref-rvalue materialization).
    assert_clean_asan_run(
        r#"
fn make(n: i64) -> Vec[f64] {
    let mut v: Vec[f64] = Vec.new();
    let mut i = 0;
    while i < n {
        v.push(1.5);
        i = i + 1;
    };
    v
}
fn main() {
    let mut v: Vec[f64] = Vec.new();
    v.push(1.0);
    v.push(2.0);
    v.push(3.0);
    println(Stats.sum(v));
    println(Stats.mean(v));
    let sorted = Stats.sort(v);
    println(sorted.len());
    println(Stats.sum(make(4)));
    println(v.len());
}
"#,
        &["6", "2", "3", "6", "3"],
        "statsref_reuse_and_fresh_temp_no_leak_no_double_free",
    );
}

#[test]
fn asan_struct_field_by_ref_to_free_fn_no_double_free() {
    // B-2026-07-12-1: passing a struct FIELD (`self.names`, a `Vec[String]`)
    // by ref to a FREE function double-freed the field's backing Vec under
    // AOT (`free(): double free detected`) — the `ref`-arg rvalue path
    // shallow-copied the field header into a temp and freed its buffer at
    // scope exit, double-freeing what the receiver's field-drop still owns.
    // Now the field is borrowed in place (a GEP off the receiver). Loops so
    // any double-free / leak accumulates for ASAN+LSan; drives both the read
    // (`ref Vec[String]`) shape (the reported repro, over heap String
    // elements) and a `mut ref Vec[i64]` field whose in-place mutation must
    // survive without freeing the shared buffer.
    assert_clean_asan_run(
        r#"
fn scan(names: ref Vec[String], name: ref String) -> i64 {
    let mut i = 0;
    loop {
        if i >= names.len() { return -1; }
        if names[i] == name { return i; }
        i = i + 1;
    }
}
fn addall(v: mut ref Vec[i64], n: i64) {
    let mut i = 0;
    loop { if i >= v.len() { return; } v[i] = v[i] + n; i = i + 1; }
}
struct T { names: Vec[String], xs: Vec[i64] }
impl T {
    fn find(ref self, q: ref String) -> i64 { scan(self.names, q) }
    fn bump(mut ref self, n: i64) { addall(mut self.xs, n) }
}
fn main() {
    let mut r: i64 = 0;
    while r < 3 {
        let mut t = T { names: Vec.new(), xs: Vec.new() };
        t.names.push(f"row-{r}-alpha");
        t.names.push(f"row-{r}-bravo");
        t.xs.push(r); t.xs.push(r + 1);
        let q = f"row-{r}-bravo";
        println(f"{t.find(q)}");
        t.bump(10);
        println(f"{t.xs[0]}");
        r = r + 1;
    }
}
"#,
        &["1", "10", "1", "11", "1", "12"],
        "struct_field_by_ref_to_free_fn_no_double_free",
    );
}

/// B-2026-07-30-7 — struct fields declared through a PLAIN type alias
/// (`type Name = String; type Ints = Vec[i64];`) must reach drop synthesis
/// as their heap base. Codegen recorded the field's type under the ALIAS
/// name, which matches no known heap shape, so `register_struct_metadata`
/// now peels the alias before writing `struct_field_type_names` /
/// `struct_field_type_exprs`. The failure mode this pins is the pair on
/// either side of that peel: no drop action at all (leak of every
/// alias-typed String/Vec field) or a drop emitted twice once the field is
/// visible to both the alias and base paths (double-free). Churns the
/// allocations in a loop so a per-iteration imbalance accumulates well past
/// LSan's noise floor.
///
/// B-2026-08-04-17: the first version of this fixture was VACUOUS — it
/// seeded `build(8i64)` from a literal and read the payloads back through
/// `.len()` alone, which is both failure modes at once (foldable content,
/// and buffers whose bytes are never read). It measured 0 program
/// allocations at `-O2`, so it proved nothing about a drop it was written
/// to pin. Now seeded from `env.args().len()` and reading the String's
/// BYTES (`contains`) plus a Vec ELEMENT, with a floor.
#[test]
fn asan_plain_alias_struct_fields_no_leak() {
    assert_clean_asan_run_min_allocs(
        r#"
type Name = String;
type Ints = Vec[i64];

struct Holder { label: Name, nums: Ints }

fn build(n: i64) -> Holder {
    let mut v: Ints = Vec.new();
    let mut i = 0i64;
    while i < n + 7i64 { v.push(i); i = i + 1i64; }
    return Holder { label: f"held-{n}-runtime-heap", nums: v };
}

fn weigh(h: Holder) -> i64 {
    let mut w = 0i64;
    if h.label.contains("runtime-heap") { w = w + 1i64; }
    return w + h.nums[0i64] + h.nums.len();
}

fn main() {
    let base: i64 = env.args().len();
    let mut total = 0i64;
    let mut k = 0i64;
    while k < base + 49i64 {
        let h = build(base);
        total = total + weigh(h);
        k = k + 1i64;
    }
    println(total);
}
"#,
        // base=1: each iter builds an 8-element Vec and a 19-char label →
        // 1 (contains) + 0 (nums[0]) + 8 (len) = 9; ×50 iterations = 450.
        &["450"],
        "plain_alias_struct_fields_no_leak",
        // 50 iterations × (label buffer + element buffer) is ~100
        // allocations; the floor sits far above the 3 of a folded-away run
        // while leaving room for allocator/inlining variation.
        50,
    );
}

/// B-2026-08-05-1 and -2 — a TUPLE ELEMENT passed to a `ref` or `mut ref`
/// parameter is borrowed IN PLACE, so nothing copies its buffer into a temp
/// that then frees it, and a `mut ref` mutation lands on the tuple itself.
///
/// One missing arm caused both. B-2026-07-12-1 taught the ref-argument path
/// to pass a pointer to a struct FIELD instead of letting it reach the
/// rvalue path, which shallow-copies the `{ptr,len,cap}` header into a temp
/// and queues a scope-exit free of a buffer the owner still holds. The
/// tuple spelling never got the sibling arm.
///
/// The last two shapes are the struct-field controls — the reference
/// implementation this was made to match, and what localized the bug.
///
/// Seeded from `env.args().len()` with element and byte reads so the
/// buffers are neither folded nor dead-stripped at `-O2` (B-2026-08-04-17);
/// ~800 allocations, floored well below that.
#[test]
fn asan_tuple_element_borrowed_in_place_for_ref_params() {
    assert_clean_asan_run_min_allocs(
        r#"
struct H { a: Vec[i64], b: i64 }
fn mkv(k: i64) -> Vec[i64] { let mut v: Vec[i64] = Vec.new(); v.push(k); v.push(k + 1i64); return v; }
fn mks(k: i64) -> String { let mut s: String = String.new(); s.push_str(f"pay-{k}"); return s; }
fn dig(i: i64) -> String { let mut d: String = String.new(); d.push_str(f"{i}"); return d; }
fn peek(v: ref Vec[i64]) -> i64 { return v[0i64] + v.len(); }
fn bump(v: mut ref Vec[i64]) { v.push(42i64); }
fn slen(s: ref String) -> i64 { return s.len(); }
fn main() {
    let base: i64 = env.args().len();
    let mut acc = 0i64;
    let mut i = base;
    while i < base + 100i64 {
        let t1: (Vec[i64], i64) = (mkv(i), 5i64);
        acc = acc + peek(t1.0) + t1.0[1i64];
        let mut t2: (Vec[i64], i64) = (mkv(i), 5i64);
        bump(mut t2.0);
        acc = acc + t2.0.len() + t2.0[2i64];
        let t3: (i64, Vec[i64]) = (5i64, mkv(i));
        acc = acc + peek(t3.1);
        let t4: (String, i64) = (mks(i), 5i64);
        if t4.0.contains(dig(i)) { acc = acc + slen(t4.0); }
        let h1: H = H { a: mkv(i), b: 5i64 };
        acc = acc + peek(h1.a);
        let mut h2: H = H { a: mkv(i), b: 5i64 };
        bump(mut h2.a);
        acc = acc + h2.a[2i64];
        i = i + 1;
    }
    println(f"{acc}");
}
"#,
        &["30192"],
        "tuple_element_borrowed_in_place_for_ref_params",
        300,
    );
}

/// B-2026-08-01-1 — the row buffer behind
/// `mk_rows().first().unwrap().len()` is freed exactly once: the
/// get-family materialization's per-element walk owns it, and the len
/// intercept must not track the unwrapped borrow a second time (the
/// double-free aborted the binary before the fix; ASAN reports it as
/// heap-use-after-free/double-free on the first iteration).
#[test]
fn asan_len_of_unwrapped_borrow_row_freed_once() {
    assert_clean_asan_run(
        r#"
fn mk_rows(n: i64) -> Vec[Vec[i64]] {
    let mut v: Vec[Vec[i64]] = Vec.new();
    let mut i = 0i64;
    while i < n {
        v.push(Vec[i, i + 1i64, i + 2i64]);
        i = i + 1;
    }
    v
}
fn main() {
    let mut total = 0i64;
    let mut it = 0i64;
    while it < 200i64 {
        total = total + mk_rows(2i64).first().unwrap().len();
        total = total + mk_rows(3i64).get(1i64).unwrap().len();
        it = it + 1i64;
    }
    println(f"{total}");
}
"#,
        &["1200"],
        "len_of_unwrapped_borrow_row_freed_once",
    );
}

/// B-2026-08-14-21, second half — the miscompile was also a leak, and the
/// value pin alone would not have caught that.
///
/// Pre-fix, each `s += x` on a `mut ref String` built a fresh concatenated
/// buffer and stored it into the 8-byte alloca holding the borrow pointer:
/// the caller never saw it AND nothing ever freed it. Routing the store
/// through the borrow reclaims the buffer it displaces, so an appending loop
/// through a `mut ref` parameter now holds one buffer rather than one per
/// iteration. (The LOCAL `+=` still leaks its displaced buffer — that is
/// B-2026-08-14-22, a different arm, deliberately not touched here.)
#[test]
fn asan_compound_append_through_mut_ref_param_is_balanced() {
    assert_clean_asan_run(
        r#"
fn append(s: mut ref String, x: String) {
    let mut i = 0i64;
    while i < 200i64 { s += x; i = i + 1i64; }
}
fn main() {
    let k = env.args().len() as i64;
    let mut acc = String.new(); acc.push_str("seed"); acc.push_str(k.to_string());
    let mut piece = String.new(); piece.push_str("abcdefgh");
    append(mut acc, piece);
    println(acc.len());
}
"#,
        // "seed1" (5) + 200 * 8. `k` is a stable 1 — the binary runs with no
        // args — and is appended so the seed is a heap buffer rather than a
        // static literal, whose `cap == 0` would make the reclaim a no-op and
        // hide the thing being asserted.
        &["1605"],
        "compound_append_through_mut_ref_param_is_balanced",
    );
}

/// B-2026-08-27-21 leg 1 — `.clone()` on an element read THROUGH a `ref`
/// binding (`let row = ref d[0]; row[0].clone()`) segfaulted under codegen
/// while `--interp` returned the element.
///
/// The ref binding registered its two element tables against DIFFERENT
/// types: `vec_elem_types[row]` correctly held the inner element
/// (`String`), while `var_elem_type_exprs[row]` held the binding's OWN type
/// (`Vec[String]`). `compile_indexed_receiver_method` registers the synth
/// receiver for `row[0]` from the TypeExpr half, so the synth for a
/// `String` element was registered as a `Vec[String]` whose slot pointed at
/// the String's `{ptr, len, cap}` — `clone` then read "aa"'s header as a
/// Vec header, took `len = 2` as an element COUNT, and cloned two `String`s
/// out of the letter bytes.
///
/// This asserts the MEMORY half of that repair; `tests/codegen.rs` asserts
/// the value. Both matter: the aliasing borrow means a spurious free here
/// would be a double free of a buffer the container still owns.
#[test]
fn asan_clone_of_element_through_ref_binding_is_clean() {
    assert_clean_asan_run(
        r#"
fn main() {
    let mut d: Vec[Vec[String]] = Vec.new();
    let mut a: Vec[String] = Vec.new();
    a.push(f"payload_aaaaaaaaaaaaaaaaaaaa{1}");
    a.push(f"payload_bbbbbbbbbbbbbbbbbbbb{2}");
    d.push(a);
    let row = ref d[0];
    let mut t = 0i64;
    let mut i = 0i64;
    while i < 20i64 {
        let c: String = row[0].clone();
        t = t + c.len();
        i = i + 1;
    }
    println(f"{t}");
}
"#,
        &["580"],
        "clone-elem-through-ref-binding",
    );
}

/// B-2026-08-27-52 — a struct-FIELD container argument bound to a
/// read-only `ref` param of a GENERIC callee.
///
/// The mono argument path computes an in-place place pointer only when the
/// param is `mut ref`; a plain `ref` fell through to
/// `materialize_rvalue_for_ref_arg`, which shallow-copies the field's
/// `{ptr,len,cap}` header into a temp and queues a scope-exit FREE of that
/// buffer. The receiver still owns it, so the owner's own field drop
/// doubled the free — `free(): double free detected in tcache 2` and abort
/// on both compiled backends, while the interpreter printed the right
/// answer. B-2026-07-12-1 fixed exactly this for the NON-generic call path;
/// the generic one never got the arm.
///
/// Both spellings of the receiver are covered, because they reach the arm
/// by different routes: `self` inside a generic impl (which parses as
/// `SelfValue`, not `Identifier`) and a plain local struct. The `mut ref`
/// case is here as the control that was already clean — it takes the arm
/// above, so a regression that removed the new one would leave it passing.
#[test]
fn asan_ref_container_param_borrows_a_struct_field_in_place() {
    for (label, prog, want) in [
        (
            "ref-field-generic-impl",
            r#"
struct Bag[=T] { xs: Vec[T] }
fn vlen[T](v: ref Vec[T]) -> i64 { return v.len(); }
impl[T] Bag[T] {
    fn go(ref self) -> i64 { return vlen(self.xs); }
}
fn main() {
    let mut a: Bag[String] = Bag { xs: Vec.new() };
    a.xs.push("aa"); a.xs.push("bb");
    println(f"len={a.go()}");
}
"#,
            "len=2",
        ),
        (
            "ref-field-plain-struct",
            r#"
struct Bag { xs: Vec[String] }
fn vlen[T](v: ref Vec[T]) -> i64 { return v.len(); }
fn main() {
    let mut a: Bag = Bag { xs: Vec.new() };
    a.xs.push("aa"); a.xs.push("bb");
    println(f"len={vlen(a.xs)}");
}
"#,
            "len=2",
        ),
        (
            "ref-field-tuple-element",
            r#"
struct Bag[=T] { xs: Vec[T] }
fn vlen[T](v: ref Vec[T]) -> i64 { return v.len(); }
impl[T] Bag[T] {
    fn go(ref self) -> i64 { return vlen(self.xs); }
}
fn main() {
    let mut a: Bag[(i64, String)] = Bag { xs: Vec.new() };
    a.xs.push((1, "aa"));
    println(f"len={a.go()}");
}
"#,
            "len=1",
        ),
        (
            "mut-ref-field-control",
            r#"
struct Bag[=T] { xs: Vec[T] }
fn swap01[T](v: mut ref Vec[T]) { v.swap(0, 1); }
impl[T] Bag[T] {
    fn go(mut ref self) { swap01(mut self.xs); }
}
fn main() {
    let mut a: Bag[String] = Bag { xs: Vec.new() };
    a.xs.push("aa"); a.xs.push("bb"); a.go();
    println(f"len={a.xs.len()} [{a.xs[0]}]");
}
"#,
            "len=2 [bb]",
        ),
    ] {
        assert_clean_asan_run(prog, &[want], label);
    }
}

/// A TUPLE ELEMENT projected out of a CONTAINER ELEMENT — `v[0].0` over
/// `Vec[(String, i64)]` and `Vec[(R, i64)]` (B-2026-08-28-24).
///
/// The read handed back a SHALLOW alias of the container element's heap, so
/// every owning destination shared one pointer with the element and both
/// freed it: `free(): double free detected in tcache 2`, rc 134, on both
/// compiled backends, from a `karac check`-clean program the interpreter
/// answered correctly.
///
/// EVERY CONSUMING POSITION IS HERE ON PURPOSE, because they do not share a
/// mechanism and the measurement proved it. Cloning at the CONSUMERS — the
/// route the whole-element read takes — fixes `let`, `return`, an argument,
/// an assignment and a branch tail, and leaves STRUCT-LITERAL FIELD,
/// `Vec.push` and TUPLE CONSTRUCTION still aborting, because those three
/// consume without going through it. A fixture that stopped at `let` would
/// have passed against that wrong fix. Cloning at the READ reaches all of
/// them through the one takeover ~87 call sites already funnel into.
///
/// The NON-consuming rows (`+ "!"`, `.len()`) are the other direction, and
/// the one this test is shaped to catch: a read nothing takes over must
/// free its own clone. Under LSan those fail as a LEAK where the consuming
/// ones fail as a double free, so both halves of the takeover are pinned.
///
/// Re-reading the container after each consumer is what keeps a "fix" that
/// cap-zeroes the SOURCE from passing: that silences the abort and hands
/// back a container whose element has been emptied — the use-after-free
/// B-2026-08-12-27 measured and rejected.
///
/// The `{ptr,len,cap}` and STRUCT members are both here because they take
/// DIFFERENT ownership contracts in the fix — only the former may be
/// registered in `vec_elem_field_clone_slots`, since the takeover zeroes
/// field 2 of a `{ptr,len,cap}` at the recorded slot and would scribble on
/// a struct. `v[0].1` is the Copy control that must not be cloned at all.
///
/// Positions deliberately EXCLUDED, all measured leaking identically in the
/// already-working FIELD spelling and so not this row's to fix: a consumer
/// that is itself an unbound TEMPORARY (`println(Box2 { w: v[0].0 }.w)`,
/// `println((v[1].0, 9).0)`, and printing an `if`/`match`/block value
/// without binding it). Same byte counts on both spellings — see
/// B-2026-08-28-32.
#[test]
fn test_tuple_element_of_a_container_element_is_cloned_not_aliased() {
    let src = r#"
struct R { id: i64, name: String }
struct Holder { r: R }
struct Box2 { w: String }

fn takes(s: String) -> i64 { return s.len(); }
fn ret_s(v: Vec[(String, i64)]) -> String { return v[0].0; }
fn ret_r(v: Vec[(R, i64)]) -> R { return v[0].0; }

fn mks() -> Vec[(String, i64)] { return [(f"a{1}", 1), (f"b{2}", 2)]; }
fn mkr() -> Vec[(R, i64)] {
    return [(R { id: 1, name: f"a{1}" }, 1), (R { id: 2, name: f"b{2}" }, 2)];
}

fn main() {
    let v = mks();
    // consuming positions
    let a = v[0].0;
    println(a);
    let b = Box2 { w: v[0].0 };
    println(b.w);
    let mut o: Vec[String] = Vec.new();
    o.push(v[0].0);
    println(o[0]);
    println(takes(v[1].0));
    let mut b2 = Box2 { w: f"z{0}" };
    b2.w = v[1].0;
    println(b2.w);
    let mut o2: Vec[String] = [f"q{0}"];
    o2[0] = v[0].0;
    println(o2[0]);
    let c = true;
    let d = if c { v[0].0 } else { v[1].0 };
    println(d);
    let e = match c { true => v[1].0, false => v[0].0 };
    println(e);
    let g = { let t = (f"t{1}", 2); t.0 };
    println(g);
    // non-consuming reads: the clone must free itself
    println(v[0].0 + "!");
    println(v[0].0.len());
    // a Copy member must not be cloned at all
    println(v[0].1);
    // the container is still intact
    println(v[0].0);
    println(v[1].0);
    println(ret_s(mks()));

    // the same matrix over a STRUCT member
    let w = mkr();
    let r = w[0].0;
    println(r.name);
    let h = Holder { r: w[0].0 };
    println(h.r.name);
    let mut po: Vec[R] = Vec.new();
    po.push(w[0].0);
    println(po[0].name);
    println(ret_r(mkr()).name);
}
"#;
    assert_clean_asan_run(
        src,
        &[
            "a1", "a1", "a1", "2", "b2", "a1", "a1", "b2", "t1", "a1!", "2", "1", "a1", "b2", "a1",
            "a1", "a1", "a1", "a1",
        ],
        "tuple-element-of-container-element",
    );
}

/// A STRUCT-VALUED heap read off a CONTAINER ELEMENT — `q[1].r` over
/// `Vec[Q]` where `Q { r: R, … }` and `R` owns a `String`
/// (B-2026-08-28-35).
///
/// `clone_vec_elem_heap_field_read` gated on `is_string_type_expr ||
/// extract_vec_elem_type` and declined a struct-valued field, so the read
/// handed back a shallow alias of the element's storage with no clone
/// anywhere: `let`, the struct-literal field, `Vec.push`, `return` and a
/// branch tail all double-freed on both compiled backends.
///
/// THE ARGUMENT ROW IS THE ONE THAT CONSTRAINS THE FIX, and it was already
/// CLEAN before it. An owned aggregate param is callee-owned by ENTRY COPY,
/// so the callee never frees the caller's alias — which means the naive
/// fix, cloning with no cleanup, trades five double frees for a leak here.
/// That is measured, not hypothetical: it is exactly what B-2026-08-28-24
/// shipped for the tuple spelling, and what B-2026-08-28-36 then repaired
/// there. This read takes that same contract by reuse rather than by a
/// second copy of it.
///
/// THE `let` ROW IS THE OTHER CONSTRAINT, from the opposite side. Adding
/// that cleanup without a takeover double-frees at every consuming
/// position, and a struct `let` does not route through the ~87-site funnel
/// the `{ptr,len,cap}` sibling relies on — its arm suppresses move SOURCES,
/// all of which correctly decline an index-rooted chain. So the two rows
/// pin the two halves: without the cleanup the argument row leaks, without
/// the takeover the `let` row double-frees, and only both together pass.
///
/// The non-consuming reads are the third corner — a clone nothing takes
/// over must free itself — and the container is re-read after every
/// consumer so a "fix" that empties the source instead cannot pass.
#[test]
fn test_struct_valued_field_of_a_container_element_is_cloned_not_aliased() {
    let src = r#"
struct R { id: i64, name: String }
struct Q { r: R, n: i64 }
struct Holder { r: R }

fn takes_r(r: R) -> i64 { return r.id; }
fn ret_r(q: Vec[Q]) -> R { return q[0].r; }
fn ret_t(v: Vec[(R, i64)]) -> R { return v[0].0; }

fn mkq() -> Vec[Q] {
    return [Q { r: R { id: 1, name: f"a{1}" }, n: 1 },
            Q { r: R { id: 2, name: f"b{2}" }, n: 2 }];
}
fn mkv() -> Vec[(R, i64)] {
    return [(R { id: 1, name: f"a{1}" }, 1), (R { id: 2, name: f"b{2}" }, 2)];
}

fn main() {
    let q = mkq();
    let e = q[1].r;
    println(e.name);
    let h = Holder { r: q[1].r };
    println(h.r.name);
    let mut o: Vec[R] = Vec.new();
    o.push(q[1].r);
    println(o[0].name);
    // already clean before the fix — the callee entry-copies, so this row is
    // the one that fails as a LEAK if the clone carries no cleanup
    println(takes_r(q[1].r));
    println(ret_r(mkq()).name);
    let c = true;
    let d = if c { q[0].r } else { q[1].r };
    println(d.name);
    // non-consuming reads: the clone must free itself
    println(q[1].r.name);
    println(q[1].r.id);
    // the container is intact
    println(q[0].r.name);
    println(q[1].r.name);

    // the TUPLE spelling of the same member, whose ownership contract this
    // read reuses (B-2026-08-28-36)
    let v = mkv();
    let te = v[1].0;
    println(te.name);
    println(takes_r(v[1].0));
    let th = Holder { r: v[0].0 };
    println(th.r.name);
    println(ret_t(mkv()).name);
}
"#;
    assert_clean_asan_run(
        src,
        &[
            "b2", "b2", "b2", "2", "a1", "a1", "b2", "2", "a1", "b2", "b2", "2", "a1", "a1",
        ],
        "struct-valued-field-of-container-element",
    );
}

/// A TUPLE-MEMBER leaf read through a BORROWED place — `fn peek(t: ref Wt)
/// -> String { t.pair.0 }` where the field is a `(String, i64)`
/// (B-2026-08-28-37).
///
/// The sibling of B-2026-08-28-25 one node kind over, and broader than that
/// one: the FieldAccess spelling had the `let` position covered already
/// (`clone_ref_chain_field_move_rhs`) and only the consuming positions were
/// broken, whereas a tuple-member leaf double-freed at BOTH — neither
/// cloner could name the leaf's type, because both resolve it through the
/// owning struct's field table and a tuple member is not in one.
///
/// So the `let` row here is not redundant with the return rows: against a
/// fix that widened only the consuming arm it is the row that still fails.
///
/// The scalar member and the owned (non-`ref`) receiver are the
/// must-not-change controls — the first has no buffer to free twice, the
/// second owns its storage and is reconciled by move-out suppression
/// instead, so a clone there would leak. The argument row covers the
/// opposite direction for the same reason it does in the sibling: this
/// helper also runs at argument positions, where an unclaimed clone shows
/// up as a leak rather than an abort.
#[test]
fn test_borrowed_tuple_member_leaf_is_cloned_not_aliased() {
    let src = r#"
struct R { id: i64, name: String }
struct Wt { pair: (String, i64), k: i64 }
struct Wr { pr: (R, i64), k: i64 }

fn sink(s: String) -> i64 { return s.len(); }

fn ret(t: ref Wt) -> String { return t.pair.0; }
fn tail(t: ref Wt) -> String { t.pair.0 }
fn bound(t: ref Wt) -> String { let s = t.pair.0; return s; }
fn arg(t: ref Wt) -> i64 { return sink(t.pair.0); }
fn scalar(t: ref Wt) -> i64 { return t.pair.1; }
fn structleaf(t: ref Wr) -> R { return t.pr.0; }
fn owned(t: Wt) -> String { return t.pair.0; }

impl Wt { fn peek(ref self) -> String { return self.pair.0; } }

fn main() {
    let v = Wt { pair: (f"n{41}", 7), k: 2 };
    println(ret(v));
    println(tail(v));
    println(bound(v));
    println(arg(v));
    println(scalar(v));
    println(v.peek());
    // the borrowed struct's member survives every one of those
    println(v.pair.0);

    let w = Wr { pr: (R { id: 1, name: f"n{41}" }, 7), k: 2 };
    println(structleaf(w).name);
    println(w.pr.0.name);

    let o = Wt { pair: (f"n{41}", 7), k: 2 };
    println(owned(o));
}
"#;
    assert_clean_asan_run(
        src,
        &[
            "n41", "n41", "n41", "3", "7", "n41", "n41", "n41", "n41", "n41",
        ],
        "borrowed-tuple-member-leaf",
    );
}

/// B-2026-08-28-13 — moving a heap field OUT of a BORROWED binding inside a
/// GENERIC fn or impl double-freed the buffer under both compiled backends.
///
/// `clone_ref_chain_field_move_rhs` (B-2026-07-21-11) exists precisely to
/// stop this: `let v = self.xs` through a `ref` receiver bit-copy-aliases
/// the caller's field header while the `let` registers an owned cleanup, so
/// the binding deep-clones and the two frees hit independent buffers. It
/// reads the field's type from the struct DECLARATION table, which inside
/// `impl[T] Bag[T]` still says `Vec[T]`; `borrow_payload_clone_supported`
/// then correctly declines the unsubstituted param and the clone was
/// skipped SILENTLY. Same erasure family as B-2026-08-25-10/-11, at the
/// borrowed-receiver site instead of the owned one, and fixed the same way
/// those were: substitute the active monomorph first.
///
/// The elements are built through `env.args().len()` and READ BACK by byte,
/// not by `.len()` alone — a constant-seeded, length-only fixture folds away
/// at `-O2` and asserts nothing (the hazard `assert_clean_asan_run`
/// documents). Each case also reads the CALLER's field after the call: that
/// is what distinguishes a genuine independent copy from two frees that
/// merely happen not to collide.
///
/// (e) and (f) are the controls that pin the diagnosis, and both were
/// already clean pre-fix: (e) is the non-generic twin, which never erases
/// the field type, and (f) is a bare `String` field of a generic struct —
/// a type with no parameter to substitute, so it reached the clone all
/// along. Verified RED pre-fix: (a) aborted under ASAN with
/// `attempting double-free`.
#[test]
fn asan_generic_borrowed_field_move_out_does_not_double_free() {
    assert_clean_asan_run(
        r#"
struct Bag[=T] { xs: Vec[T] }
impl[T] Bag[T] {
    fn n(ref self) -> i64 { let v = self.xs; return v.len(); }
}
struct Plain { xs: Vec[i64] }
impl Plain {
    fn n(ref self) -> i64 { let v = self.xs; return v.len(); }
}
struct Named[=T] { name: String, xs: Vec[T] }
impl[T] Named[T] {
    fn nm(ref self) -> i64 { let s = self.name; return s.len(); }
}
fn free_n[T](b: ref Bag[T]) -> i64 { let v = b.xs; return v.len(); }
fn main() {
    let base: i64 = env.args().len();
    // (a) the filed shape: scalar element, generic `ref self` receiver.
    let mut a: Bag[i64] = Bag { xs: Vec.new() };
    a.xs.push(base);
    a.xs.push(base + 1);
    println(f"a={a.n()} {a.xs[0]} {a.xs[1]}");
    // (b) HEAP-allocated String elements (literals are rodata with cap 0 and
    // would make the second free a silent no-op).
    let mut ss: Vec[String] = Vec.new();
    let mut i: i64 = 0;
    while i < 3 { ss.push(f"item{i + base}"); i = i + 1; }
    let b: Bag[String] = Bag { xs: ss };
    println(f"b={b.n()} {b.xs[0]}");
    // (c) generic FREE fn with a `ref` param — not about the `self` receiver.
    println(f"c={free_n(a)} {a.xs[1]}");
    // (d) nested-generic concrete: `T = Vec[i64]`, the param bound to a type
    // that itself carries generic args.
    let mut inner: Vec[i64] = Vec.new();
    inner.push(base + 6);
    let mut d: Bag[Vec[i64]] = Bag { xs: Vec.new() };
    d.xs.push(inner);
    println(f"d={d.n()} {d.xs[0][0]}");
    // (e) NON-generic control: clean pre-fix.
    let mut e: Plain = Plain { xs: Vec.new() };
    e.xs.push(base + 8);
    println(f"e={e.n()} {e.xs[0]}");
    // (f) bare `String` field of a generic struct: no param to substitute, so
    // this reached the clone pre-fix too.
    let f: Named[i64] = Named { name: f"nm{base}", xs: Vec.new() };
    println(f"f={f.nm()} {f.name}");
}
"#,
        &["a=2 1 2", "b=3 item1", "c=2 2", "d=1 7", "e=1 9", "f=3 nm1"],
        "generic-borrowed-field-move-out",
    );
}

/// B-2026-08-31-14 under ASAN/LSan — A BORROW-MODE PAYLOAD BINDING NAME
/// STAYED REGISTERED FOR EVERY LATER MATCH IN THE SAME FUNCTION.
///
/// `borrowed_agg_payload_struct_vars` is keyed by BINDING NAME and was
/// cleared only per FUNCTION. A read-only arm registers its payload
/// bindings there (so a heap field copied out of one deep-copies); a LATER
/// match that happened to reuse the name then inherited that registration
/// and stopped taking ownership of its own payload, which nothing else
/// owned — a leak.
///
/// The two matches below are individually clean and leak only together, in
/// this order, which is why it survived: the first is a read-only borrow
/// over a NAMED local, the second a FRESH TEMP whose arm genuinely moves
/// `e.msg` out. Both bind `e`. Measured on `main` before the fix: 800 bytes
/// in 50 objects, one per loop iteration. Swap the order and it is clean,
/// which is what made an earlier bisect of this read as "no interaction".
///
/// Found while working B-2026-08-30-52: relaxing the escape walker made a
/// third match qualify as a borrow and lit the same collision from another
/// direction, but the defect is entirely independent of that row and
/// reproduces on an unmodified compiler.
#[test]
fn asan_borrow_mode_binding_name_does_not_leak_into_a_later_match() {
    assert_clean_asan_run(
        r#"
struct One { msg: String }
fn s_of(i: i64) -> String {
    let mut s: String = String.new();
    s.push_str(f"payload-padded-out-well-past-thirty-six-bytes-{i}");
    return s;
}
fn g(i: i64) -> Result[i64, One] { return Result.Err(One { msg: s_of(i) }); }
fn digits(i: i64) -> String { let mut d: String = String.new(); d.push_str(f"{i}"); return d; }
fn main() {
    let base: i64 = env.args().len();
    let mut n = 0i64;
    let mut i = base;
    while i < base + 50i64 {
        // 1. READ-ONLY over a named local — a borrow, and it registers `e`.
        let r: Result[i64, One] = g(i);
        match r {
            Result.Ok(_) => { n = n + 1i64; }
            Result.Err(e) => { if e.msg.contains(digits(i)) { n = n + e.msg.len(); } }
        }
        // 2. FRESH TEMP whose arm MOVES the payload out. It must own it; the
        //    stale registration above made it think otherwise.
        match g(i) {
            Result.Ok(v) => { n = n + v; }
            Result.Err(e) => { let m: String = e.msg; if m.contains(digits(i)) { n = n + m.len(); } }
        }
        i = i + 1;
    }
    println("done");
}
"#,
        &["done"],
        "borrow-mode binding name does not leak into a later match",
    );
}

/// B-2026-09-23-31 — a heap field COPIED OUT of a container element that the
/// binding only borrows was freed twice on every compiled surface: `let name
/// = p.0` through `let p = ref ps[1]`, `let m = q.name` through `let q = ref
/// qs[0]`, and `let n = p.0` over a `for` loop's tuple element (`ps.iter()`
/// and bare `ps` alike). The binding's cleanup and the container's element
/// drop both owned the buffer. Only a signature `ref` param reached the
/// let-move clone. The fix routes an element-borrow root (`elem_borrow_roots`)
/// through the same clone, records a loop tuple element's types in full, and
/// copies a whole tuple element moved into a new owner or a sink. Legs beyond
/// the row's four: a `mut` copy that the use-after-move copy already covers
/// (a second copy leaked 25 B), a `Vec` leaf, a nested tuple leaf, a whole
/// element move, an `Option` leaf, and the two push spellings. Measured before
/// the fix: 39 valgrind errors at `-O0` on this program.
#[test]
fn asan_borrowed_element_field_copy_is_independent() {
    assert_clean_asan_run_min_allocs(
        r#"
struct P { name: String, n: i64 }
fn mkps() -> Vec[(String, i64)] {
    return [(f"alpha-{1}-long-enough-to-heap", 1), (f"beta-{2}-long-enough-to-heap", 2), (f"gamma-{3}-long-enough-to-heap", 3)];
}
fn leg_ref_tuple() {
    let ps = mkps();
    let p = ref ps[1];
    let name = p.0;
    println(f"rt {name} {p.1} {ps[1].0}");
}
fn leg_ref_struct() {
    let qs: Vec[P] = [P { name: f"delta-{4}-long-enough", n: 4 }];
    let q = ref qs[0];
    let m = q.name;
    println(f"rs {m} {q.n} {qs[0].name}");
}
fn leg_iter_tuple() {
    let ps = mkps();
    for p in ps.iter() {
        let n = p.0;
        println(f"it {n} {p.1}");
    }
    println(f"it {ps[2].0}");
}
fn leg_owned_loop_tuple() {
    let ps = mkps();
    for p in ps {
        let n = p.0;
        println(f"ol {n} {p.1}");
    }
}
fn leg_mut_copy() {
    let ps = mkps();
    for p in ps.iter() {
        let mut n = p.0;
        n.push_str("-X");
        println(f"mu {n} {p.0}");
    }
}
fn leg_vec_leaf() {
    let ps: Vec[(Vec[String], i64)] = [([f"a-{1}-long-enough-to-heap", f"b{2}"], 1)];
    for p in ps {
        let v = p.0;
        println(f"vl {v.len()} {v[0]} {p.1}");
    }
    let r = ref ps[0];
    let w = r.0;
    println(f"vl {w[1]} {ps[0].0.len()}");
}
fn leg_nested() {
    let ps: Vec[(i64, (String, i64))] = [(1, (f"nest-{7}-long-enough-to-heap", 7))];
    for p in ps.iter() {
        let n = p.1.0;
        println(f"ne {n} {p.0}");
    }
    let r = ref ps[0];
    let m = r.1.0;
    println(f"ne {m}");
}
fn leg_whole() {
    let ps = mkps();
    for p in ps.iter() {
        let q = p;
        println(f"wh {q.0} {q.1}");
    }
    println(f"wh {ps[0].0}");
}
fn leg_option_leaf() {
    let ps: Vec[(Option[String], i64)] = [(Some(f"opt-{5}-long-enough-to-heap"), 1), (None, 2)];
    for p in ps {
        let o = p.0;
        match o { Some(s) => println(f"op {s}"), None => println("op none") }
    }
}
fn leg_push() {
    let ps = mkps();
    let mut names: Vec[String] = [];
    let mut whole: Vec[(String, i64)] = [];
    for p in ps.iter() {
        names.push(p.0);
        whole.push(p);
    }
    println(f"pu {names[2]} {whole[1].0} {ps[0].0}");
}
fn main() {
    leg_ref_tuple();
    leg_ref_struct();
    leg_iter_tuple();
    leg_owned_loop_tuple();
    leg_mut_copy();
    leg_vec_leaf();
    leg_nested();
    leg_whole();
    leg_option_leaf();
    leg_push();
    println("done");
}
"#,
        &[
            "rt beta-2-long-enough-to-heap 2 beta-2-long-enough-to-heap",
            "rs delta-4-long-enough 4 delta-4-long-enough",
            "it alpha-1-long-enough-to-heap 1",
            "it beta-2-long-enough-to-heap 2",
            "it gamma-3-long-enough-to-heap 3",
            "it gamma-3-long-enough-to-heap",
            "ol alpha-1-long-enough-to-heap 1",
            "ol beta-2-long-enough-to-heap 2",
            "ol gamma-3-long-enough-to-heap 3",
            "mu alpha-1-long-enough-to-heap-X alpha-1-long-enough-to-heap",
            "mu beta-2-long-enough-to-heap-X beta-2-long-enough-to-heap",
            "mu gamma-3-long-enough-to-heap-X gamma-3-long-enough-to-heap",
            "vl 2 a-1-long-enough-to-heap 1",
            "vl b2 2",
            "ne nest-7-long-enough-to-heap 1",
            "ne nest-7-long-enough-to-heap",
            "wh alpha-1-long-enough-to-heap 1",
            "wh beta-2-long-enough-to-heap 2",
            "wh gamma-3-long-enough-to-heap 3",
            "wh alpha-1-long-enough-to-heap",
            "op opt-5-long-enough-to-heap",
            "op none",
            "pu gamma-3-long-enough-to-heap beta-2-long-enough-to-heap alpha-1-long-enough-to-heap",
            "done",
        ],
        "asan_borrowed_element_field_copy_is_independent",
        20,
    );
}

// B-2026-09-24-7 — `let t = r;` over a `mut ref` parameter is the same
// reference, so a reallocating write through `t` must reach the caller: the
// alias binds as a second pointer to the caller's place, not a header copy.
#[test]
fn asan_mut_ref_param_alias_realloc_frees_once() {
    assert_clean_asan_run_min_allocs(
        r#"
struct P { name: String, xs: Vec[i64] }
fn grow(r: mut ref Vec[i64]) -> i64 { let t = r; let mut i = 0; while i < 40 { t.push(i); i = i + 1; } t.len() }
fn app(r: mut ref String) -> i64 { let t = r; t.push_str("-x-long-enough-to-force-a-realloc-of-the-buffer"); t.len() }
fn fill(r: mut ref P) { let t = r; t.name.push_str("-grown-past-its-original-capacity-for-sure"); let mut i = 0; while i < 20 { t.xs.push(i); i = i + 1; } }
fn chain(r: mut ref Vec[String]) -> i64 { let t = r; let u = t; let mut i = 0; while i < 10 { u.push(f"s{i}"); i = i + 1; } u.len() }
fn bump(r: mut ref i64) { let t = r; *t = *t + 5; }
fn main() {
    let mut v: Vec[i64] = [1, 2, 3];
    let n = grow(mut v);
    println(f"{n} {v.len()}");
    let mut s = f"payload-{1}-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    let m = app(mut s);
    println(f"{m} {s.len()}");
    let mut p = P { name: f"n{1}", xs: [1] };
    fill(mut p);
    println(f"{p.name} {p.xs.len()}");
    let mut w: Vec[String] = [f"a{0}"];
    let k = chain(mut w);
    println(f"{k} {w.len()} {w[10]}");
    let mut c = 1;
    bump(mut c);
    println(f"{c}");
}
"#,
        &[
            "43 43",
            "87 87",
            "n1-grown-past-its-original-capacity-for-sure 21",
            "11 11 s9",
            "6",
        ],
        "asan_mut_ref_param_alias_realloc_frees_once",
        4,
    );
}
