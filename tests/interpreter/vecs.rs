//! Vec -- fixtures for `tests/interpreter.rs`.
//!
//! Split out of `tests/interpreter.rs` on 2026-09-21. The TEST TARGET is
//! unchanged: this file is a module of that target, so
//! `cargo test --features llvm --test interpreter` still runs everything
//! and CI needs no edit. Run this area alone with:
//!
//!     cargo test --features llvm --test interpreter vecs::
//!
//! New fixtures about Vec belong in this file.

use super::*;

/// B-2026-09-01-6 — the interpreter twin of `tests/codegen.rs`'s
/// `e2e_ref_binding_over_a_nested_vec_chain`, pinned to the same string.
///
/// This side was correct throughout — `v[i]` evaluates to the inner
/// `Value::Array` and the existing arm matches it — so this half is the ORACLE
/// the compiled side was built against, and it passes before the fix as well.
/// It is here so the two backends stay pinned to one string as the nested
/// lowering grows: the remaining ROOTS (an `Array` of `Vec`, a `Vec` of
/// `Array`, a struct field, a `Slice`) still decline on the compiled side and
/// are tracked separately.
#[test]
fn test_ref_binding_over_a_nested_vec_chain() {
    assert_eq!(
        run("fn main() {\n\
     let d: Vec[Vec[i64]] = [[1, 2], [3, 4]];\n\
     let g2 = ref d[0][1];\n\
     println(f\"double {g2}\");\n\
     let t: Vec[Vec[Vec[i64]]] = [[[5, 6]]];\n\
     let g3 = ref t[0][0][1];\n\
     println(f\"triple {g3}\");\n\
     let q: Vec[Vec[Vec[Vec[i64]]]] = [[[[7, 8]]]];\n\
     let g4 = ref q[0][0][0][1];\n\
     println(f\"quad {g4}\");\n\
     let s: Vec[Vec[String]] = [[\"a\", \"b\"]];\n\
     let gs = ref s[0][1];\n\
     println(f\"str {gs}\");\n\
     let mut m: Vec[Vec[i64]] = [[1, 2]];\n\
     let ga = ref m[0][1];\n\
     m[0][1] = 99;\n\
     println(f\"alias {ga}\");\n\
     let f: Vec[i64] = [10, 20];\n\
     let gf = ref f[1];\n\
     println(f\"flat {gf}\");\n\
     }"),
        "double 2\ntriple 6\nquad 8\nstr b\nalias 99\nflat 20\n"
    );
}

