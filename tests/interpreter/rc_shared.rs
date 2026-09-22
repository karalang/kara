//! shared/RC values, boxing, heap payloads -- fixtures for `tests/interpreter.rs`.
//!
//! Split out of `tests/interpreter.rs` on 2026-09-21. The TEST TARGET is
//! unchanged: this file is a module of that target, so
//! `cargo test --features llvm --test interpreter` still runs everything
//! and CI needs no edit. Run this area alone with:
//!
//!     cargo test --features llvm --test interpreter rc_shared::
//!
//! New fixtures about shared/RC values, boxing, heap payloads belong in this file.

use super::*;

#[test]
fn test_generic_shared_struct_heap_field_aliases() {
    // B-2026-07-13-9 oracle pin. A generic `shared struct Box[T] { v: T }`
    // instantiated at a HEAP type (`Box[String]`) is reference-semantics
    // (RC): `let b = a` aliases the same object, so writing `b.v` is visible
    // through `a.v`. The tree-walk interpreter is the oracle for this shape —
    // it computes the correct "bye" answer — while the native/JIT backend
    // refuses to compile it (the shared heap layout erases the `v: T` field to
    // one word and does not monomorphize per instantiation at v1). This test
    // locks the interpreter's correctness so the backend refusal never
    // masquerades as the language semantics.
    let src = "shared struct Box[T] { mut v: T }
        fn main() {
            let a = Box { v: \"hi\" };
            let b = a;
            b.v = \"bye\";
            println(a.v);
            println(b.v);
        }";
    assert_eq!(run_no_errors(src), "bye\nbye\n");
}

#[test]
fn a_shared_key_is_matched_structurally_not_by_pointer_identity() {
    // B-2026-08-27-4. The compiled backends matched a `shared` map/set key by
    // POINTER IDENTITY: `contains_key` answered `false` for a structurally
    // equal twin, `remove` silently removed nothing, and a `Set` failed to
    // dedup. design.md § Equality settles which side is right -- "`==` always
    // means structural equality ... The `Eq` trait determines `==` regardless
    // of whether the compiler chose RC or owned representation. There is no
    // reference-identity short-circuit" -- and reference identity has its own
    // name, `ref_eq`. So the interpreter was already correct and this pins it
    // as the oracle. Codegen twin:
    // `test_e2e_shared_key_is_matched_structurally`.
    //
    // Four shapes because four distinct emitter paths reach a shared key: a
    // scalar-field struct, a struct with a `String` field (the child eq fn has
    // to recurse, not compare headers), a struct with a nested shared field,
    // and a shared ENUM (whose payload area is not a field list at all).
    let src = "#[derive(Hash, Eq, PartialEq)]
        shared struct Scalar { id: i64, n: i64 }
        #[derive(Hash, Eq, PartialEq)]
        shared struct Named { id: i64, name: String }
        #[derive(Hash, Eq, PartialEq)]
        shared struct Inner { v: i64 }
        #[derive(Hash, Eq, PartialEq)]
        shared struct Outer { id: i64, inner: Inner }
        #[derive(Hash, Eq, PartialEq)]
        shared enum Tag { A { x: i64 }, B }
        fn main() {
            let mut a: Map[Scalar, i64] = Map.new();
            a.insert(Scalar { id: 1, n: 2 }, 7);
            println(f\"scalar={a.contains_key(Scalar { id: 1, n: 2 })}\");

            let mut b: Map[Named, i64] = Map.new();
            b.insert(Named { id: 1, name: f\"aa\" }, 7);
            println(f\"named={b.contains_key(Named { id: 1, name: f\"aa\" })}\");
            println(f\"named-ne={b.contains_key(Named { id: 1, name: f\"ab\" })}\");

            let mut c: Map[Outer, i64] = Map.new();
            c.insert(Outer { id: 1, inner: Inner { v: 5 } }, 7);
            println(f\"nested={c.contains_key(Outer { id: 1, inner: Inner { v: 5 } })}\");
            println(f\"nested-ne={c.contains_key(Outer { id: 1, inner: Inner { v: 6 } })}\");

            let mut d: Map[Tag, i64] = Map.new();
            d.insert(Tag.A { x: 1 }, 7);
            println(f\"enum={d.contains_key(Tag.A { x: 1 })}\");
            println(f\"enum-ne={d.contains_key(Tag.A { x: 2 })}\");

            let mut s: Set[Scalar] = Set.new();
            s.insert(Scalar { id: 1, n: 2 });
            s.insert(Scalar { id: 1, n: 2 });
            println(f\"dedup={s.len()}\");

            let mut m: Map[Named, i64] = Map.new();
            m.insert(Named { id: 1, name: f\"aa\" }, 7);
            m.remove(Named { id: 1, name: f\"aa\" });
            println(f\"removed={m.len()}\");
        }";
    assert_eq!(
        run_no_errors(src),
        "scalar=true\nnamed=true\nnamed-ne=false\n\
         nested=true\nnested-ne=false\n\
         enum=true\nenum-ne=false\n\
         dedup=1\nremoved=0\n"
    );
}

#[test]
fn removing_a_shared_key_releases_it_through_the_refcount() {
    // B-2026-08-27-3, the `shared` key half. `remove` gave back nothing: the
    // per-key drop fn declines for a shared half by design (teardown and
    // `clear` release it through the rc_dec WALK, and a drop fn there would
    // double-dec), and `remove` runs no walk — so the count the map took at
    // `insert` was never returned and the whole control block leaked.
    //
    // Both directions are asserted because the fix is a REFCOUNT dec, not an
    // unconditional destruction, and only the count knows which one is due:
    // with no other reference the body runs AT the remove; with a live alias
    // it must wait for that alias's own end. Running the body beside the dec
    // rather than inside it satisfies the first and breaks the second — and
    // prints twice in the first, which is what it did when measured.
    // Codegen twin: `test_e2e_remove_releases_a_shared_key_through_the_refcount`.
    let last = "#[derive(Hash, Eq, PartialEq)]
        shared struct K { id: i64 }
        impl Drop for K { fn drop(mut ref self) { println(f\"dropK {self.id}\"); } }
        fn main() {
            let mut m: Map[K, i64] = Map.new();
            let k = K { id: 1 };
            m.insert(k, 1);
            println(\"--before--\");
            m.remove(k);
            println(\"--after--\");
        }";
    assert_eq!(run_no_errors(last), "--before--\ndropK 1\n--after--\n");

    let aliased = "#[derive(Hash, Eq, PartialEq)]
        shared struct K { id: i64 }
        impl Drop for K { fn drop(mut ref self) { println(f\"dropK {self.id}\"); } }
        fn main() {
            let mut m: Map[K, i64] = Map.new();
            let k = K { id: 1 };
            let keep = k;
            m.insert(k, 1);
            println(\"--before--\");
            m.remove(keep);
            println(\"--after-remove--\");
            println(f\"keep still {keep.id}\");
            println(\"--end--\");
        }";
    assert_eq!(
        run_no_errors(aliased),
        "--before--\n--after-remove--\nkeep still 1\ndropK 1\n--end--\n"
    );
}

// ── phase-7 L938 user-`impl Drop` for SHARED structs (interpreter) ──
//
// A `shared struct` is `Value::SharedStruct(Arc<…>)`; the user body
// fires when the LAST live reference drops (Arc strong-count → 1 at the
// drain point), mirroring codegen's refcount→0. `drop_target` peeks the
// count without cloning so the test is exact.

#[test]
fn test_shared_struct_structural_equality() {
    // `shared struct` `==`/`!=` is structural (design.md § Equality Semantics):
    // it compares the inner fields, not Arc identity. Two separately-built P's
    // with equal fields are ==; differing a field makes them !=.
    let out = run("#[derive(Eq, PartialEq)]\n\
         shared struct P { x: i64, y: i64 }\n\
         fn main() {\n\
             let a = P { x: 1, y: 2 };\n\
             let b = P { x: 1, y: 2 };\n\
             let c = P { x: 9, y: 2 };\n\
             if a == b { println(\"eq\"); }\n\
             if a != c { println(\"ne\"); }\n\
         }");
    assert_eq!(out, "eq\nne\n");
}

#[test]
fn test_ref_eq_shared_struct_identity() {
    // `ref_eq` is REFERENCE identity (design.md § Equality Semantics): true iff
    // two shared handles point at the same allocation. `b = a` aliases the Arc
    // (true); a separately-built `c` with equal fields is a distinct alloc
    // (false) even though `==` would call it structurally equal. Parity with
    // codegen::test_e2e_ref_eq_shared_identity.
    let out = run("shared struct N { v: i64 }\n\
         fn main() {\n\
             let a = N { v: 1 };\n\
             let b = a;\n\
             let c = N { v: 1 };\n\
             println(ref_eq(a, b));\n\
             println(ref_eq(a, c));\n\
         }");
    assert_eq!(out, "true\nfalse\n");
}

#[test]
fn test_ref_eq_non_shared_is_type_error() {
    fn errs(src: &str) -> Vec<String> {
        let parsed = karac::parse(src);
        let resolved = karac::resolve(&parsed.program);
        let typed = karac::typecheck(&parsed.program, &resolved);
        typed.errors.iter().map(|e| e.message.clone()).collect()
    }
    // Owned struct / primitive: reference identity is not meaningful — error,
    // pointing at `==`.
    assert!(errs("struct P { v: i64 }\nfn main() { let a = P { v: 1 }; let b = P { v: 1 }; println(ref_eq(a, b)); }")
        .iter()
        .any(|e| e.contains("not a `shared` type")));
    assert!(errs("fn main() { println(ref_eq(1, 2)); }")
        .iter()
        .any(|e| e.contains("not a `shared` type")));
}

// ── Shared struct interior mutability ───────────────────────────

#[test]
fn test_shared_struct_aliasing_propagates_mutation() {
    // Per design.md § Part 5: Shared Types — `shared struct` values
    // have reference semantics. `let b = a` clones the Arc; mutations
    // through `b.field` are visible at `a.field` because both bindings
    // point to the same allocation.
    assert_eq!(
        run("shared struct Counter { mut value: i64 }\n\
             fn main() {\n\
                 let a = Counter { value: 1 };\n\
                 let b = a;\n\
                 b.value = 42;\n\
                 println(a.value);\n\
             }"),
        "42\n"
    );
}

#[test]
fn test_shared_struct_field_write_through_struct_field_projection() {
    // Regression (Tangle dogfooding, undo/redo): a `shared struct` reached
    // through a *plain struct field* projection (`holder.cell.value = x`) must
    // write through the shared Arc. `set_field` previously bailed for any
    // receiver that was not a bare identifier / `self`, silently dropping the
    // write — undo/redo over shared state read stale values.
    assert_eq!(
        run("shared struct Cell { mut value: i64 }\n\
             struct Holder { cell: Cell }\n\
             fn main() {\n\
                 let c = Cell { value: 1 };\n\
                 let h = Holder { cell: c };\n\
                 h.cell.value = 99;\n\
                 println(c.value);\n\
             }"),
        "99\n"
    );
}

#[test]
fn test_shared_struct_per_field_independence() {
    // Per design.md § Part 5: \"mutating `node.left` does not conflict
    // with reading `node.right`\". Per-field tracking is the entire
    // point of the spec choosing per-field over struct-wide.
    assert_eq!(
        run("shared struct Node { mut left: i64, mut right: i64 }\n\
             fn main() {\n\
                 let n = Node { left: 1, right: 2 };\n\
                 n.left = 10;\n\
                 println(n.left + n.right);\n\
             }"),
        "12\n"
    );
}

#[test]
fn test_shared_struct_immutable_field_persists() {
    // An immutable field set at construction is visible across all
    // holders and is never mutated afterwards.
    assert_eq!(
        run("shared struct Node { id: i64, mut value: i64 }\n\
             fn main() {\n\
                 let n = Node { id: 7, value: 0 };\n\
                 let m = n;\n\
                 m.value = 99;\n\
                 println(n.id);\n\
                 println(m.value);\n\
             }"),
        "7\n99\n"
    );
}

#[test]
fn test_shared_struct_mutation_through_method() {
    // Method dispatch on `shared struct` binds `self` to a SharedStruct
    // value (Arc clone). `self.field = x` inside the method writes
    // through the shared allocation.
    assert_eq!(
        run("shared struct Counter { mut value: i64 }\n\
             impl Counter {\n\
                 fn bump(ref self) { self.value = self.value + 1; }\n\
             }\n\
             fn main() {\n\
                 let c = Counter { value: 0 };\n\
                 c.bump();\n\
                 c.bump();\n\
                 c.bump();\n\
                 println(c.value);\n\
             }"),
        "3\n"
    );
}

#[test]
fn test_shared_struct_aliased_through_method_argument() {
    // Passing a `shared struct` to a function that mutates through it
    // is visible at the caller — same Arc allocation.
    assert_eq!(
        run("shared struct Box { mut value: i64 }\n\
             fn bump(b: ref Box) { b.value = b.value + 100; }\n\
             fn main() {\n\
                 let x = Box { value: 5 };\n\
                 bump(x);\n\
                 bump(x);\n\
                 println(x.value);\n\
             }"),
        "205\n"
    );
}

#[test]
fn test_shared_struct_per_field_independence_across_methods() {
    // Pins the canonical spec example from design.md:8186 —
    // \"mutating `node.left` does not conflict with reading `node.right`\".
    // Independent methods touching different fields never see a
    // borrow-conflict panic.
    assert_eq!(
        run("shared struct Node { mut left: i64, mut right: i64 }\n\
             impl Node {\n\
                 fn write_left(ref self, x: i64) { self.left = x; }\n\
                 fn read_right(ref self) -> i64 { self.right }\n\
             }\n\
             fn main() {\n\
                 let n = Node { left: 0, right: 7 };\n\
                 n.write_left(99);\n\
                 println(n.read_right());\n\
                 println(n.left);\n\
             }"),
        "7\n99\n"
    );
}

#[test]
fn test_interpreter_lock_break_releases_then_reacquire() {
    // Parity with codegen `test_e2e_lock_break_releases_then_reacquire`:
    // a `break` out of a lock body persists the mutations made before it
    // and releases the lock (the interpreter drops the guard before
    // propagating the control flow), so the post-loop re-acquire succeeds.
    // 3 pre-break increments, then the re-read prints 3.
    assert_eq!(
        run("fn main() {\n\
             let m = Mutex.new(0);\n\
             let mut i = 0;\n\
             loop {\n\
                 lock m x {\n\
                     if i >= 3 { break; }\n\
                     x = x + 1;\n\
                 }\n\
                 i = i + 1;\n\
             }\n\
             lock m v { println(v); }\n\
         }"),
        "3\n"
    );
}

#[test]
fn test_interpreter_lock_return_releases_then_reacquire() {
    // Parity with codegen `test_e2e_lock_return_releases_then_reacquire`:
    // an early `return` out of a lock body releases the lock, so the
    // caller can re-acquire the same mutex. 7 (returned) + 7 (re-read).
    assert_eq!(
        run("fn take(m: mut ref Mutex[i64]) -> i64 {\n\
             lock m x { return x; }\n\
             0\n\
         }\n\
         fn main() {\n\
             let mut m = Mutex.new(7);\n\
             let a = take(mut m);\n\
             lock m v { println(a + v); }\n\
         }"),
        "14\n"
    );
}

#[test]
fn test_with_provider_mut_ref_self_mutates_heap_field() {
    // The write-back must carry a heap-field mutation too — `put` pushes onto a
    // `Vec[String]` provider field, and a later `count()` must see all three.
    let output = run(
        "trait Store { fn put(mut ref self, k: String); fn count(ref self) -> i64; }
         effect resource Db: Store;
         struct Mem { keys: Vec[String] }
         impl Store for Mem {
             fn put(mut ref self, k: String) { self.keys.push(k); }
             fn count(ref self) -> i64 { self.keys.len() }
         }
         fn seed() with writes(Db) { Db.put(\"a\"); Db.put(\"b\"); Db.put(\"c\"); }
         fn total() -> i64 with reads(Db) { Db.count() }
         fn main() {
             with_provider[Db](Mem { keys: [] }, || { seed(); println(f\"{total()}\"); });
         }",
    );
    assert_eq!(output, "3\n");
}

#[test]
fn test_pool_release_returns_slot_for_next_acquire() {
    // Saturate the pool, release one connection, acquire again —
    // the released slot should be handed back without minting fresh
    // (the value is recycled from the slot vec). Verifies both
    // `release` populating slots and `acquire` consuming from slots
    // ahead of the create_fn-mint path.
    let output = run(r#"fn make_int() -> i64 { 99 }
         fn main() {
             let pool: Pool[i64] = Pool.new(make_int, 1, 4);
             match pool.acquire(0) {
                 Ok(c1) => {
                     pool.release(c1);
                     match pool.acquire(0) {
                         Ok(c2) => println(c2.val),
                         Err(_) => println("acq2_err"),
                     }
                 }
                 Err(_) => println("acq1_err"),
             }
         }"#);
    assert_eq!(output, "99\n");
}

#[test]
fn shared_struct_mut_field_still_persists() {
    // Regression guard: the par/shared SharedStruct change must not break the
    // existing `shared struct` `mut`-field mutation path.
    let out = run("shared struct Box { mut v: i64 }
         impl Box { fn set(ref self, x: i64) { self.v = x; } fn get(ref self) -> i64 { self.v } }
         fn main() { let b = Box { v: 1 }; b.set(99); println(b.get()); }");
    assert_eq!(out.trim(), "99");
}

#[test]
fn test_priority_queue_min_and_max_at_scalar_and_heap_t() {
    // Phase-11 `PriorityQueue[T: Ord]` on the tree-walk backend. The compiled
    // twin is `tests/codegen.rs`'s
    // `e2e_stdlib_priority_queue_min_and_max_at_scalar_and_heap_t`, and the two
    // assert the SAME expected string on purpose: this module's bodies are real
    // Kāra compiled through the baked-stdlib path, so interpreter/codegen
    // parity is the property that keeps one source honest across two backends.
    // No `import` — `PriorityQueue` is prelude-visible like `SortedSet`.
    let out = run(r#"
fn main() {
    let mut q: PriorityQueue[i64] = PriorityQueue.new();
    q.push(5); q.push(1); q.push(4); q.push(2); q.push(3);
    println(q.len());
    println(q.is_empty());
    let a = q.into_sorted_vec();
    let mut i = 0;
    while i < a.len() { println(a[i]); i = i + 1; }
    let mut m: PriorityQueue[i64] = PriorityQueue.max_first();
    m.push(5); m.push(1); m.push(4);
    let b = m.into_sorted_vec();
    let mut j = 0;
    while j < b.len() { println(b[j]); j = j + 1; }
    let v: Vec[i64] = [9, 7, 8, 1, 3];
    let c = PriorityQueue.from(v).into_sorted_vec();
    let mut k = 0;
    while k < c.len() { println(c[k]); k = k + 1; }
    let mut s: PriorityQueue[String] = PriorityQueue.new();
    s.push("pear"); s.push("apple"); s.push("fig");
    let d = s.into_sorted_vec();
    let mut z = 0;
    while z < d.len() { println(d[z]); z = z + 1; }
    let e = PriorityQueue.max_first_from(["pear", "apple", "fig"]).into_sorted_vec();
    let mut w = 0;
    while w < e.len() { println(e[w]); w = w + 1; }
    let mut f: PriorityQueue[i64] = PriorityQueue.new();
    println(f.is_empty());
    match f.pop() { Some(x) => { println(x); } None => { println("none"); } }
    let empty: Vec[i64] = [];
    let g = PriorityQueue.from(empty).into_sorted_vec();
    println(g.len());
}
"#);
    assert_eq!(
        out,
        "5\nfalse\n1\n2\n3\n4\n5\n5\n4\n1\n1\n3\n7\n8\n9\n\
         apple\nfig\npear\npear\nfig\napple\ntrue\nnone\n0\n"
    );
}

/// B-2026-08-13-6 — interpreter twin of `tests/codegen.rs`'s
/// `test_e2e_shared_struct_heap_field_read_is_a_copy`, same source and expected
/// string.
///
/// The interpreter has reference semantics for a `shared struct` and value
/// semantics for the field READ, which is exactly the combination the compiled
/// fix had to reproduce: the second handle sees the same object, and binding
/// the field out of either handle leaves the object's field intact. Pinning it
/// here keeps the oracle for that pair explicit.
#[test]
fn test_shared_struct_heap_field_read_is_a_copy() {
    assert_eq!(
        run("shared struct Inner { word: String }\n\
             struct Holder { inner: Inner, tag: i64 }\n\
             fn main() {\n\
                 let k = 1;\n\
                 let i = Inner { word: f\"a{k}\" };\n\
                 let w = i.word;\n\
                 println(w);\n\
                 println(i.word);\n\
                 let j = i;\n\
                 println(j.word);\n\
                 let mut hs: Vec[Holder] = Vec.new();\n\
                 hs.push(Holder { inner: Inner { word: f\"c{k}\" }, tag: 2 });\n\
                 let hop = hs[0].inner.word;\n\
                 println(hop);\n\
                 println(hs[0].inner.word);\n\
                 println(hs[0].tag);\n\
             }"),
        "a1\na1\na1\nc1\nc1\n2\n"
    );
}

/// B-2026-07-31-37 (heap face) — interpreter twin of `tests/codegen.rs`'s
/// `e2e_struct_assign_move_heap_body_once`, same source and expected string.
/// The interpreter's pre-fix behavior was `D7 D7 x` (double body, right
/// length both times — codegen's first firing additionally read a zeroed
/// length); the assign-move suppression makes it `D3 D7 x` everywhere.
#[test]
fn test_struct_assign_move_heap_body_once() {
    assert_eq!(
        run("struct Res { data: Vec[i64] }\n\
             impl Drop for Res {\n\
                 fn drop(mut ref self) {\n\
                     println(f\"D{self.data.len()}\")\n\
                 }\n\
             }\n\
             fn mk(n: i64) -> Res {\n\
                 let mut v: Vec[i64] = Vec.new();\n\
                 let mut i = 0;\n\
                 while i < n {\n\
                     v.push(i);\n\
                     i = i + 1;\n\
                 }\n\
                 Res { data: v }\n\
             }\n\
             fn main() {\n\
                 let mut a = mk(3);\n\
                 let b = mk(7);\n\
                 a = b;\n\
                 println(\"x\");\n\
             }\n"),
        "D3\nD7\nx\n"
    );
}

/// B-2026-08-06-15 — the ORACLE half. The interpreter has no refcount to strand,
/// so a `shared` handle escaping a value-position block has always been correct
/// there; codegen transferred the box's only ref out via the block-tail
/// null-store and then took a receive-inc for it anyway, leaving the count at 1
/// forever.
///
/// Pins the reference value its codegen twin
/// `asan_shared_field_escaping_a_value_block_transfers_exactly_one_ref` is
/// measured against. That twin is an ASAN fixture rather than an E2E one — the
/// opposite of B-2026-08-06-14's — because the output is right before and after
/// and only a leak gate can see the defect.
///
/// Passes before and after, which is its job. Seed is a literal for the usual
/// reason: an in-process interpreter test would see the TEST binary's argv.
#[test]
fn test_shared_field_escaping_a_value_block_transfers_the_handle() {
    assert_eq!(
        run(r#"shared struct Node { s: String }
struct Box[T] { v: T }
struct Holder { v: Node }

fn mk(i: i64) -> Node { return Node { s: f"blk-{i}-padded-out-to-force-a-real-heap-buffer" }; }

fn main() {
    let mut acc: i64 = 0;
    // generic and concrete spellings of the escaping block tail
    let x1 = { let b = Box { v: mk(1) }; b.v };
    acc = acc + x1.s.len();
    let x2 = { let h = Holder { v: mk(2) }; h.v };
    acc = acc + x2.s.len();
    // a nested value block
    let x3 = { { let b = Box { v: mk(3) }; b.v } };
    acc = acc + x3.s.len();
    // the non-block control, whose receive-inc must stay
    let b4 = Box { v: mk(4) };
    let x4 = b4.v;
    acc = acc + x4.s.len();
    println(acc);
}
"#),
        "176\n"
    );
}

/// B-2026-08-06-14 — the ORACLE half. The interpreter has no refcount to get
/// wrong, so it has always handed a `shared` field out of a by-value or `ref`
/// struct param correctly; codegen dec'd the rc box one below zero because the
/// CALLER-RETAINS regime leaves the caller's ref live and the returned alias got
/// none of its own.
///
/// Pins the reference value its codegen twin
/// `e2e_shared_field_returned_from_a_caller_retains_param_keeps_its_ref` is
/// measured against — that twin has to be an E2E rather than an ASAN fixture,
/// because the defect is a use-after-free READ out of uninstrumented generated
/// code and the sanitizer cannot see it.
///
/// Passes before and after, which is its job. Seed is a literal for the usual
/// reason: an in-process interpreter test would see the TEST binary's argv.
#[test]
fn test_shared_field_returned_from_a_param_transfers_the_handle() {
    assert_eq!(
        run(r#"shared struct Node { s: String }
struct Holder { v: Node }
struct Box[T] { v: T }

fn mk(i: i64) -> Node { return Node { s: f"crp-{i}-padded-out-to-force-a-real-heap-buffer" }; }

fn ret_byval(b: Holder) -> Node { return b.v; }
fn tail_byval(b: Holder) -> Node { b.v }
fn ret_ref(b: ref Holder) -> Node { return b.v; }
fn ret_generic(b: Box[Node]) -> Node { return b.v; }

fn main() {
    let mut acc: i64 = 0;
    // caller-retains: by-value return, by-value tail, and `ref`
    let h1 = Holder { v: mk(1) };
    let x1 = ret_byval(h1);
    acc = acc + x1.s.len();
    let h2 = Holder { v: mk(2) };
    let x2 = tail_byval(h2);
    acc = acc + x2.s.len();
    let h3 = Holder { v: mk(3) };
    let x3 = ret_ref(h3);
    acc = acc + x3.s.len();
    // owned-by-transfer control
    let b4 = Box { v: mk(4) };
    let x4 = ret_generic(b4);
    acc = acc + x4.s.len();
    println(acc);
}
"#),
        "176\n"
    );
}

/// B-2026-08-06-8 — the ORACLE half. The interpreter has always transferred a
/// generic wrapper's bare-`T` `shared` field correctly; codegen's gate for "does
/// this local need the shared-field rc-dec walker" read declared field types, so
/// `Box[T] { v: T }` at `T = Node` never rc-dec'd the box (a leak) — while the
/// concrete `Holder { v: Node }` always did.
///
/// The block-tail leg is the one that matters here: it is the shape whose
/// codegen twin printed a WRONG ANSWER (0, then 114 for the full fixture) rather
/// than merely leaking, so this pins the reference value that twin is measured
/// against. Memory correctness is not observable from here, only the value.
///
/// Passes before and after, which is its job. Seed is a literal for the usual
/// reason: an in-process interpreter test would see the TEST binary's argv.
#[test]
fn test_bare_generic_param_shared_field_transfers_the_handle() {
    assert_eq!(
        run(r#"shared struct Node { s: String }
struct Box[T] { v: T }
struct Holder { v: Node }

fn consume(b: Box[Node]) -> i64 { let x = b.v; return x.s.len(); }

fn main() {
    let mut acc: i64 = 0;
    // the field escaping a value-position block, both spellings
    let x1 = { let b = Box { v: Node { s: "padded-out-to-force-a-real-heap-buffer" } }; b.v };
    acc = acc + x1.s.len();
    let x2 = { let h = Holder { v: Node { s: "padded-out-to-force-a-real-heap-buffer" } }; h.v };
    acc = acc + x2.s.len();
    // the plain move-out, a whole-struct move, and a never-moved control
    let b3 = Box { v: Node { s: "padded-out-to-force-a-real-heap-buffer" } };
    let x3 = b3.v;
    acc = acc + x3.s.len();
    let b4 = Box { v: Node { s: "padded-out-to-force-a-real-heap-buffer" } };
    let b5 = b4;
    acc = acc + consume(b5);
    let b6 = Box { v: Node { s: "padded-out-to-force-a-real-heap-buffer" } };
    acc = acc + 1;
    println(acc);
}
"#),
        "153\n"
    );
}

/// B-2026-08-05-41, the interpreter side. The interpreter was ALREADY correct
/// for a shared-struct receiver — it printed 8 for every arm below while AOT
/// and JIT printed 7 — which is precisely how the codegen half was localized.
/// Pinning it here keeps the reference leg from silently drifting to match a
/// future codegen regression, and mirrors the arms of the codegen twin
/// `e2e_mut_ref_place_argument_shared_receiver_writes_back`.
///
/// The mutability side of the same row (an undeclared-`mut` shared field
/// handed to a `mut ref` parameter is now REFUSED at typecheck, as the
/// assignment spelling already was) is covered in `tests/typechecker.rs`;
/// every field written here is declared `mut`, which is what makes these
/// programs legal at all.
#[test]
fn test_mut_ref_place_argument_shared_receiver_writes_back() {
    assert_eq!(
        run("shared struct N { mut val: i64 }\n\
             shared struct T { mut s: String }\n\
             shared struct Inner { mut v: i64 }\n\
             shared struct Outer { mut inner: Inner }\n\
             impl N {\n\
                 fn go(ref self) { bump(mut self.val); }\n\
             }\n\
             fn bump(x: mut ref i64) { x = x + 1i64; }\n\
             fn app(x: mut ref String, tag: String) { x = x + tag; }\n\
             fn viaref(n: ref N) -> i64 { bump(mut n.val); return n.val; }\n\
             fn main() {\n\
                 let n: i64 = 1i64;\n\
                 let a = N { val: n + 6i64 };\n\
                 bump(mut a.val);\n\
                 println(f\"a:{a.val}\");\n\
                 let b = N { val: n + 6i64 };\n\
                 println(f\"b:{viaref(b)}\");\n\
                 let c = N { val: n + 6i64 };\n\
                 c.go();\n\
                 println(f\"c:{c.val}\");\n\
                 let di = Inner { v: n + 6i64 };\n\
                 let d = Outer { inner: di };\n\
                 bump(mut d.inner.v);\n\
                 println(f\"d:{d.inner.v}\");\n\
                 let e = T { s: \"a\" };\n\
                 app(mut e.s, \"b\");\n\
                 println(f\"e:{e.s}:{e.s.len()}\");\n\
             }\n"),
        "a:8\nb:8\nc:8\nd:8\ne:ab:2\n"
    );
}

/// B-2026-08-28-42's ORACLE. A heap read off an element of a container held in
/// a struct FIELD is a COPY: the container still has the value afterwards. The
/// interpreter has always behaved this way, and it is what the compiled
/// backends were measured against — they aborted with `free(): double free
/// detected in tcache 2` on each of these while `--interp` printed both lines.
///
/// This test therefore PASSES against the unfixed compiler BY DESIGN. Its job
/// is to pin the semantics the codegen twin
/// (`e2e_field_rooted_container_element_heap_read_is_cloned`) is asserted
/// against, so a later change cannot quietly move the oracle to meet a broken
/// backend — which matters more than usual here, because the rejected
/// alternative (cap-zeroing the source instead of cloning the read) would make
/// the second line of every case print empty and could be mistaken for correct
/// if this file agreed with it.
#[test]
fn field_rooted_container_element_heap_read_is_a_copy() {
    const DECL: &str = "struct R { id: i64, name: String }\n";
    let cases: &[(&str, &str, &str)] = &[
        (
            "vec-field",
            "struct H { xs: Vec[R] }\n\
             fn main() { let h = H { xs: [R { id: 41, name: f\"n{41}\" }] };\n\
             let s = h.xs[0].name; println(f\"{s}\"); println(f\"{h.xs[0].name}\") }",
            "n41\nn41\n",
        ),
        (
            "array-field",
            "struct H { xs: Array[R, 1] }\n\
             fn main() { let h = H { xs: [R { id: 41, name: f\"n{41}\" }] };\n\
             let s = h.xs[0].name; println(f\"{s}\"); println(f\"{h.xs[0].name}\") }",
            "n41\nn41\n",
        ),
        (
            "self-rooted-return",
            "struct H { xs: Vec[R] }\n\
             impl H { fn peek(ref self) -> String { self.xs[0].name } }\n\
             fn main() { let h = H { xs: [R { id: 41, name: f\"n{41}\" }] };\n\
             println(f\"{h.peek()}\"); println(f\"{h.xs[0].name}\") }",
            "n41\nn41\n",
        ),
        (
            "field-rooted-tuple-hop",
            "struct H { xs: Vec[(R, i64)] }\n\
             fn main() { let h = H { xs: [(R { id: 41, name: f\"n{41}\" }, 1)] };\n\
             let x = h.xs[0].0; println(f\"{x.id} {x.name}\"); println(f\"{h.xs[0].0.name}\") }",
            "41 n41\nn41\n",
        ),
        // The copy is INDEPENDENT, not just readable: mutating the binding
        // leaves the container's element untouched. This is the property a
        // move-model "fix" would break while still passing the reads above.
        (
            "copy-is-independent",
            "struct H { xs: Vec[R] }\n\
             fn main() { let h = H { xs: [R { id: 41, name: f\"n{41}\" }] };\n\
             let mut s = h.xs[0].name; s = s + \"X\";\n\
             println(f\"{s}\"); println(f\"{h.xs[0].name}\") }",
            "n41X\nn41\n",
        ),
    ];
    for (label, body, want) in cases {
        assert_eq!(run(&format!("{DECL}{body}\n")), *want, "[{label}]");
    }
}

/// B-2026-09-06-62 — the interpreter was the CORRECT column throughout this
/// row (the body count never moved and no memory fault was possible), so this
/// twin exists to hold the compiled string to it.
///
/// Twin of `tests/codegen.rs`'s `e2e_owned_self_rebind_of_a_shared_field_struct`, pinned to the same string.
#[test]
fn test_owned_self_rebind_of_a_shared_field_struct() {
    assert_eq!(
        run(r#"shared struct Inner { v: i64 }
struct R { id: i64, name: String, inner: Inner }
impl Drop for R { fn drop(mut ref self) { println(f"  dR{self.id}") } }
struct P { id: i64, name: String, xs: Vec[i64] }
impl Drop for P { fn drop(mut ref self) { println(f"  dP{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, name: f"h{i}", inner: Inner { v: i } }; }
fn mkp(i: i64) -> P { return P { id: i, name: f"p{i}", xs: [i] }; }
impl R {
    fn take(self) -> i64 { let m = self; return m.id; }
    fn twice(self) -> i64 { let m = self; let n = m; return n.id; }
    fn plain(self) -> i64 { return self.id; }
    fn borrowed(ref self) -> i64 { return self.id; }
}
impl P { fn take(self) -> i64 { let m = self; return m.id; } }
fn top(r: R) -> i64 { let m = r; return m.id; }

fn main() {
    println("temp_receiver"); println(f"  v={mk(1).take()}");
    println("named_receiver"); let a = mk(2); println(f"  v={a.take()}");
    println("twice"); println(f"  v={mk(3).twice()}");
    println("borrowed"); let b = mk(5); println(f"  v={b.borrowed()}");
    println("copyable_struct"); println(f"  v={mkp(6).take()}");
    println("free_function"); println(f"  v={top(mk(7))}");
    println("end");
}
"#),
        r#"temp_receiver
  dR1
  v=1
named_receiver
  dR2
  v=2
twice
  dR3
  v=3
borrowed
  v=5
  dR5
copyable_struct
  dP6
  v=6
free_function
  dR7
  v=7
end
"#
    );
}

#[test]
fn test_discarded_branch_of_body_less_heap_literals_keeps_one_owner() {
    let hdr = "struct P { a: String, b: i64 }\n\
               struct D { a: String, b: i64 }\n\
               impl Drop for D { fn drop(mut ref self) { println(f\"dD{self.b}\") } }\n\
               struct W { r: D, b: i64 }\n\
               fn pay() -> String { return \"heap\"; }\n\
               fn mkd(n: i64) -> D { return D { a: pay(), b: n }; }\n\
               fn mkw(n: i64) -> W { return W { r: mkd(n), b: n }; }\n";
    for (label, body, want) in [
        (
            "body-less literal through a bare if",
            "if n == 0 { P { a: pay(), b: 1 } } else { P { a: pay(), b: 2 } };",
            "end\n",
        ),
        (
            "body-less literal through a wildcard let match",
            "let _ = match n { 0 => { P { a: pay(), b: 1 } } _ => { P { a: pay(), b: 2 } } };",
            "end\n",
        ),
        (
            "own-Drop literal through a branch stays single",
            "let _ = if n == 0 { D { a: pay(), b: 1 } } else { D { a: pay(), b: 2 } };",
            "dD1\nend\n",
        ),
        (
            "own-Drop call through a branch stays single",
            "let _ = if n == 0 { mkd(1) } else { mkd(2) };",
            "dD1\nend\n",
        ),
        (
            "Drop-bearing field through a branch stays single",
            "let _ = if n == 0 { W { r: mkd(1), b: 1 } } else { W { r: mkd(2), b: 2 } };",
            "dD1\nend\n",
        ),
        (
            "control: direct own-Drop literal discard",
            "let _ = D { a: pay(), b: 1 };",
            "dD1\nend\n",
        ),
        (
            "control: bare-statement own-Drop literal discard",
            "D { a: pay(), b: 1 };",
            "dD1\nend\n",
        ),
        (
            "guard declines the aliasing shape and it is still single",
            "let _ = if n == 0 { W { r: mkw(7).r, b: 1 } } else { W { r: mkd(2), b: 2 } };",
            "dD7\nend\n",
        ),
        (
            "guard admits arms that mint their own field",
            "let _ = if n == 0 { W { r: mkd(7), b: 1 } } else { W { r: mkd(2), b: 2 } };",
            "dD7\nend\n",
        ),
    ] {
        let src = format!("{hdr}fn main() {{\nlet n = 0;\n{body}\nprintln(\"end\");\n}}\n");
        assert_eq!(run(&src), want, "[{label}]");
    }
    // B-2026-08-31-35 — the WHOLE-LOCAL move is FIXED on this backend too, and
    // asserted as correct. The codegen twin
    // (`e2e_discarded_branch_of_body_less_heap_literals_keeps_one_owner`)
    // carries the same assertion, so the two cannot drift apart: the pair was
    // pinned on both sides precisely so a one-sided change shows up here, and
    // that is how this one was kept honest while it was being written.
    {
        let src = format!(
            "{hdr}fn main() {{\nlet n = 0;\nlet t = mkd(7);\n\
             let _ = if n == 0 {{ W {{ r: t, b: 1 }} }} else {{ W {{ r: mkd(2), b: 2 }} }};\n\
             println(\"end\");\n}}\n"
        );
        assert_eq!(
            run(&src),
            "dD7\nend\n",
            "[discarded arm literal consuming a whole local runs one body]"
        );
    }
    // B-2026-09-01-17 — the PROJECTED spelling is FIXED and asserted as correct.
    // It ran the local's body twice because `compile_struct_init`'s field loop
    // (and this backend's `eval_struct_literal` twin) carried only the MEMORY
    // half of a struct-field move-out, so `t`'s field-bodies walk stayed armed
    // over the moved-out leaf. Landed AFTER B-2026-09-13-26, and that order is
    // load-bearing: until a discarded array literal had an owner, standing the
    // source down took the array spelling's body to ZERO rather than to one.
    {
        let src = format!(
            "{hdr}fn main() {{\nlet n = 0;\nlet t = mkw(7);\n\
             let _ = if n == 0 {{ W {{ r: t.r, b: 1 }} }} else {{ W {{ r: mkd(2), b: 2 }} }};\n\
             println(\"end\");\n}}\n"
        );
        assert_eq!(
            run(&src),
            "dD7\nend\n",
            "[discarded arm literal consuming a local's FIELD runs one body]"
        );
    }
}

/// B-2026-09-08-10 — `--interp` must DESTROY the field an RC-fallback-promoted
/// base retains, not mask it out of the base's walk.
///
/// `while i < N { let taken = g.one; }` promotes `g` (a consume inside a loop),
/// and a promoted base RETAINS its field while the destination takes a COPY —
/// the rule `projection_root_is_rc_boxed` states and the compiled backends
/// follow. The interpreter recorded a MOVE-OUT instead and masked `one` out of
/// `g`'s walk, so it ran N bodies where N+1 values exist and lost the surviving
/// original's outright:
///
/// ```text
///   N=1   --interp   t1 dS1 dS2 m3            jit/aot   t1 dS1 m3 dS2 dS1
///   N=3   --interp   t1 dS1 x3, dS2, m3       jit/aot   t1 dS1 x3, m3, dS2 dS1
/// ```
///
/// Its own output is what convicted it rather than the comparison: it printed
/// `t1` on EVERY trip, so it agreed the source was live and not a husk, yet ran
/// `dS2` alone at `g`'s death — destroying three copies of a value it also
/// claimed was moved once, and the original zero times.
///
/// THE ROOT WAS A PLUMBING GAP, not a wrong decision in a helper:
/// `Interpreter::new` took only the program and the typecheck result, and
/// nothing under `src/interpreter*` referenced `rc_values` at all, so the one
/// signal separating "moved" from "retained by a promoted base" had no path in.
///
/// ONLY THE BODY COUNT IS ASSERTED HERE. These cells also differ in drop
/// PLACEMENT — the interpreter drops `g` at the binding's live-range end
/// (before `m3`), the compiled backends at lexical scope exit (after) — which
/// is a separate, independent row (B-2026-09-04-32) that this fix deliberately
/// does not touch. The zero-trip cell proves the two are separable: its body
/// never runs, so no mask is ever recorded, and it diverges in placement alone
/// both before and after.
#[test]
fn rc_promoted_base_still_destroys_its_retained_field() {
    const PRE: &str = "struct Rs { id: i64, name: String }\n\
         impl Drop for Rs { fn drop(mut ref self) { println(f\"dS{self.id}\") } }\n\
         fn mks(i: i64) -> Rs { return Rs { id: i, name: f\"h{i}\" }; }\n\
         struct Bs { mut one: Rs, mut two: Rs }\n";
    // N=1 — one copy taken, so `dS1` twice: the copy's and the retained
    // original's, plus `dS2` for the untouched sibling.
    let (out1, _) = run_program_with_drops(&format!(
        "{PRE}fn main() {{\n\
         \x20 let mut g = Bs {{ one: mks(1), two: mks(2) }};\n\
         \x20 let mut i = 0;\n\
         \x20 while i < 1 {{ let taken = g.one; println(f\"t{{taken.id}}\"); i = i + 1; }}\n\
         \x20 println(\"m3\");\n}}\n"
    ));
    assert_eq!(out1, vec!["t1\n", "dS1\n", "dS2\n", "dS1\n", "m3\n"]);
    // N=3 — the count scales with the trip count and the original still dies
    // exactly once.
    let (out3, _) = run_program_with_drops(&format!(
        "{PRE}fn main() {{\n\
         \x20 let mut g = Bs {{ one: mks(1), two: mks(2) }};\n\
         \x20 let mut i = 0;\n\
         \x20 while i < 3 {{ let taken = g.one; println(f\"t{{taken.id}}\"); i = i + 1; }}\n\
         \x20 println(\"m3\");\n}}\n"
    ));
    assert_eq!(
        out3,
        vec!["t1\n", "dS1\n", "t1\n", "dS1\n", "t1\n", "dS1\n", "dS2\n", "dS1\n", "m3\n"]
    );
    // CONTROL — a genuine MOVE: `g` is never re-used, so the ownership pass
    // does not promote it and the mask must still apply. `g`'s walk runs `dS2`
    // only, and this cell is byte-identical on `--interp` and the JIT.
    let (outm, _) = run_program_with_drops(&format!(
        "{PRE}fn main() {{\n\
         \x20 let g = Bs {{ one: mks(1), two: mks(2) }};\n\
         \x20 let taken = g.one;\n\
         \x20 println(f\"t{{taken.id}}\");\n\
         \x20 println(\"m3\");\n}}\n"
    ));
    assert_eq!(outm, vec!["dS2\n", "t1\n", "dS1\n", "m3\n"]);
}