#[test]
fn vec_equality_is_content_equality_in_the_interpreter() {
    // B-2026-08-27-10. The interpreter had NO `Array` arm in its binop
    // dispatcher, so `Vec[T] == Vec[T]` fell to the catch-all whose message
    // says "this is a type error the typechecker reports as a hard error".
    // That claim was false: `karac check` printed "All checks passed." on the
    // same file, deliberately -- `type_supports_partial_eq` has recursed into
    // a `Vec`'s element since B-2026-08-18-6, and design.md § Conditional
    // `impl` Blocks specifies `impl[T: Eq] Eq for Vec[T]` outright.
    //
    // So the interpreter was the half that had to move. Asserting the ANSWERS
    // rather than the absence of an error is what makes this a regression test
    // for the compiled backends too: they were silently WRONG here, not
    // erroring, and this is the oracle their E2E twin
    // (`test_e2e_vec_equality_compares_contents_not_bytes`) is checked against.
    let src = "fn cmp_ref(a: ref Vec[i64], b: ref Vec[i64]) -> bool { return a == b; }
        fn main() {
            let mut a: Vec[i64] = Vec.new();
            a.push(1);
            a.push(2);
            let mut c: Vec[i64] = Vec.new();
            c.push(1);
            c.push(9);
            let mut d: Vec[i64] = Vec.new();
            d.push(1);
            println(f\"{a == a}\");
            println(f\"{a == c}\");
            println(f\"{a == d}\");
            println(f\"{a != c}\");
            println(f\"{cmp_ref(a, c)}\");
            let mut p: Vec[String] = Vec.new();
            p.push(\"hello\");
            let mut q: Vec[String] = Vec.new();
            q.push(\"hello\");
            println(f\"{p == q}\");
        }";
    // `a == c` is the leg that answered TRUE on the compiled backends: both
    // vecs have length 2, and the string comparison they fell into memcmp'd
    // two BYTES of the i64 element buffers, which are `01 00` in both.
    assert_eq!(
        run_no_errors(src),
        "true\nfalse\nfalse\ntrue\nfalse\ntrue\n"
    );
}

#[test]
fn vec_contains_matches_an_enum_element_by_content() {
    // B-2026-08-27-13 — `Vec.contains` compared an ENUM element by its raw
    // payload words, reaching neither of the compiler's two structural enum
    // comparators, so it answered `false` for an element that IS in the vector.
    //
    // WHY NEITHER WAS REACHABLE, which is the whole shape of the bug: the site
    // loaded `data[i]` and the needle as VALUES and called `compile_binop`.
    // `compile_enum_eq` is selected at the `==` OPERATOR site by running
    // `enum_name_of_expr` over the operand EXPRESSIONS, which a loaded value does
    // not carry; `emit_eq_fn_for_type_expr` — the one `Map`/`Set` use — takes
    // pointers, not values. So the aggregate fell to `compile_struct_eq`, whose
    // per-field recursion over an enum's `{tag, w0, w1, …}` compares payload words
    // as i64s.
    //
    // THE STRING AND STRUCT LINES ARE THE CONTROLS, and they are why this looked
    // fine almost everywhere: `compile_struct_eq` recurses per FIELD, so a
    // `String` element and a struct with typed fields both compared by content
    // already. An enum aggregate's fields are raw payload words, and recursing
    // per field over those is a pointer compare wearing a field walk.
    // `enum-unit` and `int` are the second kind of control — wholly inline, so
    // the word compare was already right for them.
    //
    // EVERY PAYLOAD IS BUILT BY A FUNCTION CALL, never a literal. Two identical
    // string literals can be folded to one global, which would let a pointer
    // compare answer `true` and hide the defect; `mk(n)` forces distinct
    // allocations, so a pointer compare must answer `false`.
    //
    // `generic` and `shared` are what the one-comparator routing buys beyond the
    // filed shape: both now go through the same fixed path rather than needing
    // their own arm here.
    //
    // Codegen twin: `test_e2e_vec_contains_matches_an_enum_element_by_content`.
    // The interpreter was already right, so this pins it as the oracle.
    let src = r#"
#[derive(Hash, Eq, PartialEq)]
struct S { a: i64, s: String }
#[derive(Hash, Eq, PartialEq)]
enum E { A(String), Pair(i64, String), B }
#[derive(Hash, Eq, PartialEq)]
shared struct Sh { id: i64, name: String }

fn mk(n: i64) -> String { return f"v{n}"; }

fn main() {
    let ve: Vec[E] = vec![E.A(mk(1)), E.Pair(2, mk(3)), E.B];
    println(f"enum={ve.contains(E.A(mk(1)))}");
    println(f"enum-ne={ve.contains(E.A(mk(2)))}");
    println(f"enum-pair={ve.contains(E.Pair(2, mk(3)))}");
    println(f"enum-pair-ne={ve.contains(E.Pair(2, mk(4)))}");
    println(f"enum-unit={ve.contains(E.B)}");

    let vo: Vec[Option[String]] = vec![Some(mk(5)), None];
    println(f"generic={vo.contains(Some(mk(5)))}");
    println(f"generic-ne={vo.contains(Some(mk(6)))}");
    println(f"generic-none={vo.contains(None)}");

    let vh: Vec[Sh] = vec![Sh { id: 1, name: mk(7) }];
    println(f"shared={vh.contains(Sh { id: 1, name: mk(7) })}");
    println(f"shared-ne={vh.contains(Sh { id: 1, name: mk(8) })}");

    let vs: Vec[String] = vec![mk(9), mk(10)];
    println(f"str={vs.contains(mk(9))}");
    println(f"str-ne={vs.contains(mk(11))}");
    let vt: Vec[S] = vec![S { a: 1, s: mk(12) }];
    println(f"struct={vt.contains(S { a: 1, s: mk(12) })}");
    println(f"struct-ne={vt.contains(S { a: 1, s: mk(13) })}");
    let vi: Vec[i64] = vec![1, 2];
    println(f"int={vi.contains(2)}");
    println(f"int-ne={vi.contains(3)}");
}
"#;
    assert_eq!(
        run_no_errors(src),
        "enum=true\nenum-ne=false\nenum-pair=true\n\
         enum-pair-ne=false\nenum-unit=true\ngeneric=true\n\
         generic-ne=false\ngeneric-none=true\nshared=true\n\
         shared-ne=false\nstr=true\nstr-ne=false\nstruct=true\n\
         struct-ne=false\nint=true\nint-ne=false\n"
    );
}

// ── `VecDeque[T]` (design.md) ───────────────────────────────────

#[test]
fn test_vec_deque_new_and_push_back() {
    let out = run(r#"
        fn main() {
            let mut q: VecDeque[i64] = VecDeque.new();
            q.push_back(1);
            q.push_back(2);
            q.push_back(3);
            println(q.len());
            println(q.is_empty());
        }
    "#);
    assert_eq!(out, "3\nfalse\n");
}

#[test]
fn test_vec_deque_push_front_then_pop_back_observes_correct_order() {
    // Mixed push_front/push_back with full pop_front/pop_back drain —
    // pins the front/back distinction under the shared `Vec[Value]`
    // storage that the interpreter uses internally.
    let out = run(r#"
        fn main() {
            let mut q: VecDeque[i64] = VecDeque.new();
            q.push_back(2);
            q.push_back(3);
            q.push_front(1);
            let f = q.pop_front();
            let b = q.pop_back();
            let m = q.pop_front();
            println(f);
            println(b);
            println(m);
        }
    "#);
    assert_eq!(out, "Some(1)\nSome(3)\nSome(2)\n");
}

#[test]
fn test_vec_deque_pop_empty_returns_none() {
    let out = run(r#"
        fn main() {
            let mut q: VecDeque[i64] = VecDeque.new();
            let f = q.pop_front();
            let b = q.pop_back();
            println(f);
            println(b);
        }
    "#);
    assert_eq!(out, "None\nNone\n");
}

#[test]
fn test_vec_insert_scalar_and_heap() {
    // `Vec[T].insert(idx, value)` — shift the tail up, place value at idx
    // (`idx == len` appends). Exercises front/middle/end for scalars and a
    // heap (String) element (move semantics). Interpreter is the oracle for the
    // codegen twin in tests/codegen.rs::test_e2e_vec_insert.
    let out = run(r#"
        fn main() {
            let mut v: Vec[i64] = [1, 2, 4, 5];
            v.insert(2, 3);
            v.insert(0, 0);
            v.insert(6, 6);
            let mut i = 0;
            while i < v.len() { print(f"{v[i]}"); i = i + 1; }
            println("");
            let mut s: Vec[String] = Vec.new();
            s.push(f"a");
            s.push(f"c");
            let mid = f"b-{1}";
            s.insert(1, mid);
            println(s[1]);
        }
    "#);
    assert_eq!(out, "0123456\nb-1\n");
}

// `Vec[T].pop()` — the bare form, alias for `pop_back`. Both the
// typechecker (`src/typechecker/expr_method_call.rs:1156`) and codegen
// (`src/codegen/vec_method.rs:300` collapses `pop | pop_back |
// pop_front` into one arm) already supported it; the interpreter
// dispatch arm was missing, so `karac run` panicked with
// "method 'pop' not found on type 'unknown'". Surfaced by
// kara-katas/leetcode/71-simplify-path which wanted stack-style
// push/pop on a `Vec[i64]`.
#[test]
fn test_vec_macro_literal_end_to_end() {
    // `vec![...]` desugars to the same node as `Vec[...]`, so it flows through
    // the interpreter unchanged. Pins the parser desugaring end-to-end.
    let out = run(r#"
        fn main() {
            let v = vec![10, 20, 30];
            let mut total = 0;
            for x in v {
                total = total + x;
            }
            println(total);
            let zeros = vec![0; 4];
            println(zeros.len());
        }
    "#);
    assert_eq!(out, "60\n4\n");
}

#[test]
fn test_vec_pop_returns_some_then_none_on_drain() {
    let out = run(r#"
        fn main() {
            let mut v: Vec[i64] = Vec.new();
            v.push(10);
            v.push(20);
            v.push(30);
            let a = v.pop();
            let b = v.pop();
            let c = v.pop();
            let d = v.pop();
            println(a);
            println(b);
            println(c);
            println(d);
            println(v.len());
        }
    "#);
    assert_eq!(out, "Some(30)\nSome(20)\nSome(10)\nNone\n0\n");
}

#[test]
fn test_vec_clear_and_extend() {
    // `Vec[T].clear()` empties the Vec (len → 0) and it stays usable
    // (push/extend rebuild it); `extend(other)` appends clones of `other`'s
    // elements. Exercised on a pod (`i64`) and a heap (`String`) element type
    // — the heap case is leak-checked in
    // `tests/memory_sanitizer.rs::asan_vec_clear_extend_heap_no_leak_no_double_free`.
    let out = run(r#"
        fn main() {
            let mut v: Vec[i64] = Vec.new();
            v.push(1);
            v.push(2);
            v.push(3);
            v.clear();
            println(v.len());
            v.push(9);
            let mut w: Vec[i64] = Vec.new();
            w.push(7);
            w.push(8);
            v.extend(w);
            println(v.len());
            println(v[0]);
            println(v[1]);
            println(v[2]);

            let mut s: Vec[String] = Vec.new();
            s.push("alpha");
            s.push("beta");
            s.clear();
            println(s.len());
            s.push("gamma");
            println(s[0]);
        }
    "#);
    assert_eq!(out, "0\n3\n9\n7\n8\n0\ngamma\n");
}

#[test]
fn test_vec_truncate() {
    // `Vec[T].truncate(n)` — shorten to at most `n` elements, dropping the tail;
    // `n >= len` is a no-op, `n < 0` clamps to 0. Exercised on pod (`i64`) and
    // heap (`String`) element types; heap-drop leak-safety is covered in
    // `tests/memory_sanitizer.rs::asan_vec_truncate_heap_no_leak`. Codegen
    // mirrors this (`tests/codegen.rs::e2e_vec_truncate`).
    let out = run(r#"
        fn main() {
            let mut a: Vec[i64] = Vec.new();
            a.push(1);
            a.push(2);
            a.push(3);
            a.push(4);
            a.truncate(2);
            println(a.len());
            println(a[0]);
            println(a[1]);
            a.truncate(5);
            println(a.len());
            a.truncate(0);
            println(a.len());

            let mut s: Vec[String] = Vec.new();
            s.push("aa");
            s.push("bb");
            s.push("cc");
            s.truncate(1);
            println(s.len());
            println(s[0]);
            s.push("dd");
            println(s.len());
            println(s[1]);
        }
    "#);
    assert_eq!(out, "2\n1\n2\n2\n0\n1\naa\n2\ndd\n");
}

#[test]
fn test_vec_swap_remove() {
    // `Vec[T].swap_remove(i) -> T` — O(1) remove: return element `i` and move
    // the LAST element into slot `i` (order NOT preserved). Covers a middle
    // index (swap-with-last) and heap (`String`) elements. Codegen mirrors this
    // (`tests/codegen.rs::e2e_vec_swap_remove`); heap leak-safety in
    // `tests/memory_sanitizer.rs::asan_vec_swap_remove_heap_no_leak`.
    let out = run(r#"
        fn main() {
            let mut a: Vec[i64] = Vec.new();
            a.push(10);
            a.push(20);
            a.push(30);
            a.push(40);
            let x = a.swap_remove(1);
            println(x);
            println(a.len());
            println(a[0]);
            println(a[1]);
            println(a[2]);

            let mut s: Vec[String] = Vec.new();
            s.push("aa");
            s.push("bb");
            s.push("cc");
            let z = s.swap_remove(0);
            println(z);
            println(s.len());
            println(s[0]);
            println(s[1]);
        }
    "#);
    // swap_remove(1) on [10,20,30,40] → returns 20, a=[10,40,30]; String
    // swap_remove(0) on [aa,bb,cc] → returns "aa", s=[cc,bb].
    assert_eq!(out, "20\n3\n10\n40\n30\naa\n2\ncc\nbb\n");
}

#[test]
fn test_vec_pop_used_as_stack_for_simplify_path_shape() {
    // Mirrors the kata-katas/leetcode/71-simplify-path stack discipline:
    // push components, pop on `'..'`, skip on `'.'`, never underflow.
    // Tests that the `pop` arm interacts cleanly with the surrounding
    // push/len control flow the kata exercises.
    let out = run(r#"
        fn main() {
            let mut stack: Vec[i64] = Vec.new();
            stack.push(1);
            stack.push(2);
            stack.push(3);
            let _ = stack.pop();
            stack.push(4);
            let _ = stack.pop();
            let _ = stack.pop();
            println(stack.len());
            println(stack.pop());
            println(stack.pop());
        }
    "#);
    assert_eq!(out, "1\nSome(1)\nNone\n");
}

#[test]
fn test_vec_deque_bfs_frontier_pattern() {
    // The kata's actual workflow shape: a BFS frontier with
    // push_back at the producer side and pop_front at the consumer
    // side. Verifies FIFO order with mixed enqueue/dequeue.
    let out = run(r#"
        fn main() {
            let mut frontier: VecDeque[i64] = VecDeque.new();
            frontier.push_back(1);
            let mut count = 0;
            loop {
                let next = frontier.pop_front();
                match next {
                    Some(node) => {
                        count = count + 1;
                        if node < 3 {
                            frontier.push_back(node + 1);
                            frontier.push_back(node + 2);
                        }
                    },
                    None => { break; },
                }
            }
            println(count);
        }
    "#);
    // Frontier: [1] → pop 1 (1), push 2,3 → [2,3]
    //               → pop 2 (2), push 3,4 → [3,3,4]
    //               → pop 3 (3), no push     → [3,4]
    //               → pop 3 (4), no push     → [4]
    //               → pop 4 (5), no push     → []
    //               → pop None, break. count=5.
    assert_eq!(out, "5\n");
}

#[test]
fn test_vec_filled_bool() {
    // Kata's actual usage shape: `Vec.filled(n, false)` for a
    // visited bitset, then index-write through it.
    let out = run(r#"
        fn main() {
            let mut visited: Vec[bool] = Vec.filled(5, false);
            visited[2] = true;
            println(visited.len());
            println(visited[0]);
            println(visited[2]);
            println(visited[4]);
        }
    "#);
    assert_eq!(out, "5\nfalse\ntrue\nfalse\n");
}

#[test]
fn test_vec_filled_nested_vec_independent_storage() {
    // `Vec.filled(n, Vec.new())` must produce n independent Vecs —
    // a per-slot deep clone, not an `Arc`-bump (the interpreter's
    // `Value::Array` storage is `Arc<RwLock<...>>`; the default
    // `Value::Clone` would alias every slot to the same underlying
    // Vec, so pushing into one would be visible in all). Spec says
    // `Vec.filled[T: Clone]` — Clone semantics for `Vec[T]` are deep.
    let out = run(r#"
        fn main() {
            let mut grid: Vec[Vec[i64]] = Vec.filled(3, Vec.new());
            grid[0].push(99);
            println(grid[0].len());
            println(grid[1].len());
            println(grid[2].len());
        }
    "#);
    assert_eq!(out, "1\n0\n0\n");
}

#[test]
fn test_vec_filled_zero_length() {
    // `Vec.filled(0, x)` is legal — produces an empty Vec.
    let out = run(r#"
        fn main() {
            let v: Vec[i64] = Vec.filled(0, 99);
            println(v.len());
        }
    "#);
    assert_eq!(out, "0\n");
}

#[test]
fn test_vec_filled_negative_length_runtime_error() {
    // Negative length is a runtime error — Kāra has no usize,
    // so the typechecker accepts `i64` and the interpreter
    // guards at the call site.
    let errs = runtime_errors(
        r#"
        fn main() {
            let v: Vec[i64] = Vec.filled(-1, 0);
            println(v.len());
        }
    "#,
    );
    assert!(
        errs.iter()
            .any(|e| format!("{e:?}").contains("Vec.filled length must be non-negative")),
        "expected non-negative-length runtime error; got: {errs:?}"
    );
}

// ── `Vec.with_capacity(n)` ──────────────────────────────────────

#[test]
fn test_vec_with_capacity_len_is_zero_then_push_works() {
    // `Vec.with_capacity(N)` returns an empty Vec — `len() == 0`
    // — but the underlying buffer is sized for at least N pushes
    // without reallocating. Test verifies the observable contract:
    // initial len is 0, push fills slots 0..N correctly.
    let out = run(r#"
        fn main() {
            let mut v: Vec[i64] = Vec.with_capacity(5);
            println(v.len());
            let mut i = 0i64;
            while i < 5 {
                v.push(i * 10);
                i = i + 1;
            }
            println(v.len());
            println(v[0]);
            println(v[4]);
        }
    "#);
    assert_eq!(out, "0\n5\n0\n40\n");
}

#[test]
fn test_vec_with_capacity_zero_is_legal() {
    // `Vec.with_capacity(0)` is the degenerate case — same shape as
    // `Vec.new()`. Subsequent push grows from there.
    let out = run(r#"
        fn main() {
            let mut v: Vec[i64] = Vec.with_capacity(0);
            println(v.len());
            v.push(42);
            println(v.len());
            println(v[0]);
        }
    "#);
    assert_eq!(out, "0\n1\n42\n");
}

#[test]
fn test_vec_with_capacity_untyped_let_inference_from_push() {
    // `let mut v = Vec.with_capacity(n); v.push(x);` — no annotation
    // on the let; element type is inferred from the downstream push.
    // Mirrors `let mut v = Vec.new(); v.push(x);`. Without the
    // typechecker arm in expr_call.rs, the call returned
    // Type::Error, the binding's inner-type table stayed empty, and
    // codegen errored "element type unknown — requires `let v:
    // Vec[T] = ...` annotation".
    let out = run(r#"
        fn main() {
            let mut v = Vec.with_capacity(5);
            v.push(10);
            v.push(20);
            v.push(30);
            println(v.len());
            println(v[0]);
            println(v[2]);
        }
    "#);
    assert_eq!(out, "3\n10\n30\n");
}

#[test]
fn test_vec_with_capacity_negative_runtime_error() {
    // Mirrors `Vec.filled`'s negative-length guard. Kāra has no
    // usize, so the typechecker accepts `i64` and the runtime
    // rejects negatives.
    let errs = runtime_errors(
        r#"
        fn main() {
            let v: Vec[i64] = Vec.with_capacity(-1);
            println(v.len());
        }
    "#,
    );
    assert!(
        errs.iter()
            .any(|e| format!("{e:?}").contains("Vec.with_capacity capacity must be non-negative")),
        "expected non-negative-capacity runtime error; got: {errs:?}"
    );
}

// ── Vec.remove (interpreter parity with codegen) ───────────────

#[test]
fn test_vec_remove_local() {
    // `Vec.remove(idx) -> T` on a local: returns the removed element,
    // shifts the tail down, decrements len. Interpreter parity with
    // codegen's `test_e2e_vec_remove_local` (same program + output).
    assert_eq!(
        run("fn main() {\n\
                 let mut xs: Vec[i64] = Vec.new();\n\
                 xs.push(10);\n\
                 xs.push(20);\n\
                 xs.push(30);\n\
                 let removed: i64 = xs.remove(1);\n\
                 println(removed);\n\
                 println(xs.len());\n\
                 println(xs[0]);\n\
                 println(xs[1]);\n\
             }"),
        "20\n2\n10\n30\n"
    );
}

#[test]
fn test_vec_remove_first_and_last() {
    // Remove the head (memmoves the whole tail down) then the new last.
    // Mirrors codegen's `test_e2e_vec_remove_first` / `_last` semantics.
    assert_eq!(
        run("fn main() {\n\
                 let mut xs: Vec[i64] = Vec.new();\n\
                 xs.push(1);\n\
                 xs.push(2);\n\
                 xs.push(3);\n\
                 let _ = xs.remove(0);\n\
                 println(xs[0]);\n\
                 println(xs.len());\n\
                 let _ = xs.remove(1);\n\
                 println(xs[0]);\n\
                 println(xs.len());\n\
             }"),
        "2\n2\n2\n1\n"
    );
}

#[test]
fn test_vec_remove_through_mut_ref_param() {
    // The case that first surfaced this gap: `Vec.remove` on a
    // `mut ref Vec[T]` receiver must write back to the caller's vector.
    // The interpreter's `Value::Array` shares its `Arc`-backed storage
    // across the borrow, so the removal propagates — same aliasing the
    // `push` arm relies on.
    assert_eq!(
        run("fn drain_first(v: mut ref Vec[i64]) -> i64 {\n\
                 v.remove(0)\n\
             }\n\
             fn main() {\n\
                 let mut xs: Vec[i64] = Vec.new();\n\
                 xs.push(10);\n\
                 xs.push(20);\n\
                 xs.push(30);\n\
                 let a = drain_first(mut xs);\n\
                 println(a);\n\
                 println(xs.len());\n\
                 println(xs[0]);\n\
             }"),
        "10\n2\n20\n"
    );
}

#[test]
fn test_vec_remove_out_of_bounds_is_runtime_error() {
    // design.md pins OOB as UB, but the tree-walk interpreter surfaces a
    // clean runtime error at the call site rather than panicking deep in
    // `Vec::remove` — matching the `index out of bounds` shape.
    let errs = runtime_errors(
        "fn main() {\n\
             let mut v: Vec[i64] = Vec.new();\n\
             v.push(1);\n\
             v.push(2);\n\
             let _ = v.remove(5);\n\
         }",
    );
    assert!(
        errs.iter()
            .any(|e| e.message.contains("Vec.remove: index 5 out of bounds")),
        "expected OOB runtime error, got: {:?}",
        errs
    );
}

// ── B-2026-06-30-15 value_compare Array arm (nested Vec sort) ──

#[test]
fn test_nested_vec_sort_orders_lexicographically() {
    // Pre-fix, two `Value::Array`s fell to value_compare's discriminant
    // fallback (always Equal), so `Vec[Vec[i64]].sort()` was a silent
    // NO-OP under the interpreter — probes just showed insertion order.
    let output = run_program(
        "fn main() {\n\
             let mut outer: Vec[Vec[i64]] = Vec.new();\n\
             outer.push(Vec[3, 1]);\n\
             outer.push(Vec[2]);\n\
             outer.push(Vec[2, 9]);\n\
             outer.sort();\n\
             let mut i = 0;\n\
             while i < outer.len() {\n\
                 println(outer[i][0]);\n\
                 println(outer[i].len());\n\
                 i = i + 1;\n\
             };\n\
         }",
    );
    assert_eq!(
        output,
        vec![
            "2\n".to_string(),
            "1\n".to_string(),
            "2\n".to_string(),
            "2\n".to_string(),
            "3\n".to_string(),
            "2\n".to_string()
        ],
        "expected ascending lexicographic order [2] < [2,9] < [3,1]; got {:?}",
        output
    );
}

/// B-2026-09-04-31 — a shared value reached through a `Vec` ELEMENT: bare
/// (`Vec[S]`), in a tuple element, and in a struct field. The element loop
/// skips shared members for the same reason the field walk does, so all three
/// were silent under `--interp` while the compiled element drain fired each
/// once. Bare `Vec[S]` is here on purpose — an earlier draft of the fix
/// excluded it on the belief that it had its own channel; it did not.
#[test]
fn test_user_drop_vec_element_shared_struct_fires_once_at_scope_exit() {
    let (output, _drops) = run_program_with_drops(
        "shared struct S { id: i64 }\n\
         impl Drop for S { fn drop(mut ref self) { println(f\"dS{self.id}\"); } }\n\
         struct Holder { s: S }\n\
         fn main() {\n\
             { let v: Vec[S] = [S { id: 1 }]; println(f\"v{v[0].id}\"); println(\"one\"); }\n\
             { let v: Vec[(S, i64)] = [(S { id: 2 }, 3)]; println(f\"v{v[0].0.id}\"); println(\"two\"); }\n\
             { let v: Vec[Holder] = [Holder { s: S { id: 3 } }]; println(f\"v{v[0].s.id}\"); println(\"three\"); }\n\
             { let s: S = S { id: 4 }; let v: Vec[S] = [s]; println(f\"v{v[0].id}{s.id}\"); println(\"four\"); }\n\
             println(\"end\");\n\
         }",
    );
    assert_eq!(
        output.concat(),
        "v1\none\ndS1\nv2\ntwo\ndS2\nv3\nthree\ndS3\nv44\nfour\ndS4\nend\n"
    );
}

#[test]
fn test_plain_struct_field_write_through_vec_element() {
    // `v[i].field = x` on plain-struct elements: update the element copy and
    // write it back into the Vec's shared storage; siblings untouched.
    assert_eq!(
        run("struct Item { v: i64 }\n\
             fn main() {\n\
                 let mut items = Vec.new();\n\
                 items.push(Item { v: 10 });\n\
                 items.push(Item { v: 20 });\n\
                 items[1].v = 99;\n\
                 println(items[0].v);\n\
                 println(items[1].v);\n\
             }"),
        "10\n99\n"
    );
}

// ── Vec-default and prefix collection literals ──────────────────

#[test]
fn test_bare_literal_defaults_to_vec_index() {
    assert_eq!(
        run("fn main() { let v = [10, 20, 30]; println(v[1]); }"),
        "20\n"
    );
}

#[test]
fn test_prefix_vec_literal_runtime() {
    assert_eq!(
        run("fn main() { let v = Vec[7, 8, 9]; println(v[2]); }"),
        "9\n"
    );
}

#[test]
fn test_prefix_vec_len() {
    assert_eq!(
        run("fn main() { let v = Vec[1, 2, 3]; println(v.len()); }"),
        "3\n"
    );
}

#[test]
fn test_interpreter_vec_new_construct_push_len() {
    // `Vec.new()` returns an empty Vec through the interpreter's path-string
    // dispatch. Exercises construct → push → len-read → indexed read so a
    // regression in the dispatch arm fails this test rather than panicking
    // inside `karac run` with `path 'Vec.new' not found`.
    assert_eq!(
        run("fn main() {\n\
                 let mut v: Vec[i64] = Vec.new();\n\
                 v.push(10_i64);\n\
                 v.push(20_i64);\n\
                 v.push(30_i64);\n\
                 println(f\"{v.len()}\");\n\
                 println(f\"{v[0]}\");\n\
                 println(f\"{v[2]}\");\n\
             }"),
        "3\n10\n30\n"
    );
}

#[test]
fn test_repeat_literal_vec_prefix_runtime() {
    assert_eq!(
        run("fn main() {
                 let v = Vec[7; 4];
                 println(v.len());
                 println(v[3]);
             }"),
        "4\n7\n"
    );
}

#[test]
fn test_vec_binary_search_duplicates_and_widths() {
    // Pins the exact index `binary_search` returns among DUPLICATE keys (std's
    // branchless `binary_search_by`, which codegen must match bit-for-bit), plus
    // unsigned (u8 200 > i8 range) and String element ordering. See
    // tests/codegen.rs::e2e_vec_binary_search_codegen for the A/B mirror.
    let output = run("fn print_opt(o: Option[i64]) {\n\
             match o { Some(i) => println(i), None => println(-1i64) }\n\
         }\n\
         fn main() {\n\
             let dup: Vec[i64] = vec![1, 2, 2, 2, 2, 3, 4];\n\
             print_opt(dup.binary_search(2));\n\
             let alleq: Vec[i64] = vec![5, 5, 5, 5];\n\
             print_opt(alleq.binary_search(5));\n\
             let u: Vec[u8] = vec![10u8, 50u8, 200u8, 250u8];\n\
             print_opt(u.binary_search(200u8));\n\
             print_opt(u.binary_search(11u8));\n\
             let s: Vec[String] = vec![\"apple\", \"banana\", \"cherry\"];\n\
             print_opt(s.binary_search(\"banana\"));\n\
         }");
    assert_eq!(output, "4\n3\n2\n-1\n1\n");
}

#[test]
fn test_bufreader_with_capacity_read_into_slice() {
    // with_capacity wraps with an explicit buffer size; `read` fills a
    // mut Slice[u8] and returns the byte count, writing bytes back
    // through the slice storage (b0 == 'A' == 65).
    let tmp = std::env::temp_dir().join("karac_test_bufreader_cap.txt");
    let path = tmp.to_str().unwrap().replace('\\', "\\\\");
    let _ = std::fs::remove_file(&tmp);
    std::fs::write(&tmp, b"ABC").expect("seed temp");
    let src = format!(
        "fn main() {{
             match File.open(\"{path}\") {{
                 Ok(f) => {{
                     let br = BufReader.with_capacity(f, 16);
                     let buf = [0u8, 0u8, 0u8, 0u8, 0u8];
                     match br.read(buf[0..5]) {{
                         Ok(n) => println(\"n=\" + n.to_string() + \" b0=\" + buf[0].to_string()),
                         Err(_) => println(\"read err\"),
                     }}
                 }}
                 Err(_) => println(\"open err\"),
             }}
         }}"
    );
    let out = run_no_errors(&src);
    assert_eq!(out, "n=3 b0=65\n");
    let _ = std::fs::remove_file(&tmp);
}

// ── Clone trait surface (canonical: phase-8-stdlib-floor.md
//    "Clone trait surface for collections") ────────────────────────

#[test]
fn test_vec_clone_preserves_contents() {
    // Cloning a Vec produces an equal Vec; both contain the same elements
    // in the same order.
    let output = run("fn main() {\n\
             let v: Vec[i64] = [1_i64, 2_i64, 3_i64];\n\
             let w: Vec[i64] = v.clone();\n\
             println(w[0]);\n\
             println(w[1]);\n\
             println(w[2]);\n\
         }");
    assert_eq!(output, "1\n2\n3\n");
}

#[test]
fn test_vec_clone_independent_after_push() {
    // Mutating the source after cloning does not affect the clone — they
    // own independent buffers.
    let output = run("fn main() {\n\
             let v: Vec[i64] = [1_i64, 2_i64];\n\
             let w: Vec[i64] = v.clone();\n\
             v.push(99_i64);\n\
             println(v.len());\n\
             println(w.len());\n\
         }");
    assert_eq!(output, "3\n2\n");
}

// ── Fallible-allocation `try_*` companions (phase-8-stdlib-floor item 2) ──
// The interpreter never OOMs, so every companion returns `Ok`; these pin the
// happy-path behaviour (operation took effect, result wrapped in `Ok`).

#[test]
fn test_try_push_wraps_ok_and_mutates() {
    let output = run("fn main() {\n\
             let mut v: Vec[i64] = Vec.new();\n\
             match v.try_push(5_i64) {\n\
                 Ok(_) => println(\"ok\"),\n\
                 Err(e) => println(\"err\"),\n\
             }\n\
             v.try_push(6_i64);\n\
             println(v.len());\n\
             println(v[0]);\n\
             println(v[1]);\n\
         }");
    assert_eq!(output, "ok\n2\n5\n6\n");
}

#[test]
fn test_try_clone_vec_wraps_ok() {
    let output = run("fn main() {\n\
             let v: Vec[i64] = [1_i64, 2_i64, 3_i64];\n\
             match v.try_clone() {\n\
                 Ok(c) => println(c.len()),\n\
                 Err(e) => println(\"err\"),\n\
             }\n\
         }");
    assert_eq!(output, "3\n");
}

#[test]
fn test_try_with_capacity_static_wraps_ok() {
    let output = run("fn main() {\n\
             match Vec.try_with_capacity(8_i64) {\n\
                 Ok(v) => {\n\
                     let mut vv = v;\n\
                     vv.push(1_i64);\n\
                     println(vv.len());\n\
                 },\n\
                 Err(e) => println(\"err\"),\n\
             }\n\
         }");
    assert_eq!(output, "1\n");
}

#[test]
fn test_process_command_arg_order_preserved() {
    let output = run(r#"fn main() {
         let cmd = Command.new("echo").arg("hello").arg("world");
         match cmd.cmd_args.get(0) {
             Some(a) => println(a),
             None => println("?"),
         }
         match cmd.cmd_args.get(1) {
             Some(a) => println(a),
             None => println("?"),
         }
     }"#);
    assert_eq!(output, "hello\nworld\n");
}

#[test]
fn test_arena_len_tracks_pushes() {
    // `len()` reports the number of live items, growing with each push.
    let output = run(r#"fn main() {
             let a: Arena[i64] = Arena.new();
             println(a.len());
             let _r0 = a.push(1);
             let _r1 = a.push(2);
             println(a.len());
         }"#);
    assert_eq!(output, "0\n2\n");
}

#[test]
fn test_url_encode_reserved() {
    let output = run("fn main() {\n\
             println(Url.encode(\"hello world\"));\n\
         }");
    assert_eq!(output, "hello%20world\n");
}

#[test]
fn test_url_encode_preserves_unreserved() {
    // RFC 3986 unreserved set: A-Za-z0-9-._~ — must round-trip unchanged.
    let output = run("fn main() {\n\
             println(Url.encode(\"abcXYZ123-._~\"));\n\
         }");
    assert_eq!(output, "abcXYZ123-._~\n");
}

#[test]
fn test_vecdeque_with_capacity_behaves_like_new() {
    let output = run_no_errors(
        r#"fn main() {
            let mut q: VecDeque[i64] = VecDeque.with_capacity(4);
            q.push_back(7);
            q.push_front(3);
            println(q.len());
            println(q.pop_front().unwrap());
        }"#,
    );
    assert_eq!(output, "2\n3\n");
}

#[test]
fn test_vec_retain_predicate() {
    // B-2026-07-15-16: `Vec.retain(|x| pred)` keeps each element for which the
    // predicate holds, mutating in place. The un-annotated closure param is
    // seeded from the element type (`i64`) so `x != 3` / `x % 2 == 0` type.
    let output = run("fn main() {
            let mut xs: Vec[i64] = Vec.new();
            xs.push(1i64); xs.push(2i64); xs.push(3i64); xs.push(4i64); xs.push(5i64);
            xs.retain(|x| x != 3i64);
            println(xs.len());
            for x in xs.iter() { println(x); }
            xs.retain(|x| x % 2i64 == 0i64);
            println(xs.len());
            for x in xs.iter() { println(x); }
        }");
    // after removing 3: [1,2,4,5] (len 4); after keeping evens: [2,4] (len 2)
    assert_eq!(output, "4\n1\n2\n4\n5\n2\n2\n4\n");
}

#[test]
fn test_vec_dedup_removes_consecutive_duplicates() {
    // `Vec.dedup()` removes CONSECUTIVE duplicate elements (keeps the first of
    // each run, Rust semantics), mutating in place. Non-adjacent duplicates are
    // preserved. Works for scalar and String elements.
    let output = run("fn main() {
            let mut xs: Vec[i64] = [1, 1, 2, 3, 3, 3, 1];
            xs.dedup();
            println(xs.len());
            for x in xs.iter() { println(x); }
            let mut ss: Vec[String] = [\"a\", \"a\", \"bb\", \"a\"];
            ss.dedup();
            println(ss.len());
            for s in ss.iter() { println(s); }
        }");
    // [1,2,3,1] (trailing non-adjacent 1 kept); ["a","bb","a"]
    assert_eq!(output, "4\n1\n2\n3\n1\n3\na\nbb\na\n");
}

#[test]
fn test_vec_split_off_splits_at_index() {
    // `Vec.split_off(i) -> Vec[T]` — self keeps [0, i), the returned Vec owns
    // [i, len). Index clamps to [0, len].
    let output = run("fn main() {
            let mut v: Vec[i64] = [1, 2, 3, 4, 5];
            let t: Vec[i64] = v.split_off(2);
            println(v.len());
            println(t.len());
            for x in v.iter() { println(x); }
            for y in t.iter() { println(y); }
            let mut s: Vec[String] = [\"a\", \"b\", \"c\"];
            let st: Vec[String] = s.split_off(1);
            println(s.get(0));
            println(st.get(1));
        }");
    // v=[1,2] (len 2), t=[3,4,5] (len 3); strings s=[a], st=[b,c] -> st[1]=c
    assert_eq!(output, "2\n3\n1\n2\n3\n4\n5\nSome(a)\nSome(c)\n");
}

#[test]
fn test_vec_sort_by_cmp_descending() {
    // The canonical idiom `b.cmp(a)` — was wedged before primitive `cmp`
    // dispatch landed because the interpreter's impl-block lookup didn't
    // know about the typechecker's builtin Ord impl for `i64`.
    let output = run("fn main() {
            let mut xs: Vec[i64] = Vec.new();
            xs.push(3i64); xs.push(1i64); xs.push(4i64); xs.push(1i64); xs.push(5i64);
            xs.sort_by(|a, b| b.cmp(a));
            for x in xs.iter() { println(x); }
        }");
    assert_eq!(output, "5\n4\n3\n1\n1\n");
}

#[test]
fn test_vec_sort_by_key_ascending() {
    // Idiomatic ascending sort by computed key.
    let output = run("fn main() {
            let mut xs: Vec[i64] = Vec.new();
            xs.push(3i64); xs.push(1i64); xs.push(4i64); xs.push(1i64); xs.push(5i64);
            xs.sort_by_key(|x| x);
            for x in xs.iter() { println(x); }
        }");
    assert_eq!(output, "1\n1\n3\n4\n5\n");
}

#[test]
fn test_vec_sort_by_key_descending_via_negation() {
    // LeetCode #1665 idiom — descending sort via key negation.
    let output = run("fn main() {
            let mut xs: Vec[i64] = Vec.new();
            xs.push(3i64); xs.push(1i64); xs.push(4i64); xs.push(1i64); xs.push(5i64);
            xs.sort_by_key(|x| -x);
            for x in xs.iter() { println(x); }
        }");
    assert_eq!(output, "5\n4\n3\n1\n1\n");
}

#[test]
fn test_mut_ref_enum_payload_vec_write_through() {
    // A `Vec` enum payload (also stored by value in the interpreter) writes
    // through the same reconstruction path.
    let output = run("enum V { List(Vec[i64]) }\n\
         fn add(v: mut ref V) { match v { List(xs) => { xs.push(99); } } }\n\
         fn size(v: ref V) -> i64 { match v { List(xs) => xs.len() as i64 } }\n\
         fn main() {\n\
             let mut xs: Vec[i64] = [1, 2];\n\
             let mut t = V.List(xs);\n\
             add(mut t);\n\
             println(size(t));\n\
         }");
    assert_eq!(output, "3\n");
}

// ── Iterating a borrowed nested collection auto-derefs scalar elements ──

/// (B-2026-06-30-4) The run side of the build/run agreement: iterating a
/// SHARED-borrowed `ref Vec[Vec[i64]]` binds the inner element as a usable
/// `i64`, so `total + x` computes `1+2+3+4 = 10` with no runtime error (and,
/// after the typechecker fix, no `expected 'i64', found 'ref i64'` warning).
/// The codegen sibling (`tests/codegen.rs`) locks the build side to the same
/// result.
#[test]
fn for_loop_borrowed_nested_vec_scalar_autoderefs() {
    let out = run_no_errors(
        "fn cell_sum(g: ref Vec[Vec[i64]]) -> i64 {\n\
         \x20   let mut total = 0i64;\n\
         \x20   for row in g { for x in row { total = total + x; } }\n\
         \x20   total\n\
         }\n\
         fn main() {\n\
         \x20   let mut grid: Vec[Vec[i64]] = Vec.new();\n\
         \x20   let mut r0: Vec[i64] = Vec.new(); r0.push(1i64); r0.push(2i64);\n\
         \x20   let mut r1: Vec[i64] = Vec.new(); r1.push(3i64); r1.push(4i64);\n\
         \x20   grid.push(r0); grid.push(r1);\n\
         \x20   println(cell_sum(grid));\n\
         }\n",
    );
    assert_eq!(out, "10\n");
}

// ── Iterating a `mut ref` collection is read-only, like bare shared `for` ──

/// (B-2026-06-30-6) The run side of the build/run agreement for the mutable-
/// borrow sibling of B-2026-06-30-4. Bare `for` over a `mut ref Vec[i64]`
/// borrows via `.iter()` (design.md line 2739), so the Copy scalar element
/// binds as a by-value `i64` and is usable in arithmetic — no `mut ref i64`
/// warning. In-place element mutation via bare `for` is NOT supported (that is
/// `.iter_mut()`'s role), so `x = x * 2` writes only the loop local: the Vec is
/// unchanged and `v[0]`/`v[1]` print `3`/`4`. `karac build` (see the codegen
/// sibling) prints the SAME — the point is that all surfaces now AGREE (the old
/// bug was `check`/`build` HARD-erroring while `run` warned-and-proceeded).
#[test]
fn for_loop_mut_ref_vec_scalar_element_is_read_only() {
    let out = run_no_errors(
        "fn double_all(xs: mut ref Vec[i64]) {\n\
         \x20   for x in xs { x = x * 2; }\n\
         }\n\
         fn main() {\n\
         \x20   let mut v: Vec[i64] = Vec.new(); v.push(3i64); v.push(4i64);\n\
         \x20   double_all(mut v);\n\
         \x20   println(v[0]); println(v[1]);\n\
         }\n",
    );
    assert_eq!(out, "3\n4\n");
}

/// (B-2026-06-30-6) Reassign-and-use of the by-value loop local IS a working
/// pattern: `x = x * 2` scales the local, `total + x` reads it, so the sum is
/// `(3*2)+(4*2) = 14`. This is why the fix binds by value rather than rejecting
/// loop-var assignment — a hard error would wrongly reject this valid program.
#[test]
fn for_loop_mut_ref_vec_scalar_reassign_and_use() {
    let out = run_no_errors(
        "fn sum_scaled(xs: mut ref Vec[i64]) -> i64 {\n\
         \x20   let mut total = 0i64;\n\
         \x20   for x in xs { x = x * 2; total = total + x; }\n\
         \x20   total\n\
         }\n\
         fn main() {\n\
         \x20   let mut v: Vec[i64] = Vec.new(); v.push(3i64); v.push(4i64);\n\
         \x20   println(sum_scaled(mut v));\n\
         }\n",
    );
    assert_eq!(out, "14\n");
}

/// B-2026-07-30-11 (discarded-temp leg, insert-displacement shape) —
/// interpreter twin of `tests/codegen.rs`'s
/// `e2e_wildcard_let_insert_displaced_payload_drop`, same source and
/// expected string.
#[test]
fn test_wildcard_let_insert_displaced_payload_drop() {
    assert_eq!(
        run("struct Res { id: i64 }\n\
             impl Drop for Res {\n\
                 fn drop(mut ref self) {\n\
                     println(f\"drop {self.id}\")\n\
                 }\n\
             }\n\
             fn main() {\n\
                 let mut m: Map[i64, Res] = Map.new();\n\
                 let _ = m.insert(1, Res { id: 7 });\n\
                 println(\"first insert done\");\n\
                 let _ = m.insert(1, Res { id: 8 });\n\
                 println(\"displacing insert done\");\n\
                 println(\"end\");\n\
             }\n"),
        "first insert done\ndrop 7\ndrop 8\ndisplacing insert done\nend\n"
    );
}

/// B-2026-08-01-24 — interpreter twin of `tests/codegen.rs`'s
/// `e2e_for_loop_struct_elem_push_move` (the interpreter was already correct;
/// the twin pins parity for the for-loop element push-move shape).
#[test]
fn test_for_loop_struct_elem_push_move() {
    assert_eq!(
        run("struct Header { name: String, value: String }\n\
             fn move_all(headers: Vec[Header]) -> Vec[Header] {\n\
                 let mut out: Vec[Header] = Vec.new();\n\
                 for h in headers {\n\
                     out.push(h);\n\
                 }\n\
                 out\n\
             }\n\
             fn main() {\n\
                 let stamp = \"20150830T123600Z\";\n\
                 let mut hs: Vec[Header] = Vec.new();\n\
                 hs.push(Header { name: \"X-Amz-Date\", value: stamp.clone() });\n\
                 let moved = move_all(hs);\n\
                 for h in moved { println(h.value); }\n\
             }\n"),
        "20150830T123600Z\n"
    );
}

/// B-2026-08-01-24 (enum leg) — interpreter twin of `tests/codegen.rs`'s
/// `e2e_for_loop_enum_elem_push_move`, same source and expected string.
#[test]
fn test_for_loop_enum_elem_push_move() {
    assert_eq!(
        run("enum Item {\n\
                 Named(String),\n\
                 Plain(i64),\n\
             }\n\
             fn main() {\n\
                 let mut src: Vec[Item] = Vec.new();\n\
                 src.push(Item.Named(f\"x{1}\"));\n\
                 src.push(Item.Plain(7));\n\
                 src.push(Item.Named(f\"y{2}\"));\n\
                 let mut out: Vec[Item] = Vec.new();\n\
                 for it in src {\n\
                     out.push(it);\n\
                 }\n\
                 for it in out {\n\
                     match it {\n\
                         Item.Named(s) => println(s),\n\
                         Item.Plain(n) => println(f\"{n}\"),\n\
                     }\n\
                 }\n\
             }\n"),
        "x1\n7\ny2\n"
    );
}

/// B-2026-08-01-22 leg b — interpreter twin of `tests/codegen.rs`'s
/// `e2e_struct_field_vec_elem_bodies_at_owner_death`, same source and
/// expected string.
#[test]
fn test_struct_field_vec_elem_bodies_at_owner_death() {
    assert_eq!(
        run("struct Res { id: i64, name: String }\n\
             impl Drop for Res {\n\
                 fn drop(mut ref self) {\n\
                     println(f\"drop {self.id} {self.name}\")\n\
                 }\n\
             }\n\
             struct Holder { xs: Vec[Res] }\n\
             fn main() {\n\
                 println(\"a\");\n\
                 let mut h = Holder { xs: Vec.new() };\n\
                 h.xs.push(Res { id: 7, name: f\"q{7}\" });\n\
                 h.xs.push(Res { id: 8, name: f\"r{8}\" });\n\
                 println(\"end\");\n\
             }\n"),
        "a\ndrop 7 q7\ndrop 8 r8\nend\n"
    );
}

/// B-2026-08-13-11 — interpreter twin of `tests/codegen.rs`'s
/// `test_e2e_field_rooted_vec_elem_read_into_enum_payload_values`, same source
/// and expected string.
///
/// The interpreter was correct throughout this row — it was the oracle that said
/// the compiled abort was a bug rather than a program error — so pinning its
/// output keeps the comparison the fix was argued from checkable.
#[test]
fn test_field_rooted_vec_elem_read_into_enum_payload_values() {
    assert_eq!(
        run("enum Cmd { Insert(i64, String), Delete(i64, String) }\n\
             struct Doc { lines: Vec[String] }\n\
             fn apply(d: mut ref Doc, c: Cmd) -> Cmd {\n\
                 match c {\n\
                     Insert(at, text) => {\n\
                         d.lines.insert(at as usize, text);\n\
                         Cmd.Delete(at, d.lines[at as usize])\n\
                     }\n\
                     Delete(at, _text) => {\n\
                         let gone = d.lines[at as usize];\n\
                         d.lines.remove(at as usize);\n\
                         Cmd.Insert(at, gone)\n\
                     }\n\
                 }\n\
             }\n\
             fn main() {\n\
                 let mut l: Vec[String] = Vec.new();\n\
                 let mut a = String.new(); a.push_str(\"alpha\");\n\
                 let mut b = String.new(); b.push_str(\"beta\");\n\
                 l.push(a); l.push(b);\n\
                 let mut d = Doc { lines: l };\n\
                 let inv = apply(mut d, Cmd.Delete(1, \"beta\"));\n\
                 match inv { Insert(i, t) => { println(t); } Delete(i, t) => { println(t); } }\n\
                 let i2 = apply(mut d, inv);\n\
                 match i2 { Insert(i, t) => { println(t); } Delete(i, t) => { println(t); } }\n\
                 println(d.lines.len());\n\
                 println(d.lines[0]);\n\
             }"),
        "beta\nbeta\n2\nalpha\n"
    );
}

/// B-2026-08-14-32 — the interpreter twin of
/// `tests/codegen.rs::test_e2e_if_arm_vec_element_binding_is_cloned`, same
/// source and same expected string.
///
/// This surface was correct the whole time: it copies values on binding, so an
/// element read through an `if` / `match` arm was never an alias to begin with.
/// The bug was compiled-only — a double free on a shape as ordinary as
/// `let w = if c { v[1] } else { "x" }`. So this test's job is to be the ORACLE
/// the compiled twin is checked against: the two assert one string, so a codegen
/// fix that stopped crashing but bound the wrong element, or left the container
/// holding a freed buffer, would fail the pair rather than quietly redefine what
/// the shape means.
#[test]
fn test_if_arm_vec_element_binding_is_cloned() {
    assert_eq!(
        run("fn fresh() -> String { \"zz\" }\n\
             fn main() {\n\
                 let mut v: Vec[String] = Vec.new();\n\
                 v.push(\"aa\"); v.push(\"bb\"); v.push(\"cc\");\n\
                 let mut n: Vec[Vec[i64]] = Vec.new();\n\
                 n.push([1, 2]); n.push([3, 4]);\n\
                 let a = if v.len() > 1 { v[1] } else { \"x\" };\n\
                 let b = if v.len() > 1 { v[1] } else { v[0] };\n\
                 let c = if v.len() > 9 { v[0] } else { fresh() };\n\
                 let d = match v.len() > 1 { true => v[2], false => \"x\" };\n\
                 let e = if v.len() > 2 { if v.len() > 9 { v[0] } else { v[1] } } else { \"x\" };\n\
                 let g = if n.len() > 1 { n[1] } else { n[0] };\n\
                 if v.len() > 0 { v[0] } else { v[1] };\n\
                 println(f\"{a} {b} {c} {d} {e} {g[0]}\");\n\
                 println(f\"{v[0]} {v[1]} {v[2]} {n[1][1]}\");\n\
             }"),
        "bb bb zz cc bb 3\naa bb cc 4\n"
    );
}

/// B-2026-08-13-4 — interpreter twin of `tests/codegen.rs`'s
/// `test_e2e_nested_vec_elem_field_read_is_a_copy`, same source and expected
/// string.
///
/// This leg is the ORACLE in the strongest sense for this row: the compiled
/// fix exists precisely to make codegen agree with what the interpreter has
/// always done — a field read off a container element is a COPY, so the
/// element still holds its value afterwards. Pinning the bytes here means a
/// future move-model shortcut has to disagree with this file to land.
#[test]
fn test_nested_vec_elem_field_read_is_a_copy() {
    assert_eq!(
        run("struct Pair { word: String, n: i64 }\n\
             struct Deep { inner: Pair, tag: i64 }\n\
             struct Outer { mid: Deep, label: String }\n\
             fn main() {\n\
                 let k = 1;\n\
                 let mut ds: Vec[Deep] = Vec.new();\n\
                 ds.push(Deep { inner: Pair { word: f\"a{k}\", n: 7 }, tag: 8 });\n\
                 let bound = ds[0].inner.word;\n\
                 println(bound);\n\
                 println(ds[0].inner.word);\n\
                 let mut out: Vec[String] = Vec.new();\n\
                 out.push(ds[0].inner.word);\n\
                 println(out[0]);\n\
                 println(ds[0].inner.word);\n\
                 println(ds[0].inner.n);\n\
                 let mut xs: Vec[Outer] = Vec.new();\n\
                 xs.push(Outer {\n\
                     mid: Deep { inner: Pair { word: f\"b{k}\", n: 9 }, tag: 10 },\n\
                     label: f\"L{k}\",\n\
                 });\n\
                 let deep = xs[0].mid.inner.word;\n\
                 println(deep);\n\
                 println(xs[0].mid.inner.word);\n\
             }"),
        "a1\na1\na1\na1\n7\nb1\nb1\n"
    );
}

/// B-2026-08-02-16 — interpreter twin of `tests/codegen.rs`'s
/// `e2e_generic_parent_vec_field_elem_bodies`, same source and expected
/// string (the interp's value-driven Vec-field arm always fired — this
/// pins the parity codegen's mono-resolved dbv walk now shares).
#[test]
fn test_generic_parent_vec_field_elem_bodies() {
    assert_eq!(
        run("struct Res { id: i64, name: String }\n\
             impl Drop for Res {\n\
                 fn drop(mut ref self) { println(f\"drop {self.id} {self.name}\") }\n\
             }\n\
             struct Pack[T] { items: Vec[T], tag: i64 }\n\
             fn main() {\n\
                 println(\"a\");\n\
                 {\n\
                     let mut p: Pack[Res] = Pack { items: Vec.new(), tag: 1 };\n\
                     p.items.push(Res { id: 7, name: f\"qq{7}\" });\n\
                     println(f\"n {p.items.len()}\");\n\
                 }\n\
                 println(\"end\");\n\
             }\n"),
        "a\nn 1\ndrop 7 qq7\nend\n"
    );
}

/// B-2026-08-02-18 (gate legs) — interpreter twin of `tests/codegen.rs`'s
/// `e2e_tuple_field_parent_displaced_and_vec_elem_bodies` (STASH-PROVEN
/// pin for the `field_value_carries_user_drop` widening: pre-fix the
/// interp's displaced-value and Vec-element gates classified a parent
/// whose only Drop content is a tuple field as no-drop, so the `drop 1
/// a1` and `drop 3 x3` lines were missing while AOT printed them).
#[test]
fn test_tuple_field_parent_displaced_and_vec_elem_bodies() {
    assert_eq!(
        run("struct Res { id: i64, name: String }\n\
             impl Drop for Res {\n\
                 fn drop(mut ref self) { println(f\"drop {self.id} {self.name}\") }\n\
             }\n\
             struct DuoR { pair: (Res, i64), tag: i64 }\n\
             fn main() {\n\
                 println(\"a\");\n\
                 {\n\
                     let mut x = DuoR { pair: (Res { id: 1, name: f\"a{1}\" }, 5), tag: 7 };\n\
                     println(x.tag);\n\
                     x = DuoR { pair: (Res { id: 2, name: f\"b{2}\" }, 6), tag: 8 };\n\
                     println(x.tag);\n\
                 }\n\
                 println(\"mid\");\n\
                 {\n\
                     let mut v: Vec[DuoR] = Vec.new();\n\
                     v.push(DuoR { pair: (Res { id: 3, name: f\"x{3}\" }, 5), tag: 9 });\n\
                     println(v.len());\n\
                 }\n\
                 println(\"end\");\n\
             }\n"),
        "a\n7\ndrop 1 a1\n8\ndrop 2 b2\nmid\n1\ndrop 3 x3\nend\n"
    );
}

#[test]
fn test_discarded_vec_removal_fires_and_frees() {
    // B-2026-08-03-2 (class 2) — interpreter twin of `tests/codegen.rs`'s
    // `e2e_discarded_vec_removal_fires_and_frees`, same source and expected
    // string. Landed AFTER the codegen half deliberately: while codegen could
    // not own a builtin removal's element, admitting these here would have
    // converted a shared gap into a divergence.
    assert_eq!(
        run("struct Res { id: i64, name: String }\n\
             impl Drop for Res {\n\
                 fn drop(mut ref self) { println(f\"drop {self.id} {self.name}\") }\n\
             }\n\
             fn main() {\n\
                 println(\"remove:\");\n\
                 {\n\
                     let mut v: Vec[Res] = Vec.new();\n\
                     v.push(Res { id: 1, name: f\"a{1}\" });\n\
                     v.remove(0);\n\
                     println(v.len());\n\
                 }\n\
                 println(\"swapremove:\");\n\
                 {\n\
                     let mut w: Vec[Res] = Vec.new();\n\
                     w.push(Res { id: 2, name: f\"b{2}\" });\n\
                     w.swap_remove(0);\n\
                     println(w.len());\n\
                 }\n\
                 println(\"bound:\");\n\
                 {\n\
                     let mut u: Vec[Res] = Vec.new();\n\
                     u.push(Res { id: 3, name: f\"c{3}\" });\n\
                     let r = u.remove(0);\n\
                     println(u.len());\n\
                 }\n\
                 println(\"survivor:\");\n\
                 {\n\
                     let mut t: Vec[Res] = Vec.new();\n\
                     t.push(Res { id: 4, name: f\"d{4}\" });\n\
                     t.push(Res { id: 5, name: f\"e{5}\" });\n\
                     t.remove(0);\n\
                     println(t.len());\n\
                 }\n\
                 println(\"end\");\n\
             }\n"),
        "remove:\ndrop 1 a1\n0\nswapremove:\ndrop 2 b2\n0\nbound:\ndrop 3 c3\n0\nsurvivor:\ndrop 4 d4\n1\ndrop 5 e5\nend\n"
    );
}

#[test]
fn test_vec_of_tuple_element_drop_bodies() {
    // B-2026-08-02-22 — interpreter twin of `tests/codegen.rs`'s
    // `e2e_vec_of_tuple_element_drop_bodies`, same source and expected
    // string. The interp was silent on BOTH shapes pre-fix (its Vec
    // element-bodies loop had no Tuple arm), so this pins the parity
    // codegen's new vec-of-tuple walker now shares.
    assert_eq!(
        run("struct Res { id: i64, name: String }\n\
             impl Drop for Res {\n\
                 fn drop(mut ref self) { println(f\"drop {self.id} {self.name}\") }\n\
             }\n\
             fn main() {\n\
                 println(\"inline:\");\n\
                 {\n\
                     let mut t: Vec[(Res, i64)] = Vec.new();\n\
                     t.push((Res { id: 9, name: f\"in{9}\" }, 8));\n\
                     println(t.len());\n\
                 }\n\
                 println(\"named:\");\n\
                 {\n\
                     let mut u: Vec[(Res, i64)] = Vec.new();\n\
                     let r = Res { id: 4, name: f\"tt{4}\" };\n\
                     u.push((r, 8));\n\
                     println(u.len());\n\
                 }\n\
                 println(\"end\");\n\
             }\n"),
        "inline:\n1\ndrop 9 in9\nnamed:\n1\ndrop 4 tt4\nend\n"
    );
}

/// B-2026-07-30-11 (Vec leg) — the interpreter half: a `Vec[T]`'s elements run
/// their user `impl Drop` bodies when the Vec dies.
///
/// `Env::drop_target` reports only Struct / SharedStruct / EnumVariant, so an
/// array binding resolved to `None` and the whole user-drop hook early-returned
/// — a resource held in a Vec element was held for the program's lifetime.
///
/// Same source and same expected output as `tests/codegen.rs`'s
/// `e2e_vec_elements_run_user_drop_bodies`. The pair IS the parity contract,
/// and it is load-bearing here: the first attempt at this fix ran the bodies on
/// both backends but at different TIMES (interp at the NLL point, codegen at
/// scope exit), which is a run/build divergence and strictly worse than the
/// leak it replaced.
#[test]
fn test_vec_elements_run_user_drop_bodies() {
    assert_eq!(
        run("struct A { t: i64 }\n\
             impl Drop for A { fn drop(mut ref self) { println(f\"dA{self.t}\"); } }\n\
             struct W { a: A }\n\
             fn main() {\n\
                 { let v: Vec[A] = [A { t: 1 }, A { t: 2 }]; println(f\"{v.len()}\"); }\n\
                 { let w: Vec[W] = [W { a: A { t: 3 } }]; println(f\"{w.len()}\"); }\n\
                 { let mut p: Vec[A] = [A { t: 4 }]; let g = p.pop(); println(f\"{p.len()}\"); }\n\
                 println(\"end\");\n\
             }\n"),
        "2\ndA1\ndA2\n1\ndA3\ndA4\n0\nend\n"
    );
}

/// B-2026-08-06-4, the interpreter side. The interpreter was ALREADY correct
/// here — it ran every arm below and printed these values while `karac build`
/// could not produce a binary at all (LLVM module verification hard-failed on
/// the 3-word Vec header passed against a 2-word `Slice[T]` formal). That
/// asymmetry is what localized the row to codegen's `coerce_to_slice`.
///
/// Pinning the reference leg here keeps it from drifting to match a future
/// codegen regression, and mirrors the arms of the codegen twin
/// `e2e_shared_struct_vec_field_coerces_to_slice`.
///
/// Arm (d) is the read-only one: a bare `Slice[T]` over a NON-`mut` field is
/// legal because it does not write, and it was equally unbuildable before —
/// the codegen gap was never limited to the mutating spelling, even though
/// only the mutating one had a soundness question attached.
///
/// The mutability gate is this row's other half and lives in
/// `tests/typechecker.rs`; every field WRITTEN here is declared `mut`, which
/// is what makes these programs legal at all.
#[test]
fn test_shared_struct_vec_field_coerces_to_slice() {
    assert_eq!(
        run("shared struct H { mut v: Vec[i64] }\n\
             shared struct M { tag: i64, mut v: Vec[i64], mut w: Vec[i64] }\n\
             shared struct R { v: Vec[i64] }\n\
             impl H {\n\
                 fn go(ref self) { zap(mut self.v); }\n\
             }\n\
             fn zap(s: mut Slice[i64]) { s[0] = 99i64; }\n\
             fn total(s: Slice[i64]) -> i64 {\n\
                 let mut t = 0i64;\n\
                 let mut i = 0i64;\n\
                 while i < s.len() { t = t + s[i]; i = i + 1i64; }\n\
                 return t;\n\
             }\n\
             fn main() {\n\
                 let n: i64 = 1i64;\n\
                 let a = H { v: [n, n + 1i64, n + 2i64] };\n\
                 zap(mut a.v);\n\
                 println(f\"a:{a.v[0]}:{a.v[1]}\");\n\
                 let b = M { tag: n + 6i64, v: [n, n], w: [n, n + 1i64, n + 2i64] };\n\
                 zap(mut b.w);\n\
                 println(f\"b:{b.tag}:{b.v[0]}:{b.w[0]}:{b.w[2]}\");\n\
                 let c = H { v: [n, n + 1i64, n + 2i64] };\n\
                 c.go();\n\
                 println(f\"c:{c.v[0]}\");\n\
                 let d = R { v: [n, n + 1i64, n + 2i64] };\n\
                 println(f\"d:{total(d.v)}\");\n\
             }\n"),
        "a:99:2\nb:7:1:99:3\nc:99\nd:6\n"
    );
}

#[test]
fn test_vec_shared_struct_clone_temp_original_survives_oracle() {
    // Oracle twin of `tests/codegen.rs`'s
    // `test_e2e_vec_shared_struct_clone_temp_original_survives`
    // (B-2026-08-15-14). The interpreter was always correct here — a
    // `Vec[shared struct]` clone-temp leaked only under codegen — so this pins
    // the values the build side must match, and keeps run-vs-build parity
    // asserted from both ends rather than from the codegen test alone.
    let out = run("\n\
         shared struct Node { label: String }\n\
         fn agg(ns: Vec[Node]) -> i64 { return ns.len(); }\n\
         fn main() {\n\
             let mut ns: Vec[Node] = Vec.new();\n\
             ns.push(Node { label: \"alpha\" });\n\
             ns.push(Node { label: \"beta\" });\n\
             ns.push(Node { label: \"gamma\" });\n\
             let mut k = 0;\n\
             let mut t = 0;\n\
             while k < 5 { t = t + agg(ns.clone()); k = k + 1; }\n\
             println(t);\n\
             println(ns.clone().len());\n\
             println(ns[0].label);\n\
             println(ns[2].label);\n\
             println(ns.len());\n\
         }\n");
    assert_eq!(out, "15\n3\nalpha\ngamma\n3\n");
}

#[test]
fn test_derive_partial_eq_vec_field_oracle() {
    // Oracle twin of `tests/codegen.rs`'s
    // `test_e2e_derive_partial_eq_vec_field_compares_contents`
    // (B-2026-08-18-6). The `PartialEq` spelling was refused at check time, so
    // its lowering had never been exercised; these are B-2026-08-12-5's own
    // shapes, where a byte-compare instead of an element-compare gives
    // SILENTLY wrong answers ([7] == [263] and [1,2] == [1,3] both true, and
    // equal `Vec[String]` contents false).
    let out = run("\n\
         #[derive(PartialEq)]\n\
         struct I { v: Vec[i64] }\n\
         #[derive(PartialEq)]\n\
         struct S { v: Vec[String] }\n\
         fn one(a: i64) -> Vec[i64] { let mut v: Vec[i64] = Vec.new(); v.push(a); v }\n\
         fn two(a: i64, b: i64) -> Vec[i64] { let mut v: Vec[i64] = Vec.new(); v.push(a); v.push(b); v }\n\
         fn strs(a: String) -> Vec[String] { let mut v: Vec[String] = Vec.new(); v.push(a); v }\n\
         fn main() {\n\
             println(I { v: one(7) } == I { v: one(263) });\n\
             println(I { v: two(1, 2) } == I { v: two(1, 3) });\n\
             println(I { v: one(7) } == I { v: one(7) });\n\
             println(I { v: one(1) } == I { v: two(1, 2) });\n\
             println(S { v: strs(\"abcd\") } == S { v: strs(\"abcd\") });\n\
             println(S { v: strs(\"abcd\") } == S { v: strs(\"zzzz\") });\n\
             println(I { v: two(1, 2) } != I { v: two(1, 3) });\n\
         }\n");
    assert_eq!(out, "false\nfalse\ntrue\nfalse\ntrue\nfalse\ntrue\n");
}

// ── B-2026-08-21-10: `Vec.from_fn(n, f)` ────────────────────────
//
// The index-driven constructor beside `Vec.filled` in design.md's `Vec`
// table. The interpreter calls the function once per index in ascending
// order and keeps the result as-is — no clone, because each call produces a
// fresh value, which is exactly what separates it from `filled`.

#[test]
fn vec_from_fn_computes_each_slot_by_index() {
    // design.md's own worked example, verbatim.
    let out = run("fn main() {\n\
        let evens = Vec.from_fn(5, |i| i * 2);\n\
        println(f\"{evens[0]} {evens[1]} {evens[2]} {evens[3]} {evens[4]} len={evens.len()}\");\n\
    }");
    assert_eq!(out, "0 2 4 6 8 len=5\n");
}

#[test]
fn vec_from_fn_handles_zero_and_heap_elements() {
    let out = run("fn main() {\n\
        let e: Vec[i64] = Vec.from_fn(0, |i| i);\n\
        let s: Vec[String] = Vec.from_fn(3, |i| f\"n{i}\");\n\
        println(f\"{e.len()} {s[0]} {s[2]}\");\n\
    }");
    assert_eq!(out, "0 n0 n2\n");
}

#[test]
fn vec_from_fn_sees_the_enclosing_scope() {
    // The function is a closure, so an outer binding it captures is live at
    // every index — not just the first.
    let out = run("fn main() {\n\
        let base = 100i64;\n\
        let v = Vec.from_fn(3, |i| base + i);\n\
        println(f\"{v[0]} {v[2]}\");\n\
    }");
    assert_eq!(out, "100 102\n");
}

#[test]
fn vec_from_fn_rejects_a_negative_length() {
    // Kāra has no `usize`, so a negative count is a runtime error rather than
    // an empty result — the same rule `Vec.filled` follows.
    let errors = runtime_errors(
        "fn main() {\n\
             let n = -1i64;\n\
             let v: Vec[i64] = Vec.from_fn(n, |i| i);\n\
             println(v.len());\n\
         }",
    );
    assert!(
        errors.iter().any(|e| e
            .message
            .contains("Vec.from_fn length must be non-negative")),
        "negative from_fn length must be a runtime error; got {:?}",
        errors.iter().map(|e| &e.message).collect::<Vec<_>>()
    );
}

// ── B-2026-08-25-20: the Vec capacity family ────────────────────
//
// design.md § Fallible Allocation's panicking/fallible table named
// `reserve` / `reserve_exact` / `resize` / `append` and their `try_*` twins;
// none of the eight existed. The companions could not land first —
// `fallible_alloc.rs` DERIVES every `try_X` from its panicking `X` — so the
// missing half was always the base.
//
// `capacity()` lands with them because without it `reserve` is unobservable
// from Kāra and no test can tell `v.reserve(1000)` from a no-op.

/// Capacity is a LOWER-BOUND query, so every assertion here is `>=`. Pinning an
/// exact number would be asserting something neither backend promises: the
/// interpreter grows a host `Vec<Value>` on Rust's policy and codegen grows on
/// `max(4, cap * 2)`. See the design.md row.
#[test]
fn vec_reserve_makes_room_without_changing_len() {
    let out = run(r#"
fn main() {
    let mut v: Vec[i64] = Vec.new();
    v.reserve(100);
    println(f"{v.len()} {v.capacity() >= 100}");
    let mut i = 0;
    while i < 100 { v.push(i); i = i + 1; }
    println(f"{v.len()} {v[0]} {v[99]}");
}
"#);
    assert_eq!(out, "0 true\n100 0 99\n");
}

#[test]
fn vec_reserve_exact_and_a_nonpositive_reserve_is_a_no_op() {
    let out = run(r#"
fn main() {
    let mut v: Vec[i64] = Vec.new();
    v.push(1);
    v.reserve_exact(7);
    println(f"{v.capacity() >= 8} {v.len()}");
    v.reserve(-5);
    v.reserve(0);
    println(f"{v.len()} {v[0]}");
}
"#);
    assert_eq!(out, "true 1\n1 1\n");
}

#[test]
fn vec_resize_grows_with_copies_and_shrinks_like_truncate() {
    let out = run(r#"
fn main() {
    let mut v: Vec[i64] = Vec.new();
    v.push(1); v.push(2); v.push(3);
    v.resize(5, 9);
    println(f"{v.len()} {v[3]} {v[4]}");
    v.resize(2, 0);
    println(f"{v.len()} {v[0]} {v[1]}");
    v.resize(-3, 0);
    println(v.len());
}
"#);
    assert_eq!(out, "5 9 9\n2 1 2\n0\n");
}

/// The heap-element leg, which is where `resize`'s ownership rule bites: the
/// fill value is MOVED into the first new slot and DEEP-CLONED into the rest,
/// so no two slots share a buffer.
#[test]
fn vec_resize_deep_copies_a_heap_fill_value_into_every_new_slot() {
    let out = run(r#"
fn main() {
    let mut v: Vec[String] = Vec.new();
    v.push("a"); v.push("b");
    v.resize(5, "z");
    println(f"{v.len()} {v[2]} {v[3]} {v[4]}");
    v.resize(1, "q");
    println(f"{v.len()} {v[0]}");
}
"#);
    assert_eq!(out, "5 z z z\n1 a\n");
}

/// `append` takes its source BY VALUE. That is the deliberate divergence from
/// Rust's `&mut Vec<T>`: Kāra call sites never write `mut` on a method
/// argument, so a borrow-and-drain spelling would empty `other` with nothing at
/// the call site to say so.
#[test]
fn vec_append_moves_every_element_of_an_owned_source() {
    let out = run(r#"
fn main() {
    let mut a: Vec[i64] = Vec.new();
    a.push(10); a.push(20);
    let mut b: Vec[i64] = Vec.new();
    b.push(30); b.push(40);
    a.append(b);
    println(f"{a.len()} {a[0]} {a[1]} {a[2]} {a[3]}");
    let mut e: Vec[String] = Vec.new();
    let mut f: Vec[String] = Vec.new();
    f.push("solo");
    e.append(f);
    println(f"{e.len()} {e[0]}");
}
"#);
    assert_eq!(out, "4 10 20 30 40\n1 solo\n");
}

/// The whole point of the row: with the panicking bases in place, the `try_*`
/// companions derive. `try_reserve` returns `Result[(), AllocError]` and
/// propagates through `?` like any other `Result`.
#[test]
fn vec_try_reserve_companions_derive_from_their_panicking_bases() {
    let out = run(r#"
fn build() -> Result[bool, AllocError] {
    let mut v: Vec[i64] = Vec.new();
    v.try_reserve(64)?;
    let mut w: Vec[i64] = Vec.new();
    w.try_reserve_exact(9)?;
    return Ok(v.capacity() >= 64 and w.capacity() >= 9);
}
fn main() {
    match build() {
        Ok(n) => { println(f"ok {n}"); }
        Err(e) => { println("alloc failed"); }
    }
}
"#);
    assert_eq!(out, "ok true\n");
}

#[test]
fn vec_try_resize_and_try_append_derive_from_their_bases() {
    let out = run(r#"
fn build() -> Result[i64, AllocError] {
    let mut v: Vec[i64] = Vec.new();
    v.push(1); v.push(2); v.push(3);
    v.try_resize(6, 7)?;
    println(f"grow {v.len()} {v[5]}");
    v.try_resize(2, 0)?;
    println(f"shrink {v.len()}");
    let mut a: Vec[i64] = Vec.new();
    a.push(10);
    let mut c: Vec[i64] = Vec.new();
    c.push(30);
    a.try_append(c)?;
    println(f"append {a.len()} {a[1]}");
    let mut p: Vec[String] = Vec.new();
    p.push("one");
    p.try_resize(3, "pad")?;
    let mut q: Vec[String] = Vec.new();
    q.push("tail");
    p.try_append(q)?;
    println(f"heap {p.len()} {p[2]} {p[3]}");
    return Ok(p.len());
}
fn main() {
    match build() {
        Ok(n) => { println(f"ok {n}"); }
        Err(e) => { println("alloc failed"); }
    }
}
"#);
    assert_eq!(
        out,
        "grow 6 7\nshrink 2\nappend 2 30\nheap 4 pad tail\nok 4\n"
    );
}

/// B-2026-09-15-23 — a `Vec`/`VecDeque` STRUCT FIELD whose ELEMENT is itself
/// an aggregate runs the innermost value's `Drop` body on the tree-walk
/// backend.
///
/// The interpreter twin of the codegen E2E
/// (`e2e_nested_container_in_a_vec_field_runs_its_innermost_drop_bodies`),
/// which lives behind `--features llvm` and so is invisible to the DEFAULT leg.
/// Both backends were silent here, so the row moved all six gates in one
/// commit; this fixture keeps the interpreter half under the gate CI runs.
///
/// Two sites on this side. `field_te_runs_user_drop` read the element's HEAD
/// NAME, so it asked `"Vec"` / `"Array"` and answered false, and a tuple
/// element is not a `Path` at all and fell off its `_ => false` tail — the
/// field classified drop-free and no walk was registered. Then the walk itself
/// (`drop_user_drop_fields_of_value`) dispatched per element on `Value::Struct`
/// / user `Value::EnumVariant` only, so an `Array`, `Tuple` or `Option`
/// element fell to `_ => {}`.
///
/// The `Option` element is the one this row's own fix broke before fixing:
/// with the five other sites in place and that arm absent, `Vec[Option[D]]`
/// fired on the three compiled surfaces and stayed silent here, because
/// codegen's `emit_nested_vec_elem_bodies_fn` has an Option arm. Measured
/// agreed-silent before, agreed-firing after.
#[test]
fn test_nested_container_in_a_vec_field_runs_its_innermost_drop_bodies() {
    const H: &str = "struct D { id: i64, s: String }\n\
         impl Drop for D { fn drop(mut ref self) { println(f\"dD{self.id}\") } }\n\
         fn mkd(n: i64) -> D { return D { id: n, s: f\"heap-{n}\" }; }\n";
    for (label, decls, body, want) in [
        (
            "the row's own cell — a Vec[Vec[D]] field",
            "struct H { xs: Vec[Vec[D]] }\n",
            "let h: H = H { xs: [[mkd(1)]] };",
            "dD1\nend\n",
        ),
        (
            "a Vec of fixed Arrays",
            "struct H { xs: Vec[Array[D, 2]] }\n",
            "let h: H = H { xs: [[mkd(1), mkd(2)]] };",
            "dD1\ndD2\nend\n",
        ),
        (
            "a Vec of tuples",
            "struct H { xs: Vec[(D, i64)] }\n",
            "let h: H = H { xs: [(mkd(1), 5)] };",
            "dD1\nend\n",
        ),
        (
            "three container levels",
            "struct H { xs: Vec[Vec[Vec[D]]] }\n",
            "let h: H = H { xs: [[[mkd(1)]]] };",
            "dD1\nend\n",
        ),
        (
            "an Option element — the arm this row's fix needed before it was correct",
            "struct H { xs: Vec[Option[D]] }\n",
            "let h: H = H { xs: [Some(mkd(1))] };",
            "dD1\nend\n",
        ),
        (
            "the push-built spelling of the same field",
            "struct H { xs: Vec[Vec[D]] }\n",
            "let mut v: Vec[Vec[D]] = [];\n\
             let e: Vec[D] = [mkd(1)];\n\
             v.push(e);\n\
             let h: H = H { xs: v };",
            "dD1\nend\n",
        ),
        (
            "the holder declares its own Drop — own body first",
            "struct H { xs: Vec[Vec[D]] }\n\
             impl Drop for H { fn drop(mut ref self) { println(\"dH\") } }\n",
            "let h: H = H { xs: [[mkd(1)]] };",
            "dH\ndD1\nend\n",
        ),
        (
            "control: the flat Vec[D] field",
            "struct H { xs: Vec[D] }\n",
            "let h: H = H { xs: [mkd(1)] };",
            "dD1\nend\n",
        ),
        (
            "control: an EMPTY outer vec fires no body",
            "struct H { xs: Vec[Vec[D]] }\n",
            "let h: H = H { xs: [] };",
            "end\n",
        ),
        (
            "pinned: a shared holder stays an agreed silence",
            "shared struct H { xs: Vec[Vec[D]] }\n",
            "let h: H = H { xs: [[mkd(1)]] };",
            "end\n",
        ),
    ] {
        assert_eq!(
            run(&format!(
                "{H}{decls}fn main() {{\n{body}\nprintln(\"end\");\n}}\n"
            )),
            want,
            "{label}"
        );
    }
}

/// B-2026-09-10-20 — the INTERPRETER twin of `tests/codegen.rs`'s
/// `e2e_declared_vec_enum_payload_runs_element_drop_bodies`, byte-identical
/// source and expectation.
///
/// This side was silent too — the gap was AGREED, so a one-backend fix would
/// have turned a missing body into an A/B divergence, which is the trade
/// B-2026-09-12-6 refused and B-2026-09-12-24 restates as a rule.
///
/// B-2026-09-17-19 made the `sharedec` cell a knowing divergence, and
/// B-2026-09-19-17 CLOSED IT: `H3.P(SMono.P(mkr(1)))` now prints `d2:9` here
/// as well as on the compiled backends. That row expected the fix to be
/// expensive — "a `shared enum` value is a plain `Value::EnumVariant` with no
/// `Arc`", so there is no refcount to consult — and the measurement that made
/// it cheap is that NO REFCOUNT IS NEEDED to match the compiled count. The
/// four sibling holders of a shared enum (a struct FIELD, a tuple ELEMENT, a
/// `Vec` ELEMENT, an `Option` payload) already fire the payload body on their
/// holder's death with no count, and agree with every compiled surface on it;
/// the enum holder was the one position whose walk stopped short, because
/// `run_enum_payload_user_drops_value` destructured a `Value::Struct` payload
/// and let an enum payload fall through. `E_ENUM_NESTED_ENUM_PAYLOAD` is what
/// makes the new arm safe without a `shared_types` gate: a PLAIN enum cannot
/// be an enum variant's payload at all, so the arm can only ever see the
/// `shared` / `par` spelling the diagnostic itself prescribes.
///
/// The `d2:9` lands BEFORE `x` here and after it on the compiled backends —
/// one body either way. That placement gap is the four siblings' too, and is
/// B-2026-09-19-18 rather than this cell's; design.md `:866` puts the correct
/// firing point at the binding's live-range end, which is this side.
///
/// `gensh` is the cell that KEEPS the arm narrow and must not move with it.
/// `enum G[T] { X(T), Y }` at `T = SMono` is silent on every compiled surface,
/// so the B-2026-09-10-2 own-generic-param exception — which exists because
/// codegen's instantiation-keyed walker DOES run a struct payload's body there
/// — is withheld from an enum payload. Admitting it fired a body on this side
/// alone, measured while writing the arm. It is STILL silent on all four
/// surfaces after B-2026-09-20-62 (measured 2026-09-21), which is what says
/// that row widened the container arm and not the exception.
///
/// `genvec` WAS THE OTHER HALF OF THAT NARROWNESS AND HAS BEEN FLIPPED.
/// `G[T]` at `T = Vec[Mono]` was silent on every surface when this fixture
/// was written, and the note here read that both container arms are keyed on
/// the same DECLARED payload head — `Array[T, N]` and `Vec[T]` share one
/// `Value::Array` on this backend, so nothing about the VALUE could tell them
/// apart. B-2026-09-20-62 is the row that made the declared head the wrong
/// question on BOTH backends at once: codegen's instantiation-keyed walker
/// head stopped refusing the `Vec` arm by the spelling of the declaration,
/// and this side resolves a bare-parameter payload against the binding's
/// recorded instantiation for `Vec` as well as `Array`. So `genvec` now
/// prints `d2:9` — the `Mono` element's own `R2` payload body, one level in —
/// on `--interp`, the JIT and AOT at both opt levels. The agreed gap this
/// cell pinned is gone; the AGREEMENT is not.
#[test]
fn test_declared_vec_enum_payload_runs_element_drop_bodies() {
    let out = run(r#"struct R2 { s: String, t: String, u: String }
impl Drop for R2 { fn drop(mut ref self) { println(f"  d2:{self.s.len()}") } }
fn mkr(i: i64) -> R2 { return R2 { s: f"aaaaaaaaa", t: f"b", u: f"c" } }
struct S1 { v: i64 }
impl Drop for S1 { fn drop(mut ref self) { println(f"  dS{self.v}") } }
enum Mono { P(R2), Q }
shared enum SMono { P(R2), Q }
enum G[T] { X(T), Y }
enum H3 { P(SMono), Q }
enum H4 { P(Vec[Mono]), Q }
enum H5 { P(Vec[S1]), Q }
enum H6 { P(Vec[S1], i64), Q }
enum H7 { P(Array[S1, 2]), Q }

fn main() {
    println("vecenum"); { let mut w: Vec[Mono] = []; w.push(Mono.P(mkr(1))); let h = H4.P(w); println("  x") }
    println("vecstruct"); { let mut w: Vec[S1] = []; w.push(S1 { v: 7 }); w.push(S1 { v: 8 }); let h = H5.P(w); println("  x") }
    println("vecmixed"); { let mut w: Vec[S1] = []; w.push(S1 { v: 9 }); let h = H6.P(w, 3); println("  x") }
    println("vecempty"); { let w: Vec[S1] = []; let h = H5.P(w); println("  x") }
    println("unitvar"); { let h = H5.Q; println("  x") }
    println("array"); { let h = H7.P([S1 { v: 1 }, S1 { v: 2 }]); println("  x") }
    println("struct"); { let g = G.X(mkr(1)); println("  x") }
    println("sharedec"); { let h = H3.P(SMono.P(mkr(1))); println("  x") }
    println("genvec"); { let mut w: Vec[Mono] = []; w.push(Mono.P(mkr(1))); let g = G.X(w); println("  x") }
    println("gensh"); { let g = G.X(SMono.P(mkr(1))); println("  x") }
    println("end")
}
"#);
    assert_eq!(out, "vecenum\n  d2:9\n  x\nvecstruct\n  dS7\n  dS8\n  x\nvecmixed\n  dS9\n  x\nvecempty\n  x\nunitvar\n  x\narray\n  dS1\n  dS2\n  x\nstruct\n  d2:9\n  x\nsharedec\n  d2:9\n  x\ngenvec\n  d2:9\n  x\ngensh\n  x\nend\n", "got:\n{out}");
}

/// B-2026-09-20-45 — a generic enum whose payload instantiates to
/// `Array[R, N]` must run each element's user `Drop` body when the binding
/// dies. It ran NONE, because the payload walk discriminates on the DECLARED
/// head — the interpreter gives `Array[T, N]` and `Vec[T]` one `Value::Array`,
/// so only the declaration tells them apart — and a generic enum's declared
/// head is the bare parameter `T`.
///
/// THE CELL WITH NO CALLEE IS THE POINT. The filing row's program passed the
/// enum to a by-value callee, which made the fault look like an ownership or
/// parameter-passing question and made it AGREED across all four surfaces, so
/// no A/B against `--interp` could see it. Drop the call and the gate is still
/// there while the three compiled surfaces are correct and valgrind is clean
/// at 13 allocs / 13 frees — a plain run-vs-build divergence, and the smallest
/// program that exhibits this defect at all.
///
/// The three controls are what keep the fix from being the one an earlier
/// draft of the `Array` payload arm was thrown away for. A MONOMORPHIC `Vec`
/// payload runs its bodies on all four surfaces today; when this fixture was
/// written the generic `G[Vec[R]]` was silent on BOTH, so a substitution that
/// let the walk's arms dispatch on any instantiation would have made this
/// backend fire where every compiled surface was silent — trading an agreed
/// gap for a new divergence. `k_vec` pinned that silence. `k_struct` pins the
/// plain-struct payload, which was already correct and must not double. `k_ng`
/// pins the NON-generic `Array` payload, which reaches the same arm by the
/// declared head and must be untouched by any of this.
///
/// `k_vec` HAS SINCE BEEN FLIPPED, and it is the flip this cell was designed
/// to make safe. B-2026-09-20-62 moved BOTH halves in one commit — codegen's
/// instantiation-keyed walker head stopped refusing the `Vec` arm by the
/// spelling of the declaration, and this backend gained the matching `Vec`
/// head — so `G[Vec[R]]` now prints `dR31 dR32` on `--interp`, the JIT and
/// AOT at both opt levels (measured, four surfaces, 2026-09-21). The
/// divergence this control existed to forbid is exactly what a ONE-backend
/// fix would still produce; what retired the pin is that no such fix was
/// made. The `Array` target above is unchanged by that work and still passes
/// on B-2026-09-20-45's own fix.
/// B-2026-09-21-11 (interpreter twin) — the reference answers for a `Vec`
/// payload bound out of a fresh ctor temp `match` scrutinee.
///
/// The first three cells are what the compiled surfaces were brought up to.
/// The last two are AGREED GAPS pinned as such: an arm that discards its
/// payload stands the whole construct down (the husk's gate is a union over
/// every arm), and an `Option` payload is silent everywhere. Arming either on
/// the compiled side alone would trade an agreement for a divergence.
#[test]
fn test_arm_binding_vec_payload_elem_drop_bodies() {
    let hdr = "struct R { id: i64 }\n\
               impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\") } }\n\
               fn mkr(i: i64) -> R { return R { id: i }; }\n\
               struct W { v: R }\n\
               impl Drop for W { fn drop(mut ref self) { println(\"dW\") } }\n\
               enum Slot[T] { S(T), N }\n";
    for (label, stmts, want) in [
        (
            "Vec payload, fresh ctor temp, read-only arm",
            "let a: Vec[R] = [mkr(1), mkr(2)];\n\
             match Slot.S(a) { Slot.S(v) => { println(f\"x{v[0].id}\") } Slot.N => { println(\"no\") } }",
            "x1\ndR1\ndR2\nend\n",
        ),
        (
            "the same with a GUARD and two binding arms",
            "let a: Vec[R] = [mkr(1), mkr(2)];\n\
             match Slot.S(a) {\n\
               Slot.S(v) if v[0].id > 99 => { println(f\"big{v[0].id}\") }\n\
               Slot.S(w) => { println(f\"x{w[1].id}\") }\n\
               Slot.N => { println(\"no\") }\n\
             }",
            "x2\ndR1\ndR2\nend\n",
        ),
        (
            "an element carrying its own body over a Drop-bearing field",
            "let a: Vec[W] = [W { v: mkr(1) }, W { v: mkr(2) }];\n\
             match Slot.S(a) { Slot.S(v) => { println(\"x\") } Slot.N => { println(\"no\") } }",
            "x\ndW\ndR1\ndW\ndR2\nend\n",
        ),
        (
            "AGREED GAP (B-2026-09-21-12): one binding arm beside one that discards",
            "let a: Vec[R] = [mkr(1), mkr(2)];\n\
             match Slot.S(a) {\n\
               Slot.S(v) if v[0].id > 99 => { println(\"big\") }\n\
               Slot.S(_) => { println(\"x\") }\n\
               Slot.N => { println(\"no\") }\n\
             }",
            "x\nend\n",
        ),
        (
            "AGREED GAP: an `Option[R]` payload, silent on all four surfaces",
            "let a: Option[R] = Option.Some(mkr(3));\n\
             match Slot.S(a) { Slot.S(v) => { println(\"x\") } Slot.N => { println(\"no\") } }",
            "x\nend\n",
        ),
    ] {
        let src = format!("{hdr}fn main() {{\n{stmts}\nprintln(\"end\");\n}}\n");
        assert_eq!(run(&src), want, "[{label}]");
    }
}
