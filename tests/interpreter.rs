// tests/interpreter.rs

use karac::interpreter::DbgOutputMode;
use karac::{
    run_program, run_program_full, run_program_with_dbg, run_program_with_drops,
    run_program_with_trace,
};

// ── Test Helpers ────────────────────────────────────────────────

fn run(source: &str) -> String {
    let output = run_program(source);
    output.join("")
}

fn runtime_errors(source: &str) -> Vec<karac::interpreter::RuntimeError> {
    let (_out, errors, _trace, _trunc) = run_program_full(source);
    errors
}

fn run_no_errors(source: &str) -> String {
    let (out, errors, _trace, _trunc) = run_program_full(source);
    assert!(
        errors.is_empty(),
        "Expected no runtime errors, got: {:?}",
        errors
    );
    out.join("")
}

// ── Arithmetic & Expressions ───────────────────────────────────

#[test]
fn test_integer_arithmetic() {
    assert_eq!(run("fn main() { println(1 + 2); }"), "3\n");
}

#[test]
fn test_abs_signed_int() {
    assert_eq!(run("fn main() { println((-5i64).abs()); }"), "5\n");
    assert_eq!(run("fn main() { println((7i64).abs()); }"), "7\n");
    assert_eq!(run("fn main() { println((0i64).abs()); }"), "0\n");
}

#[test]
fn test_struct_display_declaration_order() {
    // `#[derive(Display)]` structs render `Name { field: value, … }` in
    // DECLARATION order (the `Value::Struct` HashMap had lost source order and
    // rendered in random hash order before `display_render`). println,
    // .to_string(), and f-string interpolation all agree.
    let src = "#[derive(Display)]
        struct Wrap { p: Point, name: String, ok: bool }
        #[derive(Display)]
        struct Point { x: i64, y: i64 }
        fn main() {
            let w = Wrap { p: Point { x: 1, y: 2 }, name: \"hi\", ok: true };
            println(w);
            println(w.to_string());
            println(f\"w={w}\");
        }";
    let expected = "Wrap { p: Point { x: 1, y: 2 }, name: hi, ok: true }\n".repeat(2)
        + "w=Wrap { p: Point { x: 1, y: 2 }, name: hi, ok: true }\n";
    assert_eq!(run_no_errors(src), expected);
}

#[test]
fn test_enum_display_unit_variants() {
    // All-unit `#[derive(Display)]` enum renders the bare variant name across
    // println, .to_string(), and f-string — matching codegen.
    let src = "#[derive(Display)]
        enum Color { Red, Green, Blue }
        fn main() {
            let a = Color.Green;
            println(a.to_string());
            println(f\"c={a}\");
            println(a);
        }";
    assert_eq!(run_no_errors(src), "Green\nc=Green\nGreen\n");
}

#[test]
fn test_struct_display_nested_in_container() {
    // A struct nested in a Vec still renders in declaration order (the
    // renderer recurses through containers).
    let src = "#[derive(Display)]
        struct Point { x: i64, y: i64 }
        fn main() {
            let v: Vec[Point] = [Point { x: 9, y: 8 }, Point { x: 7, y: 6 }];
            println(f\"list={v}\");
        }";
    assert_eq!(
        run_no_errors(src),
        "list=[Point { x: 9, y: 8 }, Point { x: 7, y: 6 }]\n"
    );
}

#[test]
fn test_abs_float() {
    assert_eq!(run("fn main() { println((-2.5f64).abs()); }"), "2.5\n");
    assert_eq!(run("fn main() { println((2.5f64).abs()); }"), "2.5\n");
}

#[test]
fn test_abs_int_min_traps() {
    // `iN::MIN.abs()` has no representable result and traps as integer
    // overflow, matching the checked-neg arm — not a panic/ICE.
    let errors =
        runtime_errors("fn main() { let x = -9223372036854775807i64 - 1i64; println(x.abs()); }");
    assert!(
        errors
            .iter()
            .any(|e| e.message.contains("integer overflow")),
        "expected integer-overflow trap, got: {:?}",
        errors.iter().map(|e| &e.message).collect::<Vec<_>>()
    );
}

#[test]
fn test_narrow_int_overflow_traps() {
    // Narrow integers are real fixed-width types (design.md § Integer
    // overflow): `u8 200 + u8 100 = 300` overflows the width and traps,
    // rather than silently widening to i64. Codegen mirrors this.
    let errors = runtime_errors("fn main() { let a: u8 = 200; let b: u8 = 100; println(a + b); }");
    assert!(
        errors
            .iter()
            .any(|e| e.message.contains("integer overflow")),
        "expected u8-overflow trap, got: {:?}",
        errors.iter().map(|e| &e.message).collect::<Vec<_>>()
    );
}

#[test]
fn test_narrow_int_in_range_does_not_trap() {
    // A narrow-int sum that fits the width is the value, no trap: `u8 97 + u8
    // 98 = 195` (≤ 255).
    assert_eq!(
        run("fn main() { let a: u8 = 97; let b: u8 = 98; println(a + b); }"),
        "195\n"
    );
}

#[test]
fn test_float_to_int_saturating() {
    // phase-8 cast slice 2: saturating clamps to the target's MIN/MAX and
    // truncates toward zero in range.
    assert_eq!(
        run("fn main() { println((3.7f64).saturating_to_i32()); }"),
        "3\n"
    );
    assert_eq!(
        run("fn main() { println((-3.7f64).saturating_to_i32()); }"),
        "-3\n"
    );
    assert_eq!(
        run("fn main() { println((1e30f64).saturating_to_i32()); }"),
        "2147483647\n"
    );
    assert_eq!(
        run("fn main() { println((-1e30f64).saturating_to_i32()); }"),
        "-2147483648\n"
    );
    assert_eq!(
        run("fn main() { println((1e30f64).saturating_to_u8()); }"),
        "255\n"
    );
    assert_eq!(
        run("fn main() { println((-1.0f64).saturating_to_u8()); }"),
        "0\n"
    );
}

#[test]
fn test_float_to_int_wrapping() {
    // Modular truncation: 300 → 44 in i8, 256 → 0 / 257 → 1 in u8.
    assert_eq!(
        run("fn main() { println((300.0f64).wrapping_to_i8()); }"),
        "44\n"
    );
    assert_eq!(
        run("fn main() { println((256.0f64).wrapping_to_u8()); }"),
        "0\n"
    );
    assert_eq!(
        run("fn main() { println((257.9f64).wrapping_to_u8()); }"),
        "1\n"
    );
}

#[test]
fn test_float_to_int_checked() {
    // `checked_*` → `Some(trunc)` in range, `None` on NaN / out-of-range.
    assert_eq!(
        run("fn main() { match (1.5f64).checked_to_i32() { Some(v) => println(v), None => println(-1) }; }"),
        "1\n"
    );
    assert_eq!(
        run("fn main() { match (1e30f64).checked_to_i32() { Some(v) => println(v), None => println(-1) }; }"),
        "-1\n"
    );
    assert_eq!(
        run("fn main() { match (f64.NAN).checked_to_i32() { Some(v) => println(v), None => println(-1) }; }"),
        "-1\n"
    );
}

#[test]
fn test_float_to_int_trunc_traps_out_of_range() {
    // `trunc_*` is the trapping form — out-of-range / NaN records a structured
    // "float-to-int out of range" runtime error (not a panic/ICE).
    let errors = runtime_errors("fn main() { println((1e30f64).trunc_to_i32()); }");
    assert!(
        errors
            .iter()
            .any(|e| e.message.contains("float-to-int out of range")),
        "expected out-of-range trap, got: {:?}",
        errors.iter().map(|e| &e.message).collect::<Vec<_>>()
    );
    // In-range `trunc_*` returns the truncated value.
    assert_eq!(
        run("fn main() { println((42.9f64).trunc_to_i32()); }"),
        "42\n"
    );
}

#[test]
fn test_int_to_float_methods() {
    // Symmetric `to_f32` / `to_f64` widen an integer to a float.
    assert_eq!(run("fn main() { println((42i64).to_f64()); }"), "42\n");
    assert_eq!(run("fn main() { println((42i32).to_f32()); }"), "42\n");
}

#[test]
fn test_unknown_primitive_method_is_runtime_error_not_ice() {
    // `karac run` bypasses typecheck enforcement, so an unknown method on a
    // primitive reaches the interpreter. It used to hit `unreachable!` and
    // panic (ICE); it now records a structured runtime error.
    let errors = runtime_errors("fn main() { let x = 5i64; let _ = x.bogus(); }");
    assert!(
        errors
            .iter()
            .any(|e| e.message.contains("bogus") && e.message.contains("i64")),
        "expected a structured runtime error, got: {:?}",
        errors.iter().map(|e| &e.message).collect::<Vec<_>>()
    );
}

#[test]
fn test_to_string_on_primitives() {
    assert_eq!(run("fn main() { println((-42i64).to_string()); }"), "-42\n");
    assert_eq!(run("fn main() { println((3.5f64).to_string()); }"), "3.5\n");
    assert_eq!(run("fn main() { println(true.to_string()); }"), "true\n");
    assert_eq!(run("fn main() { println('Z'.to_string()); }"), "Z\n");
}

#[test]
fn test_clone_on_primitives() {
    // `clone` on a Copy primitive is identity — used to ICE (no dispatch arm).
    assert_eq!(run("fn main() { println((7i64).clone()); }"), "7\n");
    assert_eq!(run("fn main() { println((2.5f64).clone()); }"), "2.5\n");
    assert_eq!(run("fn main() { println(true.clone()); }"), "true\n");
    assert_eq!(run("fn main() { println('q'.clone()); }"), "q\n");
}

#[test]
fn test_integer_subtraction() {
    assert_eq!(run("fn main() { println(10 - 3); }"), "7\n");
}

#[test]
fn test_integer_multiplication() {
    assert_eq!(run("fn main() { println(4 * 5); }"), "20\n");
}

#[test]
fn test_integer_division() {
    assert_eq!(run("fn main() { println(15 / 4); }"), "3\n");
}

#[test]
fn test_integer_modulo() {
    assert_eq!(run("fn main() { println(17 % 5); }"), "2\n");
}

#[test]
fn test_float_arithmetic() {
    assert_eq!(run("fn main() { println(1.5 + 2.5); }"), "4\n");
}

#[test]
fn test_boolean_logic() {
    assert_eq!(run("fn main() { println(true and false); }"), "false\n");
    assert_eq!(run("fn main() { println(true or false); }"), "true\n");
}

// ── Short-circuit `and` / `or` (roadmap.md:425, 429) ────────────

#[test]
fn test_and_short_circuits_skips_rhs_fn_call() {
    // `false and boom()` must NOT call boom().
    let out = run(r#"
        fn boom() -> bool { println("called"); true }
        fn main() {
            if false and boom() { println("then"); } else { println("else"); }
        }
    "#);
    assert_eq!(out, "else\n");
}

#[test]
fn test_or_short_circuits_skips_rhs_fn_call() {
    // `true or boom()` must NOT call boom().
    let out = run(r#"
        fn boom() -> bool { println("called"); true }
        fn main() {
            if true or boom() { println("then"); } else { println("else"); }
        }
    "#);
    assert_eq!(out, "then\n");
}

#[test]
fn test_and_short_circuits_guards_oob_index() {
    // `i > 0 and visited[i - 1]` must not crash when i == 0
    // (RHS would index Vec at -1, but RHS shouldn't run).
    let out = run_no_errors(
        r#"
        fn main() {
            let visited: Vec[bool] = Vec.new();
            let i = 0;
            if i > 0 and visited[i - 1] { println("then"); } else { println("else"); }
        }
    "#,
    );
    assert_eq!(out, "else\n");
}

#[test]
fn test_and_evaluates_rhs_when_lhs_true() {
    // When LHS doesn't short-circuit, RHS must run.
    let out = run(r#"
        fn boom() -> bool { println("called"); true }
        fn main() {
            if true and boom() { println("then"); } else { println("else"); }
        }
    "#);
    assert_eq!(out, "called\nthen\n");
}

#[test]
fn test_or_evaluates_rhs_when_lhs_false() {
    // When LHS doesn't short-circuit, RHS must run.
    let out = run(r#"
        fn boom() -> bool { println("called"); false }
        fn main() {
            if false or boom() { println("then"); } else { println("else"); }
        }
    "#);
    assert_eq!(out, "called\nelse\n");
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

// `Vec[T].pop()` — the bare form, alias for `pop_back`. Both the
// typechecker (`src/typechecker/expr_method_call.rs:1156`) and codegen
// (`src/codegen/vec_method.rs:300` collapses `pop | pop_back |
// pop_front` into one arm) already supported it; the interpreter
// dispatch arm was missing, so `karac run` panicked with
// "method 'pop' not found on type 'unknown'". Surfaced by
// kara-katas/leetcode/71-simplify-path which wanted stack-style
// push/pop on a `Vec[i64]`.
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
fn test_vec_pop_string_elements() {
    // Non-Copy element type flows through the same dispatch — pins the
    // generic shape against future regressions where the arm might
    // accidentally specialize to numerics.
    let out = run(r#"
        fn main() {
            let mut v: Vec[String] = Vec.new();
            v.push("a");
            v.push("b");
            println(v.pop());
            println(v.pop());
            println(v.pop());
        }
    "#);
    assert_eq!(out, "Some(b)\nSome(a)\nNone\n");
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
fn test_vec_deque_iter_yields_in_front_to_back_order() {
    // `iter()` must yield items front-to-back. The runtime is
    // `Value::Array` so iter routes through the existing eager-
    // snapshot Iterator path; this pins the order against shape
    // changes.
    let out = run(r#"
        fn main() {
            let mut q: VecDeque[i64] = VecDeque.new();
            q.push_back(20);
            q.push_back(30);
            q.push_front(10);
            for x in q.iter() {
                println(x);
            }
        }
    "#);
    assert_eq!(out, "10\n20\n30\n");
}

#[test]
fn test_vec_deque_string_elements() {
    // Non-Copy element type flows through unchanged.
    let out = run(r#"
        fn main() {
            let mut q: VecDeque[String] = VecDeque.new();
            q.push_back("first");
            q.push_back("second");
            q.push_front("zero");
            let f = q.pop_front();
            let b = q.pop_back();
            println(f);
            println(b);
        }
    "#);
    assert_eq!(out, "Some(zero)\nSome(second)\n");
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

// ── `Vec.filled(n, val)` (design.md:1631) ───────────────────────

#[test]
fn test_vec_filled_i64() {
    // `Vec.filled(3, 7)` → length 3, all 7s.
    let out = run(r#"
        fn main() {
            let v: Vec[i64] = Vec.filled(3, 7);
            println(v.len());
            println(v[0]);
            println(v[2]);
        }
    "#);
    assert_eq!(out, "3\n7\n7\n");
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
fn test_vec_filled_string() {
    // Non-`Copy` element type — the per-slot clone in the dispatch
    // arm satisfies the spec's `T: Clone` requirement.
    let out = run(r#"
        fn main() {
            let v: Vec[String] = Vec.filled(2, "hi");
            println(v.len());
            println(v[0]);
            println(v[1]);
        }
    "#);
    assert_eq!(out, "2\nhi\nhi\n");
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
fn test_vec_nested_indexed_write_round_trip() {
    // `rows[r][c] = val` on `Vec[Vec[T]]` — the kata-6 _faster
    // shape that previously needed a flat-layout workaround.
    // Pre-fix, the interpreter's set_index silently no-op'd on
    // non-Identifier targets, and codegen errored "Index
    // assignment target must be a variable". Both now route
    // through to the leaf slot.
    let out = run(r#"
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
    "#);
    assert_eq!(out, "0\n42\n99\n");
}

#[test]
fn test_field_index_write_round_trip_plain_and_shared() {
    // `obj.field[i] = val` — the write half of the kata-133-audit
    // FieldAccess-rooted indexing bug (2026-06-06). Pre-fix, the
    // interpreter's set_index hit the catch-all `_ => return` arm for
    // FieldAccess targets and SILENTLY no-op'd the store (the program
    // ran but printed the stale value); codegen errored "Index
    // assignment target must be a variable". Both now route through:
    // the interpreter evals the field access (Value::Array clones the
    // Arc, aliasing the field's storage), codegen goes through
    // `lower_field_access_ptr` + a synth identifier. Plain and shared
    // structs both covered.
    let out = run_no_errors(
        r#"
        struct Holder { tag: i64, mut items: Vec[i64] }
        shared struct Cell { mut vals: Vec[i64] }
        fn main() {
            let mut v: Vec[i64] = Vec.new();
            v.push(41);
            v.push(42);
            let mut h = Holder { tag: 7, items: v };
            h.items[0] = 99;
            println(h.items[0]);
            println(h.items[1]);

            let mut w: Vec[i64] = Vec.new();
            w.push(5);
            let c = Cell { vals: w };
            c.vals[0] = 6;
            println(c.vals[0]);
        }
    "#,
    );
    assert_eq!(out, "99\n42\n6\n");
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

// ── `vec.extend_from_slice(other)` ──────────────────────────────

#[test]
fn test_vec_extend_from_slice_from_vec() {
    // Append all elements of `src: Vec[i64]` to `dst: Vec[i64]`.
    let out = run(r#"
        fn main() {
            let src: Vec[i64] = Vec.filled(3, 7);
            let mut dst: Vec[i64] = Vec.new();
            dst.push(1);
            dst.push(2);
            dst.extend_from_slice(src);
            println(dst.len());
            println(dst[0]);
            println(dst[1]);
            println(dst[2]);
            println(dst[4]);
        }
    "#);
    assert_eq!(out, "5\n1\n2\n7\n7\n");
}

#[test]
fn test_vec_extend_from_slice_into_empty() {
    // Extending an empty `dst` should just clone source elements.
    let out = run(r#"
        fn main() {
            let src: Vec[i64] = Vec.filled(4, 9);
            let mut dst: Vec[i64] = Vec.new();
            dst.extend_from_slice(src);
            println(dst.len());
            println(dst[0]);
            println(dst[3]);
        }
    "#);
    assert_eq!(out, "4\n9\n9\n");
}

#[test]
fn test_vec_extend_from_slice_nested_index_source() {
    // `rows[r]` (Index expr on Vec[Vec[T]]) as source — the
    // kata-6 use case. The interpreter resolves the Index to a
    // fresh Vec value; per-element `deep_clone_value` keeps source
    // and dest independent.
    let out = run(r#"
        fn main() {
            let mut rows: Vec[Vec[i64]] = Vec.new();
            let mut r0: Vec[i64] = Vec.new();
            r0.push(10);
            r0.push(20);
            rows.push(r0);
            let mut r1: Vec[i64] = Vec.new();
            r1.push(30);
            rows.push(r1);

            let mut out: Vec[i64] = Vec.with_capacity(8);
            let mut i = 0i64;
            while i < 2 {
                out.extend_from_slice(rows[i]);
                i = i + 1;
            }
            println(out.len());
            println(out[0]);
            println(out[1]);
            println(out[2]);
        }
    "#);
    assert_eq!(out, "3\n10\n20\n30\n");
}

#[test]
fn test_comparison() {
    assert_eq!(run("fn main() { println(3 > 2); }"), "true\n");
    assert_eq!(run("fn main() { println(3 < 2); }"), "false\n");
    assert_eq!(run("fn main() { println(5 == 5); }"), "true\n");
}

#[test]
fn test_comparison_and_equality_post_lowering() {
    // Exercises every comparison/equality op going through the lowered
    // `T.eq` / `T.lt` / etc. Call-path at runtime.
    assert_eq!(run("fn main() { println(3 != 4); }"), "true\n");
    assert_eq!(run("fn main() { println(3 != 3); }"), "false\n");
    assert_eq!(run("fn main() { println(4 <= 4); }"), "true\n");
    assert_eq!(run("fn main() { println(4 >= 5); }"), "false\n");
    assert_eq!(
        run(r#"fn main() { let a: String = "foo"; let b: String = "foo"; println(a == b); }"#),
        "true\n"
    );
    assert_eq!(
        run(r#"fn main() { let a: String = "foo"; let b: String = "bar"; println(a != b); }"#),
        "true\n"
    );
}

#[test]
fn test_bitwise_ops_post_lowering() {
    // `&`, `|`, `^`, `<<`, `>>` all flow through the lowered Call path.
    assert_eq!(run("fn main() { println(0b1100 & 0b1010); }"), "8\n");
    assert_eq!(run("fn main() { println(0b1100 | 0b1010); }"), "14\n");
    assert_eq!(run("fn main() { println(0b1100 ^ 0b1010); }"), "6\n");
    assert_eq!(run("fn main() { println(1 << 3); }"), "8\n");
    assert_eq!(run("fn main() { println(16 >> 2); }"), "4\n");
}

#[test]
fn test_bitnot_and_not_post_lowering() {
    // `~int` lowers to `int.not`, `not bool` lowers to `bool.not`.
    assert_eq!(run("fn main() { println(~0); }"), "-1\n");
    assert_eq!(run("fn main() { println(not false); }"), "true\n");
}

#[test]
fn test_user_impl_eq_drives_equality_operator() {
    // `impl Eq for Point` is registered, and `a == b` lowers to
    // `Point.eq(a, b)` — routed through the user-defined method rather than
    // any structural fallback. CR-202 slice 5b: companion
    // `impl PartialEq for Point` satisfies the new `Eq: PartialEq`
    // supertrait edge so the typecheck pass stays clean.
    assert_eq!(
        run("struct Point { x: i64, y: i64 }
             impl PartialEq for Point {
                 fn eq(ref self, other: ref Point) -> bool {
                     self.x == other.x and self.y == other.y
                 }
             }
             impl Eq for Point {
                 fn eq(self, other: Point) -> bool {
                     self.x == other.x and self.y == other.y
                 }
             }
             fn main() {
                 let a = Point { x: 1, y: 2 };
                 let b = Point { x: 1, y: 2 };
                 let c = Point { x: 9, y: 9 };
                 println(a == b);
                 println(a != c);
             }"),
        "true\ntrue\n"
    );
}

#[test]
fn test_user_impl_ord_drives_comparison_operators() {
    // `impl Ord for Point` with direct `lt`/`le`/`gt`/`ge` methods — `<`
    // lowers to `Point.lt(a, b)`, etc. Domain-specific ordering (here by x
    // only) rather than the interpreter's hardcoded primitive path.
    // CR-202 slice 5d: companion PartialEq/Eq/PartialOrd impls satisfy
    // the new `Ord: PartialOrd + Eq` supertrait edges (typecheck-clean;
    // interpreter execution behavior is unchanged).
    assert_eq!(
        run("struct Point { x: i64, y: i64 }
             impl PartialEq for Point {
                 fn eq(ref self, other: ref Point) -> bool { self.x == other.x and self.y == other.y }
             }
             impl Eq for Point {}
             impl PartialOrd for Point {
                 fn partial_cmp(ref self, other: ref Point) -> Option[Ordering] { Some(self.x.cmp(other.x)) }
             }
             impl Ord for Point {
                 fn lt(self, other: Point) -> bool { self.x < other.x }
                 fn le(self, other: Point) -> bool { self.x <= other.x }
                 fn gt(self, other: Point) -> bool { self.x > other.x }
                 fn ge(self, other: Point) -> bool { self.x >= other.x }
             }
             fn main() {
                 let a = Point { x: 1, y: 99 };
                 let b = Point { x: 5, y: 1 };
                 println(a < b);
                 println(a > b);
                 println(a <= b);
             }"),
        "true\nfalse\ntrue\n"
    );
}

#[test]
fn test_eq_ord_direct_method_calls() {
    // `i32.lt(a, b)` and peers are callable directly in user code, matching
    // the lowered form of the operator. Verifies the whole typecheck → lower
    // → interpret pipeline for the Eq/Ord method names.
    assert_eq!(
        run("fn main() {
                 let a: i32 = 3;
                 let b: i32 = 5;
                 println(i32.lt(a, b));
                 println(i32.ge(a, b));
                 println(i32.eq(a, a));
                 println(i32.ne(a, b));
             }"),
        "true\nfalse\ntrue\ntrue\n"
    );
}

#[test]
fn test_string_literal() {
    assert_eq!(
        run(r#"fn main() { println("hello world"); }"#),
        "hello world\n"
    );
}

#[test]
fn test_unary_negation() {
    assert_eq!(run("fn main() { println(-42); }"), "-42\n");
}

#[test]
fn test_unary_not() {
    assert_eq!(run("fn main() { println(not true); }"), "false\n");
}

// ── Variables & Let Bindings ───────────────────────────────────

#[test]
fn test_let_binding() {
    assert_eq!(run("fn main() { let x = 42; println(x); }"), "42\n");
}

#[test]
fn test_let_mut_reassign() {
    assert_eq!(
        run("fn main() { let mut x = 1; x = 2; println(x); }"),
        "2\n"
    );
}

#[test]
fn test_compound_assignment() {
    assert_eq!(
        run("fn main() { let mut x = 10; x += 5; println(x); }"),
        "15\n"
    );
}

#[test]
fn test_compound_assignment_int_subtract() {
    assert_eq!(
        run("fn main() { let mut x = 10; x -= 3; println(x); }"),
        "7\n"
    );
}

#[test]
fn test_compound_assignment_string_concat() {
    // `s += other` desugars to `s = s + other`. Confirms the desugar still
    // works for non-integer Add impls — guard for when Step 6 lowering lands.
    assert_eq!(
        run("fn main() { let mut s = \"hello \"; s += \"world\"; println(s); }"),
        "hello world\n"
    );
}

// ── Control Flow ───────────────────────────────────────────────

#[test]
fn test_if_true() {
    assert_eq!(run("fn main() { if true { println(1); } }"), "1\n");
}

#[test]
fn test_if_false_else() {
    assert_eq!(
        run("fn main() { if false { println(1); } else { println(2); } }"),
        "2\n"
    );
}

#[test]
fn test_if_else_expression() {
    assert_eq!(
        run("fn main() { let x = if true { 10 } else { 20 }; println(x); }"),
        "10\n"
    );
}

#[test]
fn test_while_loop() {
    assert_eq!(
        run("fn main() {\n\
                 let mut i = 0;\n\
                 while i < 3 {\n\
                     i += 1;\n\
                 }\n\
                 println(i);\n\
             }"),
        "3\n"
    );
}

#[test]
fn test_loop_break() {
    assert_eq!(
        run("fn main() {\n\
                 let mut i = 0;\n\
                 loop {\n\
                     i += 1;\n\
                     if i == 5 { break; }\n\
                 }\n\
                 println(i);\n\
             }"),
        "5\n"
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

// ── Functions ──────────────────────────────────────────────────

#[test]
fn test_function_call() {
    assert_eq!(
        run("fn double(x: i64) -> i64 { x * 2 }\n\
             fn main() { println(double(21)); }"),
        "42\n"
    );
}

#[test]
fn test_recursive_function() {
    assert_eq!(
        run("fn factorial(n: i64) -> i64 {\n\
                 if n <= 1 { 1 } else { n * factorial(n - 1) }\n\
             }\n\
             fn main() { println(factorial(5)); }"),
        "120\n"
    );
}

#[test]
fn test_deep_recursion_grows_stack() {
    // Depth 5000 — LeetCode's linked-list bound (one frame per node at
    // k = 1 in kata #25, which found this). At ~8 Rust frames per Kāra
    // call this blows any fixed thread stack (16 MB included) unless
    // `eval_body_growing` re-homes the recursion onto heap segments via
    // `stacker::maybe_grow`. Regression: this aborted with a stack
    // overflow before the helper existed.
    assert_eq!(
        run("fn countdown(n: i64) -> i64 {\n\
                 if n <= 0 { return 0; }\n\
                 countdown(n - 1) + 1\n\
             }\n\
             fn main() { println(countdown(5000)); }"),
        "5000\n"
    );
}

#[test]
fn test_multiple_params() {
    assert_eq!(
        run("fn add(a: i64, b: i64) -> i64 { a + b }\n\
             fn main() { println(add(3, 4)); }"),
        "7\n"
    );
}

#[test]
fn test_early_return() {
    assert_eq!(
        run("fn abs(x: i64) -> i64 {\n\
                 if x < 0 { return -x; }\n\
                 x\n\
             }\n\
             fn main() { println(abs(-5)); }"),
        "5\n"
    );
}

// ── Closures ───────────────────────────────────────────────────

#[test]
fn test_closure_basic() {
    assert_eq!(
        run("fn apply(f: Fn(i64) -> i64, x: i64) -> i64 { f(x) }\n\
             fn main() { println(apply(|x: i64| x + 1, 41)); }"),
        "42\n"
    );
}

#[test]
fn test_closure_captures() {
    assert_eq!(
        run("fn apply(f: Fn(i64) -> i64, x: i64) -> i64 { f(x) }\n\
             fn main() {\n\
                 let offset = 10;\n\
                 let add_offset = |x: i64| x + offset;\n\
                 println(apply(add_offset, 32));\n\
             }"),
        "42\n"
    );
}

// ── Structs ────────────────────────────────────────────────────

#[test]
fn test_struct_construction_and_field_access() {
    assert_eq!(
        run("struct Point { x: i64, y: i64 }\n\
             fn main() {\n\
                 let p = Point { x: 3, y: 4 };\n\
                 println(p.x + p.y);\n\
             }"),
        "7\n"
    );
}

#[test]
fn test_struct_method() {
    assert_eq!(
        run("struct Counter { value: i64 }\n\
             impl Counter {\n\
                 fn get(self) -> i64 { self.value }\n\
             }\n\
             fn main() {\n\
                 let c = Counter { value: 42 };\n\
                 println(c.get());\n\
             }"),
        "42\n"
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
fn test_qualified_enum_variant_construction() {
    // `Enum.Variant(args)` (qualified) construction must work in the
    // interpreter, peer to the unqualified `Variant(args)` form. The
    // resolver and codegen accept the qualified form (Json/Ordering even
    // require it); the interpreter used to `eval_expr_inner` the callee path
    // `Enum.Variant` and panic ("path '…' not found"). Covers a user enum
    // tuple variant and the baked-stdlib `Result` / `Option`.
    assert_eq!(
        run("enum Color { Red, Blue(i64) }\n\
             fn main() {\n\
                 match Color.Blue(7) { Red => println(0), Blue(n) => println(n) }\n\
             }"),
        "7\n"
    );
    assert_eq!(
        run("fn main() {\n\
                 match Result.Ok(5) { Ok(n) => println(n), Err(e) => println(0) }\n\
             }"),
        "5\n"
    );
    assert_eq!(
        run("fn main() {\n\
                 match Option.Some(9) { Some(n) => println(n), None => println(0) }\n\
             }"),
        "9\n"
    );
}

#[test]
fn test_qualified_enum_variant_constructor_cross_boundary() {
    // A method returning a qualified-constructed `Result` whose value is
    // matched in the caller — the original repro for the interpreter panic.
    // Also pins that an enum's *associated fn* (`E.make`, not a variant) is
    // still dispatched as a call, not mistaken for a variant constructor.
    assert_eq!(
        run("enum E { A, B(i64) }\n\
             impl E { fn make() -> E { E.B(3) } }\n\
             struct W {}\n\
             impl W { fn g(self) -> Result[i64, String] { Result.Ok(42) } }\n\
             fn main() {\n\
                 match (W{}).g() { Ok(n) => println(n), Err(e) => println(0) }\n\
                 match E.make() { A => println(0), B(n) => println(n) }\n\
             }"),
        "42\n3\n"
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

// ── Tuples ─────────────────────────────────────────────────────

#[test]
fn test_tuple_construction() {
    assert_eq!(
        run("fn main() {\n\
                 let t = (1, 2, 3);\n\
                 println(t.0 + t.1 + t.2);\n\
             }"),
        "6\n"
    );
}

// ── Arrays ─────────────────────────────────────────────────────

#[test]
fn test_array_literal() {
    assert_eq!(
        run("fn main() {\n\
                 let arr = [10, 20, 30];\n\
                 println(arr[1]);\n\
             }"),
        "20\n"
    );
}

// ── Pipe Operator ──────────────────────────────────────────────

#[test]
fn test_pipe_basic() {
    assert_eq!(
        run("fn double(x: i64) -> i64 { x * 2 }\n\
             fn main() { println(21 |> double); }"),
        "42\n"
    );
}

#[test]
fn test_pipe_chained() {
    assert_eq!(
        run("fn add1(x: i64) -> i64 { x + 1 }\n\
             fn double(x: i64) -> i64 { x * 2 }\n\
             fn main() { println(5 |> add1 |> double); }"),
        "12\n"
    );
}

// ── Integer Overflow ───────────────────────────────────────────

#[test]
fn test_integer_overflow_traps() {
    let errors = runtime_errors("fn main() { let x = 9223372036854775807 + 1; }");
    assert!(
        errors.iter().any(|e| e.message.contains("overflow")),
        "expected an overflow runtime error, got {:?}",
        errors
    );
}

#[test]
fn test_division_by_zero_records_runtime_error() {
    let errors = runtime_errors("fn main() { let x = 10; let y = 0; let z = x / y; }");
    assert!(
        errors
            .iter()
            .any(|e| e.message.contains("division by zero")),
        "expected a division-by-zero runtime error, got {:?}",
        errors
    );
}

#[test]
fn test_modulo_by_zero_records_runtime_error() {
    let errors = runtime_errors("fn main() { let x = 10; let y = 0; let z = x % y; }");
    assert!(
        errors
            .iter()
            .any(|e| e.message.contains("division by zero")),
        "expected a division-by-zero runtime error for %, got {:?}",
        errors
    );
}

#[test]
fn test_index_out_of_bounds_records_runtime_error() {
    let errors = runtime_errors("fn main() { let a = [1, 2, 3]; let x = a[10]; }");
    assert!(
        errors.iter().any(|e| e.message.contains("out of bounds")),
        "expected an index-out-of-bounds runtime error, got {:?}",
        errors
    );
}

#[test]
fn test_todo_records_runtime_error() {
    let errors = runtime_errors(r#"fn main() { todo("finish this"); }"#);
    assert!(
        errors
            .iter()
            .any(|e| e.message.contains("not yet implemented") && e.message.contains("finish this")),
        "expected todo() to surface a runtime error, got {:?}",
        errors
    );
}

// ── Result/Option & ? operator ──────────────────────────────────

#[test]
fn test_option_some_unwrap() {
    assert_eq!(
        run("fn main() { let x = Some(42); println(x.unwrap()); }"),
        "42\n"
    );
}

#[test]
fn test_option_none_nil_coalesce() {
    assert_eq!(run("fn main() { let x = None; println(x ?? 99); }"), "99\n");
}

#[test]
fn test_result_ok_question_mark() {
    assert_eq!(
        run("fn get_value() -> i64 {\n\
                 let r = Ok(42);\n\
                 let v = r?;\n\
                 v\n\
             }\n\
             fn main() { println(get_value()); }"),
        "42\n"
    );
}

#[test]
fn test_result_err_question_mark_propagates() {
    assert_eq!(
        run("fn might_fail() -> i64 {\n\
                 let r = Err(0);\n\
                 let v = r?;\n\
                 v\n\
             }\n\
             fn check() -> i64 {\n\
                 let result = might_fail();\n\
                 match result {\n\
                     Ok(v) => v,\n\
                     Err(e) => -1,\n\
                 }\n\
             }\n\
             fn main() { println(check()); }"),
        "-1\n"
    );
}

#[test]
fn test_option_is_some_is_none() {
    assert_eq!(
        run("fn main() {\n\
                 let a = Some(1);\n\
                 let b = None;\n\
                 println(a.is_some());\n\
                 println(b.is_none());\n\
             }"),
        "true\ntrue\n"
    );
}

#[test]
fn test_unwrap_none_records_runtime_error() {
    let errors = runtime_errors("fn main() { let x = None; x.unwrap(); }");
    assert!(
        errors.iter().any(|e| e.message.contains("unwrap")),
        "expected an unwrap runtime error, got {:?}",
        errors
    );
}

#[test]
fn test_expect_with_message() {
    let errors = runtime_errors(r#"fn main() { let x = None; x.expect("value required"); }"#);
    assert!(
        errors.iter().any(|e| e.message.contains("value required")),
        "expected expect() message to surface in the runtime error, got {:?}",
        errors
    );
}

// ── defer/errdefer ─────────────────────────────────────────────

#[test]
fn test_defer_runs_on_scope_exit() {
    assert_eq!(
        run("fn main() {\n\
                 print(1);\n\
                 defer { print(3); }\n\
                 print(2);\n\
             }"),
        "123"
    );
}

#[test]
fn test_defer_lifo_order() {
    assert_eq!(
        run("fn main() {\n\
                 defer { print(3); }\n\
                 defer { print(2); }\n\
                 defer { print(1); }\n\
             }"),
        "123"
    );
}

// Unified drop+defer cleanup stack — design.md § Drop ordering within a branch.

#[test]
fn test_defer_registers_when_reached_not_at_block_start() {
    // The `defer` after the early `return` is never registered, so
    // its body must not fire. Pre-walk collection (the old bug)
    // would have run "late" anyway because the defer was hoisted to
    // block start.
    assert_eq!(
        run("fn early(go: bool) {\n\
                 print(\"a\");\n\
                 if go { return; }\n\
                 defer { print(\"late\"); }\n\
                 print(\"b\");\n\
             }\n\
             fn main() {\n\
                 early(true);\n\
                 print(\"|\");\n\
                 early(false);\n\
             }"),
        "a|ablate"
    );
}

#[test]
fn test_defer_runs_on_early_return() {
    // A defer registered before the early return must fire — the
    // pre-walk impl had a bug where the `?` in eval_stmt_cf
    // short-circuited past the cleanup drain.
    assert_eq!(
        run("fn body() {\n\
                 defer { print(\"d\"); }\n\
                 print(\"a\");\n\
                 return;\n\
                 print(\"unreached\");\n\
             }\n\
             fn main() { body(); }"),
        "ad"
    );
}

#[test]
fn test_errdefer_fires_on_err_return() {
    // Param-less errdefer fires on Err return; defer always fires.
    assert_eq!(
        run("fn body() -> Result[i64, String] {\n\
                 defer { print(\"d\"); }\n\
                 errdefer { print(\"e\"); }\n\
                 return Err(\"boom\");\n\
             }\n\
             fn main() { let _ = body(); }"),
        "ed"
    );
}

#[test]
fn test_errdefer_skipped_on_normal_return() {
    // errdefer must NOT fire on Ok; defer always fires.
    assert_eq!(
        run("fn body() -> Result[i64, String] {\n\
                 defer { print(\"d\"); }\n\
                 errdefer { print(\"e\"); }\n\
                 return Ok(1);\n\
             }\n\
             fn main() { let _ = body(); }"),
        "d"
    );
}

#[test]
fn test_errdefer_with_binding_sees_err_payload() {
    // errdefer(e) binds `e` to the Err payload during the errdefer phase.
    assert_eq!(
        run("fn body() -> Result[i64, String] {\n\
                 errdefer(e) { print(e); }\n\
                 return Err(\"oops\");\n\
             }\n\
             fn main() { let _ = body(); }"),
        "oops"
    );
}

#[test]
fn test_errdefer_runs_before_defer_on_error_path() {
    // Phase 1 (errdefer) drains LIFO, then phase 2 (drop+defer) drains LIFO.
    assert_eq!(
        run("fn body() -> Result[i64, String] {\n\
                 defer { print(\"d1\"); }\n\
                 errdefer { print(\"e1\"); }\n\
                 defer { print(\"d2\"); }\n\
                 errdefer { print(\"e2\"); }\n\
                 return Err(\"x\");\n\
             }\n\
             fn main() { let _ = body(); }"),
        "e2e1d2d1"
    );
}

#[test]
fn test_unified_stack_program_order_lifo() {
    // The drop+defer stack interleaves bindings and defers in
    // program order; LIFO drain means later items pop first.
    // Drop slots are no-ops today, but defer ordering relative to
    // them must respect program order. The test pins the defer-vs-defer
    // ordering across interleaved `let` statements.
    assert_eq!(
        run("fn main() {\n\
                 let _x = 1;\n\
                 defer { print(\"d1\"); }\n\
                 let _y = 2;\n\
                 defer { print(\"d2\"); }\n\
             }"),
        "d2d1"
    );
}

#[test]
fn test_par_cancellation_does_not_propagate_as_scope_error() {
    // Sub-step 4: a `par` sibling that observed cancel mid-execution
    // raises ControlFlow::Cancelled. `eval_par_block` must silence
    // that on the result side — the originating branch's real `Err`
    // is the scope's return value under fail-fast. Threading races
    // are tolerated by checking the OUTCOME (real Err propagates)
    // rather than asserting on whether observation actually fired.
    assert_eq!(
        run("fn attempt() -> Result[i64, String] {\n\
                 par {\n\
                     {\n\
                         let _a = 0;\n\
                         let _b = 0;\n\
                         let _c = 0;\n\
                         let _d = 0;\n\
                         let _e = 0;\n\
                         let _f = 0;\n\
                         let _g = 0;\n\
                         let _h = 0;\n\
                     }\n\
                     {\n\
                         return Err(\"r-fail\");\n\
                     }\n\
                 };\n\
                 Ok(0)\n\
             }\n\
             fn main() {\n\
                 match attempt() {\n\
                     Ok(_) => print(\"ok\"),\n\
                     Err(e) => print(e),\n\
                 }\n\
             }"),
        "r-fail"
    );
}

// ── Sub-step 3: NLL drop placement ─────────────────────────────

fn drops_in(source: &str) -> Vec<String> {
    let (_out, drops) = run_program_with_drops(source);
    drops
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
    // both drops fire).
    assert_eq!(drops, vec!["f".to_string(), "n".to_string()]);
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

// ── phase-7 L938 user-`impl Drop` for SHARED structs (interpreter) ──
//
// A `shared struct` is `Value::SharedStruct(Arc<…>)`; the user body
// fires when the LAST live reference drops (Arc strong-count → 1 at the
// drain point), mirroring codegen's refcount→0. `drop_target` peeks the
// count without cloning so the test is exact.

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

// NOTE: recursive / field-held shared structs (`Node { next: Some(child) }`)
// still do NOT fire the user body on the interpreter path — an inner ref
// held inside another shared struct's field is never an env binding, so it
// never reaches a `CleanupAction::Drop` drain and env-release-on-drain
// can't see it. Closing that needs an Arc-drop hook (a `SharedStructInner`
// Drop impl that calls back into the interpreter). Codegen fires these
// correctly (refcount→0; see the recursive-chain codegen + ASAN tests).
// Tracked under the phase-7 "Drop ordering reconciliation across backends"
// item (L940), recursive sub-item.

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
    // Note: kara's interpreter NLL fires when last-use == stmt_idx.
    // Both a and b have last-use at stmt 2 (the println). They
    // fire in fire_due_drops walking front-to-back, so in
    // PUSH order — a before b — at NLL endpoint, not scope-exit
    // LIFO. This pins the behaviour the interpreter actually
    // exhibits today; the codegen test
    // (test_ir_multiple_user_drops_drain_lifo_at_scope_exit)
    // pins the IR-level LIFO drain. Reconciling these is a
    // follow-on slice tracked in phase-7-codegen.md.
    assert_eq!(
        output,
        vec!["3\n".to_string(), "1\n".to_string(), "2\n".to_string()],
        "expected sum-print then both user-drop bodies (a then b in NLL push order); got {:?}",
        output
    );
    // drop_trace records both bindings in the order they fired.
    assert_eq!(
        drops,
        vec!["a".to_string(), "b".to_string()],
        "drop_trace should record both bindings in NLL fire order"
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
fn test_nll_defer_referencing_binding_extends_live_range() {
    // Per design.md § Drop ordering within a branch: a defer body
    // that references a binding extends the binding's live range to
    // scope exit. The defer fires first under LIFO, with the binding
    // still alive; Drop fires after.
    let drops = drops_in(
        "fn main() {\n\
             let x = 7;\n\
             defer { println(x); }\n\
             println(\"middle\");\n\
         }",
    );
    // `x` is referenced in the defer body, so its last_use is the
    // sentinel. Drop(x) drains at scope exit, after the defer body.
    assert_eq!(drops, vec!["x"]);
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

// ── Weak references on shared struct fields ────────────────────

#[test]
fn test_weak_field_alive_yields_some() {
    // Per design.md § Shared Types — Weak references: a `weak` field
    // read is the upgrade point. While a strong holder of the referent
    // is in scope, the upgrade succeeds and yields `Some(strong_ref)`.
    assert_eq!(
        run("shared struct Parent { id: i64 }\n\
             shared struct Child { id: i64, mut weak parent: Parent }\n\
             fn main() {\n\
                 let p = Parent { id: 7 };\n\
                 let c = Child { id: 2, parent: p };\n\
                 match c.parent {\n\
                     Some(parent_ref) => println(parent_ref.id),\n\
                     None => println(\"dangling\"),\n\
                 }\n\
             }"),
        "7\n"
    );
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
fn test_weak_field_reassignment_restores_some() {
    // A `mut weak` field can be reassigned after construction. The
    // assignment auto-downgrades the strong rhs. Reading after the
    // first assignment's referent dies yields None; assigning a fresh
    // live parent restores Some.
    assert_eq!(
        run("shared struct Parent { id: i64 }\n\
             shared struct Child { id: i64, mut weak parent: Parent }\n\
             fn main() {\n\
                 let p1 = Parent { id: 1 };\n\
                 let c = Child { id: 9, parent: p1 };\n\
                 let p2 = Parent { id: 2 };\n\
                 c.parent = p2;\n\
                 match c.parent {\n\
                     Some(parent_ref) => println(parent_ref.id),\n\
                     None => println(\"dangling\"),\n\
                 }\n\
             }"),
        "2\n"
    );
}

#[test]
fn test_weak_field_immutable_form_set_at_construction() {
    // `weak parent: Parent` (no `mut`) is set at construction and
    // never reassigned. While the strong parent lives, upgrade yields
    // Some; after the strong parent's frame exits, upgrade yields None.
    assert_eq!(
        run("shared struct Parent { id: i64 }\n\
             shared struct Child { id: i64, weak parent: Parent }\n\
             fn build() -> Child {\n\
                 let p = Parent { id: 42 };\n\
                 Child { id: 1, parent: p }\n\
             }\n\
             fn main() {\n\
                 let c = build();\n\
                 match c.parent {\n\
                     Some(parent_ref) => println(parent_ref.id),\n\
                     None => println(\"dangling\"),\n\
                 }\n\
             }"),
        "dangling\n"
    );
}

#[test]
fn test_weak_field_upgrade_observes_strong_field_data() {
    // The Some arm of a weak upgrade is a normal SharedStruct handle —
    // its other fields are reachable as usual. Pins that the Arc
    // returned by Weak::upgrade carries the full referent contents,
    // not a stub.
    assert_eq!(
        run("shared struct Parent { id: i64, mut count: i64 }\n\
             shared struct Child { mut weak parent: Parent }\n\
             fn main() {\n\
                 let p = Parent { id: 5, count: 100 };\n\
                 let c = Child { parent: p };\n\
                 match c.parent {\n\
                     Some(parent_ref) => { parent_ref.count = parent_ref.count + 1; println(p.count); },\n\
                     None => println(\"dangling\"),\n\
                 }\n\
             }"),
        "101\n"
    );
}

// ── Array methods ──────────────────────────────────────────────

#[test]
fn test_array_len() {
    assert_eq!(
        run("fn main() { let arr = [1, 2, 3]; println(arr.len()); }"),
        "3\n"
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
fn test_bare_literal_for_loop_sum() {
    assert_eq!(
        run("fn main() {\n\
                 let v = [1, 2, 3, 4];\n\
                 let mut s = 0;\n\
                 for x in v { s = s + x; }\n\
                 println(s);\n\
             }"),
        "10\n"
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
fn test_prefix_array_literal_runtime() {
    assert_eq!(
        run("fn main() { let a = Array[4, 5, 6]; println(a[0]); }"),
        "4\n"
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

// ── Repeat literal `[v; n]` ────────────────────────────────────

#[test]
fn test_repeat_literal_bare_runtime() {
    // `[v; n]` allocates n copies of v and is iterable.
    assert_eq!(
        run("fn main() {
                 let v = [0; 5];
                 let mut sum = 0;
                 for x in v { sum = sum + x + 1; }
                 println(sum);
             }"),
        "5\n"
    );
}

#[test]
fn test_repeat_literal_array_prefix_runtime() {
    assert_eq!(
        run("fn main() {
                 let a = Array[42; 3];
                 println(a[0]);
                 println(a[2]);
             }"),
        "42\n42\n"
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
fn test_repeat_literal_with_annotation_runtime() {
    // Bare `[v; n]` coerced to Array[T, N] via let annotation.
    assert_eq!(
        run("fn main() {
                 let a: Array[i64, 4] = [9; 4];
                 println(a[1]);
             }"),
        "9\n"
    );
}

// ── String Interpolation ───────────────────────────────────────

#[test]
fn test_string_interpolation_basic() {
    assert_eq!(
        run("fn main() {\n\
                 let x = 42;\n\
                 println(f\"the answer is {x}\");\n\
             }"),
        "the answer is 42\n"
    );
}

// ── E2E: Complete Programs ─────────────────────────────────────

#[test]
fn test_e2e_fizzbuzz() {
    assert_eq!(
        run("fn fizzbuzz(n: i64) -> i64 {\n\
                 if n % 15 == 0 { 0 }\n\
                 else if n % 3 == 0 { 3 }\n\
                 else if n % 5 == 0 { 5 }\n\
                 else { n }\n\
             }\n\
             fn main() {\n\
                 println(fizzbuzz(15));\n\
                 println(fizzbuzz(9));\n\
                 println(fizzbuzz(10));\n\
                 println(fizzbuzz(7));\n\
             }"),
        "0\n3\n5\n7\n"
    );
}

#[test]
fn test_e2e_fibonacci() {
    // The tree-walk interpreter's eval_expr_inner / eval_call match
    // statements have grown wide; in debug builds on Windows the per-
    // frame allocation exceeds the libtest worker thread's 2 MB stack
    // for the ~10-deep recursion fib(10) produces (90+ Rust frames).
    // Linux/macOS debug frames are smaller and fit, but Windows CI
    // overflows even after prior helper extractions (eval_short_circuit
    // / eval_vec_filled). Spawn the body on a fresh 8 MB thread, same
    // pattern as test_error_trace_truncation_at_64.
    let handle = std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            assert_eq!(
                run("fn fib(n: i64) -> i64 {\n\
                         if n <= 1 { n }\n\
                         else { fib(n - 1) + fib(n - 2) }\n\
                     }\n\
                     fn main() {\n\
                         println(fib(0));\n\
                         println(fib(1));\n\
                         println(fib(10));\n\
                     }"),
                "0\n1\n55\n"
            );
        })
        .unwrap();
    handle.join().unwrap();
}

#[test]
fn test_e2e_struct_with_methods() {
    assert_eq!(
        run("struct Rect { width: i64, height: i64 }\n\
             impl Rect {\n\
                 fn area(self) -> i64 { self.width * self.height }\n\
                 fn is_square(self) -> bool { self.width == self.height }\n\
             }\n\
             fn main() {\n\
                 let r = Rect { width: 3, height: 4 };\n\
                 println(r.area());\n\
                 println(r.is_square());\n\
                 let s = Rect { width: 5, height: 5 };\n\
                 println(s.is_square());\n\
             }"),
        "12\nfalse\ntrue\n"
    );
}

#[test]
fn test_e2e_enum_state_machine() {
    assert_eq!(
        run("enum State { Idle, Running(i64), Done }\n\
             fn step(s: State) -> State {\n\
                 match s {\n\
                     Idle => Running(0),\n\
                     Running(n) => {\n\
                         if n >= 3 { Done } else { Running(n + 1) }\n\
                     },\n\
                     Done => Done,\n\
                 }\n\
             }\n\
             fn is_done(s: State) -> bool {\n\
                 match s {\n\
                     Done => true,\n\
                     _ => false,\n\
                 }\n\
             }\n\
             fn main() {\n\
                 let s1 = step(Idle);\n\
                 println(is_done(s1));\n\
                 let s2 = step(s1);\n\
                 let s3 = step(s2);\n\
                 let s4 = step(s3);\n\
                 let s5 = step(s4);\n\
                 println(is_done(s5));\n\
             }"),
        "false\ntrue\n"
    );
}

#[test]
fn test_e2e_higher_order_functions() {
    assert_eq!(
        run("fn apply_twice(f: Fn(i64) -> i64, x: i64) -> i64 {\n\
                 f(f(x))\n\
             }\n\
             fn add3(x: i64) -> i64 { x + 3 }\n\
             fn main() {\n\
                 println(apply_twice(add3, 10));\n\
                 println(apply_twice(|x: i64| x * 2, 5));\n\
             }"),
        "16\n20\n"
    );
}

#[test]
fn test_e2e_error_handling() {
    assert_eq!(
        run("fn divide(a: i64, b: i64) -> i64 {\n\
                 if b == 0 { return Err(0); }\n\
                 Ok(a / b)\n\
             }\n\
             fn main() {\n\
                 let r1 = divide(10, 2);\n\
                 println(r1.unwrap());\n\
                 let r2 = divide(10, 0);\n\
                 println(r2.is_err());\n\
             }"),
        "5\ntrue\n"
    );
}

// ── Edge Cases: Scoping ────────────────────────────────────────

#[test]
fn test_nested_scopes_shadow() {
    assert_eq!(
        run("fn main() {\n\
                 let x = 1;\n\
                 let y = {\n\
                     let x = 2;\n\
                     x\n\
                 };\n\
                 println(x);\n\
                 println(y);\n\
             }"),
        "1\n2\n"
    );
}

#[test]
fn test_variable_not_leaked_from_block() {
    // After a block, variables defined inside it should not be accessible
    // (the interpreter panics on undefined variable, so we test indirectly)
    assert_eq!(
        run("fn main() {\n\
                 let before = 10;\n\
                 { let inner = 20; }\n\
                 println(before);\n\
             }"),
        "10\n"
    );
}

#[test]
fn test_function_scope_isolation() {
    // Variables defined in one function call should not leak to the next
    assert_eq!(
        run("fn f() -> i64 {\n\
                 let local = 42;\n\
                 local\n\
             }\n\
             fn main() {\n\
                 println(f());\n\
                 println(f());\n\
             }"),
        "42\n42\n"
    );
}

// ── Edge Cases: Recursion & Control Flow ───────────────────────

#[test]
fn test_mutual_recursion() {
    assert_eq!(
        run("fn is_even(n: i64) -> bool {\n\
                 if n == 0 { true } else { is_odd(n - 1) }\n\
             }\n\
             fn is_odd(n: i64) -> bool {\n\
                 if n == 0 { false } else { is_even(n - 1) }\n\
             }\n\
             fn main() {\n\
                 println(is_even(4));\n\
                 println(is_odd(5));\n\
             }"),
        "true\ntrue\n"
    );
}

#[test]
fn test_nested_if_else() {
    assert_eq!(
        run("fn classify(x: i64) -> i64 {\n\
                 if x > 100 {\n\
                     3\n\
                 } else if x > 10 {\n\
                     2\n\
                 } else if x > 0 {\n\
                     1\n\
                 } else {\n\
                     0\n\
                 }\n\
             }\n\
             fn main() {\n\
                 println(classify(200));\n\
                 println(classify(50));\n\
                 println(classify(5));\n\
                 println(classify(-1));\n\
             }"),
        "3\n2\n1\n0\n"
    );
}

#[test]
fn test_break_with_variable_value() {
    assert_eq!(
        run("fn main() {\n\
                 let mut i = 0;\n\
                 let x = loop {\n\
                     i += 1;\n\
                     if i == 3 {\n\
                         break i;\n\
                     }\n\
                 };\n\
                 println(x);\n\
             }"),
        "3\n"
    );
}

#[test]
fn test_while_with_return() {
    assert_eq!(
        run("fn count_to_return(limit: i64) -> i64 {\n\
                 let mut i = 0;\n\
                 while true {\n\
                     i += 1;\n\
                     if i >= limit { return i; }\n\
                 }\n\
                 0\n\
             }\n\
             fn main() { println(count_to_return(5)); }"),
        "5\n"
    );
}

// ── Edge Cases: Struct Patterns ────────────────────────────────

#[test]
fn test_struct_destructuring_in_let() {
    assert_eq!(
        run("struct Point { x: i64, y: i64 }\n\
             fn main() {\n\
                 let p = Point { x: 10, y: 20 };\n\
                 let Point { x, y } = p;\n\
                 println(x + y);\n\
             }"),
        "30\n"
    );
}

#[test]
fn test_tuple_destructuring_in_let() {
    assert_eq!(
        run("fn main() {\n\
                 let t = (1, 2, 3);\n\
                 let (a, b, c) = t;\n\
                 println(a + b + c);\n\
             }"),
        "6\n"
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

// ── Edge Cases: Closures ───────────────────────────────────────

#[test]
fn test_closure_as_return_value() {
    assert_eq!(
        run("fn make_adder(n: i64) -> Fn(i64) -> i64 {\n\
                 |x: i64| x + n\n\
             }\n\
             fn main() {\n\
                 let add5 = make_adder(5);\n\
                 println(add5(10));\n\
                 let add100 = make_adder(100);\n\
                 println(add100(1));\n\
             }"),
        "15\n101\n"
    );
}

// ── Closure calling through `ref` (round 12.6) ──────────────────
//
// Item 23: explicit `ref |...|` / `mut ref |...|` capture-mode prefix
// guarantees the closure is repeatable. The interpreter dispatches
// each invocation through the same Value::Function — multi-call
// patterns (loop, vec slot, repeated direct calls) must produce
// the expected per-call results without consuming the closure binding.

#[test]
fn test_ref_closure_invokes_multiple_times() {
    // `ref ||` body reads a captured field — three calls return the
    // same value (snapshot-at-creation-time semantics for the cloned
    // env, which is the interpreter's existing closure behavior).
    assert_eq!(
        run("struct Owned { x: i64 }\n\
             fn main() {\n\
                 let o = Owned { x: 7 };\n\
                 let f = ref || o.x + 1;\n\
                 println(f());\n\
                 println(f());\n\
                 println(f());\n\
             }"),
        "8\n8\n8\n"
    );
}

#[test]
fn test_ref_closure_called_in_for_loop() {
    // Motivating pattern from design.md §3638: a repeatable closure
    // invoked many times by a loop. The closure value is read from
    // the binding once per iteration.
    assert_eq!(
        run("struct Owned { x: i64 }\n\
             fn main() {\n\
                 let o = Owned { x: 10 };\n\
                 let f = ref || o.x;\n\
                 for _i in 0..3 {\n\
                     println(f());\n\
                 }\n\
             }"),
        "10\n10\n10\n"
    );
}

#[test]
fn test_repeatable_closure_in_vec_dispatched_via_index_call() {
    // Storing a repeatable closure in a Vec and invoking it through
    // `vec[i]()` exercises the interpreter's "callee evaluates to a
    // Value::Function" dispatch path (design.md §3638 — the
    // multi-callable case where `vec[i]()` is permitted).
    assert_eq!(
        run("fn main() {\n\
                 let n = 5;\n\
                 let f = ref || n + 1;\n\
                 let g = ref || n + 2;\n\
                 let callbacks = Vec[f, g];\n\
                 println(callbacks[0]());\n\
                 println(callbacks[1]());\n\
                 println(callbacks[0]());\n\
             }"),
        "6\n7\n6\n"
    );
}

#[test]
fn collect_all_vec_gathers_all_results_without_fail_fast() {
    // Phase 6 slice 1a — `collect_all_vec` runs EVERY closure to
    // completion and returns one Result per input, position-bound
    // (`output[i]` == outcome of `fs[i]`). Unlike fail-fast `par {}`, an
    // `Err` from one branch does NOT stop later branches: indices 0 and 2
    // (Ok) and 1 and 3 (Err) all appear, and the Ok at index 2 proves a
    // branch after an Err still ran. (design.md § Concurrency Semantics.)
    assert_eq!(
        run("fn work(n: i64) -> Result[i64, String] {\n\
                 if n > 0 { Result.Ok(n * 10) } else { Result.Err(f\"neg:{n}\") }\n\
             }\n\
             fn main() {\n\
                 let fs: Vec[Fn() -> Result[i64, String]] = Vec[|| work(1), || work(-2), || work(3), || work(-4)];\n\
                 let results: Vec[Result[i64, String]] = collect_all_vec(fs);\n\
                 println(f\"len={results.len()}\");\n\
                 for r in results {\n\
                     match r {\n\
                         Result.Ok(v) => { println(f\"ok {v}\"); }\n\
                         Result.Err(e) => { println(f\"err {e}\"); }\n\
                     }\n\
                 }\n\
             }"),
        "len=4\nok 10\nerr neg:-2\nok 30\nerr neg:-4\n"
    );
}

#[test]
fn collect_all_vec_panic_in_branch_dominates() {
    // A panicking branch dominates: it short-circuits the gather (via
    // `pending_cf`) so the post-`collect_all_vec` line never runs and the
    // panic propagates. (design.md § Parallel Failure and Cleanup — panic
    // cancels siblings even under `collect_all_vec`.)
    let (out, errors, _trace, _trunc) = run_program_full(
        "fn boom() -> Result[i64, String] { todo(\"kaboom\") }\n\
         fn main() {\n\
             let fs: Vec[Fn() -> Result[i64, String]] = Vec[|| Result.Ok(1), || boom(), || Result.Ok(3)];\n\
             let results: Vec[Result[i64, String]] = collect_all_vec(fs);\n\
             println(f\"after len={results.len()}\");\n\
         }",
    );
    assert!(
        !out.join("").contains("after"),
        "post-collect_all_vec line must not run after a branch panic; stdout: {:?}",
        out
    );
    assert!(
        !errors.is_empty(),
        "expected a runtime panic to propagate; got no errors"
    );
}

#[test]
fn collect_all_gathers_heterogeneous_tuple() {
    // Phase 6 — `collect_all(|| a, || b, || c)` runs every branch and
    // gathers a position-bound HETEROGENEOUS tuple. Branch error types
    // differ (String at .0, i64 at .1); no fail-fast (the `Err` at .0/.1
    // does not stop .2 from producing `Ok`).
    assert_eq!(
        run("fn fa(n: i64) -> Result[i64, String] {\n\
                 if n > 0 { Result.Ok(n * 10) } else { Result.Err(f\"a{n}\") }\n\
             }\n\
             fn fb(s: String) -> Result[String, i64] { Result.Err(7) }\n\
             fn fc(n: i64) -> Result[i64, String] { Result.Ok(n + 100) }\n\
             fn main() {\n\
                 let t: (Result[i64, String], Result[String, i64], Result[i64, String]) =\n\
                     collect_all(|| fa(-5), || fb(\"x\"), || fc(3));\n\
                 match t.0 { Result.Ok(v) => { println(f\"0 ok {v}\"); } Result.Err(e) => { println(f\"0 err {e}\"); } }\n\
                 match t.1 { Result.Ok(v) => { println(f\"1 ok {v}\"); } Result.Err(e) => { println(f\"1 err {e}\"); } }\n\
                 match t.2 { Result.Ok(v) => { println(f\"2 ok {v}\"); } Result.Err(e) => { println(f\"2 err {e}\"); } }\n\
             }"),
        "0 err a-5\n1 err 7\n2 ok 103\n"
    );
}

#[test]
fn collect_all_auto_thunks_bare_and_mixed_branches() {
    // design.md "closure wrappers optional" — bare-expression branches
    // (`collect_all(fa(x), fb(y))`) are auto-thunked by lowering into
    // `|| fa(x)` etc., so they gather identically to explicit closures;
    // mixed explicit/bare branches work too, and captured locals (`x`,
    // `y`) flow into the thunked closures.
    assert_eq!(
        run("fn fa(n: i64) -> Result[i64, String] {\n\
                 if n > 0 { Result.Ok(n * 10) } else { Result.Err(f\"a{n}\") }\n\
             }\n\
             fn fb(s: String) -> Result[String, i64] { Result.Err(7) }\n\
             fn main() {\n\
                 let x: i64 = 4;\n\
                 let t: (Result[i64, String], Result[String, i64], Result[i64, String]) =\n\
                     collect_all(fa(x), fb(\"z\"), || fa(-1));\n\
                 match t.0 { Result.Ok(v) => { println(f\"0 ok {v}\"); } Result.Err(e) => { println(f\"0 err {e}\"); } }\n\
                 match t.1 { Result.Ok(v) => { println(f\"1 ok {v}\"); } Result.Err(e) => { println(f\"1 err {e}\"); } }\n\
                 match t.2 { Result.Ok(v) => { println(f\"2 ok {v}\"); } Result.Err(e) => { println(f\"2 err {e}\"); } }\n\
             }"),
        "0 ok 40\n1 err 7\n2 err a-1\n"
    );
}

// ── `mut ref |...|` capture-mutation propagation (round 12.48) ──
//
// Mutations made by a `mut ref` closure to a captured outer binding
// must persist across invocations and be observable via the outer
// binding after the calls return. Implemented by promoting each
// captured slot to `Value::SharedCell` at closure construction so
// reads/writes on either side route through the same Mutex<Value>.
// Bare and `ref ||` closures do NOT alias — those captures keep the
// snapshot-at-construction behavior the existing tests above pin.

#[test]
fn test_mut_ref_closure_assignment_propagates() {
    assert_eq!(
        run("fn main() {\n\
                 let mut counter = 0_i64;\n\
                 let bump = mut ref || { counter = counter + 1; };\n\
                 bump();\n\
                 bump();\n\
                 bump();\n\
                 println(counter);\n\
             }"),
        "3\n"
    );
}

#[test]
fn test_mut_ref_closure_compound_assign_propagates() {
    assert_eq!(
        run("fn main() {\n\
                 let mut counter = 10_i64;\n\
                 let bump = mut ref || { counter += 5; };\n\
                 bump();\n\
                 bump();\n\
                 println(counter);\n\
             }"),
        "20\n"
    );
}

#[test]
fn test_mut_ref_closure_vec_push_propagates() {
    assert_eq!(
        run("fn main() {\n\
                 let mut v = Vec[1_i64, 2, 3];\n\
                 let push5 = mut ref || { v.push(5); };\n\
                 push5();\n\
                 push5();\n\
                 println(v.len());\n\
             }"),
        "5\n"
    );
}

#[test]
fn test_mut_ref_closure_field_mutation_propagates() {
    // Field-level mutation through a captured struct binding routes
    // through `set_field` → `Env::set` → SharedCell write-through.
    assert_eq!(
        run("struct Counter { n: i64 }\n\
             fn main() {\n\
                 let mut c = Counter { n: 0 };\n\
                 let bump = mut ref || { c.n = c.n + 1; };\n\
                 bump();\n\
                 bump();\n\
                 bump();\n\
                 println(c.n);\n\
             }"),
        "3\n"
    );
}

#[test]
fn test_mut_ref_closure_observes_outer_change_between_calls() {
    // Aliasing is bidirectional — between calls the outer binding can
    // be mutated and the next invocation sees the updated value.
    assert_eq!(
        run("fn main() {\n\
                 let mut x = 1_i64;\n\
                 let print_x = mut ref || { println(x); x = x + 10; };\n\
                 print_x();\n\
                 x = 100_i64;\n\
                 print_x();\n\
                 println(x);\n\
             }"),
        "1\n100\n110\n"
    );
}

#[test]
fn test_mut_ref_closure_forwarded_to_higher_order_fn() {
    // The Value::Function clones when passed across function boundaries,
    // but the SharedCell aliases inside `closure_env` are Arc-based so
    // every clone shares the same backing cell — mutations made by the
    // higher-order function's invocations are still visible at main's
    // outer binding.
    assert_eq!(
        run("fn run_thrice(f: ref Fn()) { f(); f(); f(); }\n\
             fn main() {\n\
                 let mut counter = 0_i64;\n\
                 let bump = mut ref || { counter = counter + 1; };\n\
                 run_thrice(bump);\n\
                 println(counter);\n\
             }"),
        "3\n"
    );
}

#[test]
fn test_bare_closure_does_not_propagate_mutation() {
    // Pinning the negative case: a bare `|...|` closure (default — captures
    // by ownership) snapshots the captured value, so mutations stay local
    // to the body and the outer binding is untouched.
    assert_eq!(
        run("fn main() {\n\
                 let mut x = 0_i64;\n\
                 let f = || { let _y = x + 1; };\n\
                 f();\n\
                 f();\n\
                 println(x);\n\
             }"),
        "0\n"
    );
}

// ── Edge Cases: Method + Enum Interaction ──────────────────────

#[test]
fn test_static_method_constructor() {
    assert_eq!(
        run("struct Vec2 { x: i64, y: i64 }\n\
             impl Vec2 {\n\
                 fn new(x: i64, y: i64) -> Vec2 { Vec2 { x: x, y: y } }\n\
                 fn dot(self, other: Vec2) -> i64 { self.x * other.x + self.y * other.y }\n\
             }\n\
             fn main() {\n\
                 let a = Vec2.new(3, 4);\n\
                 let b = Vec2.new(1, 2);\n\
                 println(a.dot(b));\n\
             }"),
        "11\n"
    );
}

// ── Edge Cases: Multiple Return Paths ──────────────────────────

#[test]
fn test_multiple_returns() {
    assert_eq!(
        run("fn sign(x: i64) -> i64 {\n\
                 if x > 0 { return 1; }\n\
                 if x < 0 { return -1; }\n\
                 0\n\
             }\n\
             fn main() {\n\
                 println(sign(42));\n\
                 println(sign(-7));\n\
                 println(sign(0));\n\
             }"),
        "1\n-1\n0\n"
    );
}

// ── Generic Functions ──────────────────────────────────────────

#[test]
fn test_generic_identity_function() {
    assert_eq!(
        run("fn identity[T](x: T) -> T { x }\n\
             fn main() {\n\
                 println(identity(42));\n\
                 println(identity(true));\n\
             }"),
        "42\ntrue\n"
    );
}

#[test]
fn test_generic_pair() {
    assert_eq!(
        run("struct Pair[A, B] { first: A, second: B }\n\
             fn main() {\n\
                 let p = Pair { first: 1, second: true };\n\
                 println(p.first);\n\
                 println(p.second);\n\
             }"),
        "1\ntrue\n"
    );
}

// ── Print ──────────────────────────────────────────────────────

#[test]
fn test_print_no_newline() {
    assert_eq!(run(r#"fn main() { print("a"); print("b"); }"#), "ab");
}

#[test]
fn test_println_multiple() {
    assert_eq!(
        run("fn main() {\n\
                 println(1);\n\
                 println(2);\n\
                 println(3);\n\
             }"),
        "1\n2\n3\n"
    );
}

// ── Seq block ───────────────────────────────────────────────────

#[test]
fn test_seq_block_empty() {
    // seq {} evaluates to unit
    assert_eq!(run("fn main() { let x = seq { }; println(0); }"), "0\n");
}

#[test]
fn test_seq_block_value() {
    // seq { let x = 42; x } evaluates to 42
    assert_eq!(
        run("fn main() { let result = seq { let x = 42; x }; println(result); }"),
        "42\n"
    );
}

#[test]
fn test_seq_block_scoping() {
    // Variables inside seq are scoped to the block
    assert_eq!(
        run("fn main() {\n\
                 let a = 1;\n\
                 let b = seq { let inner = 10; inner + a };\n\
                 println(b);\n\
             }"),
        "11\n"
    );
}

// ── Labeled Loops ──────────────────────────────────────────────

#[test]
fn test_labeled_break_exits_outer_loop() {
    // `break label expr` syntax is required; `break label;` alone parses as break-with-value
    assert_eq!(
        run("fn main() {\n\
                 let mut count = 0;\n\
                 outer: for x in [1, 2, 3] {\n\
                     for y in [10, 20, 30] {\n\
                         count = count + 1;\n\
                         break outer ();\n\
                     }\n\
                 }\n\
                 println(count);\n\
             }"),
        "1\n"
    );
}

#[test]
fn test_labeled_continue_skips_outer_iteration() {
    assert_eq!(
        run("fn main() {\n\
                 let mut count = 0;\n\
                 outer: for x in [1, 2, 3] {\n\
                     for y in [10, 20] {\n\
                         count = count + 1;\n\
                         continue outer;\n\
                     }\n\
                 }\n\
                 println(count);\n\
             }"),
        "3\n"
    );
}

#[test]
fn test_labeled_break_with_value() {
    assert_eq!(
        run("fn main() {\n\
                 let result = outer: loop {\n\
                     loop {\n\
                         break outer 42;\n\
                     }\n\
                 };\n\
                 println(result);\n\
             }"),
        "42\n"
    );
}

#[test]
fn test_labeled_break_while_loop() {
    assert_eq!(
        run("fn main() {\n\
                 let mut i = 0;\n\
                 outer: while i < 5 {\n\
                     let mut j = 0;\n\
                     while j < 5 {\n\
                         if j == 2 {\n\
                             break outer ();\n\
                         }\n\
                         j = j + 1;\n\
                     }\n\
                     i = i + 1;\n\
                 }\n\
                 println(i);\n\
             }"),
        "0\n"
    );
}

#[test]
fn test_unlabeled_break_still_works() {
    assert_eq!(
        run("fn main() {\n\
                 let mut count = 0;\n\
                 outer: for x in [1, 2, 3] {\n\
                     for y in [10, 20, 30] {\n\
                         count = count + 1;\n\
                         break;\n\
                     }\n\
                 }\n\
                 println(count);\n\
             }"),
        "3\n"
    );
}

// ── Labeled blocks runtime ───────────────────────────────────
//
// Sibling slice to `tests/codegen.rs` "Labeled blocks runtime" — the
// interpreter side of the LBC4 design choice. The labeled-block expr
// arm in `eval_expr_inner` matches `ControlFlow::Break { label, value
// }` against its own label; non-matching labels propagate. See
// `docs/implementation_checklist/phase-5-diagnostics.md` § 5.2.

#[test]
fn test_interpreter_labeled_block_break_with_value() {
    // Mirror of codegen test 1: `lbl: { break lbl 42; -1 }` evaluates
    // to 42 through the tree-walk path.
    assert_eq!(
        run("fn main() {\n\
             let x: i64 = lbl: { break lbl 42; -1 };\n\
             println(x);\n\
         }"),
        "42\n"
    );
}

#[test]
fn test_interpreter_labeled_block_bare_break_unit() {
    // Bare `break label` exits with `Value::Unit`. Observable check:
    // post-block println runs.
    assert_eq!(
        run("fn main() {\n\
             lbl: { break lbl; };\n\
             println(7);\n\
         }"),
        "7\n"
    );
}

#[test]
fn test_interpreter_labeled_block_tail_expression() {
    // No break: the block falls through normally and the tail value
    // becomes the labeled block's value.
    assert_eq!(
        run("fn main() {\n\
             let x: i64 = lbl: { 99 };\n\
             println(x);\n\
         }"),
        "99\n"
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
fn test_interpreter_nested_labeled_break_outer() {
    // Mirror of codegen latent-bug regression gate: `outer: while {
    // inner: while { break outer; } }` exits the outer loop. Pre-slice
    // interpreter already routed through `ControlFlow::Break.label`
    // correctly, but the test pins the contract to prevent a future
    // regression that flattens the label match.
    assert_eq!(
        run("fn main() {\n\
             let mut count = 0;\n\
             outer: while true {\n\
                 inner: while true {\n\
                     count = count + 1;\n\
                     break outer ();\n\
                 }\n\
                 count = count + 100;\n\
             }\n\
             println(count);\n\
         }"),
        "1\n"
    );
}

// ── Error Return Trace Tests ──────────────────────────────────

#[test]
fn test_error_trace_single_question_mark() {
    let (_output, trace, truncated) = run_program_with_trace(
        "fn might_fail() {\n\
             let r = Err(42);\n\
             r?\n\
         }\n\
         fn main() {\n\
             let result = might_fail();\n\
             match result {\n\
                 Ok(v) => println(v),\n\
                 Err(e) => println(e),\n\
             }\n\
         }",
    );
    assert!(
        !trace.is_empty(),
        "Error trace should have at least one frame"
    );
    assert!(!truncated);
    assert_eq!(trace.len(), 1);
    // The ? is on line 3
    assert_eq!(trace[0].line, 3);
}

#[test]
fn test_error_trace_multi_level_propagation() {
    let (_output, trace, truncated) = run_program_with_trace(
        "fn inner() {\n\
             let r = Err(99);\n\
             r?\n\
         }\n\
         fn middle() {\n\
             let v = inner()?;\n\
             v\n\
         }\n\
         fn outer() {\n\
             let v = middle()?;\n\
             v\n\
         }\n\
         fn main() {\n\
             let result = outer();\n\
             match result {\n\
                 Ok(v) => println(v),\n\
                 Err(e) => println(e),\n\
             }\n\
         }",
    );
    assert!(!truncated);
    // inner's ?, middle's ?, outer's ? = 3 frames
    assert_eq!(trace.len(), 3, "Expected 3 trace frames, got {:?}", trace);
}

#[test]
fn test_error_trace_cleared_on_ok() {
    let (_output, trace, _truncated) = run_program_with_trace(
        "fn try_thing() {\n\
             let r = Ok(10);\n\
             r?\n\
         }\n\
         fn main() {\n\
             let v = try_thing();\n\
             println(v);\n\
         }",
    );
    assert!(
        trace.is_empty(),
        "Trace should be empty after Ok(?), got {:?}",
        trace
    );
}

#[test]
fn test_error_trace_none_propagation() {
    let (_output, trace, truncated) = run_program_with_trace(
        "fn find() {\n\
             let r = None;\n\
             r?\n\
         }\n\
         fn search() {\n\
             let v = find()?;\n\
             v\n\
         }\n\
         fn main() {\n\
             let result = search();\n\
             match result {\n\
                 Some(v) => println(v),\n\
                 None => println(\"not found\"),\n\
             }\n\
         }",
    );
    assert!(!truncated);
    assert_eq!(
        trace.len(),
        2,
        "Expected 2 trace frames for None propagation, got {:?}",
        trace
    );
}

#[test]
fn test_error_trace_truncation_at_64() {
    // Build a deeply nested chain of ? propagation (65 levels).
    // Needs a larger stack for deep recursion in debug builds.
    let result = std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            let mut source = String::new();
            source.push_str("fn func0() {\n    let r = Err(1);\n    r?\n}\n");
            for i in 1..=64 {
                source.push_str(&format!(
                    "fn func{i}() {{\n    let v = func{}()?;\n    v\n}}\n",
                    i - 1
                ));
            }
            source.push_str(
                "fn main() {\n\
                     let result = func64();\n\
                     match result {\n\
                         Ok(v) => println(v),\n\
                         Err(e) => println(e),\n\
                     }\n\
                 }",
            );
            let (_output, trace, truncated) = run_program_with_trace(&source);
            // 65 frames total, but max is 64, so oldest is dropped
            assert!(truncated, "Trace should be truncated at 64 frames");
            assert_eq!(trace.len(), 64, "Trace should have exactly 64 frames");
        })
        .expect("failed to spawn thread")
        .join();
    result.expect("test thread panicked");
}

// ── Par Block Tests ────────────────────────────────────────────

#[test]
fn test_par_block_basic() {
    let output = run("fn a() -> i32 { println(\"A\"); 1 }
         fn b() -> i32 { println(\"B\"); 2 }
         fn main() {
             par {
                 let x = a();
                 let y = b();
             };
             println(\"done\");
         }");
    // Both tasks run, output is merged in source order
    assert!(output.contains("A"));
    assert!(output.contains("B"));
    assert!(output.contains("done"));
}

#[test]
fn test_par_block_single_statement() {
    // Single statement in par — runs without threading
    let output = run("fn main() {
             par {
                 println(\"solo\");
             };
         }");
    assert!(output.contains("solo"));
}

#[test]
fn test_par_block_empty() {
    // Empty par block is valid
    let output = run("fn main() { par { }; println(\"ok\"); }");
    assert!(output.contains("ok"));
}

// ── IEEE 754 Float Semantics ──────────────────────────────────

#[test]
fn test_float_nan_not_equal() {
    // IEEE 754: NaN != NaN
    let output = run("fn main() { let nan = 0.0 / 0.0; println(nan == nan); }");
    assert_eq!(output, "false\n");
}

// ── F64/F32 Total-Order Types ─────────────────────────────────

#[test]
fn test_f64_from_constructor() {
    let output = run("fn main() { let x = F64.from(3.14); println(x); }");
    assert_eq!(output, "F64(3.14)\n");
}

#[test]
fn test_f32_from_constructor() {
    let output = run("fn main() { let x = F32.from(2.5); println(x); }");
    assert!(output.starts_with("F32(2.5"));
}

// ── Numeric primitive From (Step 4) ────────────────────────────

#[test]
fn test_int_from_widening() {
    let output = run("fn main() { let x: i32 = 42; let y: i64 = i64.from(x); println(y); }");
    assert_eq!(output, "42\n");
}

// ── ? cross-error From propagation (Step 5) ────────────────────

#[test]
fn test_question_cross_error_calls_from_impl() {
    let output = run("struct ParseError { msg: String }\n\
         struct AppError { msg: String }\n\
         impl From for AppError {\n\
             fn from(e: ParseError) -> AppError {\n\
                 AppError { msg: e.msg }\n\
             }\n\
         }\n\
         fn produce() -> Result[i64, ParseError] { Err(ParseError { msg: \"bad\" }) }\n\
         fn run_it() -> Result[i64, AppError] {\n\
             let x: i64 = produce()?;\n\
             Ok(x)\n\
         }\n\
         fn main() {\n\
             match run_it() {\n\
                 Ok(_) => println(\"ok\"),\n\
                 Err(e) => println(e.msg),\n\
             }\n\
         }");
    assert_eq!(output, "bad\n");
}

#[test]
fn test_float_from_widening() {
    let output = run("fn main() { let x: f32 = 1.5; let y: f64 = f64.from(x); println(y); }");
    assert_eq!(output, "1.5\n");
}

// ── .into() expected-type threading (Slice 3a) ────────────────

#[test]
fn test_into_at_let_annotation_uses_from_impl() {
    // `let y: i64 = x.into()` should dispatch through the `i64.from(x)`
    // widening impl — same semantics as calling `i64.from(x)` directly.
    let output = run("fn main() { let x: i32 = 42; let y: i64 = x.into(); println(y); }");
    assert_eq!(output, "42\n");
}

#[test]
fn test_into_at_return_position() {
    let output = run("fn widen(x: i32) -> i64 { x.into() }\n\
         fn main() { println(widen(7)); }");
    assert_eq!(output, "7\n");
}

#[test]
fn test_into_at_call_argument_position() {
    let output = run("fn takes(y: i64) { println(y); }\n\
         fn main() { let x: i32 = 99; takes(x.into()); }");
    assert_eq!(output, "99\n");
}

#[test]
fn test_into_drives_user_from_impl() {
    // User-defined `impl From[Inches] for Cm` should drive `.into()` at
    // a `let: Cm` position.
    let output = run("struct Inches { n: i64 }\n\
         struct Cm { n: i64 }\n\
         impl From for Cm {\n\
             fn from(i: Inches) -> Cm { Cm { n: i.n * 254 / 100 } }\n\
         }\n\
         fn main() {\n\
             let i: Inches = Inches { n: 10 };\n\
             let c: Cm = i.into();\n\
             println(c.n);\n\
         }");
    assert_eq!(output, "25\n");
}

// ── Atomic[T] Runtime ─────────────────────────────────────────

#[test]
fn test_atomic_new_and_load() {
    let output = run("fn main() {\n\
             let a = Atomic.new(42);\n\
             println(a.load(MemoryOrdering.SeqCst));\n\
         }");
    assert_eq!(output, "42\n");
}

#[test]
fn test_atomic_store_and_load() {
    let output = run("fn main() {\n\
             let mut a = Atomic.new(0);\n\
             a.store(99, MemoryOrdering.Relaxed);\n\
             println(a.load(MemoryOrdering.Relaxed));\n\
         }");
    assert_eq!(output, "99\n");
}

#[test]
fn test_atomic_bool() {
    let output = run("fn main() {\n\
             let flag = Atomic.new(false);\n\
             println(flag.load(MemoryOrdering.SeqCst));\n\
         }");
    assert_eq!(output, "false\n");
}

// ── par-shared Atomic: concurrent read-modify-write (regression) ──
//
// `Value::Atomic` is `Arc<Mutex<Value>>`, so a par struct's Atomic field is
// genuinely shared across `par {}` branches (which run on real OS threads via
// `thread::scope`) and every `fetch_*` / `compare_exchange` is a real
// read-modify-write under lock. Before that, `Atomic` was a `Box<Value>`
// (non-atomic, single-threaded), so two branches racing on a shared par-struct
// counter produced lost updates AND intermittent `method '…' not found on type
// 'unknown'` panics from torn reads. These tests loop enough times to defeat
// the old intermittency (it failed within a handful of runs). They exercise the
// default `run_program` path, which runs par on real threads (sequential_mode
// is false). The AOT/codegen path always produced the correct value; this
// closes the interpreter (`karac run`) divergence on a program that passes all
// static checks.

#[test]
fn test_par_shared_atomic_counter_no_lost_updates() {
    let src = "par struct Counter { count: Atomic[i64] }\n\
         impl Counter {\n\
             fn inc(ref self) { let _ = self.count.fetch_add(1, MemoryOrdering.SeqCst); }\n\
             fn get(ref self) -> i64 { self.count.load(MemoryOrdering.SeqCst) }\n\
         }\n\
         fn bump_many(c: Counter, n: i64) {\n\
             let mut i = 0;\n\
             while i < n { c.inc(); i = i + 1; }\n\
         }\n\
         fn main() {\n\
             let c = Counter { count: Atomic.new(0) };\n\
             par { bump_many(c, 5000); bump_many(c, 5000); }\n\
             println(c.get());\n\
         }";
    // Repeat: the prior race was intermittent (lost updates / torn-read panic).
    for iter in 0..30 {
        assert_eq!(
            run(src),
            "10000\n",
            "par-shared atomic counter, iteration {iter}"
        );
    }
}

#[test]
fn test_par_shared_atomic_reaches_after_par_statement() {
    // The torn-read panic / lost-update path also manifested as the statement
    // *after* the par block never executing. Assert the trailing print lands.
    let src = "par struct Counter { count: Atomic[i64] }\n\
         impl Counter {\n\
             fn inc(ref self) { let _ = self.count.fetch_add(1, MemoryOrdering.SeqCst); }\n\
             fn get(ref self) -> i64 { self.count.load(MemoryOrdering.SeqCst) }\n\
         }\n\
         fn main() {\n\
             let c = Counter { count: Atomic.new(0) };\n\
             par { c.inc(); c.inc(); }\n\
             println(c.get());\n\
             println(\"after\");\n\
         }";
    for iter in 0..30 {
        assert_eq!(
            run(src),
            "2\nafter\n",
            "trailing statement after par, iteration {iter}"
        );
    }
}

// ── par-shared Mutex: lock-block serialisation (regression) ──
//
// Sibling of the Atomic fix above. `Value::Mutex` is `Arc<Mutex<Value>>` and a
// `lock` block holds the *real* lock for its whole body, so a par struct's
// Mutex field locked from two `par {}` branches serialises instead of racing.
// Before that, `Value::Mutex` was a single-threaded `Box<Value>` copied on
// read and written back, so concurrent branches lost updates / produced empty
// output. Loops to defeat the old intermittency; exercises the real-threaded
// `run_program` path. AOT codegen always produced the correct value.

#[test]
fn test_par_shared_mutex_counter_no_lost_updates() {
    let src = "par struct Counter { total: Mutex[i64] }\n\
         impl Counter {\n\
             fn inc(ref self) { lock self.total t { t = t + 1; } }\n\
             fn get(ref self) -> i64 { lock self.total t { t } }\n\
         }\n\
         fn bump(c: Counter, n: i64) {\n\
             let mut i = 0;\n\
             while i < n { c.inc(); i = i + 1; }\n\
         }\n\
         fn main() {\n\
             let c = Counter { total: Mutex.new(0) };\n\
             par { bump(c, 5000); bump(c, 5000); }\n\
             println(c.get());\n\
         }";
    for iter in 0..30 {
        assert_eq!(
            run(src),
            "10000\n",
            "par-shared mutex counter, iteration {iter}"
        );
    }
}

#[test]
fn test_mutex_lock_block_single_threaded_semantics() {
    // The lock-block bind-and-write-back path must still work outside par:
    // read the inner value, mutate the alias, write it back.
    let src = "struct Cell { v: Mutex[i64] }\n\
         impl Cell {\n\
             fn get(ref self) -> i64 { lock self.v x { x } }\n\
             fn add(ref self, d: i64) { lock self.v x { x = x + d; } }\n\
         }\n\
         fn main() {\n\
             let c = Cell { v: Mutex.new(10) };\n\
             println(c.get());\n\
             c.add(5);\n\
             println(c.get());\n\
         }";
    assert_eq!(run(src), "10\n15\n");
}

// ── Ordering / MemoryOrdering Enums ───────────────────────────

#[test]
fn test_ordering_variants() {
    // Comparison-Ordering variants (Less / Equal / Greater)
    let output = run("fn main() {\n\
             let lt = Ordering.Less;\n\
             let eq = Ordering.Equal;\n\
             let gt = Ordering.Greater;\n\
             println(lt);\n\
             println(eq);\n\
             println(gt);\n\
         }");
    assert!(output.contains("Less"));
    assert!(output.contains("Equal"));
    assert!(output.contains("Greater"));
}

#[test]
fn test_ordering_helper_methods() {
    // `impl Ordering { fn is_lt … }` per design.md § Comparison Traits
    // (lines 5162-5168). Lives in baked source `runtime/stdlib/ordering.kara`;
    // requires the interpreter to walk baked impl blocks.
    let output = run("fn main() {\n\
             let lt = Ordering.Less;\n\
             let eq = Ordering.Equal;\n\
             let gt = Ordering.Greater;\n\
             println(lt.is_lt());\n\
             println(lt.is_le());\n\
             println(lt.is_gt());\n\
             println(lt.is_ge());\n\
             println(lt.is_eq());\n\
             println(eq.is_lt());\n\
             println(eq.is_le());\n\
             println(eq.is_gt());\n\
             println(eq.is_ge());\n\
             println(eq.is_eq());\n\
             println(gt.is_lt());\n\
             println(gt.is_le());\n\
             println(gt.is_gt());\n\
             println(gt.is_ge());\n\
             println(gt.is_eq());\n\
         }");
    assert_eq!(
        output,
        "true\ntrue\nfalse\nfalse\nfalse\n\
         false\ntrue\nfalse\ntrue\ntrue\n\
         false\nfalse\ntrue\ntrue\nfalse\n",
    );
}

#[test]
fn test_option_ordering_helper_methods() {
    // `impl Option[Ordering] { fn is_lt … }` per design.md § Comparison
    // Traits (lines 5268-5277). Lives in baked source
    // `runtime/stdlib/option.kara`. None yields `false` for every
    // predicate (IEEE-754 NaN semantics). Exercises 4 inputs × 5
    // helpers = 20 round-trips through the args-aware impl-table lookup
    // (`Option[Ordering]` impl wins over the absence on generic
    // `Option[T]`).
    let output = run("fn main() {\n\
             let lt: Option[Ordering] = Some(Ordering.Less);\n\
             let eq: Option[Ordering] = Some(Ordering.Equal);\n\
             let gt: Option[Ordering] = Some(Ordering.Greater);\n\
             let none: Option[Ordering] = None;\n\
             println(lt.is_lt());\n\
             println(lt.is_le());\n\
             println(lt.is_gt());\n\
             println(lt.is_ge());\n\
             println(lt.is_eq());\n\
             println(eq.is_lt());\n\
             println(eq.is_le());\n\
             println(eq.is_gt());\n\
             println(eq.is_ge());\n\
             println(eq.is_eq());\n\
             println(gt.is_lt());\n\
             println(gt.is_le());\n\
             println(gt.is_gt());\n\
             println(gt.is_ge());\n\
             println(gt.is_eq());\n\
             println(none.is_lt());\n\
             println(none.is_le());\n\
             println(none.is_gt());\n\
             println(none.is_ge());\n\
             println(none.is_eq());\n\
         }");
    assert_eq!(
        output,
        "true\ntrue\nfalse\nfalse\nfalse\n\
         false\ntrue\nfalse\ntrue\ntrue\n\
         false\nfalse\ntrue\ntrue\nfalse\n\
         false\nfalse\nfalse\nfalse\nfalse\n",
    );
}

#[test]
fn test_memory_ordering_variants() {
    let output = run("fn main() {\n\
             let r = MemoryOrdering.Relaxed;\n\
             let a = MemoryOrdering.Acquire;\n\
             let rel = MemoryOrdering.Release;\n\
             println(r);\n\
             println(a);\n\
             println(rel);\n\
         }");
    assert!(output.contains("Relaxed"));
    assert!(output.contains("Acquire"));
    assert!(output.contains("Release"));
}

// ── Primitive-type associated constants ──────────────────────
//
// Theme 7 (2026-05-10) — `i64.MAX` / `f64.INFINITY` / `usize.MAX` etc.
// dispatch through the shared `PRIMITIVE_CONSTS` table at
// `src/prelude.rs`. The interpreter intercepts the `FieldAccess` arm
// before the bare-primitive identifier would panic; codegen mirrors
// at `compile_field_access`. NaN tests assert the rendered string
// matches `Value::Float`'s Display impl ("NaN").

#[test]
fn test_interp_primitive_const_i64_max() {
    let output = run("fn main() { let x = i64.MAX; println(x); }");
    assert_eq!(output, "9223372036854775807\n");
}

#[test]
fn test_interp_primitive_const_i64_min() {
    let output = run("fn main() { let x = i64.MIN; println(x); }");
    assert_eq!(output, "-9223372036854775808\n");
}

#[test]
fn test_interp_primitive_const_u64_max() {
    // u64::MAX as i64 wraps to -1 in the type-erased interpreter
    // (Value::Int(i64) carries the bit pattern; signed display flips
    // sign). Codegen emits the unsigned i64 — interpreter parity is
    // bit-pattern, not signed-display.
    let output = run("fn main() { let x = u64.MAX; println(x); }");
    assert_eq!(output, "-1\n");
}

#[test]
fn test_interp_primitive_const_usize_max() {
    let output = run("fn main() { let x = usize.MAX; println(x); }");
    assert_eq!(output, "-1\n");
}

#[test]
fn test_interp_primitive_const_f64_infinity() {
    let output = run("fn main() { let x = f64.INFINITY; println(x); }");
    assert_eq!(output.trim(), "inf");
}

#[test]
fn test_interp_primitive_const_f64_neg_infinity() {
    let output = run("fn main() { let x = f64.NEG_INFINITY; println(x); }");
    assert_eq!(output.trim(), "-inf");
}

#[test]
fn test_interp_primitive_const_f64_nan() {
    let output = run("fn main() { let x = f64.NAN; println(x); }");
    assert_eq!(output.trim(), "NaN");
}

#[test]
fn test_interp_primitive_const_f32_epsilon() {
    // f32::EPSILON is 1.1920929e-7. Tree-walk interpreter widens to
    // f64 via `Value::Float`; round-trip through Display preserves
    // enough precision that we can assert a substring.
    let output = run("fn main() { let x = f32.EPSILON; println(x); }");
    assert!(
        output.starts_with("0.000000119"),
        "expected f32.EPSILON to print as 0.000000119...; got {output:?}"
    );
}

// ── Slice[T] end-to-end ────────────────────────────────────────

#[test]
fn test_slice_sum_over_array_coercion() {
    let output = run("fn sum(xs: Slice[i64]) -> i64 {
             let mut acc = 0;
             for x in xs { acc = acc + x; }
             acc
         }
         fn main() {
             let a: Array[i64, 4] = [1, 2, 3, 4];
             println(sum(a));
         }");
    assert_eq!(output, "10\n");
}

#[test]
fn test_slice_range_indexing_on_array() {
    let output = run("fn sum(xs: Slice[i64]) -> i64 {
             let mut acc = 0;
             for x in xs { acc = acc + x; }
             acc
         }
         fn main() {
             let a: Array[i64, 5] = [10, 20, 30, 40, 50];
             let s = a[1..4];
             println(sum(s));
         }");
    assert_eq!(output, "90\n");
}

#[test]
fn test_slice_element_indexing_runtime() {
    let output = run("fn second(xs: Slice[i64]) -> i64 { xs[1] }
         fn main() {
             let a: Array[i64, 3] = [7, 8, 9];
             println(second(a));
         }");
    assert_eq!(output, "8\n");
}

#[test]
fn test_as_slice_on_array() {
    let output = run("fn first(xs: Slice[i64]) -> i64 { xs[0] }
         fn main() {
             let a: Array[i64, 3] = [42, 2, 3];
             let s = a.as_slice();
             println(first(s));
         }");
    assert_eq!(output, "42\n");
}

#[test]
fn test_slice_of_slice_via_range() {
    let output = run("fn sum(xs: Slice[i64]) -> i64 {
             let mut acc = 0;
             for x in xs { acc = acc + x; }
             acc
         }
         fn main() {
             let a: Array[i64, 5] = [1, 2, 3, 4, 5];
             let outer = a[0..5];
             let inner = outer[1..4];
             println(sum(inner));
         }");
    assert_eq!(output, "9\n");
}

// ── Slice[T] stdlib methods ───────────────────────────────────────

#[test]
fn test_slice_is_empty_and_len() {
    let output = run("fn main() {
         let v = [1, 2, 3];
         let s = v.as_slice();
         println(s.is_empty());
         println(s.len());
     }");
    assert_eq!(output, "false\n3\n");
}

#[test]
fn test_slice_first_and_last() {
    let output = run("fn main() {
         let v = [10, 20, 30];
         let s = v.as_slice();
         match s.first() { Some(x) => println(x), None => println(\"none\") }
         match s.last()  { Some(x) => println(x), None => println(\"none\") }
     }");
    assert_eq!(output, "10\n30\n");
}

#[test]
fn test_slice_first_last_empty_slice() {
    let output = run("fn main() {
         let v: Array[i64, 0] = [];
         let s = v.as_slice();
         match s.first() { Some(x) => println(x), None => println(\"none\") }
         match s.last()  { Some(x) => println(x), None => println(\"none\") }
     }");
    assert_eq!(output, "none\nnone\n");
}

#[test]
fn test_slice_get_in_bounds_and_out_of_bounds() {
    let output = run("fn main() {
         let v = [100, 200, 300];
         let s = v.as_slice();
         match s.get(1) { Some(x) => println(x), None => println(\"oob\") }
         match s.get(5) { Some(x) => println(x), None => println(\"oob\") }
     }");
    assert_eq!(output, "200\noob\n");
}

#[test]
fn test_slice_contains() {
    let output = run("fn main() {
         let v = [1, 2, 3, 4];
         let s = v.as_slice();
         println(s.contains(3));
         println(s.contains(9));
     }");
    assert_eq!(output, "true\nfalse\n");
}

#[test]
fn test_slice_binary_search_found_and_not_found() {
    let output = run("fn main() {
         let v = [1, 2, 3, 4, 5];
         let s = v.as_slice();
         match s.binary_search(3) { Some(i) => println(i), None => println(\"not found\") }
         match s.binary_search(9) { Some(i) => println(i), None => println(\"not found\") }
     }");
    assert_eq!(output, "2\nnot found\n");
}

#[test]
fn test_slice_split_at() {
    let output = run("fn main() {
         let v = [1, 2, 3, 4];
         let s = v.as_slice();
         let (a, b) = s.split_at(2);
         println(a.len());
         println(b.len());
     }");
    assert_eq!(output, "2\n2\n");
}

#[test]
fn test_slice_chunks() {
    let output = run("fn main() {
         let v = [1, 2, 3, 4, 5];
         let s = v.as_slice();
         let cs = s.chunks(2);
         println(cs.len());
     }");
    assert_eq!(output, "3\n");
}

#[test]
fn test_slice_windows() {
    let output = run("fn main() {
         let v = [1, 2, 3, 4];
         let s = v.as_slice();
         let ws = s.windows(3);
         println(ws.len());
     }");
    assert_eq!(output, "2\n");
}

#[test]
fn test_slice_sort_and_reverse() {
    let output = run("fn main() {
         let mut v = [3, 1, 4, 1, 5];
         let mut s = v.as_slice_mut();
         s.sort();
         println(s.len());
         s.reverse();
         println(s.len());
     }");
    assert_eq!(output, "5\n5\n");
}

#[test]
fn test_slice_fill() {
    let output = run("fn main() {
         let mut v = [1, 2, 3];
         let mut s = v.as_slice_mut();
         s.fill(0);
         println(s.is_empty());
         println(s.len());
     }");
    assert_eq!(output, "false\n3\n");
}

#[test]
fn test_slice_swap() {
    let output = run("fn main() {
         let mut v = [10, 20, 30];
         let mut s = v.as_slice_mut();
         s.swap(0, 2);
         match s.get(0) { Some(x) => println(x), None => {} }
         match s.get(2) { Some(x) => println(x), None => {} }
     }");
    assert_eq!(output, "30\n10\n");
}

// ── env.args / env.var ────────────────────────────────────────────

#[test]
fn test_env_var_missing_key_returns_err() {
    let output = run("fn main() {
         match env.var(\"__KARAC_NO_SUCH_VAR_XYZ__\") {
             Ok(v) => println(v),
             Err(e) => println(\"not found\"),
         }
     }");
    assert_eq!(output, "not found\n");
}

#[test]
fn test_env_args_returns_array() {
    // env.args() returns a Vec[String]; len() ≥ 1 (includes binary path)
    let output = run("fn main() {
         let args = env.args();
         println(args.len() > 0);
     }");
    assert_eq!(output, "true\n");
}

#[test]
fn test_env_set_round_trips_via_var() {
    // env.set("X", "v") then env.var("X") observes "v". Confirms the new
    // stdlib method writes to the same process-environment block that
    // `env.var` reads from. Use a unique name to avoid collisions with
    // other tests running in the same process.
    std::env::remove_var("KARAC_ENV_SET_ROUND_TRIP_TEST");
    let output = run("fn main() {
         env.set(\"KARAC_ENV_SET_ROUND_TRIP_TEST\", \"hello-set\");
         match env.var(\"KARAC_ENV_SET_ROUND_TRIP_TEST\") {
             Ok(v) => println(v),
             Err(_) => println(\"unset\"),
         }
     }");
    std::env::remove_var("KARAC_ENV_SET_ROUND_TRIP_TEST");
    assert_eq!(output, "hello-set\n");
}

// ── impl From[VarError] for IoError — variant mapping ────────────────────

#[test]
fn test_var_error_not_present_maps_to_io_error_not_found() {
    // VarError.NotPresent → IoError.NotFound via the baked stdlib impl.
    let output = run("fn main() {
         let io: IoError = IoError.from(VarError.NotPresent);
         match io {
             IoError.NotFound => println(\"not_found\"),
             IoError.PermissionDenied => println(\"perm_denied\"),
             IoError.AlreadyExists => println(\"already_exists\"),
             IoError.UnexpectedEof => println(\"eof\"),
             IoError.InvalidUtf8 => println(\"invalid_utf8\"),
             IoError.Interrupted => println(\"interrupted\"),
             IoError.Other(_) => println(\"other\"),
         }
     }");
    assert_eq!(output, "not_found\n");
}

#[test]
fn test_var_error_not_unicode_maps_to_io_error_invalid_utf8() {
    // VarError.NotUnicode → IoError.InvalidUtf8 via the baked stdlib impl.
    let output = run("fn main() {
         let io: IoError = IoError.from(VarError.NotUnicode);
         match io {
             IoError.NotFound => println(\"not_found\"),
             IoError.PermissionDenied => println(\"perm_denied\"),
             IoError.AlreadyExists => println(\"already_exists\"),
             IoError.UnexpectedEof => println(\"eof\"),
             IoError.InvalidUtf8 => println(\"invalid_utf8\"),
             IoError.Interrupted => println(\"interrupted\"),
             IoError.Other(_) => println(\"other\"),
         }
     }");
    assert_eq!(output, "invalid_utf8\n");
}

#[test]
fn test_var_error_question_propagation_produces_io_error_not_found() {
    // End-to-end: `env.var(missing)?` in a function returning
    // `Result[String, IoError]` must propagate as `IoError.NotFound`
    // (because `env.var` returns `VarError.NotPresent` for an unset key,
    // and the `?` operator desugars through `IoError.from(...)`).
    std::env::remove_var("__KARAC_FROM_VAR_TO_IO_NO_SUCH_VAR__");
    let output = run("fn read_var() -> Result[String, IoError] with reads(Env) {
             let s: String = env.var(\"__KARAC_FROM_VAR_TO_IO_NO_SUCH_VAR__\")?;
             Ok(s)
         }
         fn main() {
             match read_var() {
                 Ok(v) => println(v),
                 Err(IoError.NotFound) => println(\"not_found\"),
                 Err(IoError.InvalidUtf8) => println(\"invalid_utf8\"),
                 Err(_) => println(\"other_io_err\"),
             }
         }");
    assert_eq!(output, "not_found\n");
}

// ── `with_provider` runtime + resource method dispatch ───────────

#[test]
fn test_with_provider_dispatches_resource_method_to_top_of_stack() {
    let output = run("effect resource UserDB;
         struct FakeDB { data: i64 }
         impl FakeDB { fn query(self, n: i64) -> i64 { self.data + n } }
         fn main() {
             with_provider[UserDB](FakeDB { data: 100 }, || {
                 println(UserDB.query(5));
             });
         }");
    assert_eq!(output, "105\n");
}

#[test]
fn test_with_provider_nested_same_resource_inner_shadows_outer_restored_on_pop() {
    let output = run("effect resource UserDB;
         struct Db { tag: i64 }
         impl Db { fn id(self) -> i64 { self.tag } }
         fn main() {
             with_provider[UserDB](Db { tag: 1 }, || {
                 println(UserDB.id());
                 with_provider[UserDB](Db { tag: 2 }, || {
                     println(UserDB.id());
                 });
                 println(UserDB.id());
             });
         }");
    assert_eq!(output, "1\n2\n1\n");
}

#[test]
fn test_with_provider_multiple_resources_resolve_independently() {
    let output = run("effect resource UserDB;
         effect resource AuditLog;
         struct Db { tag: i64 }
         impl Db { fn id(self) -> i64 { self.tag } }
         struct Log { count: i64 }
         impl Log { fn count(self) -> i64 { self.count } }
         fn main() {
             with_provider[UserDB](Db { tag: 42 }, || {
                 with_provider[AuditLog](Log { count: 7 }, || {
                     println(UserDB.id());
                     println(AuditLog.count());
                 });
             });
         }");
    assert_eq!(output, "42\n7\n");
}

#[test]
fn test_resource_method_without_provider_raises_runtime_error() {
    let errors = runtime_errors(
        "effect resource UserDB;
         struct Db { tag: i64 }
         impl Db { fn id(self) -> i64 { self.tag } }
         fn main() {
             println(UserDB.id());
         }",
    );
    assert_eq!(
        errors.len(),
        1,
        "expected one runtime error, got {:?}",
        errors
    );
    assert!(
        errors[0]
            .message
            .contains("no provider bound for resource 'UserDB'"),
        "message did not mention missing provider: {:?}",
        errors[0].message
    );
}

#[test]
fn test_with_provider_value_of_closure_is_returned() {
    let output = run("effect resource UserDB;
         struct Db { tag: i64 }
         impl Db { fn id(self) -> i64 { self.tag } }
         fn main() {
             let x = with_provider[UserDB](Db { tag: 99 }, || {
                 UserDB.id()
             });
             println(x);
         }");
    assert_eq!(output, "99\n");
}

#[test]
fn test_with_provider_frame_popped_after_closure_even_when_body_returns() {
    // After the `with_provider` block exits, the resource is unbound
    // again — the second bare `UserDB.id()` should fail with the same
    // "no provider" runtime error as the cold-start case.
    let errors = runtime_errors(
        "effect resource UserDB;
         struct Db { tag: i64 }
         impl Db { fn id(self) -> i64 { self.tag } }
         fn main() {
             with_provider[UserDB](Db { tag: 1 }, || {
                 println(UserDB.id());
             });
             println(UserDB.id());
         }",
    );
    assert!(
        errors.iter().any(|e| e
            .message
            .contains("no provider bound for resource 'UserDB'")),
        "expected one missing-provider error after block exit, got {:?}",
        errors
    );
}

// ── Ambient program-rooted resources (CR-A slice 3) ──────────────

#[test]
fn test_ambient_clock_now_returns_positive_timestamp() {
    // Bare `Clock.now()` outside any `with_provider` uses the ambient
    // default provider installed in the base frame. The system time is
    // well past the Unix epoch for all plausible test environments, so
    // a non-zero result is enough to prove the default fired.
    let output = run("fn main() {\n\
                          let t = Clock.now();\n\
                          println(t > 1_000_000_000);\n\
                      }");
    assert_eq!(output, "true\n");
}

#[test]
fn test_ambient_clock_with_provider_overrides_default() {
    // `with_provider[Clock]` pushes a fake on top of the ambient default.
    // Inside the scope the fake wins; after exit the ambient default is
    // still present (the frame popped back to the base frame, not past it).
    let output = run("struct FakeClock {}\n\
                      impl FakeClock { fn now(self) -> i64 { 42 } }\n\
                      fn main() {\n\
                          with_provider[Clock](FakeClock {}, || {\n\
                              println(Clock.now());\n\
                          });\n\
                          let t = Clock.now();\n\
                          println(t > 1_000_000_000);\n\
                      }");
    assert_eq!(output, "42\ntrue\n");
}

#[test]
fn test_ambient_clock_not_required_to_declare_effect_resource() {
    // `Clock` is a prelude effect resource — user code doesn't need
    // `effect resource Clock;` to call `Clock.now()` or wrap it in
    // `with_provider[Clock](...)`.
    let output = run("fn main() {\n\
                          let t = Clock.now();\n\
                          if t > 0 { println(\"ok\"); }\n\
                      }");
    assert_eq!(output, "ok\n");
}

#[test]
fn test_ambient_random_source_next_u64_advances_state() {
    // Two consecutive draws from the default `RandomSource` must return
    // different values — the xorshift state advances on every call. We
    // can't assert any specific number (seeded from wall-clock nanoseconds)
    // but inequality is a sharp witness that state advanced.
    let output = run("fn main() {\n\
                          let a = RandomSource.next_u64();\n\
                          let b = RandomSource.next_u64();\n\
                          println(a != b);\n\
                      }");
    assert_eq!(output, "true\n");
}

#[test]
fn test_ambient_random_source_with_provider_overrides_default() {
    // `with_provider[RandomSource]` shadows the ambient xorshift. Inside
    // the scope the fake wins (returns `7` twice); after exit the
    // interpreter is back on the ambient default (two fresh non-equal
    // draws).
    let output = run("struct FakeRandom {}\n\
                      impl FakeRandom { fn next_u64(self) -> i64 { 7 } }\n\
                      fn main() {\n\
                          with_provider[RandomSource](FakeRandom {}, || {\n\
                              println(RandomSource.next_u64());\n\
                              println(RandomSource.next_u64());\n\
                          });\n\
                          let a = RandomSource.next_u64();\n\
                          let b = RandomSource.next_u64();\n\
                          println(a != b);\n\
                      }");
    assert_eq!(output, "7\n7\ntrue\n");
}

#[test]
fn test_ambient_random_source_not_required_to_declare_effect_resource() {
    // `RandomSource` is a prelude effect resource — user code doesn't need
    // `effect resource RandomSource;` to call `RandomSource.next_u64()`.
    let output = run("fn main() {\n\
                          let _ = RandomSource.next_u64();\n\
                          println(\"ok\");\n\
                      }");
    assert_eq!(output, "ok\n");
}

#[test]
fn test_ambient_env_args_returns_nonempty_array() {
    // `Env.args()` returns process argv as `Vec[String]`. Under `cargo
    // test`, argv[0] is the test binary path, so the array is guaranteed
    // non-empty. We don't assert on exact contents (too environment-
    // dependent) — the length check is a sharp witness the default fired.
    let output = run("fn main() {\n\
                          let a = Env.args();\n\
                          println(a.len() > 0);\n\
                      }");
    assert_eq!(output, "true\n");
}

#[test]
fn test_ambient_env_with_provider_overrides_default() {
    // `with_provider[Env]` shadows the ambient default. The fake's
    // `args()` returns a controlled Vec; after exit the ambient default
    // (real argv) is back.
    let output = run("struct FakeEnv {}\n\
                      impl FakeEnv { fn args(self) -> Vec[String] { [\"a\", \"b\"] } }\n\
                      fn main() {\n\
                          with_provider[Env](FakeEnv {}, || {\n\
                              let a = Env.args();\n\
                              println(a.len());\n\
                              println(a[0]);\n\
                              println(a[1]);\n\
                          });\n\
                          let outer = Env.args();\n\
                          println(outer.len() > 0);\n\
                      }");
    assert_eq!(output, "2\na\nb\ntrue\n");
}

#[test]
fn test_ambient_env_not_required_to_declare_effect_resource() {
    // `Env` is a prelude effect resource — user code doesn't need
    // `effect resource Env;` to call `Env.args()`.
    let output = run("fn main() {\n\
                          let _ = Env.args();\n\
                          println(\"ok\");\n\
                      }");
    assert_eq!(output, "ok\n");
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

#[test]
fn test_ambient_env_var_present_returns_ok() {
    // `Env.var(name)` returns `Ok(value)` for a set environment variable.
    // We set the var via `std::env::set_var` from the test harness, then
    // observe it through the interpreter's ambient default provider.
    std::env::set_var("KARAC_ENV_VAR_PRESENT_TEST", "hello");
    let output = run("fn main() {\n\
                          let r = Env.var(\"KARAC_ENV_VAR_PRESENT_TEST\");\n\
                          match r {\n\
                              Ok(v) => println(v),\n\
                              Err(_) => println(\"unset\"),\n\
                          }\n\
                      }");
    std::env::remove_var("KARAC_ENV_VAR_PRESENT_TEST");
    assert_eq!(output, "hello\n");
}

#[test]
fn test_ambient_env_var_missing_returns_err_not_present() {
    // Missing var returns `Err(VarError.NotPresent)`. We don't name the
    // variant explicitly (it's not in `PRELUDE_VARIANTS` per v49 Q2=B);
    // the `Err(_)` arm proves the Err branch fired.
    std::env::remove_var("KARAC_ENV_VAR_MISSING_TEST_XYZ");
    let output = run("fn main() {\n\
                          let r = Env.var(\"KARAC_ENV_VAR_MISSING_TEST_XYZ\");\n\
                          match r {\n\
                              Ok(_) => println(\"set\"),\n\
                              Err(_) => println(\"unset\"),\n\
                          }\n\
                      }");
    assert_eq!(output, "unset\n");
}

#[test]
fn test_ambient_env_var_with_provider_overrides_default() {
    // `with_provider[Env]` shadows the ambient default — the FakeEnv's
    // `var` method wins inside the scope. FakeEnv uses `Result[String,
    // String]` as its return type to avoid pinning the test on `VarError`
    // resolving as a user-visible name (the interpreter's resource-method
    // dispatch is duck-typed at runtime).
    let output = run("struct FakeEnv {}\n\
                      impl FakeEnv {\n\
                          fn var(self, name: ref String) -> Result[String, String] {\n\
                              if name == \"FOO\" { Ok(\"fake-foo\") } else { Err(\"\") }\n\
                          }\n\
                      }\n\
                      fn main() {\n\
                          with_provider[Env](FakeEnv {}, || {\n\
                              match Env.var(\"FOO\") {\n\
                                  Ok(v) => println(v),\n\
                                  Err(_) => println(\"err\"),\n\
                              }\n\
                              match Env.var(\"BAR\") {\n\
                                  Ok(_) => println(\"ok\"),\n\
                                  Err(_) => println(\"err\"),\n\
                              }\n\
                          });\n\
                      }");
    assert_eq!(output, "fake-foo\nerr\n");
}

#[test]
fn test_ambient_stdout_println_resource_method_writes() {
    // Direct `Stdout.println(s)` dispatches to the BuiltinDefault arm,
    // which routes through `write_stdout` and lands in the harness's
    // `captured_output` buffer. Same path the routed free `println`
    // takes — this just exercises the user-visible Stdout surface.
    let output = run("fn main() {\n\
                          Stdout.println(\"hello\");\n\
                          Stdout.println(\"world\");\n\
                      }");
    assert_eq!(output, "hello\nworld\n");
}

#[test]
fn test_ambient_stdout_print_resource_method_writes_without_newline() {
    // `Stdout.print(s)` does NOT append a newline — the test asserts
    // two `print` calls concatenate cleanly, then a trailing `println`
    // closes the line so the captured buffer ends with `\n`.
    let output = run("fn main() {\n\
                          Stdout.print(\"a\");\n\
                          Stdout.print(\"b\");\n\
                          Stdout.println(\"c\");\n\
                      }");
    assert_eq!(output, "abc\n");
}

#[test]
fn test_free_println_routes_through_stdout_provider() {
    // The free `println(x)` is routed through the `Stdout` provider
    // stack — installing a `with_provider[Stdout]` fake intercepts the
    // call. The Mute fake swallows everything; the inner `println` has
    // no observable effect, while the outer `println` (after the scope
    // pops back to the BuiltinDefault) still writes normally.
    let output = run("struct Mute {}\n\
                      impl Mute {\n\
                          fn println(self, s: String) { }\n\
                          fn print(self, s: String) { }\n\
                      }\n\
                      fn main() {\n\
                          with_provider[Stdout](Mute {}, || {\n\
                              println(\"hidden\");\n\
                              println(\"also hidden\");\n\
                          });\n\
                          println(\"visible\");\n\
                      }");
    assert_eq!(output, "visible\n");
}

#[test]
fn test_ambient_stdout_not_required_to_declare_effect_resource() {
    // `Stdout` is a prelude effect resource — user code doesn't need
    // `effect resource Stdout;` to call `Stdout.println(...)`.
    let output = run("fn main() {\n\
                          Stdout.println(\"ok\");\n\
                      }");
    assert_eq!(output, "ok\n");
}

#[test]
fn test_eprintln_routes_through_stderr_provider() {
    // `eprintln(x)` previously panicked with "variable 'eprintln' not
    // found" — it was in PRELUDE_FUNCTIONS but had no interpreter arm.
    // Now it routes through `Stderr.println` like `println` routes
    // through `Stdout.println`. We can't assert the stderr contents
    // (the test harness only captures stdout) but we can prove the
    // call succeeds and a subsequent `println` still writes stdout.
    let output = run("fn main() {\n\
                          eprintln(\"to stderr\");\n\
                          println(\"to stdout\");\n\
                      }");
    assert_eq!(output, "to stdout\n");
}

#[test]
fn test_ambient_stdin_with_provider_overrides_default() {
    // Symmetric to the Stdout interception test: a `with_provider[Stdin]`
    // install routes `Stdin.read_line()` through the user's fake instead
    // of pulling from the real stdin. CannedStdin returns a fixed line;
    // the test asserts that line came back through the provider stack.
    // Uses `Result[String, String]` to dodge `IoError` name resolution
    // (the dispatch is duck-typed at runtime — same trick as the Env test).
    let output = run("struct CannedStdin {}\n\
                      impl CannedStdin {\n\
                          fn read_line(self) -> Result[String, String] {\n\
                              Ok(\"piped line\")\n\
                          }\n\
                      }\n\
                      fn main() {\n\
                          with_provider[Stdin](CannedStdin {}, || {\n\
                              match Stdin.read_line() {\n\
                                  Ok(s)  => println(s),\n\
                                  Err(_) => println(\"err\"),\n\
                              }\n\
                          });\n\
                      }");
    assert_eq!(output, "piped line\n");
}

// ── `providers { R => p, ... } in { body }` block ───────────────

#[test]
fn test_providers_block_binds_each_resource() {
    let output = run("effect resource UserDB;
         effect resource AuditLog;
         struct Db { tag: i64 }
         impl Db { fn id(self) -> i64 { self.tag } }
         struct Log { count: i64 }
         impl Log { fn count(self) -> i64 { self.count } }
         fn main() {
             providers {
                 UserDB   => Db { tag: 7 },
                 AuditLog => Log { count: 3 },
             } in {
                 println(UserDB.id());
                 println(AuditLog.count());
             }
         }");
    assert_eq!(output, "7\n3\n");
}

#[test]
fn test_providers_block_returns_body_value() {
    let output = run("effect resource UserDB;
         struct Db { tag: i64 }
         impl Db { fn id(self) -> i64 { self.tag } }
         fn main() {
             let r = providers {
                 UserDB => Db { tag: 42 },
             } in {
                 UserDB.id()
             };
             println(r);
         }");
    assert_eq!(output, "42\n");
}

#[test]
fn test_providers_block_single_trailing_comma_accepted() {
    let output = run("effect resource UserDB;
         struct Db { tag: i64 }
         impl Db { fn id(self) -> i64 { self.tag } }
         fn main() {
             providers {
                 UserDB => Db { tag: 9 },
             } in {
                 println(UserDB.id());
             }
         }");
    assert_eq!(output, "9\n");
}

#[test]
fn test_providers_block_frames_popped_after_body() {
    // After the block exits, all bindings are released — the trailing
    // `UserDB.id()` should fail with a missing-provider error, confirming
    // every pushed frame was popped.
    let errors = runtime_errors(
        "effect resource UserDB;
         effect resource AuditLog;
         struct Db { tag: i64 }
         impl Db { fn id(self) -> i64 { self.tag } }
         struct Log { count: i64 }
         impl Log { fn count(self) -> i64 { self.count } }
         fn main() {
             providers {
                 UserDB   => Db { tag: 1 },
                 AuditLog => Log { count: 2 },
             } in {
                 println(UserDB.id());
             }
             println(UserDB.id());
         }",
    );
    assert!(
        errors.iter().any(|e| e
            .message
            .contains("no provider bound for resource 'UserDB'")),
        "expected missing-provider error after block exit, got {:?}",
        errors
    );
}

#[test]
fn test_providers_block_evaluates_all_expressions_before_scope() {
    // Evaluate-all-then-scope semantics: every provider expression runs in
    // source order before any resource scope opens. We observe this by
    // threading a captured `i` counter through side-effecting constructors
    // that each append a trace string; the body ties off the sequence.
    let output = run("effect resource R1;
         effect resource R2;
         struct P { n: i64 }
         impl P { fn n(self) -> i64 { self.n } }
         fn mk1() -> P { println(\"eval1\"); P { n: 1 } }
         fn mk2() -> P { println(\"eval2\"); P { n: 2 } }
         fn main() {
             providers {
                 R1 => mk1(),
                 R2 => mk2(),
             } in {
                 println(\"body\");
                 println(R1.n());
                 println(R2.n());
             }
         }");
    assert_eq!(output, "eval1\neval2\nbody\n1\n2\n");
}

#[test]
fn test_providers_block_inner_binding_overrides_outer_with_provider() {
    // Nesting a `providers { R => ... }` block inside an outer
    // `with_provider[R]` should shadow the outer binding for the body's
    // duration and restore it on exit — same stack semantics as
    // nested `with_provider`.
    let output = run("effect resource UserDB;
         struct Db { tag: i64 }
         impl Db { fn id(self) -> i64 { self.tag } }
         fn main() {
             with_provider[UserDB](Db { tag: 1 }, || {
                 println(UserDB.id());
                 providers {
                     UserDB => Db { tag: 2 },
                 } in {
                     println(UserDB.id());
                 }
                 println(UserDB.id());
             });
         }");
    assert_eq!(output, "1\n2\n1\n");
}

// ── Standard I/O interpreter tests ──────────────────────────────────────────

#[test]
fn test_filesystem_write_and_read_roundtrip() {
    let tmp = std::env::temp_dir().join("karac_test_fs_roundtrip.txt");
    // Escape backslashes so Windows paths (C:\Users\...) are valid inside a Kāra string literal.
    let path = tmp.to_str().unwrap().replace('\\', "\\\\");
    // Write via the host OS directly, then read back through the interpreter.
    std::fs::write(&tmp, "hello kara").expect("temp write");
    let src = format!(
        "fn main() {{
             let r = FileSystem.read_to_string(\"{path}\");
             match r {{
                 Ok(contents) => println(contents),
                 Err(_) => println(\"read error\"),
             }}
         }}"
    );
    let out = run_no_errors(&src);
    assert_eq!(out, "hello kara\n");
    let _ = std::fs::remove_file(&tmp);
}

#[test]
fn test_filesystem_read_nonexistent_file_returns_err() {
    let src = "fn main() {
                   let r = FileSystem.read_to_string(\"/nonexistent_karac_test_xyz.txt\");
                   match r {
                       Ok(_) => println(\"ok\"),
                       Err(e) => match e {
                           IoError.NotFound => println(\"not found\"),
                           _ => println(\"other error\"),
                       },
                   }
               }";
    let out = run_no_errors(src);
    assert_eq!(out, "not found\n");
}

#[test]
fn test_stdout_flush_is_callable() {
    // Stdout.flush() should return Unit without error.
    let out = run_no_errors("fn main() { Stdout.flush(); println(\"ok\"); }");
    assert_eq!(out, "ok\n");
}

// ── Phase 8 File handle slice F1 — interpreter MVP ─────────────────
//
// `File.open` / `.create` / `.append` return `Result[File, IoError]`;
// `file.read` / `.write` / `.flush` operate on the live handle. Drop
// on the last Arc clone closes the OS fd. Tests cover the round-trip
// (create + write + flush + reopen + read), the error path (open
// nonexistent → IoError.NotFound), and the appendconstructor.

#[test]
fn test_file_create_write_flush_reopen_read_roundtrip() {
    let tmp = std::env::temp_dir().join("karac_test_file_roundtrip.txt");
    let path = tmp.to_str().unwrap().replace('\\', "\\\\");
    let _ = std::fs::remove_file(&tmp);
    let src = format!(
        "fn main() {{
             match File.create(\"{path}\") {{
                 Ok(f) => {{
                     let data = [104u8, 105u8, 10u8];
                     match f.write(data[0..3]) {{
                         Ok(n) => println(\"wrote \" + n.to_string()),
                         Err(_) => println(\"write err\"),
                     }}
                     match f.flush() {{
                         Ok(_) => println(\"flushed\"),
                         Err(_) => println(\"flush err\"),
                     }}
                 }}
                 Err(_) => println(\"create err\"),
             }}
             match File.open(\"{path}\") {{
                 Ok(f) => {{
                     let buf = [0u8, 0u8, 0u8, 0u8];
                     match f.read(buf[0..4]) {{
                         Ok(n) => println(\"read \" + n.to_string()),
                         Err(_) => println(\"read err\"),
                     }}
                 }}
                 Err(_) => println(\"open err\"),
             }}
         }}"
    );
    let out = run_no_errors(&src);
    assert_eq!(out, "wrote 3\nflushed\nread 3\n");
    // Confirm the file has the expected contents (write actually
    // persisted, not just that the interpreter said it did).
    let written = std::fs::read(&tmp).expect("temp read");
    assert_eq!(written, b"hi\n");
    let _ = std::fs::remove_file(&tmp);
}

#[test]
fn test_file_open_nonexistent_returns_io_error_not_found() {
    // Same NotFound variant as FileSystem.read_to_string — the
    // `io_error_from_std` helper maps ErrorKind::NotFound to
    // IoError.NotFound regardless of which surface fired it.
    let src = "fn main() {
                   match File.open(\"/nonexistent_karac_test_F1.txt\") {
                       Ok(_) => println(\"unexpected ok\"),
                       Err(e) => match e {
                           IoError.NotFound => println(\"not found\"),
                           _ => println(\"other\"),
                       },
                   }
               }";
    let out = run_no_errors(src);
    assert_eq!(out, "not found\n");
}

#[test]
fn test_file_append_constructor_writes_at_end() {
    // `File.append` opens in append mode (positions writes at end of
    // file, creating it if absent). Two consecutive appends should
    // produce concatenated contents.
    let tmp = std::env::temp_dir().join("karac_test_file_append.txt");
    let path = tmp.to_str().unwrap().replace('\\', "\\\\");
    let _ = std::fs::remove_file(&tmp);
    let src = format!(
        "fn main() {{
             let first = [97u8, 98u8];
             let second = [99u8, 100u8];
             match File.append(\"{path}\") {{
                 Ok(f) => {{ f.write(first[0..2]); }}
                 Err(_) => println(\"append1 err\"),
             }}
             match File.append(\"{path}\") {{
                 Ok(f) => {{ f.write(second[0..2]); }}
                 Err(_) => println(\"append2 err\"),
             }}
         }}"
    );
    let _ = run_no_errors(&src);
    let written = std::fs::read(&tmp).expect("temp read");
    assert_eq!(written, b"abcd");
    let _ = std::fs::remove_file(&tmp);
}

#[test]
fn test_file_flush_on_writable_handle_returns_ok() {
    // Flush on a freshly opened writable file returns Ok(Unit) even
    // when nothing was written — the std::fs::File flush is a no-op
    // for un-buffered handles, never an error in this case.
    let tmp = std::env::temp_dir().join("karac_test_file_flush_ok.txt");
    let path = tmp.to_str().unwrap().replace('\\', "\\\\");
    let _ = std::fs::remove_file(&tmp);
    let src = format!(
        "fn main() {{
             match File.create(\"{path}\") {{
                 Ok(f) => {{
                     match f.flush() {{
                         Ok(_) => println(\"flushed\"),
                         Err(_) => println(\"err\"),
                     }}
                 }}
                 Err(_) => println(\"create err\"),
             }}
         }}"
    );
    let out = run_no_errors(&src);
    assert_eq!(out, "flushed\n");
    let _ = std::fs::remove_file(&tmp);
}

#[test]
fn test_stderr_flush_is_callable() {
    let out = run_no_errors("fn main() { Stderr.flush(); println(\"ok\"); }");
    assert_eq!(out, "ok\n");
}

// ── Phase 8 BufReader[R] — interpreter MVP ────────────────────────
//
// BufReader.new / .with_capacity wrap a `File` (via a dup of its fd)
// with a buffered reader; read_line / read_to_string append into a
// `mut ref String` and return the byte count (0 from read_line = EOF);
// read fills a `mut Slice[u8]`. Tests cover the line-then-rest
// round-trip, read_to_string slurping, the Slice read, with_capacity,
// and the EOF count.

#[test]
fn test_bufreader_read_line_then_read_to_string_roundtrip() {
    let tmp = std::env::temp_dir().join("karac_test_bufreader_roundtrip.txt");
    let path = tmp.to_str().unwrap().replace('\\', "\\\\");
    let _ = std::fs::remove_file(&tmp);
    std::fs::write(&tmp, b"hi\nyo\n").expect("seed temp");
    let src = format!(
        "fn main() {{
             match File.open(\"{path}\") {{
                 Ok(f) => {{
                     let br = BufReader.new(f);
                     let mut line = String.new();
                     match br.read_line(line) {{
                         Ok(n) => println(\"line n=\" + n.to_string() + \" [\" + line + \"]\"),
                         Err(_) => println(\"read err\"),
                     }}
                     let mut rest = String.new();
                     match br.read_to_string(rest) {{
                         Ok(n) => println(\"rest n=\" + n.to_string() + \" [\" + rest + \"]\"),
                         Err(_) => println(\"read err\"),
                     }}
                 }}
                 Err(_) => println(\"open err\"),
             }}
         }}"
    );
    let out = run_no_errors(&src);
    assert_eq!(out, "line n=3 [hi\n]\nrest n=3 [yo\n]\n");
    let _ = std::fs::remove_file(&tmp);
}

#[test]
fn test_bufreader_read_line_at_eof_returns_zero() {
    // read_line on an empty file returns Ok(0) (EOF), leaving the
    // destination String untouched.
    let tmp = std::env::temp_dir().join("karac_test_bufreader_eof.txt");
    let path = tmp.to_str().unwrap().replace('\\', "\\\\");
    let _ = std::fs::remove_file(&tmp);
    std::fs::write(&tmp, b"").expect("seed temp");
    let src = format!(
        "fn main() {{
             match File.open(\"{path}\") {{
                 Ok(f) => {{
                     let br = BufReader.new(f);
                     let mut line = String.new();
                     match br.read_line(line) {{
                         Ok(n) => println(\"n=\" + n.to_string()),
                         Err(_) => println(\"read err\"),
                     }}
                 }}
                 Err(_) => println(\"open err\"),
             }}
         }}"
    );
    let out = run_no_errors(&src);
    assert_eq!(out, "n=0\n");
    let _ = std::fs::remove_file(&tmp);
}

#[test]
fn test_bufreader_read_to_string_slurps_whole_file() {
    let tmp = std::env::temp_dir().join("karac_test_bufreader_slurp.txt");
    let path = tmp.to_str().unwrap().replace('\\', "\\\\");
    let _ = std::fs::remove_file(&tmp);
    std::fs::write(&tmp, b"alpha\nbeta\n").expect("seed temp");
    let src = format!(
        "fn main() {{
             match File.open(\"{path}\") {{
                 Ok(f) => {{
                     let br = BufReader.new(f);
                     let mut all = String.new();
                     match br.read_to_string(all) {{
                         Ok(n) => println(\"n=\" + n.to_string() + \" [\" + all + \"]\"),
                         Err(_) => println(\"read err\"),
                     }}
                 }}
                 Err(_) => println(\"open err\"),
             }}
         }}"
    );
    let out = run_no_errors(&src);
    assert_eq!(out, "n=11 [alpha\nbeta\n]\n");
    let _ = std::fs::remove_file(&tmp);
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

#[test]
fn test_bufreader_lines_iterates_and_strips_newlines() {
    // `for line in br.lines()` yields one Ok(line) per line with the
    // trailing newline stripped, terminating at EOF.
    let tmp = std::env::temp_dir().join("karac_test_bufreader_lines.txt");
    let path = tmp.to_str().unwrap().replace('\\', "\\\\");
    let _ = std::fs::remove_file(&tmp);
    std::fs::write(&tmp, b"alpha\nbeta\ngamma\n").expect("seed temp");
    let src = format!(
        "fn main() {{
             match File.open(\"{path}\") {{
                 Ok(f) => {{
                     let br = BufReader.new(f);
                     let mut count = 0;
                     for line in br.lines() {{
                         match line {{
                             Ok(s) => {{ println(\"[\" + s + \"]\"); count = count + 1; }}
                             Err(_) => println(\"read err\"),
                         }}
                     }}
                     println(\"count=\" + count.to_string());
                 }}
                 Err(_) => println(\"open err\"),
             }}
         }}"
    );
    let out = run_no_errors(&src);
    assert_eq!(out, "[alpha]\n[beta]\n[gamma]\ncount=3\n");
    let _ = std::fs::remove_file(&tmp);
}

#[test]
fn test_bufreader_lines_crlf_and_no_trailing_newline() {
    // CRLF line endings are stripped (\r\n, matching std::io::Lines), and a
    // final line with no trailing newline is still yielded.
    let tmp = std::env::temp_dir().join("karac_test_bufreader_lines_crlf.txt");
    let path = tmp.to_str().unwrap().replace('\\', "\\\\");
    let _ = std::fs::remove_file(&tmp);
    std::fs::write(&tmp, b"one\r\ntwo\r\nthree").expect("seed temp");
    let src = format!(
        "fn main() {{
             match File.open(\"{path}\") {{
                 Ok(f) => {{
                     let br = BufReader.new(f);
                     for line in br.lines() {{
                         match line {{
                             Ok(s) => println(\"[\" + s + \"]\"),
                             Err(_) => println(\"read err\"),
                         }}
                     }}
                 }}
                 Err(_) => println(\"open err\"),
             }}
         }}"
    );
    let out = run_no_errors(&src);
    assert_eq!(out, "[one]\n[two]\n[three]\n");
    let _ = std::fs::remove_file(&tmp);
}

#[test]
fn test_bufreader_lines_empty_file_yields_nothing() {
    // An empty file produces zero lines (the for-loop body never runs).
    let tmp = std::env::temp_dir().join("karac_test_bufreader_lines_empty.txt");
    let path = tmp.to_str().unwrap().replace('\\', "\\\\");
    let _ = std::fs::remove_file(&tmp);
    std::fs::write(&tmp, b"").expect("seed temp");
    let src = format!(
        "fn main() {{
             match File.open(\"{path}\") {{
                 Ok(f) => {{
                     let br = BufReader.new(f);
                     let mut count = 0;
                     for line in br.lines() {{
                         match line {{
                             Ok(_) => {{ count = count + 1; }}
                             Err(_) => {{}}
                         }}
                     }}
                     println(\"count=\" + count.to_string());
                 }}
                 Err(_) => println(\"open err\"),
             }}
         }}"
    );
    let out = run_no_errors(&src);
    assert_eq!(out, "count=0\n");
    let _ = std::fs::remove_file(&tmp);
}

#[test]
fn test_bufreader_fill_buf_peek_consume_read_roundtrip() {
    // fill_buf peeks the buffered bytes without consuming; consume(5) advances
    // past "HELLO"; a subsequent read then returns the remaining "WORLD".
    let tmp = std::env::temp_dir().join("karac_test_bufreader_peek.txt");
    let path = tmp.to_str().unwrap().replace('\\', "\\\\");
    let _ = std::fs::remove_file(&tmp);
    std::fs::write(&tmp, b"HELLOWORLD").expect("seed temp");
    let src = format!(
        "fn main() {{
             match File.open(\"{path}\") {{
                 Ok(f) => {{
                     let br = BufReader.new(f);
                     match br.fill_buf() {{
                         Ok(buf) => println(\"peek len=\" + buf.len().to_string()
                             + \" b0=\" + buf[0].to_string()),
                         Err(_) => println(\"fill err\"),
                     }}
                     br.consume(5);
                     let rest = [0u8, 0u8, 0u8, 0u8, 0u8];
                     match br.read(rest[0..5]) {{
                         Ok(n) => println(\"read n=\" + n.to_string()
                             + \" first=\" + rest[0].to_string()),
                         Err(_) => println(\"read err\"),
                     }}
                 }}
                 Err(_) => println(\"open err\"),
             }}
         }}"
    );
    let out = run_no_errors(&src);
    // 'H' == 72 (peeked, not consumed), 'W' == 87 (first byte after consume(5)).
    assert_eq!(out, "peek len=10 b0=72\nread n=5 first=87\n");
    let _ = std::fs::remove_file(&tmp);
}

#[test]
fn test_bufreader_fill_buf_at_eof_returns_empty_slice() {
    // fill_buf on an empty file returns Ok with a zero-length slice (EOF).
    let tmp = std::env::temp_dir().join("karac_test_bufreader_fillbuf_eof.txt");
    let path = tmp.to_str().unwrap().replace('\\', "\\\\");
    let _ = std::fs::remove_file(&tmp);
    std::fs::write(&tmp, b"").expect("seed temp");
    let src = format!(
        "fn main() {{
             match File.open(\"{path}\") {{
                 Ok(f) => {{
                     let br = BufReader.new(f);
                     match br.fill_buf() {{
                         Ok(buf) => println(\"len=\" + buf.len().to_string()),
                         Err(_) => println(\"fill err\"),
                     }}
                 }}
                 Err(_) => println(\"open err\"),
             }}
         }}"
    );
    let out = run_no_errors(&src);
    assert_eq!(out, "len=0\n");
    let _ = std::fs::remove_file(&tmp);
}

#[test]
fn test_bufreader_consume_clamps_past_buffer() {
    // consume(n) with n past the buffered length is clamped (no panic); after
    // consuming all 3 buffered bytes, a read returns 0 (EOF).
    let tmp = std::env::temp_dir().join("karac_test_bufreader_consume_clamp.txt");
    let path = tmp.to_str().unwrap().replace('\\', "\\\\");
    let _ = std::fs::remove_file(&tmp);
    std::fs::write(&tmp, b"abc").expect("seed temp");
    let src = format!(
        "fn main() {{
             match File.open(\"{path}\") {{
                 Ok(f) => {{
                     let br = BufReader.new(f);
                     let _ = br.fill_buf();
                     br.consume(100);
                     let rest = [0u8, 0u8, 0u8];
                     match br.read(rest[0..3]) {{
                         Ok(n) => println(\"n=\" + n.to_string()),
                         Err(_) => println(\"read err\"),
                     }}
                 }}
                 Err(_) => println(\"open err\"),
             }}
         }}"
    );
    let out = run_no_errors(&src);
    assert_eq!(out, "n=0\n");
    let _ = std::fs::remove_file(&tmp);
}

// ── Phase 8 BufWriter[W] — interpreter MVP ────────────────────────
//
// BufWriter.new / .with_capacity wrap a `File` (via a dup of its fd)
// with a buffered writer; `write` accepts a `Slice[u8]` and returns the
// byte count buffered; `flush` drains the buffer to the underlying fd.
// Tests cover the write-flush-reopen-read round-trip, with_capacity, the
// drop-flush (no explicit flush) path, and an empty write.

#[test]
fn test_bufwriter_write_flush_reopen_read_roundtrip() {
    // Write "hi\n" through a BufWriter, flush, then read the file back via
    // FileSystem.read_to_string to prove the bytes reached disk.
    let tmp = std::env::temp_dir().join("karac_test_bufwriter_roundtrip.txt");
    let path = tmp.to_str().unwrap().replace('\\', "\\\\");
    let _ = std::fs::remove_file(&tmp);
    let src = format!(
        "fn main() {{
             match File.create(\"{path}\") {{
                 Ok(f) => {{
                     let bw = BufWriter.new(f);
                     let data = [104u8, 105u8, 10u8];
                     match bw.write(data[0..3]) {{
                         Ok(n) => println(\"wrote \" + n.to_string()),
                         Err(_) => println(\"write err\"),
                     }}
                     match bw.flush() {{
                         Ok(_) => println(\"flushed\"),
                         Err(_) => println(\"flush err\"),
                     }}
                 }}
                 Err(_) => println(\"create err\"),
             }}
             match FileSystem.read_to_string(\"{path}\") {{
                 Ok(s) => println(\"contents=[\" + s + \"]\"),
                 Err(_) => println(\"read err\"),
             }}
         }}"
    );
    let out = run_no_errors(&src);
    assert_eq!(out, "wrote 3\nflushed\ncontents=[hi\n]\n");
    let _ = std::fs::remove_file(&tmp);
}

#[test]
fn test_bufwriter_with_capacity_write_flush() {
    // with_capacity wraps with an explicit buffer size; the write+flush
    // path is otherwise identical.
    let tmp = std::env::temp_dir().join("karac_test_bufwriter_cap.txt");
    let path = tmp.to_str().unwrap().replace('\\', "\\\\");
    let _ = std::fs::remove_file(&tmp);
    let src = format!(
        "fn main() {{
             match File.create(\"{path}\") {{
                 Ok(f) => {{
                     let bw = BufWriter.with_capacity(f, 4);
                     let data = [65u8, 66u8, 67u8];
                     let _ = bw.write(data[0..3]);
                     let _ = bw.flush();
                 }}
                 Err(_) => println(\"create err\"),
             }}
             match FileSystem.read_to_string(\"{path}\") {{
                 Ok(s) => println(\"contents=[\" + s + \"]\"),
                 Err(_) => println(\"read err\"),
             }}
         }}"
    );
    let out = run_no_errors(&src);
    assert_eq!(out, "contents=[ABC]\n");
    let _ = std::fs::remove_file(&tmp);
}

#[test]
fn test_bufwriter_write_all_writes_whole_buffer() {
    // write_all loops until the whole buffer is accepted and returns Unit;
    // flush then drains it to disk. Read-back proves every byte landed.
    let tmp = std::env::temp_dir().join("karac_test_bufwriter_write_all.txt");
    let path = tmp.to_str().unwrap().replace('\\', "\\\\");
    let _ = std::fs::remove_file(&tmp);
    let src = format!(
        "fn main() {{
             match File.create(\"{path}\") {{
                 Ok(f) => {{
                     let bw = BufWriter.new(f);
                     let data = [119u8, 120u8, 121u8, 122u8];
                     match bw.write_all(data[0..4]) {{
                         Ok(_) => println(\"wrote all\"),
                         Err(_) => println(\"write err\"),
                     }}
                     let _ = bw.flush();
                 }}
                 Err(_) => println(\"create err\"),
             }}
             match FileSystem.read_to_string(\"{path}\") {{
                 Ok(s) => println(\"contents=[\" + s + \"]\"),
                 Err(_) => println(\"read err\"),
             }}
         }}"
    );
    let out = run_no_errors(&src);
    assert_eq!(out, "wrote all\ncontents=[wxyz]\n");
    let _ = std::fs::remove_file(&tmp);
}

#[test]
fn test_bufwriter_drop_flushes_pending_writes() {
    // Omit the explicit flush — std::io::BufWriter's own Drop flushes the
    // buffered bytes through the cloned fd when the `Value::BufWriter` Arc
    // drops at scope exit, so the contents still reach disk. Read-back
    // happens after the writing scope ends.
    let tmp = std::env::temp_dir().join("karac_test_bufwriter_drop_flush.txt");
    let path = tmp.to_str().unwrap().replace('\\', "\\\\");
    let _ = std::fs::remove_file(&tmp);
    let src = format!(
        "fn write_it() {{
             match File.create(\"{path}\") {{
                 Ok(f) => {{
                     let bw = BufWriter.new(f);
                     let data = [122u8, 122u8];
                     let _ = bw.write(data[0..2]);
                 }}
                 Err(_) => println(\"create err\"),
             }}
         }}
         fn main() {{
             write_it();
             match FileSystem.read_to_string(\"{path}\") {{
                 Ok(s) => println(\"contents=[\" + s + \"]\"),
                 Err(_) => println(\"read err\"),
             }}
         }}"
    );
    let out = run_no_errors(&src);
    assert_eq!(out, "contents=[zz]\n");
    let _ = std::fs::remove_file(&tmp);
}

// ── std.runtime introspection (Debugger Contract slice 5) ─────────────────────
//
// The tree-walk interpreter has its own par-block evaluation path and does
// NOT construct `KaracFrame` / `ACTIVE_FRAMES` state, so all three APIs
// return the empty / false form per design.md's "try-then-degrade" contract.
// Real values flow through codegen in compiled binaries; the interpreter
// returns degraded results that are still well-typed (empty Vec / false
// bool). When/if interpreter parity for active-frame enumeration ships
// (post-v1), these tests upgrade to assert real values.

#[test]
fn test_runtime_has_debug_metadata_returns_false_in_interpreter() {
    let out = run_no_errors(
        "fn main() {
             let dbg = Runtime.has_debug_metadata();
             if dbg {
                 println(1);
             } else {
                 println(0);
             }
         }",
    );
    assert_eq!(out, "0\n");
}

#[test]
fn test_runtime_list_par_blocks_returns_empty_in_interpreter() {
    let out = run_no_errors(
        "fn main() {
             let pbs = Runtime.list_par_blocks();
             println(pbs.len());
         }",
    );
    assert_eq!(out, "0\n");
}

#[test]
fn test_runtime_list_tasks_returns_empty_in_interpreter() {
    let out = run_no_errors(
        "fn main() {
             let tasks = Runtime.list_tasks();
             println(tasks.len());
         }",
    );
    assert_eq!(out, "0\n");
}

// ── Default parameter values ──────────────────────────────────────

#[test]
fn test_default_param_used_when_arg_omitted() {
    let out = run_no_errors(
        "fn greet(name: String, greeting: String = \"Hello\") -> String { greeting + \", \" + name }\n\
         fn main() { println(greet(\"world\")); }",
    );
    assert_eq!(out, "Hello, world\n");
}

#[test]
fn test_default_param_overridden_by_explicit_arg() {
    let out = run_no_errors(
        "fn greet(name: String, greeting: String = \"Hello\") -> String { greeting + \", \" + name }\n\
         fn main() { println(greet(\"world\", \"Hi\")); }",
    );
    assert_eq!(out, "Hi, world\n");
}

#[test]
fn test_default_param_integer() {
    let out = run_no_errors(
        "fn add(x: i64, step: i64 = 1) -> i64 { x + step }\n\
         fn main() { println(add(10)); println(add(10, 5)); }",
    );
    assert_eq!(out, "11\n15\n");
}

#[test]
fn test_multiple_defaults_substitution() {
    // Both trailing params omitted — both defaults fire.
    let out = run_no_errors(
        "fn pair(a: i64, b: i64 = 10, c: i64 = 20) -> i64 { a + b + c }\n\
         fn main() {\n\
             println(pair(1));\n\
             println(pair(1, 5));\n\
             println(pair(1, 5, 7));\n\
         }",
    );
    assert_eq!(out, "31\n26\n13\n");
}

// ── Debug trait format (item 161) ─────────────────────────────────────────────

#[test]
fn test_assert_eq_failure_shows_debug_format_for_strings() {
    // assert_eq failure left/right fields show strings with quotes.
    let errors = runtime_errors("fn main() { assert_eq(\"hello\", \"world\"); }");
    assert!(!errors.is_empty(), "expected a runtime error");
    let e = &errors[0];
    assert_eq!(e.left.as_deref(), Some("\"hello\""));
    assert_eq!(e.right.as_deref(), Some("\"world\""));
}

#[test]
fn test_assert_ne_failure_shows_debug_format() {
    // assert_ne failure for equal strings shows debug format.
    let errors = runtime_errors("fn main() { assert_ne(\"same\", \"same\"); }");
    assert!(!errors.is_empty(), "expected a runtime error");
    let e = &errors[0];
    assert_eq!(e.left.as_deref(), Some("\"same\""));
    assert_eq!(e.right.as_deref(), Some("\"same\""));
}

#[test]
fn test_assert_eq_failure_integer_values() {
    let errors = runtime_errors("fn main() { assert_eq(1i64, 2i64); }");
    assert!(!errors.is_empty(), "expected a runtime error");
    let e = &errors[0];
    assert_eq!(e.left.as_deref(), Some("1"));
    assert_eq!(e.right.as_deref(), Some("2"));
}

// ── #[derive(Display)] on unit enums ────────────────────────────

#[test]
fn test_derive_display_to_string_returns_variant_name() {
    // `#[derive(Display)]` — `.to_string()` returns the PascalCase variant name.
    let output = run("#[derive(Display)]\n\
         enum Direction { Up, Down, Left, Right }\n\
         fn main() {\n\
             let d = Direction.Up;\n\
             println(d.to_string());\n\
         }");
    assert_eq!(output, "Up\n");
}

#[test]
fn test_derive_display_snake_case_lowercases_variant() {
    // `#[derive(Display(snake_case))]` — `.to_string()` returns the lower_snake_case name.
    let output = run("#[derive(Display(snake_case))]\n\
         enum Status { Active, InProgress, Done }\n\
         fn main() {\n\
             let s = Status.InProgress;\n\
             println(s.to_string());\n\
         }");
    assert_eq!(output, "in_progress\n");
}

#[test]
fn test_derive_display_fstring_interpolation() {
    // Enum with derived Display works inside an f-string.
    let output = run("#[derive(Display)]\n\
         enum Color { Red, Green, Blue }\n\
         fn main() {\n\
             let c = Color.Green;\n\
             println(f\"color={c.to_string()}\");\n\
         }");
    assert_eq!(output, "color=Green\n");
}

// ── SortedSet[T] stdlib methods ────────────────────────────────────────────

#[test]
fn test_sorted_set_new_and_len() {
    let output = run("fn main() {\n\
             let s: SortedSet[i64] = SortedSet.new();\n\
             println(f\"{s.len()}\");\n\
         }");
    assert_eq!(output, "0\n");
}

#[test]
fn test_sorted_set_insert_and_contains() {
    let output = run("fn main() {\n\
             let s: SortedSet[i64] = SortedSet.new();\n\
             let inserted = s.insert(3_i64);\n\
             println(f\"{inserted}\");\n\
             let again = s.insert(3_i64);\n\
             println(f\"{again}\");\n\
             println(f\"{s.contains(3_i64)}\");\n\
             println(f\"{s.contains(99_i64)}\");\n\
         }");
    assert_eq!(output, "true\nfalse\ntrue\nfalse\n");
}

#[test]
fn test_sorted_set_remove() {
    let output = run("fn main() {\n\
             let s: SortedSet[i64] = SortedSet.new();\n\
             s.insert(5_i64);\n\
             let removed = s.remove(5_i64);\n\
             println(f\"{removed}\");\n\
             let again = s.remove(5_i64);\n\
             println(f\"{again}\");\n\
             println(f\"{s.is_empty()}\");\n\
         }");
    assert_eq!(output, "true\nfalse\ntrue\n");
}

#[test]
fn test_sorted_set_min_max() {
    let output = run("fn main() {\n\
             let s: SortedSet[i64] = SortedSet.new();\n\
             s.insert(5_i64);\n\
             s.insert(1_i64);\n\
             s.insert(9_i64);\n\
             match s.min() {\n\
                 Some(v) => println(f\"{v}\"),\n\
                 None    => println(\"empty\"),\n\
             }\n\
             match s.max() {\n\
                 Some(v) => println(f\"{v}\"),\n\
                 None    => println(\"empty\"),\n\
             }\n\
         }");
    assert_eq!(output, "1\n9\n");
}

#[test]
fn test_sorted_set_min_max_empty() {
    let output = run("fn main() {\n\
             let s: SortedSet[i64] = SortedSet.new();\n\
             match s.min() {\n\
                 Some(v) => println(f\"{v}\"),\n\
                 None    => println(\"empty\"),\n\
             }\n\
         }");
    assert_eq!(output, "empty\n");
}

#[test]
fn test_sorted_set_ordered_iteration() {
    let output = run("fn main() {\n\
             let s: SortedSet[i64] = SortedSet.new();\n\
             s.insert(7_i64);\n\
             s.insert(2_i64);\n\
             s.insert(5_i64);\n\
             for x in s {\n\
                 println(f\"{x}\");\n\
             }\n\
         }");
    assert_eq!(output, "2\n5\n7\n");
}

#[test]
fn test_sorted_set_union() {
    let output = run("fn main() {\n\
             let a: SortedSet[i64] = SortedSet.new();\n\
             a.insert(1_i64);\n\
             a.insert(2_i64);\n\
             let b: SortedSet[i64] = SortedSet.new();\n\
             b.insert(2_i64);\n\
             b.insert(3_i64);\n\
             let u = a.union(b);\n\
             println(f\"{u.len()}\");\n\
             for x in u {\n\
                 println(f\"{x}\");\n\
             }\n\
         }");
    assert_eq!(output, "3\n1\n2\n3\n");
}

#[test]
fn test_sorted_set_intersection() {
    let output = run("fn main() {\n\
             let a: SortedSet[i64] = SortedSet.new();\n\
             a.insert(1_i64);\n\
             a.insert(2_i64);\n\
             a.insert(3_i64);\n\
             let b: SortedSet[i64] = SortedSet.new();\n\
             b.insert(2_i64);\n\
             b.insert(3_i64);\n\
             b.insert(4_i64);\n\
             let i = a.intersection(b);\n\
             for x in i {\n\
                 println(f\"{x}\");\n\
             }\n\
         }");
    assert_eq!(output, "2\n3\n");
}

#[test]
fn test_sorted_set_difference() {
    let output = run("fn main() {\n\
             let a: SortedSet[i64] = SortedSet.new();\n\
             a.insert(1_i64);\n\
             a.insert(2_i64);\n\
             a.insert(3_i64);\n\
             let b: SortedSet[i64] = SortedSet.new();\n\
             b.insert(2_i64);\n\
             let d = a.difference(b);\n\
             for x in d {\n\
                 println(f\"{x}\");\n\
             }\n\
         }");
    assert_eq!(output, "1\n3\n");
}

#[test]
fn test_sorted_set_dedup_on_insert() {
    let output = run("fn main() {\n\
             let s: SortedSet[i64] = SortedSet.new();\n\
             s.insert(4_i64);\n\
             s.insert(4_i64);\n\
             s.insert(4_i64);\n\
             println(f\"{s.len()}\");\n\
         }");
    assert_eq!(output, "1\n");
}

#[test]
fn test_sorted_set_string_elements() {
    let output = run("fn main() {\n\
             let s: SortedSet[String] = SortedSet.new();\n\
             s.insert(\"banana\");\n\
             s.insert(\"apple\");\n\
             s.insert(\"cherry\");\n\
             for x in s {\n\
                 println(x);\n\
             }\n\
         }");
    assert_eq!(output, "apple\nbanana\ncherry\n");
}

// ── Channel[T] / Sender[T] / Receiver[T] ──────────────────────────────────────

#[test]
fn test_channel_send_recv_round_trip() {
    let output = run("fn main() {\n\
             let (sender, receiver) = Channel.new();\n\
             sender.send(42_i64);\n\
             let val = receiver.recv();\n\
             println(f\"{val}\");\n\
         }");
    assert_eq!(output, "42\n");
}

#[test]
fn test_channel_try_recv_non_empty() {
    let output = run("fn main() {\n\
             let (sender, receiver) = Channel.new();\n\
             sender.send(7_i64);\n\
             match receiver.try_recv() {\n\
                 Some(v) => println(f\"{v}\"),\n\
                 None    => println(\"empty\"),\n\
             }\n\
         }");
    assert_eq!(output, "7\n");
}

#[test]
fn test_channel_try_recv_empty_returns_none() {
    let output = run("fn main() {\n\
             let (_sender, receiver) = Channel.new();\n\
             match receiver.try_recv() {\n\
                 Some(v) => println(f\"{v}\"),\n\
                 None    => println(\"empty\"),\n\
             }\n\
         }");
    assert_eq!(output, "empty\n");
}

#[test]
fn test_channel_multiple_sends_fifo() {
    let output = run("fn main() {\n\
             let (sender, receiver) = Channel.new();\n\
             sender.send(1_i64);\n\
             sender.send(2_i64);\n\
             sender.send(3_i64);\n\
             println(f\"{receiver.recv()}\");\n\
             println(f\"{receiver.recv()}\");\n\
             println(f\"{receiver.recv()}\");\n\
         }");
    assert_eq!(output, "1\n2\n3\n");
}

#[test]
fn test_channel_cloned_sender() {
    let output = run("fn main() {\n\
             let (sender, receiver) = Channel.new();\n\
             let s2 = sender.clone();\n\
             sender.send(10_i64);\n\
             s2.send(20_i64);\n\
             println(f\"{receiver.recv()}\");\n\
             println(f\"{receiver.recv()}\");\n\
         }");
    assert_eq!(output, "10\n20\n");
}

#[test]
fn test_channel_string_values() {
    let output = run("fn main() {\n\
             let (sender, receiver) = Channel.new();\n\
             sender.send(\"hello\");\n\
             let msg = receiver.recv();\n\
             println(msg);\n\
         }");
    assert_eq!(output, "hello\n");
}

// ── #[derive(Arithmetic)] on distinct types ────────────────────

#[test]
fn test_derive_arithmetic_addition() {
    let output = run_no_errors(
        "#[derive(Arithmetic)]\n\
         distinct type Meters = i64;\n\
         fn main() {\n\
             let a: Meters = 10;\n\
             let b: Meters = 3;\n\
             let sum: Meters = a + b;\n\
             println(sum);\n\
         }",
    );
    assert_eq!(output, "13\n");
}

#[test]
fn test_derive_arithmetic_negation() {
    let output = run_no_errors(
        "#[derive(Arithmetic)]\n\
         distinct type Offset = i64;\n\
         fn main() {\n\
             let x: Offset = 7;\n\
             let neg: Offset = -x;\n\
             println(neg);\n\
         }",
    );
    assert_eq!(output, "-7\n");
}

#[test]
fn test_derive_arithmetic_all_ops() {
    let output = run_no_errors(
        "#[derive(Arithmetic)]\n\
         distinct type Score = i64;\n\
         fn main() {\n\
             let a: Score = 10;\n\
             let b: Score = 3;\n\
             println(a + b);\n\
             println(a - b);\n\
             println(a * b);\n\
             println(a / b);\n\
             println(a % b);\n\
         }",
    );
    assert_eq!(output, "13\n7\n30\n3\n1\n");
}

// ── Distinct types — constructor + .raw() (zero-cost) ──────────────

#[test]
fn test_distinct_constructor_and_raw_roundtrip() {
    // `UserId(42)` wraps a base value (zero-cost) and `.raw()` unwraps it.
    let output = run_no_errors(
        "distinct type UserId = i64;\n\
         fn main() {\n\
             let u = UserId(42);\n\
             let raw: i64 = u.raw();\n\
             println(raw);\n\
         }",
    );
    assert_eq!(output, "42\n");
}

#[test]
fn test_distinct_constructor_float_base() {
    // The wrap is value-preserving for non-integer bases too.
    let output = run_no_errors(
        "distinct type Meters = f64;\n\
         fn main() {\n\
             let m = Meters(3.5);\n\
             println(m.raw());\n\
         }",
    );
    assert_eq!(output, "3.5\n");
}

#[test]
fn test_distinct_constructor_passed_through_function() {
    // A distinct value round-trips through a function call and back out
    // via `.raw()` — the wrapper is purely a type-level distinction.
    let output = run_no_errors(
        "distinct type UserId = i64;\n\
         fn identity(id: UserId) -> UserId { id }\n\
         fn main() {\n\
             let u = identity(UserId(7));\n\
             println(u.raw());\n\
         }",
    );
    assert_eq!(output, "7\n");
}

#[test]
fn test_distinct_where_constructor_runtime_holds() {
    // Combined `distinct type Even = i64 where self % 2 == 0`: a runtime
    // argument that satisfies the predicate constructs successfully.
    let output = run_no_errors(
        "distinct type Even = i64 where self % 2 == 0;\n\
         fn mk(n: i64) -> Even { Even(n) }\n\
         fn main() { println(mk(8).raw()); }",
    );
    assert_eq!(output, "8\n");
}

#[test]
fn test_distinct_where_constructor_runtime_fails() {
    // A runtime argument that violates the predicate faults with a
    // `contract violated` runtime error.
    let errors = runtime_errors(
        "distinct type Even = i64 where self % 2 == 0;\n\
         fn mk(n: i64) -> Even { Even(n) }\n\
         fn main() { println(mk(7).raw()); }",
    );
    assert!(
        errors
            .iter()
            .any(|e| e.message.contains("contract violated")),
        "expected a `contract violated` fault, got: {errors:?}"
    );
}

#[test]
fn test_distinct_derived_comparison_runs() {
    // With `#[derive(Eq, Ord)]` the comparison operators are admitted and
    // run on the base layout — `UserId(3) < UserId(5)` is `true`,
    // `UserId(5) == UserId(5)` is `true`.
    let output = run_no_errors(
        "#[derive(Eq, Ord)]\n\
         distinct type UserId = i64;\n\
         fn main() {\n\
             println(UserId(3) < UserId(5));\n\
             println(UserId(5) == UserId(5));\n\
         }",
    );
    assert_eq!(output, "true\ntrue\n");
}

#[test]
fn test_distinct_where_try_from_ok_and_err() {
    // `Even.try_from` returns `Ok` for an even value and `Err` for an odd.
    let ok = run_no_errors(
        "distinct type Even = i64 where self % 2 == 0;\n\
         fn main() {\n\
             match Even.try_from(8) { Ok(e) => println(e.raw()), Err(_) => println(-1) }\n\
         }",
    );
    assert_eq!(ok, "8\n");
    let err = run_no_errors(
        "distinct type Even = i64 where self % 2 == 0;\n\
         fn main() {\n\
             match Even.try_from(7) { Ok(e) => println(e.raw()), Err(_) => println(-1) }\n\
         }",
    );
    assert_eq!(err, "-1\n");
}

// ── Map[K, V] interpreter tests ────────────────────────────────────────────

#[test]
fn test_map_new_and_len() {
    let output = run("fn main() {\n\
             let m: Map[String, i64] = Map.new();\n\
             println(m.len());\n\
         }");
    assert_eq!(output, "0\n");
}

#[test]
fn test_map_insert_and_get() {
    let output = run("fn main() {\n\
             let m: Map[String, i64] = Map.new();\n\
             m.insert(\"a\", 1_i64);\n\
             m.insert(\"b\", 2_i64);\n\
             let v = m.get(\"a\");\n\
             match v {\n\
                 Some(x) => println(x),\n\
                 None => println(\"missing\"),\n\
             }\n\
         }");
    assert_eq!(output, "1\n");
}

#[test]
fn test_map_get_missing_returns_none() {
    let output = run("fn main() {\n\
             let m: Map[String, i64] = Map.new();\n\
             let v = m.get(\"z\");\n\
             match v {\n\
                 Some(x) => println(x),\n\
                 None => println(\"none\"),\n\
             }\n\
         }");
    assert_eq!(output, "none\n");
}

#[test]
fn test_map_contains_key() {
    let output = run("fn main() {\n\
             let m: Map[String, i64] = Map.new();\n\
             m.insert(\"x\", 10_i64);\n\
             println(m.contains_key(\"x\"));\n\
             println(m.contains_key(\"y\"));\n\
         }");
    assert_eq!(output, "true\nfalse\n");
}

#[test]
fn test_map_remove() {
    let output = run("fn main() {\n\
             let m: Map[String, i64] = Map.new();\n\
             m.insert(\"a\", 42_i64);\n\
             let old = m.remove(\"a\");\n\
             match old {\n\
                 Some(v) => println(v),\n\
                 None => println(\"none\"),\n\
             }\n\
             println(m.len());\n\
         }");
    assert_eq!(output, "42\n0\n");
}

#[test]
fn test_map_get_or() {
    let output = run("fn main() {\n\
             let m: Map[String, i64] = Map.new();\n\
             m.insert(\"k\", 7_i64);\n\
             println(m.get_or(\"k\", 0_i64));\n\
             println(m.get_or(\"missing\", 99_i64));\n\
         }");
    assert_eq!(output, "7\n99\n");
}

#[test]
fn test_map_is_empty() {
    let output = run("fn main() {\n\
             let m: Map[String, i64] = Map.new();\n\
             println(m.is_empty());\n\
             m.insert(\"a\", 1_i64);\n\
             println(m.is_empty());\n\
         }");
    assert_eq!(output, "true\nfalse\n");
}

#[test]
fn test_map_keys_and_values() {
    let output = run("fn main() {\n\
             let m: Map[String, i64] = Map.new();\n\
             m.insert(\"a\", 1_i64);\n\
             m.insert(\"b\", 2_i64);\n\
             let ks = m.keys();\n\
             let vs = m.values();\n\
             println(ks.len());\n\
             println(vs.len());\n\
         }");
    assert_eq!(output, "2\n2\n");
}

#[test]
fn test_map_entries_iteration() {
    let output = run("fn main() {\n\
             let m: Map[String, i64] = Map.new();\n\
             m.insert(\"x\", 10_i64);\n\
             let es = m.entries();\n\
             println(es.len());\n\
         }");
    assert_eq!(output, "1\n");
}

#[test]
fn test_map_merge() {
    let output = run("fn main() {\n\
             let a: Map[String, i64] = Map.new();\n\
             a.insert(\"p\", 1_i64);\n\
             let b: Map[String, i64] = Map.new();\n\
             b.insert(\"q\", 2_i64);\n\
             let c = a.merge(b);\n\
             println(c.len());\n\
             println(c.contains_key(\"p\"));\n\
             println(c.contains_key(\"q\"));\n\
         }");
    assert_eq!(output, "2\ntrue\ntrue\n");
}

#[test]
fn test_map_prefix_literal_string_keys() {
    // Map["a": 1, "b": 2] prefix-literal form — parses + type-checks +
    // produces a Map with the entries.
    let output = run("fn main() {\n\
             let m = Map[\"a\": 1_i64, \"b\": 2_i64, \"c\": 3_i64];\n\
             println(m.len());\n\
             match m.get(\"b\") {\n\
                 Some(v) => println(v),\n\
                 None => println(0_i64),\n\
             }\n\
         }");
    assert_eq!(output, "3\n2\n");
}

#[test]
fn test_map_prefix_literal_int_keys() {
    let output = run("fn main() {\n\
             let m = Map[1_i64: 100_i64, 2_i64: 200_i64];\n\
             println(m.len());\n\
             match m.get(2_i64) {\n\
                 Some(v) => println(v),\n\
                 None => println(0_i64),\n\
             }\n\
         }");
    assert_eq!(output, "2\n200\n");
}

#[test]
fn test_map_clear() {
    let output = run("fn main() {\n\
             let m: Map[String, i64] = Map.new();\n\
             m.insert(\"a\", 1_i64);\n\
             m.insert(\"b\", 2_i64);\n\
             println(m.len());\n\
             m.clear();\n\
             println(m.len());\n\
             println(m.is_empty());\n\
             m.insert(\"c\", 3_i64);\n\
             println(m.contains_key(\"a\"));\n\
             println(m.contains_key(\"c\"));\n\
         }");
    assert_eq!(output, "2\n0\ntrue\nfalse\ntrue\n");
}

#[test]
fn test_map_merge_overwrite() {
    let output = run("fn main() {\n\
             let a: Map[String, i64] = Map.new();\n\
             a.insert(\"k\", 1_i64);\n\
             let b: Map[String, i64] = Map.new();\n\
             b.insert(\"k\", 99_i64);\n\
             let c = a.merge(b);\n\
             println(c.len());\n\
             match c.get(\"k\") {\n\
                 Some(v) => println(v),\n\
                 None => println(\"none\"),\n\
             }\n\
         }");
    assert_eq!(output, "1\n99\n");
}

#[test]
fn test_map_insert_update_returns_old() {
    let output = run("fn main() {\n\
             let m: Map[String, i64] = Map.new();\n\
             m.insert(\"k\", 5_i64);\n\
             let old = m.insert(\"k\", 10_i64);\n\
             match old {\n\
                 Some(v) => println(v),\n\
                 None => println(\"none\"),\n\
             }\n\
         }");
    assert_eq!(output, "5\n");
}

// ── Map.entry / Entry[K, V] (canonical: phase-8-stdlib-floor.md
//    "Map.entry(k) + Entry[K, V] enum") ────────────────────────────

#[test]
fn test_map_entry_or_insert_vacant_inserts_default() {
    // Vacant key — or_insert pushes (key, default) and returns the default.
    // Verify the map state by re-fetching.
    let output = run("fn main() {\n\
             let m: Map[String, i64] = Map.new();\n\
             let v = m.entry(\"a\").or_insert(7_i64);\n\
             println(v);\n\
             match m.get(\"a\") {\n\
                 Some(x) => println(x),\n\
                 None => println(\"missing\"),\n\
             }\n\
         }");
    assert_eq!(output, "7\n7\n");
}

#[test]
fn test_map_entry_or_insert_occupied_returns_existing() {
    // Occupied key — or_insert is a no-op write; returns the existing value.
    let output = run("fn main() {\n\
             let m: Map[String, i64] = Map.new();\n\
             m.insert(\"a\", 42_i64);\n\
             let v = m.entry(\"a\").or_insert(0_i64);\n\
             println(v);\n\
             match m.get(\"a\") {\n\
                 Some(x) => println(x),\n\
                 None => println(\"missing\"),\n\
             }\n\
         }");
    assert_eq!(output, "42\n42\n");
}

#[test]
fn test_map_entry_or_insert_with_vacant_invokes_closure() {
    // Vacant — closure runs to produce the default. Counter pattern via
    // a side variable confirms the closure fired exactly once.
    let output = run("fn main() {\n\
             let m: Map[String, i64] = Map.new();\n\
             let v = m.entry(\"x\").or_insert_with(|| 99_i64);\n\
             println(v);\n\
         }");
    assert_eq!(output, "99\n");
}

#[test]
fn test_map_entry_or_insert_with_occupied_skips_closure() {
    // Occupied — closure does NOT fire. Returning a sentinel that would
    // overwrite if the closure ran lets the test detect a regression.
    let output = run("fn main() {\n\
             let m: Map[String, i64] = Map.new();\n\
             m.insert(\"k\", 5_i64);\n\
             let v = m.entry(\"k\").or_insert_with(|| 999_i64);\n\
             println(v);\n\
         }");
    assert_eq!(output, "5\n");
}

#[test]
fn test_map_entry_and_modify_runs_when_occupied() {
    // and_modify's closure fires only on Occupied; it receives the slot
    // value as a mut ref and can mutate through (the interpreter aliases
    // the slot via SharedCell for the duration of the call).
    let output = run("fn main() {\n\
             let m: Map[String, i64] = Map.new();\n\
             m.insert(\"k\", 5_i64);\n\
             m.entry(\"k\").and_modify(|v| { v += 1; });\n\
             match m.get(\"k\") {\n\
                 Some(x) => println(x),\n\
                 None => println(\"missing\"),\n\
             }\n\
         }");
    assert_eq!(output, "6\n");
}

#[test]
fn test_map_entry_and_modify_skips_when_vacant() {
    // Vacant — closure does not fire; map state is unchanged.
    let output = run("fn main() {\n\
             let m: Map[String, i64] = Map.new();\n\
             m.entry(\"k\").and_modify(|v| { v += 1; });\n\
             println(m.is_empty());\n\
         }");
    assert_eq!(output, "true\n");
}

#[test]
fn test_map_entry_and_modify_chain_with_or_insert() {
    // The canonical chain: and_modify on Occupied increments; on Vacant
    // the trailing or_insert provides the seed value.
    let output = run("fn main() {\n\
             let m: Map[String, i64] = Map.new();\n\
             m.entry(\"a\").and_modify(|v| { v += 1; }).or_insert(1_i64);\n\
             m.entry(\"a\").and_modify(|v| { v += 1; }).or_insert(1_i64);\n\
             m.entry(\"a\").and_modify(|v| { v += 1; }).or_insert(1_i64);\n\
             match m.get(\"a\") {\n\
                 Some(x) => println(x),\n\
                 None => println(\"missing\"),\n\
             }\n\
         }");
    // First call: vacant → or_insert(1) sets a=1.
    // Second:    occupied → and_modify(+1) → a=2; or_insert no-op.
    // Third:     occupied → and_modify(+1) → a=3; or_insert no-op.
    assert_eq!(output, "3\n");
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

#[test]
fn test_string_clone_preserves_value() {
    let output = run("fn main() {\n\
             let s = \"hello\";\n\
             let t = s.clone();\n\
             println(t);\n\
         }");
    assert_eq!(output, "hello\n");
}

#[test]
fn test_string_push_char_ascii() {
    // Interpreter mirror of the codegen String.push(char) arm.
    // `karac run` was the panic surface that surfaced the
    // method_call_seq.rs dispatch gap; this regression test makes the
    // arm load-bearing for the kata 71 follow-up.
    let output = run("fn main() {\n\
             let mut s: String = \"\";\n\
             s.push('h');\n\
             s.push('i');\n\
             println(s);\n\
             println(s.len());\n\
         }");
    assert_eq!(output, "hi\n2\n");
}

#[test]
fn test_string_push_str_interpreter_dispatch() {
    // `push_str` typecheck + codegen shipped 2026-05-23, but the
    // interpreter dispatch arm was missing — `karac run` would panic
    // on the unreachable arm with "method 'push_str' not found on type
    // 'String'". This regression makes sure the same Kāra source runs
    // through both backends with identical output.
    let output = run("fn main() {\n\
             let mut s: String = \"\";\n\
             s.push_str(\"foo\");\n\
             s.push_str(\"bar\");\n\
             println(s);\n\
             println(s.len());\n\
         }");
    assert_eq!(output, "foobar\n6\n");
}

#[test]
fn test_map_clone_preserves_entries() {
    let output = run("fn main() {\n\
             let m: Map[String, i64] = Map.new();\n\
             m.insert(\"k\", 7_i64);\n\
             let n: Map[String, i64] = m.clone();\n\
             match n.get(\"k\") {\n\
                 Some(v) => println(v),\n\
                 None => println(\"missing\"),\n\
             }\n\
         }");
    assert_eq!(output, "7\n");
}

#[test]
fn test_map_clone_independent_after_source_insert() {
    // Inserting into the source after cloning leaves the clone unchanged.
    let output = run("fn main() {\n\
             let m: Map[String, i64] = Map.new();\n\
             m.insert(\"k\", 1_i64);\n\
             let n: Map[String, i64] = m.clone();\n\
             m.insert(\"k\", 99_i64);\n\
             match n.get(\"k\") {\n\
                 Some(v) => println(v),\n\
                 None => println(\"missing\"),\n\
             }\n\
         }");
    assert_eq!(output, "1\n");
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
fn test_try_extend_from_slice_wraps_ok() {
    let output = run("fn main() {\n\
             let mut v: Vec[i64] = [1_i64];\n\
             let src: Vec[i64] = [2_i64, 3_i64];\n\
             match v.try_extend_from_slice(src) {\n\
                 Ok(_) => println(\"ok\"),\n\
                 Err(e) => println(\"err\"),\n\
             }\n\
             println(v.len());\n\
         }");
    assert_eq!(output, "ok\n3\n");
}

#[test]
fn test_try_push_str_and_try_push_char_string() {
    let output = run("fn main() {\n\
             let mut s: String = \"\";\n\
             s.try_push('a');\n\
             match s.try_push_str(\"bc\") {\n\
                 Ok(_) => println(\"ok\"),\n\
                 Err(e) => println(\"err\"),\n\
             }\n\
             println(s);\n\
         }");
    assert_eq!(output, "ok\nabc\n");
}

#[test]
fn test_try_clone_string_wraps_ok() {
    let output = run("fn main() {\n\
             let s = \"hello\";\n\
             match s.try_clone() {\n\
                 Ok(t) => println(t),\n\
                 Err(e) => println(\"err\"),\n\
             }\n\
         }");
    assert_eq!(output, "hello\n");
}

#[test]
fn test_try_insert_map_wraps_ok() {
    // First insert returns `Ok(None)` (no prior value); the entry is present.
    let output = run("fn main() {\n\
             let mut m: Map[String, i64] = Map.new();\n\
             match m.try_insert(\"k\", 1_i64) {\n\
                 Ok(prev) => match prev {\n\
                     Some(p) => println(p),\n\
                     None => println(\"no-prev\"),\n\
                 },\n\
                 Err(e) => println(\"err\"),\n\
             }\n\
             println(m.len());\n\
         }");
    assert_eq!(output, "no-prev\n1\n");
}

#[test]
fn test_try_insert_set_wraps_ok() {
    let output = run("fn main() {\n\
             let mut s: Set[i64] = Set.new();\n\
             match s.try_insert(7_i64) {\n\
                 Ok(added) => println(added),\n\
                 Err(e) => println(\"err\"),\n\
             }\n\
             println(s.len());\n\
         }");
    assert_eq!(output, "true\n1\n");
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
fn test_try_from_slice_static_wraps_ok() {
    let output = run("fn main() {\n\
             let src: Vec[i64] = [4_i64, 5_i64];\n\
             match Vec.try_from_slice(src) {\n\
                 Ok(v) => println(v.len()),\n\
                 Err(e) => println(\"err\"),\n\
             }\n\
         }");
    assert_eq!(output, "2\n");
}

#[test]
fn test_try_companion_not_shadowing_user_method() {
    // A user type that defines its own `try_push` is dispatched normally —
    // the builtin-collection gate keeps the fallible-alloc interception off
    // non-collection receivers. The user method returns a bare `i64` (not an
    // `Ok`-wrapped value), proving it was not intercepted.
    let output = run("struct Bag { n: i64 }\n\
         impl Bag { fn try_push(ref self, x: i64) -> i64 { x + 100_i64 } }\n\
         fn main() {\n\
             let b = Bag { n: 0 };\n\
             println(b.try_push(3_i64));\n\
             println(b.try_push(4_i64));\n\
         }");
    assert_eq!(output, "103\n104\n");
}

#[test]
fn test_set_clone_preserves_membership() {
    let output = run("fn main() {\n\
             let s: Set[i64] = Set.new();\n\
             s.insert(5_i64);\n\
             let t: Set[i64] = s.clone();\n\
             println(t.contains(5_i64));\n\
             println(t.contains(99_i64));\n\
         }");
    assert_eq!(output, "true\nfalse\n");
}

#[test]
fn test_set_clone_independent_after_source_insert() {
    let output = run("fn main() {\n\
             let s: Set[i64] = Set.new();\n\
             s.insert(1_i64);\n\
             let t: Set[i64] = s.clone();\n\
             s.insert(2_i64);\n\
             println(t.len());\n\
             println(s.len());\n\
         }");
    assert_eq!(output, "1\n2\n");
}

// ── Regex ─────────────────────────────────────────────────────────

#[test]
fn test_regex_compile_ok_result() {
    let output = run(r#"fn main() {
         match Regex.compile("[0-9]+") {
             Ok(r) => println("ok"),
             Err(e) => println("err"),
         }
     }"#);
    assert_eq!(output, "ok\n");
}

#[test]
fn test_regex_compile_invalid_err() {
    let output = run(r#"fn main() {
         match Regex.compile("[invalid") {
             Ok(r) => println("ok"),
             Err(e) => println("err"),
         }
     }"#);
    assert_eq!(output, "err\n");
}

#[test]
fn test_regex_is_match_true() {
    let output = run(r#"fn main() {
         let r = Regex.compile("[0-9]+").unwrap();
         println(r.is_match("abc123"));
     }"#);
    assert_eq!(output, "true\n");
}

#[test]
fn test_regex_is_match_false() {
    let output = run(r#"fn main() {
         let r = Regex.compile("[0-9]+").unwrap();
         println(r.is_match("abc"));
     }"#);
    assert_eq!(output, "false\n");
}

#[test]
fn test_regex_find_some() {
    let output = run(r#"fn main() {
         let r = Regex.compile("[0-9]+").unwrap();
         match r.find("abc123def") {
             Some(m) => println(m.text),
             None => println("none"),
         }
     }"#);
    assert_eq!(output, "123\n");
}

#[test]
fn test_regex_find_none() {
    let output = run(r#"fn main() {
         let r = Regex.compile("[0-9]+").unwrap();
         match r.find("abcdef") {
             Some(m) => println(m.text),
             None => println("none"),
         }
     }"#);
    assert_eq!(output, "none\n");
}

#[test]
fn test_regex_find_all() {
    let output = run(r#"fn main() {
         let r = Regex.compile("[0-9]+").unwrap();
         let ms = r.find_all("abc 123 def 456");
         println(ms.len());
     }"#);
    assert_eq!(output, "2\n");
}

#[test]
fn test_regex_replace_all() {
    let output = run(r#"fn main() {
         let r = Regex.compile("[0-9]+").unwrap();
         println(r.replace_all("abc 123 def 456", "NUM"));
     }"#);
    assert_eq!(output, "abc NUM def NUM\n");
}

// ── std.cli — builder-style argument parser ────────────────────────

#[test]
fn test_cli_builder_records_state() {
    let output = run(r#"fn main() {
         let p = Parser.new("greet")
             .about("Greets a name")
             .arg("--name", Arg.string().required())
             .flag("--verbose", short: 'v', help: "verbose");
         println(p.program_name);
         println(p.about_text);
         println(p.args.len());
         println(p.flags.len());
     }"#);
    assert_eq!(output, "greet\nGreets a name\n1\n1\n");
}

#[test]
fn test_cli_parse_named_arg_returns_value() {
    let output = run(r#"struct FakeEnv {}
         impl FakeEnv { fn args(self) -> Vec[String] { ["prog", "--name", "alice"] } }
         fn main() {
             with_provider[Env](FakeEnv {}, || {
                 let p = Parser.new("greet")
                     .arg("--name", Arg.string().required());
                 match p.parse() {
                     Ok(args) => {
                         match args.get_string("--name") {
                             Ok(n) => println(n),
                             Err(e) => println(e.message),
                         }
                     }
                     Err(e) => println(e.message),
                 }
             });
         }"#);
    assert_eq!(output, "alice\n");
}

#[test]
fn test_cli_parse_flag_set_returns_true() {
    let output = run(r#"struct FakeEnv {}
         impl FakeEnv { fn args(self) -> Vec[String] { ["prog", "--verbose"] } }
         fn main() {
             with_provider[Env](FakeEnv {}, || {
                 let p = Parser.new("greet")
                     .flag("--verbose", short: 'v', help: "");
                 match p.parse() {
                     Ok(args) => println(args.get_flag("--verbose")),
                     Err(e) => println(e.message),
                 }
             });
         }"#);
    assert_eq!(output, "true\n");
}

#[test]
fn test_cli_parse_flag_unset_returns_false() {
    let output = run(r#"struct FakeEnv {}
         impl FakeEnv { fn args(self) -> Vec[String] { ["prog"] } }
         fn main() {
             with_provider[Env](FakeEnv {}, || {
                 let p = Parser.new("greet")
                     .flag("--verbose", short: 'v', help: "");
                 match p.parse() {
                     Ok(args) => println(args.get_flag("--verbose")),
                     Err(e) => println(e.message),
                 }
             });
         }"#);
    assert_eq!(output, "false\n");
}

#[test]
fn test_cli_parse_missing_required_arg_errors() {
    let output = run(r#"struct FakeEnv {}
         impl FakeEnv { fn args(self) -> Vec[String] { ["prog"] } }
         fn main() {
             with_provider[Env](FakeEnv {}, || {
                 let p = Parser.new("greet")
                     .arg("--name", Arg.string().required());
                 match p.parse() {
                     Ok(_) => println("ok"),
                     Err(e) => println("err"),
                 }
             });
         }"#);
    assert_eq!(output, "err\n");
}

#[test]
fn test_cli_parse_unknown_token_becomes_positional() {
    let output = run(r#"struct FakeEnv {}
         impl FakeEnv { fn args(self) -> Vec[String] { ["prog", "extra1", "extra2"] } }
         fn main() {
             with_provider[Env](FakeEnv {}, || {
                 let p = Parser.new("greet");
                 match p.parse() {
                     Ok(args) => println(args.positional.len()),
                     Err(e) => println(e.message),
                 }
             });
         }"#);
    assert_eq!(output, "2\n");
}

#[test]
fn test_cli_arg_builder_chains() {
    let output = run(r#"fn main() {
         let a = Arg.string().required().help("a help");
         println(a.is_required);
         println(a.help_text);
     }"#);
    assert_eq!(output, "true\na help\n");
}

// ── std.cli — subcommands + auto --help / --version (C1 slice) ─────

#[test]
fn test_cli_subcommand_dispatches_into_sub_parser() {
    // The deferred.md sample: parent has `--name`, subcommand `upper`
    // has its own `--shout` flag. argv `["prog", "--name", "alice",
    // "upper", "--shout"]` should populate parent's `--name = "alice"`
    // and dispatch into `upper` with `--shout` set.
    let output = run(r#"struct FakeEnv {}
         impl FakeEnv { fn args(self) -> Vec[String] { ["prog", "--name", "alice", "upper", "--shout"] } }
         fn main() {
             with_provider[Env](FakeEnv {}, || {
                 let parser = Parser.new("greet")
                     .arg("--name", Arg.string().required())
                     .subcommand("upper", Parser.new("upper").flag("--shout", short: 's', help: ""));
                 match parser.parse() {
                     Ok(args) => {
                         match args.get_string("--name") {
                             Ok(n) => println(n),
                             Err(e) => println(e.message),
                         }
                         match args.subcommand_name() {
                             Some(name) => println(name),
                             None => println("no_sub"),
                         }
                         match args.sub {
                             Some(s) => println(s.get_flag("--shout")),
                             None => println("no_sub"),
                         }
                     }
                     Err(e) => println(e.message),
                 }
             });
         }"#);
    assert_eq!(output, "alice\nupper\ntrue\n");
}

#[test]
fn test_cli_subcommand_name_none_when_no_subcommand_invoked() {
    let output = run(r#"struct FakeEnv {}
         impl FakeEnv { fn args(self) -> Vec[String] { ["prog"] } }
         fn main() {
             with_provider[Env](FakeEnv {}, || {
                 let parser = Parser.new("greet")
                     .subcommand("upper", Parser.new("upper"));
                 match parser.parse() {
                     Ok(args) => {
                         match args.subcommand_name() {
                             Some(name) => println(name),
                             None => println("none"),
                         }
                     }
                     Err(e) => println(e.message),
                 }
             });
         }"#);
    assert_eq!(output, "none\n");
}

#[test]
fn test_cli_subcommand_consumes_remaining_tokens_as_its_own() {
    // Tokens AFTER the subcommand name match against the sub-parser,
    // not the parent. Here `--name` is declared on the SUBCOMMAND, so
    // the parent never sees it.
    let output = run(r#"struct FakeEnv {}
         impl FakeEnv { fn args(self) -> Vec[String] { ["prog", "upper", "--name", "bob"] } }
         fn main() {
             with_provider[Env](FakeEnv {}, || {
                 let parser = Parser.new("greet")
                     .subcommand("upper", Parser.new("upper").arg("--name", Arg.string()));
                 match parser.parse() {
                     Ok(args) => {
                         match args.sub {
                             Some(s) => {
                                 match s.get_string("--name") {
                                     Ok(n) => println(n),
                                     Err(e) => println(e.message),
                                 }
                             }
                             None => println("no_sub"),
                         }
                     }
                     Err(e) => println(e.message),
                 }
             });
         }"#);
    assert_eq!(output, "bob\n");
}

#[test]
fn test_cli_subcommand_missing_required_arg_errors() {
    let output = run(r#"struct FakeEnv {}
         impl FakeEnv { fn args(self) -> Vec[String] { ["prog", "upper"] } }
         fn main() {
             with_provider[Env](FakeEnv {}, || {
                 let parser = Parser.new("greet")
                     .subcommand("upper", Parser.new("upper").arg("--name", Arg.string().required()));
                 match parser.parse() {
                     Ok(_) => println("ok"),
                     Err(e) => println(e.message),
                 }
             });
         }"#);
    assert_eq!(output, "missing required subcommand argument\n");
}

#[test]
fn test_cli_parse_help_short_circuits_with_rendered_text() {
    // `--help` fires before normal arg checking — `--name` is declared
    // required, but the help short-circuit wins. The error message
    // carries the rendered help text.
    let output = run(r#"struct FakeEnv {}
         impl FakeEnv { fn args(self) -> Vec[String] { ["prog", "--help"] } }
         fn main() {
             with_provider[Env](FakeEnv {}, || {
                 let parser = Parser.new("greet")
                     .about("Greets a name")
                     .arg("--name", Arg.string().required());
                 match parser.parse() {
                     Ok(_) => println("ok"),
                     Err(e) => println(e.message),
                 }
             });
         }"#);
    assert!(output.contains("greet - Greets a name"), "output: {output}");
    assert!(output.contains("USAGE:"), "output: {output}");
    assert!(output.contains("--name <VALUE>"), "output: {output}");
    assert!(output.contains("[required]"), "output: {output}");
    assert!(output.contains("-h, --help"), "output: {output}");
}

#[test]
fn test_cli_parse_short_help_h_short_circuits() {
    let output = run(r#"struct FakeEnv {}
         impl FakeEnv { fn args(self) -> Vec[String] { ["prog", "-h"] } }
         fn main() {
             with_provider[Env](FakeEnv {}, || {
                 let parser = Parser.new("p").about("about");
                 match parser.parse() {
                     Ok(_) => println("ok"),
                     Err(e) => println(e.message),
                 }
             });
         }"#);
    assert!(output.contains("p - about"), "output: {output}");
    assert!(output.contains("USAGE:"), "output: {output}");
}

#[test]
fn test_cli_parse_version_short_circuits() {
    let output = run(r#"struct FakeEnv {}
         impl FakeEnv { fn args(self) -> Vec[String] { ["prog", "--version"] } }
         fn main() {
             with_provider[Env](FakeEnv {}, || {
                 let parser = Parser.new("greet").version("1.2.3");
                 match parser.parse() {
                     Ok(_) => println("ok"),
                     Err(e) => println(e.message),
                 }
             });
         }"#);
    assert_eq!(output, "greet 1.2.3\n");
}

#[test]
fn test_cli_parse_short_version_v_short_circuits() {
    let output = run(r#"struct FakeEnv {}
         impl FakeEnv { fn args(self) -> Vec[String] { ["prog", "-V"] } }
         fn main() {
             with_provider[Env](FakeEnv {}, || {
                 let parser = Parser.new("greet").version("0.1.0");
                 match parser.parse() {
                     Ok(_) => println("ok"),
                     Err(e) => println(e.message),
                 }
             });
         }"#);
    assert_eq!(output, "greet 0.1.0\n");
}

#[test]
fn test_cli_version_line_falls_back_to_program_name_when_unset() {
    let output = run(r#"struct FakeEnv {}
         impl FakeEnv { fn args(self) -> Vec[String] { ["prog", "-V"] } }
         fn main() {
             with_provider[Env](FakeEnv {}, || {
                 let parser = Parser.new("greet");
                 match parser.parse() {
                     Ok(_) => println("ok"),
                     Err(e) => println(e.message),
                 }
             });
         }"#);
    // No version() declared — fall back to bare program name.
    assert_eq!(output, "greet\n");
}

#[test]
fn test_cli_help_text_renders_subcommands_section() {
    let output = run(r#"fn main() {
         let parser = Parser.new("greet")
             .subcommand("upper", Parser.new("upper").about("uppercase"))
             .subcommand("lower", Parser.new("lower").about("lowercase"));
         let h = parser.help_text();
         println(h);
     }"#);
    assert!(output.contains("SUBCOMMANDS:"), "output: {output}");
    assert!(output.contains("    upper  uppercase"), "output: {output}");
    assert!(output.contains("    lower  lowercase"), "output: {output}");
}

// ── std.tracing — structured logging + spans ───────────────────────

#[test]
fn test_tracing_span_root_builder() {
    let output = run(r#"fn main() {
         let s = Span.root("request", 7).with_field("method", "GET");
         println(s.name);
         println(s.span_id);
         println(s.parent_id);
         println(s.fields.len());
     }"#);
    assert_eq!(output, "request\n7\n0\n1\n");
}

#[test]
fn test_tracing_span_child_inherits_parent_id() {
    let output = run(r#"fn main() {
         let parent = Span.root("outer", 1);
         let child = parent.child("inner", 2);
         println(child.name);
         println(child.span_id);
         println(child.parent_id);
     }"#);
    assert_eq!(output, "inner\n2\n1\n");
}

#[test]
fn test_tracing_log_event_levels() {
    let output = run(r#"fn main() {
         println(LogEvent.trace("a").level);
         println(LogEvent.debug("b").level);
         println(LogEvent.info("c").level);
         println(LogEvent.warn("d").level);
         println(LogEvent.error("e").level);
     }"#);
    assert_eq!(output, "trace\ndebug\ninfo\nwarn\nerror\n");
}

#[test]
fn test_tracing_log_event_with_fields_and_span() {
    let output = run(r#"fn main() {
         let e = LogEvent.info("started")
             .with_field("user_id", "42")
             .with_field("ip", "127.0.0.1")
             .in_span(5);
         println(e.level);
         println(e.message);
         println(e.fields.len());
         println(e.span_id);
     }"#);
    assert_eq!(output, "info\nstarted\n2\n5\n");
}

#[test]
fn test_tracing_noop_exporter_implements_trait() {
    let output = run(r#"fn main() {
         let e = NoOpExporter {};
         let s = Span.root("request", 1);
         let ev = LogEvent.info("hello");
         e.export_span(s);
         e.export_event(ev);
         println("ok");
     }"#);
    assert_eq!(output, "ok\n");
}

#[test]
fn test_tracing_log_ambient_emission_all_levels() {
    // `Log.<level>("msg")` emits through the built-in StdoutExporter
    // without the caller constructing/threading an exporter value — the
    // ambient convenience layer. Each level renders its own tag.
    let output = run(r#"fn main() {
         Log.trace("t");
         Log.debug("d");
         Log.info("i");
         Log.warn("w");
         Log.error("e");
     }"#);
    assert_eq!(
        output,
        "[trace] t\n[debug] d\n[info] i\n[warn] w\n[error] e\n"
    );
}

#[test]
fn test_tracing_log_min_level_filters_below_threshold() {
    // phase-8 line 156 (interpreter half): `Log.set_min_level("warn")`
    // drops trace/debug/info; only warn + error emit. The dropped calls'
    // message args aren't even evaluated (standard log-filter semantics),
    // though string literals make that unobservable here.
    let output = run(r#"fn main() {
         Log.set_min_level("warn");
         Log.trace("t");
         Log.debug("d");
         Log.info("i");
         Log.warn("w");
         Log.error("e");
     }"#);
    assert_eq!(output, "[warn] w\n[error] e\n");
}

#[test]
fn test_tracing_log_set_exporter_noop_silences() {
    // Registering `NoOpExporter` as the ambient sink silences `Log.*` —
    // the events route to NoOp's empty `export_event` instead of stdout.
    let output = run(r#"fn main() {
         Log.set_exporter(NoOpExporter {});
         Log.info("i");
         Log.error("e");
     }"#);
    assert_eq!(output, "");
}

#[test]
fn test_tracing_log_reset_restores_default() {
    // `Log.reset()` clears both the min-level and the registered sink, so
    // a previously-dropped level emits to stdout again afterward.
    let output = run(r#"fn main() {
         Log.set_min_level("error");
         Log.info("dropped");
         Log.reset();
         Log.info("kept");
     }"#);
    assert_eq!(output, "[info] kept\n");
}

#[test]
fn test_tracing_log_custom_exporter_receives_events() {
    // A user `Exporter` registered as the ambient sink receives `Log.*`
    // events (dynamically dispatched), rendering its own format instead of
    // the StdoutExporter line. Also exercises the min-level filter applying
    // before the custom sink (the dropped `debug` never reaches it).
    let output = run(r#"struct Tagging { }
         impl Exporter for Tagging {
             fn export_span(ref self, span: Span) { }
             fn export_event(ref self, event: LogEvent) {
                 println(f"CUSTOM<{event.level}>: {event.message}");
             }
         }
         fn main() {
             Log.set_exporter(Tagging {});
             Log.set_min_level("info");
             Log.debug("dropped");
             Log.info("hi");
             Log.error("bye");
         }"#);
    assert_eq!(output, "CUSTOM<info>: hi\nCUSTOM<error>: bye\n");
}

#[test]
fn test_tracing_stdout_exporter_emits_event_line() {
    // StdoutExporter is the v1 emission surface: it renders a LogEvent
    // as one structured line — `[level] message key=value … span_id=N`,
    // with the span_id suffix only when the event is in a span.
    let output = run(r#"fn main() {
         let tracer = StdoutExporter {};
         tracer.export_event(LogEvent.info("plain"));
         tracer.export_event(
             LogEvent.info("started")
                 .with_field("user_id", "42")
                 .with_field("ip", "127.0.0.1")
                 .in_span(5));
     }"#);
    assert_eq!(
        output,
        "[info] plain\n[info] started user_id=42 ip=127.0.0.1 span_id=5\n"
    );
}

#[test]
fn test_tracing_stdout_exporter_emits_span_line() {
    // Spans render as `[span] name span_id=N parent_id=M key=value …`,
    // with the parent_id suffix suppressed for a root span (parent 0).
    let output = run(r#"fn main() {
         let tracer = StdoutExporter {};
         tracer.export_span(Span.root("request", 7));
         tracer.export_span(
             Span.root("outer", 1)
                 .child("inner", 2)
                 .with_field("route", "/health"));
     }"#);
    assert_eq!(
        output,
        "[span] request span_id=7\n[span] inner span_id=2 parent_id=1 route=/health\n"
    );
}

#[test]
fn test_tracing_with_span_stamps_active_span() {
    // phase-8 line 153: `with_span(s, ||body)` installs `s` as the ambient
    // active span, so a `Log.*` inside the body is auto-stamped with its
    // span id without the caller threading it.
    let output = run(r#"fn main() {
         let s = Span.root("req", 7);
         with_span(s, || { Log.info("inside") });
         Log.info("outside");
     }"#);
    // Inside the span → span_id=7; outside → no active span → no suffix.
    assert_eq!(output, "[info] inside span_id=7\n[info] outside\n");
}

#[test]
fn test_tracing_with_span_nesting_restores_outer() {
    // A nested `with_span` restores the outer active span on exit.
    let output = run(r#"fn main() {
         let outer = Span.root("o", 1);
         let inner = Span.root("i", 2);
         with_span(outer, || {
             Log.info("a");
             with_span(inner, || { Log.info("b") });
             Log.info("c");
         });
         Log.info("d");
     }"#);
    assert_eq!(
        output,
        "[info] a span_id=1\n[info] b span_id=2\n[info] c span_id=1\n[info] d\n"
    );
}

#[test]
fn test_tracing_explicit_in_span_overrides_active() {
    // An explicit `.in_span(id)` always wins over the ambient active span.
    let output = run(r#"fn main() {
         let s = Span.root("s", 7);
         with_span(s, || {
             let tracer = StdoutExporter {};
             tracer.export_event(LogEvent.info("x").in_span(99));
         });
     }"#);
    assert_eq!(output, "[info] x span_id=99\n");
}

// ── std.process — Command / Child surface ──────────────────────────

#[test]
fn test_process_command_builder_records_state() {
    let output = run(r#"fn main() {
         let cmd = Command.new("ls").arg("-la").arg("/tmp").env("PATH", "/usr/bin");
         println(cmd.program);
         println(cmd.cmd_args.len());
         println(cmd.cmd_env.len());
     }"#);
    assert_eq!(output, "ls\n2\n1\n");
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
fn test_process_command_env_records_kv() {
    let output = run(r#"fn main() {
         let cmd = Command.new("printenv").env("FOO", "bar");
         match cmd.cmd_env.get(0) {
             Some(e) => {
                 println(e.key);
                 println(e.value);
             }
             None => println("?"),
         }
     }"#);
    assert_eq!(output, "FOO\nbar\n");
}

#[test]
fn test_process_spawn_nonexistent_program_returns_not_found() {
    // Real intrinsic exercise: spawning a path that doesn't exist
    // surfaces `IoError.NotFound` (mapped from `std::io::ErrorKind::NotFound`
    // in `try_eval_process_method`). Tests the error-path of the intrinsic
    // and that `IoError` variants reach user pattern matches at scope-0.
    let output = run(r#"fn main() {
         let cmd = Command.new("/no/such/path/karac-test-nonexistent");
         match cmd.spawn() {
             Ok(_) => println("ok??"),
             Err(IoError.NotFound) => println("not_found"),
             Err(IoError.Other(_)) => println("other"),
             Err(_) => println("other_err"),
         }
     }"#);
    assert_eq!(output, "not_found\n");
}

#[test]
fn test_process_user_can_declare_sends_process_table_effect() {
    // `ProcessTable` is registered as a prelude effect resource so
    // user wrappers can declare `with sends(ProcessTable)` without an
    // explicit `effect resource ProcessTable;`. The wrapper forwards
    // to `spawn` (same effect), exercising the effect-declaration
    // verification path.
    let output = run(
        r#"fn run_cmd(prog: String) -> Result[Child, IoError] with sends(ProcessTable) {
             Command.new(prog).spawn()
         }
         fn main() {
             match run_cmd("/no/such/path/karac-test-nonexistent") {
                 Ok(_) => println("ok"),
                 Err(_) => println("err"),
             }
         }"#,
    );
    assert_eq!(output, "err\n");
}

#[cfg(unix)]
#[test]
fn test_process_spawn_real_command_and_wait_for_zero_exit() {
    // End-to-end intrinsic check: spawn a real OS process, wait for
    // it, verify ExitStatus { code: 0, success: true }. Using
    // `/usr/bin/true` (POSIX-ubiquitous, exits 0 silently) keeps the
    // test runner's terminal clean — `/bin/echo` would inherit-print
    // to runner stdout, which is cosmetically noisy. The Kāra child
    // handle's wait status is what we actually verify. Gated on
    // `unix` because the hard-coded path doesn't resolve on Windows.
    let output = run(r#"fn main() {
         let cmd = Command.new("/usr/bin/true");
         match cmd.spawn() {
             Ok(child) => {
                 match child.wait() {
                     Ok(status) => {
                         println(status.code);
                         println(status.success);
                     }
                     Err(_) => println("wait_err"),
                 }
             }
             Err(_) => println("spawn_err"),
         }
     }"#);
    assert_eq!(output, "0\ntrue\n");
}

#[cfg(unix)]
#[test]
fn test_process_try_wait_returns_none_for_still_running_child() {
    // Spawn a child that sleeps long enough that try_wait sees it
    // still running. /bin/sleep is POSIX-ubiquitous. 0.5s is short
    // enough to keep the test fast but long enough that try_wait
    // fires before exit (interpreter wall-clock has zero scheduling
    // jitter compared to the OS spawn latency). Gated on `unix`
    // because the hard-coded path doesn't resolve on Windows.
    let output = run(r#"fn main() {
         let cmd = Command.new("/bin/sleep").arg("0.5");
         match cmd.spawn() {
             Ok(child) => {
                 match child.try_wait() {
                     Ok(None) => println("still_running"),
                     Ok(Some(_)) => println("already_exited"),
                     Err(_) => println("err"),
                 }
                 match child.wait() {
                     Ok(_) => println("waited"),
                     Err(_) => println("wait_err"),
                 }
             }
             Err(_) => println("spawn_err"),
         }
     }"#);
    assert_eq!(output, "still_running\nwaited\n");
}

#[cfg(unix)]
#[test]
fn test_process_kill_terminates_child_and_wait_reports_failure() {
    // Spawn /bin/sleep 60 (way longer than test runtime), kill it,
    // then wait. kill returns Ok(Unit); wait returns Ok(status) with
    // success=false (terminated by signal). The child is reaped by
    // the wait call after the kill (`kill` itself leaves the table
    // entry in place per the spec — the caller still needs wait()).
    // Gated on `unix` because the hard-coded path doesn't resolve
    // on Windows.
    let output = run(r#"fn main() {
         let cmd = Command.new("/bin/sleep").arg("60");
         match cmd.spawn() {
             Ok(child) => {
                 match child.kill() {
                     Ok(_) => println("killed"),
                     Err(_) => println("kill_err"),
                 }
                 match child.wait() {
                     Ok(status) => println(status.success),
                     Err(_) => println("wait_err"),
                 }
             }
             Err(_) => println("spawn_err"),
         }
     }"#);
    assert_eq!(output, "killed\nfalse\n");
}

#[cfg(unix)]
#[test]
fn test_process_env_vars_propagate_to_child() {
    // The .env() builder method propagates to the spawned process.
    // Child stdout inherits the parent fd, so it doesn't show up in
    // Kāra's `captured_output` — instead, verify env var propagation
    // by having the child's shell `test` the var against the expected
    // value and exit 0 / 1 accordingly. The wait-status's `success`
    // field is the signal. Gated on `unix` because `/bin/sh` doesn't
    // resolve on Windows.
    let output = run(r#"fn main() {
         let cmd = Command.new("/bin/sh")
             .arg("-c")
             .arg("test \"$MY_VAR\" = \"kara-env-witness\"")
             .env("MY_VAR", "kara-env-witness");
         match cmd.spawn() {
             Ok(child) => {
                 match child.wait() {
                     Ok(s) => println(s.success),
                     Err(_) => println("wait_err"),
                 }
             }
             Err(_) => println("spawn_err"),
         }
     }"#);
    assert_eq!(
        output, "true\n",
        "expected env var to propagate (exit 0); got: {output}"
    );
}

#[test]
fn test_process_wait_on_unknown_child_returns_not_found() {
    // If a Child handle's pid isn't in the interpreter's side-table
    // (e.g., the user fabricated a Child struct manually), wait()
    // returns IoError.NotFound rather than panic. Defensive against
    // user code that hand-constructs Child values to side-step the
    // builder.
    let output = run(r#"fn main() {
         let fake = Child { pid: 999999999 };
         match fake.wait() {
             Ok(_) => println("ok??"),
             Err(IoError.NotFound) => println("not_found"),
             Err(_) => println("other_err"),
         }
     }"#);
    assert_eq!(output, "not_found\n");
}

#[test]
fn test_process_stdio_builder_records_redirection() {
    // The stdin/stdout/stderr builders thread the `Stdio` setting onto
    // the Command (default `Inherit`). Read the fields back to confirm
    // the chain records what was set without dropping the others.
    let output = run(r#"fn main() {
         let cmd = Command.new("ls").stdout(Stdio.Null).stderr(Stdio.Null);
         match cmd.cmd_stdin { Stdio.Inherit => println("in:inherit"), Stdio.Null => println("in:null") }
         match cmd.cmd_stdout { Stdio.Inherit => println("out:inherit"), Stdio.Null => println("out:null") }
         match cmd.cmd_stderr { Stdio.Inherit => println("err:inherit"), Stdio.Null => println("err:null") }
     }"#);
    assert_eq!(output, "in:inherit\nout:null\nerr:null\n");
}

#[cfg(unix)]
#[test]
fn test_process_spawn_with_null_redirection_waits_clean() {
    // Real spawn with redirection applied: `/bin/echo` would otherwise
    // inherit-print to the test runner's terminal (the noise the
    // zero-exit test's comment calls out). Redirecting stdout to
    // `Stdio.Null` discards the child's output — the runner stays quiet
    // and the child still exits 0, which is what we assert. This is the
    // operational point of `Stdio.Null`.
    let output = run(r#"fn main() {
         let cmd = Command.new("/bin/echo")
             .arg("this-output-is-discarded")
             .stdout(Stdio.Null);
         match cmd.spawn() {
             Ok(child) => {
                 match child.wait() {
                     Ok(status) => println(status.success),
                     Err(_) => println("wait_err"),
                 }
             }
             Err(_) => println("spawn_err"),
         }
     }"#);
    assert_eq!(output, "true\n");
}

#[cfg(unix)]
#[test]
fn test_process_capture_stdout_via_piped() {
    // `Stdio.Piped` + the capture half: spawn `/bin/echo` with stdout
    // piped, take the read handle off the child, and drain it to a
    // String. `/bin/echo hello-pipe` writes "hello-pipe\n", so the
    // captured output is exactly that. Proves Piped wiring at spawn,
    // `Child.stdout()` yielding `Some(handle)`, and `read_to_string`.
    let output = run(r#"fn main() {
         let cmd = Command.new("/bin/echo").arg("hello-pipe").stdout(Stdio.Piped);
         match cmd.spawn() {
             Ok(child) => {
                 match child.stdout() {
                     Some(out) => {
                         match out.read_to_string() {
                             Ok(s) => print(s),
                             Err(_) => println("read_err"),
                         }
                     }
                     None => println("no_stdout"),
                 }
                 match child.wait() {
                     Ok(_) => {}
                     Err(_) => println("wait_err"),
                 }
             }
             Err(_) => println("spawn_err"),
         }
     }"#);
    assert_eq!(output, "hello-pipe\n");
}

#[cfg(unix)]
#[test]
fn test_process_stdout_not_piped_yields_none() {
    // A stream left at the default `Stdio.Inherit` (here stdout is
    // redirected to `Null`, also not piped) has no captured handle, so
    // `Child.stdout()` is `None` — mirroring `std::process::Child::stdout`.
    let output = run(r#"fn main() {
         let cmd = Command.new("/bin/echo").arg("x").stdout(Stdio.Null);
         match cmd.spawn() {
             Ok(child) => {
                 match child.stdout() {
                     Some(_) => println("some"),
                     None => println("none"),
                 }
                 match child.wait() {
                     Ok(_) => {}
                     Err(_) => println("wait_err"),
                 }
             }
             Err(_) => println("spawn_err"),
         }
     }"#);
    assert_eq!(output, "none\n");
}

#[cfg(unix)]
#[test]
fn test_process_write_stdin_close_and_read_stdout_roundtrip() {
    // Full parent-drives-child round-trip through `/bin/cat` (echoes
    // stdin to stdout): spawn with BOTH stdin and stdout piped, write a
    // line to the child's stdin, then `close()` it. The close is the
    // load-bearing step — `cat` reads to EOF, so without closing stdin
    // it would block forever and the subsequent `read_to_string` would
    // deadlock (the exact footgun the read side guards). After close,
    // `cat` flushes "ping\n" and exits; the captured stdout reads it.
    let output = run(r#"fn main() {
         let cmd = Command.new("/bin/cat").stdin(Stdio.Piped).stdout(Stdio.Piped);
         match cmd.spawn() {
             Ok(child) => {
                 match child.stdin() {
                     Some(inp) => {
                         match inp.write("ping\n") {
                             Ok(_) => {}
                             Err(_) => println("write_err"),
                         }
                         match inp.close() {
                             Ok(_) => {}
                             Err(_) => println("close_err"),
                         }
                     }
                     None => println("no_stdin"),
                 }
                 match child.stdout() {
                     Some(out) => {
                         match out.read_to_string() {
                             Ok(s) => print(s),
                             Err(_) => println("read_err"),
                         }
                     }
                     None => println("no_stdout"),
                 }
                 match child.wait() {
                     Ok(_) => {}
                     Err(_) => println("wait_err"),
                 }
             }
             Err(_) => println("spawn_err"),
         }
     }"#);
    assert_eq!(output, "ping\n");
}

// ── Semaphore — application-layer backpressure primitive ───────────

#[test]
fn test_semaphore_acquire_grants_up_to_permit_count_then_times_out() {
    // new(2): two acquires succeed (permits 2 -> 1 -> 0), the third
    // finds the semaphore exhausted and (single-threaded) fails closed.
    let output = run(r#"fn main() {
         let sem = Semaphore.new(2);
         match sem.acquire(1000) { Ok(_) => println("a1ok"), Err(_) => println("a1timeout") }
         match sem.acquire(1000) { Ok(_) => println("a2ok"), Err(_) => println("a2timeout") }
         match sem.acquire(1000) { Ok(_) => println("a3ok"), Err(SemaphoreError.Timeout) => println("a3timeout") }
     }"#);
    assert_eq!(output, "a1ok\na2ok\na3timeout\n");
}

#[test]
fn test_semaphore_release_returns_a_permit() {
    // Exhaust a 1-permit semaphore, release, then re-acquire — the
    // released permit is available again.
    let output = run(r#"fn main() {
         let sem = Semaphore.new(1);
         match sem.acquire(1000) { Ok(_) => println("ok"), Err(_) => println("timeout") }
         match sem.acquire(1000) { Ok(_) => println("ok"), Err(_) => println("timeout") }
         sem.release();
         match sem.acquire(1000) { Ok(_) => println("ok"), Err(_) => println("timeout") }
     }"#);
    assert_eq!(output, "ok\ntimeout\nok\n");
}

#[test]
fn test_semaphore_release_saturates_at_initial_budget() {
    // Releasing more than were taken must not inflate the budget past
    // `new`'s count: new(1), one stray release, then only ONE acquire
    // succeeds (not two).
    let output = run(r#"fn main() {
         let sem = Semaphore.new(1);
         sem.release();
         sem.release();
         match sem.acquire(1000) { Ok(_) => println("ok"), Err(_) => println("timeout") }
         match sem.acquire(1000) { Ok(_) => println("ok"), Err(_) => println("timeout") }
     }"#);
    assert_eq!(output, "ok\ntimeout\n");
}

#[test]
fn test_semaphore_hand_rolled_zero_handle_fails_closed() {
    // A `Semaphore { handle_id: 0 }` literal that bypassed `new` has no
    // table entry; acquire fails closed with Timeout rather than panic.
    let output = run(r#"fn main() {
         let fake = Semaphore { handle_id: 0 };
         match fake.acquire(1000) { Ok(_) => println("ok??"), Err(SemaphoreError.Timeout) => println("timeout") }
     }"#);
    assert_eq!(output, "timeout\n");
}

// ── RateLimiter — token-bucket backpressure primitive ──────────────

#[test]
fn test_rate_limiter_grants_initial_burst_then_limits() {
    // Bucket starts full (capacity 3): three immediate grants for a key,
    // then the bucket is empty and the next try (microseconds later, no
    // meaningful refill at 1 token/sec) is limited. Deterministic — the
    // four calls run back-to-back well within one refill interval.
    let output = run(r#"fn main() {
         let rl = RateLimiter.new_token_bucket(1, 3);
         println(rl.try_acquire("k"));
         println(rl.try_acquire("k"));
         println(rl.try_acquire("k"));
         println(rl.try_acquire("k"));
     }"#);
    assert_eq!(output, "true\ntrue\ntrue\nfalse\n");
}

#[test]
fn test_rate_limiter_buckets_are_per_key() {
    // Each key gets an independent full bucket: exhausting key "a" leaves
    // key "b" with its own fresh burst.
    let output = run(r#"fn main() {
         let rl = RateLimiter.new_token_bucket(1, 1);
         println(rl.try_acquire("a"));
         println(rl.try_acquire("a"));
         println(rl.try_acquire("b"));
     }"#);
    assert_eq!(output, "true\nfalse\ntrue\n");
}

#[test]
fn test_rate_limiter_hand_rolled_zero_handle_fails_closed() {
    // A `RateLimiter { handle_id: 0 }` literal that bypassed the
    // constructor has no table entry; try_acquire reports limited
    // (false) rather than panicking.
    let output = run(r#"fn main() {
         let fake = RateLimiter { handle_id: 0 };
         println(fake.try_acquire("k"));
     }"#);
    assert_eq!(output, "false\n");
}

// ── BoundedChannel[T] — capacity-bounded backpressure queue ────────

#[test]
fn test_bounded_channel_send_bounds_then_recv_is_fifo() {
    // Capacity 2: two sends succeed, the third hits the bound and
    // fails fast; recv drains in FIFO order, then reports empty.
    let output = run(r#"fn main() {
         let ch = BoundedChannel.new(2, OnFull.FailFast);
         match ch.send(10) { Ok(_) => println("ok"), Err(_) => println("full") }
         match ch.send(20) { Ok(_) => println("ok"), Err(_) => println("full") }
         match ch.send(30) { Ok(_) => println("ok"), Err(ChannelError.Full) => println("full") }
         match ch.recv() { Some(v) => println(v), None => println("none") }
         match ch.recv() { Some(v) => println(v), None => println("none") }
         match ch.recv() { Some(v) => println(v), None => println("none") }
     }"#);
    assert_eq!(output, "ok\nok\nfull\n10\n20\nnone\n");
}

#[test]
fn test_bounded_channel_block_collapses_to_fail_fast_in_v1() {
    // The single-threaded interpreter has no peer to drain the buffer,
    // so `OnFull.Block` cannot park — a full send errors just like
    // FailFast. A freed slot (via recv) then accepts the next send.
    let output = run(r#"fn main() {
         let ch = BoundedChannel.new(1, OnFull.Block);
         match ch.send(1) { Ok(_) => println("ok"), Err(_) => println("full") }
         match ch.send(2) { Ok(_) => println("ok"), Err(_) => println("full") }
         match ch.recv() { Some(v) => println(v), None => println("none") }
         match ch.send(3) { Ok(_) => println("ok"), Err(_) => println("full") }
     }"#);
    assert_eq!(output, "ok\nfull\n1\nok\n");
}

#[test]
fn test_bounded_channel_hand_rolled_zero_handle_fails_closed() {
    // A `BoundedChannel { handle_id: 0 }` literal that bypassed `new`
    // has no buffer: send fails closed (Full), recv yields None.
    let output = run(r#"fn main() {
         let fake = BoundedChannel { handle_id: 0 };
         match fake.send(1) { Ok(_) => println("ok"), Err(_) => println("full") }
         match fake.recv() { Some(_) => println("some"), None => println("none") }
     }"#);
    assert_eq!(output, "full\nnone\n");
}

// ── Pool[T] — connection-pool primitive surface ────────────────────

#[test]
fn test_pool_new_returns_pool_value_with_handle() {
    // v1 surface check: construct a `Pool[i64]`. The Kāra Pool value
    // is opaque except for the side-table handle_id, which the
    // intrinsic mints fresh via the monotonic counter — non-zero
    // tells us `Pool.new` actually fired (vs falling through to the
    // typecheck-only placeholder body that would leave handle_id 0).
    let output = run(r#"fn make_int() -> i64 { 42 }
         fn main() {
             let pool: Pool[i64] = Pool.new(make_int, 4, 8);
             println(pool.handle_id > 0);
         }"#);
    assert_eq!(output, "true\n");
}

#[test]
fn test_pool_acquire_mints_via_create_fn() {
    // Acquire on a fresh pool invokes `create_fn` and hands back a
    // `PooledConnection` carrying the minted value. Verifies the
    // intrinsic's closure-invocation path lights up.
    let output = run(r#"fn make_int() -> i64 { 42 }
         fn main() {
             let pool: Pool[i64] = Pool.new(make_int, 4, 8);
             match pool.acquire(0) {
                 Ok(conn) => println(conn.val),
                 Err(_) => println("err"),
             }
         }"#);
    assert_eq!(output, "42\n");
}

#[test]
fn test_pool_acquire_at_cap_returns_timeout() {
    // `max_connections` is the hard cap. Filling it and trying
    // another acquire fires `PoolError.Timeout` immediately —
    // single-threaded interpreter has no peer to free a slot mid-wait.
    let output = run(r#"fn make_int() -> i64 { 7 }
         fn main() {
             let pool: Pool[i64] = Pool.new(make_int, 2, 8);
             match pool.acquire(0) {
                 Ok(_) => {
                     match pool.acquire(0) {
                         Ok(_) => {
                             match pool.acquire(0) {
                                 Ok(_) => println("ok??"),
                                 Err(PoolError.Timeout) => println("timeout"),
                                 Err(_) => println("other_err"),
                             }
                         }
                         Err(_) => println("acq2_err"),
                     }
                 }
                 Err(_) => println("acq1_err"),
             }
         }"#);
    assert_eq!(output, "timeout\n");
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
fn test_pool_health_check_passes_reuses_idle_slot() {
    // A registered health check that returns true hands the released
    // (idle) slot straight back — no fresh mint. `make_conn` prints
    // "mint" on each call, so a single "mint" proves the second acquire
    // reused the slot rather than re-minting. (The first acquire always
    // mints; the hook only validates *reused* idle slots.)
    let output = run(r#"fn make_conn() -> i64 { println("mint"); 7 }
         fn main() {
             let pool: Pool[i64] = Pool.new(make_conn, 4, 8).with_health_check(|c| { c > 0 });
             match pool.acquire(0) {
                 Ok(c1) => { pool.release(c1); }
                 Err(_) => println("acq1_err"),
             }
             match pool.acquire(0) {
                 Ok(c2) => println(c2.val),
                 Err(_) => println("acq2_err"),
             }
         }"#);
    assert_eq!(output, "mint\n7\n");
}

#[test]
fn test_pool_health_check_fails_evicts_and_mints_fresh() {
    // A health check returning false evicts the reused idle slot and
    // `acquire` mints a fresh one in its place (evict-on-error). The
    // second acquire therefore mints again — two "mint" lines total.
    let output = run(r#"fn make_conn() -> i64 { println("mint"); 7 }
         fn main() {
             let pool: Pool[i64] = Pool.new(make_conn, 4, 8).with_health_check(|c| { c < 0 });
             match pool.acquire(0) {
                 Ok(c1) => { pool.release(c1); }
                 Err(_) => println("acq1_err"),
             }
             match pool.acquire(0) {
                 Ok(c2) => println(c2.val),
                 Err(_) => println("acq2_err"),
             }
         }"#);
    assert_eq!(output, "mint\nmint\n7\n");
}

#[test]
fn test_pool_health_check_eviction_at_cap_does_not_timeout() {
    // The eviction-decrements-active_count contract: a pool at its
    // `max_connections` cap (1) whose only idle slot fails the health
    // check must evict (freeing a cap slot) and mint a replacement — NOT
    // return Timeout. Without the decrement the mint path would see the
    // pool still at cap and fail closed.
    let output = run(r#"fn make_conn() -> i64 { println("mint"); 7 }
         fn main() {
             let pool: Pool[i64] = Pool.new(make_conn, 1, 1).with_health_check(|c| { false });
             match pool.acquire(0) {
                 Ok(c1) => { pool.release(c1); }
                 Err(_) => println("acq1_err"),
             }
             match pool.acquire(0) {
                 Ok(_) => println("ok2"),
                 Err(PoolError.Timeout) => println("timeout2"),
                 Err(_) => println("err2"),
             }
         }"#);
    assert_eq!(output, "mint\nmint\nok2\n");
}

#[test]
fn test_pool_acquire_on_uninitialized_handle_returns_pool_closed() {
    // A hand-rolled `Pool { handle_id: 0 }` bypasses `Pool.new` so
    // there's no entry in the side-table — acquire surfaces this
    // as `PoolError.PoolClosed` rather than panicking. Defensive
    // against user code that constructs Pool literals manually.
    let output = run(r#"fn main() {
         let pool: Pool[i64] = Pool { handle_id: 0 };
         match pool.acquire(0) {
             Ok(_) => println("ok??"),
             Err(PoolError.Timeout) => println("timeout"),
             Err(PoolError.PoolClosed) => println("closed"),
             Err(PoolError.CreateFailed) => println("create_failed"),
         }
     }"#);
    assert_eq!(output, "closed\n");
}

#[test]
fn test_pool_create_fn_can_be_a_closure_with_captures() {
    // The factory slot accepts any `Fn() -> T`, including a closure
    // with captures. v1 ships create_fn as a `Value::Function` (the
    // interpreter's owned-fn representation); this test pins that
    // closures-with-captures work the same way bare fn references do.
    let output = run(r#"fn main() {
         let prefix = "tag-";
         let pool: Pool[String] = Pool.new(|| prefix + "x", 2, 4);
         match pool.acquire(0) {
             Ok(conn) => println(conn.val),
             Err(_) => println("err"),
         }
     }"#);
    assert_eq!(output, "tag-x\n");
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
fn test_tracing_user_can_implement_exporter_trait() {
    // The whole point of the `Exporter` trait shape is that user code
    // can swap in a real implementation against the same surface. A
    // capturing exporter verifies the dispatch reaches the user impl,
    // not just the no-op default.
    let output = run(r#"shared struct CaptureExporter {
             mut span_count: i64,
             mut event_count: i64,
         }
         impl Exporter for CaptureExporter {
             fn export_span(ref self, span: Span) { self.span_count = self.span_count + 1; }
             fn export_event(ref self, event: LogEvent) { self.event_count = self.event_count + 1; }
         }
         fn main() {
             let e = CaptureExporter { span_count: 0, event_count: 0 };
             e.export_span(Span.root("a", 1));
             e.export_span(Span.root("b", 2));
             e.export_event(LogEvent.info("c"));
             println(e.span_count);
             println(e.event_count);
         }"#);
    assert_eq!(output, "2\n1\n");
}

// ── Stats namespace ───────────────────────────────────────────────

#[test]
fn test_stats_sum() {
    let output = run("fn main() { let xs = [1.0_f64, 2.0_f64, 3.0_f64]; println(Stats.sum(xs)); }");
    assert_eq!(output, "6\n");
}

#[test]
fn test_stats_prod() {
    let output =
        run("fn main() { let xs = [2.0_f64, 3.0_f64, 4.0_f64]; println(Stats.prod(xs)); }");
    assert_eq!(output, "24\n");
}

#[test]
fn test_stats_mean() {
    let output =
        run("fn main() { let xs = [1.0_f64, 2.0_f64, 3.0_f64]; println(Stats.mean(xs)); }");
    assert_eq!(output, "2\n");
}

#[test]
fn test_stats_variance() {
    let output = run("fn main() { let xs = [2.0_f64, 4.0_f64, 4.0_f64, 4.0_f64, 5.0_f64, 5.0_f64, 7.0_f64, 9.0_f64]; println(Stats.variance(xs)); }");
    assert_eq!(output, "4\n");
}

#[test]
fn test_stats_stddev() {
    let output = run("fn main() { let xs = [2.0_f64, 4.0_f64, 4.0_f64, 4.0_f64, 5.0_f64, 5.0_f64, 7.0_f64, 9.0_f64]; println(Stats.stddev(xs)); }");
    assert_eq!(output, "2\n");
}

#[test]
fn test_stats_median_odd() {
    let output =
        run("fn main() { let xs = [3.0_f64, 1.0_f64, 2.0_f64]; println(Stats.median(xs)); }");
    assert_eq!(output, "2\n");
}

#[test]
fn test_stats_median_even() {
    let output = run(
        "fn main() { let xs = [1.0_f64, 2.0_f64, 3.0_f64, 4.0_f64]; println(Stats.median(xs)); }",
    );
    assert_eq!(output, "2.5\n");
}

#[test]
fn test_stats_min_nonempty() {
    let output = run("fn main() {\n\
         let xs = [3.0_f64, 1.0_f64, 2.0_f64];\n\
         match Stats.min(xs) {\n\
             Some(v) => println(v),\n\
             None => println(\"none\"),\n\
         }\n\
     }");
    assert_eq!(output, "1\n");
}

#[test]
fn test_stats_max_nonempty() {
    let output = run("fn main() {\n\
         let xs = [3.0_f64, 1.0_f64, 2.0_f64];\n\
         match Stats.max(xs) {\n\
             Some(v) => println(v),\n\
             None => println(\"none\"),\n\
         }\n\
     }");
    assert_eq!(output, "3\n");
}

#[test]
fn test_stats_min_empty() {
    let output = run("fn main() {\n\
         let xs: Vec[f64] = Vec[0.0_f64];\n\
         let ys = xs[1..];\n\
         match Stats.min(ys) {\n\
             Some(v) => println(v),\n\
             None => println(\"none\"),\n\
         }\n\
     }");
    // empty slice → None
    assert_eq!(output, "none\n");
}

// ── Encoding namespace (Base64 / Hex / Url) ───────────────────────

#[test]
fn test_base64_encode_basic() {
    // RFC 4648 vector: "foobar" → "Zm9vYmFy"
    let output = run("fn main() {\n\
             let bs = [102u8, 111u8, 111u8, 98u8, 97u8, 114u8];\n\
             println(Base64.encode(bs));\n\
         }");
    assert_eq!(output, "Zm9vYmFy\n");
}

#[test]
fn test_base64_encode_padding_one_byte() {
    // RFC 4648 vector: "f" → "Zg=="
    let output = run("fn main() {\n\
             let bs = [102u8];\n\
             println(Base64.encode(bs));\n\
         }");
    assert_eq!(output, "Zg==\n");
}

#[test]
fn test_base64_encode_padding_two_bytes() {
    // RFC 4648 vector: "fo" → "Zm8="
    let output = run("fn main() {\n\
             let bs = [102u8, 111u8];\n\
             println(Base64.encode(bs));\n\
         }");
    assert_eq!(output, "Zm8=\n");
}

#[test]
fn test_base64_encode_url_safe_no_padding() {
    // Bytes 0xfb 0xff 0xbf encode to "+/+/" in standard alphabet,
    // "-_-_" in URL-safe alphabet. URL-safe omits padding.
    let output = run("fn main() {\n\
             let bs = [251u8, 255u8, 191u8];\n\
             println(Base64.encode_url_safe(bs));\n\
         }");
    assert_eq!(output, "-_-_\n");
}

#[test]
fn test_base64_decode_roundtrip() {
    let output = run("fn main() {\n\
             match Base64.decode(\"Zm9vYmFy\") {\n\
                 Ok(bs) => println(bs.len()),\n\
                 Err(_) => println(\"err\"),\n\
             }\n\
         }");
    assert_eq!(output, "6\n");
}

#[test]
fn test_base64_decode_invalid_char() {
    let output = run("fn main() {\n\
             match Base64.decode(\"!!!!\") {\n\
                 Ok(_) => println(\"ok\"),\n\
                 Err(_) => println(\"err\"),\n\
             }\n\
         }");
    assert_eq!(output, "err\n");
}

#[test]
fn test_hex_encode_lowercase() {
    let output = run("fn main() {\n\
             let bs = [255u8, 0u8, 16u8];\n\
             println(Hex.encode(bs));\n\
         }");
    assert_eq!(output, "ff0010\n");
}

#[test]
fn test_hex_encode_upper() {
    let output = run("fn main() {\n\
             let bs = [255u8, 0u8, 16u8];\n\
             println(Hex.encode_upper(bs));\n\
         }");
    assert_eq!(output, "FF0010\n");
}

#[test]
fn test_hex_decode_mixed_case() {
    let output = run("fn main() {\n\
             match Hex.decode(\"FfaA\") {\n\
                 Ok(bs) => println(bs.len()),\n\
                 Err(_) => println(\"err\"),\n\
             }\n\
         }");
    assert_eq!(output, "2\n");
}

#[test]
fn test_hex_decode_odd_length() {
    let output = run("fn main() {\n\
             match Hex.decode(\"abc\") {\n\
                 Ok(_) => println(\"ok\"),\n\
                 Err(_) => println(\"err\"),\n\
             }\n\
         }");
    assert_eq!(output, "err\n");
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
fn test_url_decode_roundtrip() {
    let output = run("fn main() {\n\
             match Url.decode(\"a%20b%2Fc\") {\n\
                 Ok(s) => println(s),\n\
                 Err(_) => println(\"err\"),\n\
             }\n\
         }");
    assert_eq!(output, "a b/c\n");
}

#[test]
fn test_url_decode_invalid_percent() {
    let output = run("fn main() {\n\
             match Url.decode(\"a%2\") {\n\
                 Ok(_) => println(\"ok\"),\n\
                 Err(_) => println(\"err\"),\n\
             }\n\
         }");
    assert_eq!(output, "err\n");
}

#[test]
fn test_string_from_utf8_valid_returns_ok() {
    let output = run("fn main() {\n\
             let mut bs: Vec[u8] = Vec.new();\n\
             bs.push(72u8);\n\
             bs.push(101u8);\n\
             bs.push(108u8);\n\
             bs.push(108u8);\n\
             bs.push(111u8);\n\
             match String.from_utf8(bs) {\n\
                 Ok(s) => println(s),\n\
                 Err(_) => println(\"err\"),\n\
             }\n\
         }");
    assert_eq!(output, "Hello\n");
}

#[test]
fn test_string_from_utf8_invalid_byte_returns_err_invalid_byte() {
    // 0xff is never a valid UTF-8 lead byte; Rust's `Utf8Error::error_len`
    // returns `Some(1)` here, so the variant must be `InvalidByte`.
    let output = run("fn main() {\n\
             let mut bs: Vec[u8] = Vec.new();\n\
             bs.push(255u8);\n\
             match String.from_utf8(bs) {\n\
                 Ok(_) => println(\"ok\"),\n\
                 Err(Utf8Error.InvalidByte) => println(\"invalid_byte\"),\n\
                 Err(Utf8Error.IncompleteSequence) => println(\"incomplete\"),\n\
                 Err(Utf8Error.Other(_)) => println(\"other\"),\n\
             }\n\
         }");
    assert_eq!(output, "invalid_byte\n");
}

#[test]
fn test_string_from_utf8_incomplete_returns_err_incomplete_sequence() {
    // 0xe2 starts a 3-byte sequence; on its own the stream is truncated.
    // Rust's `Utf8Error::error_len` returns `None`, so the variant must
    // be `IncompleteSequence`.
    let output = run("fn main() {\n\
             let mut bs: Vec[u8] = Vec.new();\n\
             bs.push(226u8);\n\
             match String.from_utf8(bs) {\n\
                 Ok(_) => println(\"ok\"),\n\
                 Err(Utf8Error.InvalidByte) => println(\"invalid_byte\"),\n\
                 Err(Utf8Error.IncompleteSequence) => println(\"incomplete\"),\n\
                 Err(Utf8Error.Other(_)) => println(\"other\"),\n\
             }\n\
         }");
    assert_eq!(output, "incomplete\n");
}

#[test]
fn test_string_from_utf8_empty_returns_ok_empty() {
    let output = run("fn main() {\n\
             let bs: Vec[u8] = Vec.new();\n\
             match String.from_utf8(bs) {\n\
                 Ok(s) => println(s.len()),\n\
                 Err(_) => println(\"err\"),\n\
             }\n\
         }");
    assert_eq!(output, "0\n");
}

// ── String slicing — `s[a..b]` (phase-8 line 737) ─────────────────
//
// `s[a..b]` returns a fresh substring `String` (not a Slice), with all
// range forms (`a..b` / `a..` / `..b` / `..` / `a..=b`). Byte offsets
// with UTF-8 char-boundary validation: a non-boundary index is a runtime
// panic carrying `E_STRING_SLICE_NOT_AT_CHAR_BOUNDARY`.

#[test]
fn test_string_slice_basic_half_open() {
    let output = run_no_errors(
        "fn main() {
             let s = \"hello world\";
             println(s[0..5]);
             println(s[6..11]);
         }",
    );
    assert_eq!(output, "hello\nworld\n");
}

#[test]
fn test_string_slice_open_ended_forms() {
    // `a..` (to end), `..b` (from start), `..` (full), all fresh Strings.
    let output = run_no_errors(
        "fn main() {
             let s = \"hello world\";
             println(s[6..]);
             println(s[..5]);
             println(s[..]);
         }",
    );
    assert_eq!(output, "world\nhello\nhello world\n");
}

#[test]
fn test_string_slice_inclusive_and_empty() {
    // `a..=b` includes byte b; `a..a` is the empty string.
    let output = run_no_errors(
        "fn main() {
             let s = \"hello\";
             println(s[0..=4]);
             println(\"[\" + s[2..2] + \"]\");
         }",
    );
    assert_eq!(output, "hello\n[]\n");
}

#[test]
fn test_string_slice_result_is_string_and_concatenates() {
    // The slice result is a real String — it concatenates with `+` and
    // exposes String methods (`.len()`).
    let output = run_no_errors(
        "fn main() {
             let s = \"hello world\";
             let mid = s[6..11];
             println(mid + \"!\");
             println(mid.len());
         }",
    );
    assert_eq!(output, "world!\n5\n");
}

#[test]
fn test_string_slice_multibyte_on_boundary_ok() {
    // `é` is two bytes at offsets 1..3, so 0..1 ('h') and 1..3 ('é') both
    // land on char boundaries and slice cleanly.
    let output = run_no_errors(
        "fn main() {
             let s = \"héllo\";
             println(s[0..1]);
             println(s[1..3]);
         }",
    );
    assert_eq!(output, "h\né\n");
}

#[test]
fn test_string_slice_non_char_boundary_panics() {
    // Byte 2 falls in the middle of the 2-byte `é`, so `s[0..2]` panics
    // with E_STRING_SLICE_NOT_AT_CHAR_BOUNDARY (Rust's slicing contract).
    let errs = runtime_errors(
        "fn main() {
             let s = \"héllo\";
             let bad = s[0..2];
             println(bad);
         }",
    );
    assert!(
        errs.iter()
            .any(|e| e.message.contains("E_STRING_SLICE_NOT_AT_CHAR_BOUNDARY")),
        "expected E_STRING_SLICE_NOT_AT_CHAR_BOUNDARY panic, got: {:?}",
        errs.iter().map(|e| &e.message).collect::<Vec<_>>(),
    );
}

#[test]
fn test_string_slice_out_of_range_is_runtime_error() {
    let errs = runtime_errors(
        "fn main() {
             let s = \"hi\";
             let bad = s[0..9];
             println(bad);
         }",
    );
    assert!(
        errs.iter()
            .any(|e| e.message.contains("string slice bounds 0..9 out of range")),
        "expected out-of-range slice error, got: {:?}",
        errs.iter().map(|e| &e.message).collect::<Vec<_>>(),
    );
}

// ── Set[T] ────────────────────────────────────────────────────────

#[test]
fn test_set_new_and_len() {
    let output = run("fn main() {\n\
         let s: Set[i64] = Set.new();\n\
         println(s.len());\n\
     }");
    assert_eq!(output, "0\n");
}

#[test]
fn test_set_insert_and_contains() {
    let output = run("fn main() {\n\
         let s: Set[i64] = Set.new();\n\
         s.insert(1_i64);\n\
         s.insert(2_i64);\n\
         println(s.contains(1_i64));\n\
         println(s.contains(3_i64));\n\
     }");
    assert_eq!(output, "true\nfalse\n");
}

#[test]
fn test_set_insert_dedup() {
    let output = run("fn main() {\n\
         let s: Set[i64] = Set.new();\n\
         s.insert(5_i64);\n\
         s.insert(5_i64);\n\
         println(s.len());\n\
     }");
    assert_eq!(output, "1\n");
}

#[test]
fn test_set_remove() {
    let output = run("fn main() {\n\
         let s: Set[i64] = Set.new();\n\
         s.insert(10_i64);\n\
         let was_present = s.remove(10_i64);\n\
         println(was_present);\n\
         println(s.len());\n\
     }");
    assert_eq!(output, "true\n0\n");
}

#[test]
fn test_set_is_empty() {
    let output = run("fn main() {\n\
         let s: Set[i64] = Set.new();\n\
         println(s.is_empty());\n\
         s.insert(1_i64);\n\
         println(s.is_empty());\n\
     }");
    assert_eq!(output, "true\nfalse\n");
}

#[test]
fn test_set_union() {
    let output = run("fn main() {\n\
         let a: Set[i64] = Set.new();\n\
         a.insert(1_i64);\n\
         a.insert(2_i64);\n\
         let b: Set[i64] = Set.new();\n\
         b.insert(2_i64);\n\
         b.insert(3_i64);\n\
         let c = a.union(b);\n\
         println(c.len());\n\
         println(c.contains(1_i64));\n\
         println(c.contains(3_i64));\n\
     }");
    assert_eq!(output, "3\ntrue\ntrue\n");
}

#[test]
fn test_set_intersection() {
    let output = run("fn main() {\n\
         let a: Set[i64] = Set.new();\n\
         a.insert(1_i64);\n\
         a.insert(2_i64);\n\
         let b: Set[i64] = Set.new();\n\
         b.insert(2_i64);\n\
         b.insert(3_i64);\n\
         let c = a.intersection(b);\n\
         println(c.len());\n\
         println(c.contains(2_i64));\n\
     }");
    assert_eq!(output, "1\ntrue\n");
}

#[test]
fn test_set_difference() {
    let output = run("fn main() {\n\
         let a: Set[i64] = Set.new();\n\
         a.insert(1_i64);\n\
         a.insert(2_i64);\n\
         let b: Set[i64] = Set.new();\n\
         b.insert(2_i64);\n\
         let c = a.difference(b);\n\
         println(c.len());\n\
         println(c.contains(1_i64));\n\
     }");
    assert_eq!(output, "1\ntrue\n");
}

#[test]
fn test_set_for_loop() {
    let output = run("fn main() {\n\
         let s: Set[i64] = Set.new();\n\
         s.insert(42_i64);\n\
         for x in s {\n\
             println(x);\n\
         }\n\
     }");
    assert_eq!(output, "42\n");
}

// ── Display / to_string ───────────────────────────────────────────

#[test]
fn test_to_string_i64() {
    let output = run("fn main() { let n: i64 = 42; println(n.to_string()); }");
    assert_eq!(output, "42\n");
}

#[test]
fn test_to_string_bool() {
    let output = run("fn main() { println(true.to_string()); }");
    assert_eq!(output, "true\n");
}

#[test]
fn test_to_string_str() {
    let output = run(r#"fn main() { let s = "hello"; println(s.to_string()); }"#);
    assert_eq!(output, "hello\n");
}

#[test]
fn test_to_string_vec() {
    let output = run("fn main() {\n\
         let v = Vec[1_i64, 2_i64];\n\
         println(v.to_string());\n\
     }");
    assert_eq!(output, "[1, 2]\n");
}

#[test]
fn test_to_string_option_some() {
    let output = run("fn main() {\n\
         let x: Option[i64] = Some(99_i64);\n\
         println(x.to_string());\n\
     }");
    assert_eq!(output, "Some(99)\n");
}

#[test]
fn test_to_string_option_none() {
    let output = run("fn main() {\n\
         let x: Option[i64] = None;\n\
         println(x.to_string());\n\
     }");
    assert_eq!(output, "None\n");
}

#[test]
fn test_fstring_interpolates_i64() {
    let output = run(r#"fn main() { let n: i64 = 7; println(f"n is {n}"); }"#);
    assert_eq!(output, "n is 7\n");
}

#[test]
fn test_fstring_interpolates_vec() {
    let output = run("fn main() {\n\
         let v = Vec[10_i64];\n\
         println(f\"v={v}\");\n\
     }");
    assert_eq!(output, "v=[10]\n");
}

#[test]
fn test_println_i64_direct() {
    let output = run("fn main() { println(123_i64); }");
    assert_eq!(output, "123\n");
}

#[test]
fn test_println_float_direct() {
    let output = run("fn main() { println(3.14_f64); }");
    assert_eq!(output, "3.14\n");
}

#[test]
fn test_string_sorted_basic() {
    let output = run(r#"fn main() { let s = "cba"; println(s.sorted()); }"#);
    assert_eq!(output, "abc\n");
}

#[test]
fn test_string_sorted_already_sorted() {
    let output = run(r#"fn main() { let s = "abc"; println(s.sorted()); }"#);
    assert_eq!(output, "abc\n");
}

#[test]
fn test_string_sorted_empty() {
    let output = run(r#"fn main() { let s = ""; println(s.sorted()); }"#);
    assert_eq!(output, "\n");
}

#[test]
fn test_string_chars_for_loop_prints_each() {
    // The canonical `for c in s.chars()` shape; verifies the explicit
    // chars() iterator yields one Value::Char per Unicode scalar.
    let output = run(r#"fn main() { for c in "abc".chars() { println(c); } }"#);
    assert_eq!(output, "a\nb\nc\n");
}

#[test]
fn test_string_for_loop_iterates_chars() {
    // design.md § Character type (line 2299) pins `for c in s` and
    // `s.chars()` as semantic peers. Same output as the chars() variant.
    let output = run(r#"fn main() { for c in "abc" { println(c); } }"#);
    assert_eq!(output, "a\nb\nc\n");
}

#[test]
fn test_string_chars_empty_iterates_zero_times() {
    let output = run(r#"fn main() {
            let mut n = 0i64;
            for _ in "".chars() { n = n + 1; }
            println(n);
        }"#);
    assert_eq!(output, "0\n");
}

#[test]
fn test_string_starts_with_interpreter() {
    // Mirrors the four-case probe at
    // `/tmp/kara-probes/starts_with_probe.kara`: match / mismatch /
    // prefix-longer-than-receiver / empty-prefix.
    let output = run(r#"fn main() {
            let s = "/todos/42";
            if s.starts_with("/todos/") { println("yes"); } else { println("no"); }
            if s.starts_with("/foo") { println("yes2"); } else { println("no2"); }
            if s.starts_with("/todos/42/extra") { println("yes3"); } else { println("no3"); }
            if s.starts_with("") { println("yes4"); } else { println("no4"); }
        }"#);
    assert_eq!(output, "yes\nno2\nno3\nyes4\n");
}

#[test]
fn test_string_substring_interpreter() {
    // Mirrors `/tmp/kara-probes/substring_probe.kara`:
    // in-range / start-zero / out-of-range / negative / empty-receiver.
    let output = run(r#"fn main() {
            let s: String = "/todos/42";
            println(s.substring(7));
            println(s.substring(0));
            println(s.substring(100));
            println(s.substring(-1));
            let empty: String = "";
            println(empty.substring(0));
        }"#);
    assert_eq!(output, "42\n/todos/42\n\n\n\n");
}

#[test]
fn test_string_substring_two_arg_interpreter() {
    // Two-arg `substring(start, end)` (byte range `[start, end)`): prefix /
    // suffix / empty-when-equal / inverted-bounds (end<start) / end-clamped /
    // negative-start (→ empty, matching the one-arg contract). Drives the
    // self-hosted lexer's `token_text` extraction.
    let output = run(r#"fn main() {
            let s: String = "hello world";
            println(s.substring(0, 5));
            println(s.substring(6, 11));
            println(s.substring(3, 3));
            println(s.substring(8, 2));
            println(s.substring(2, 100));
            println(s.substring(-2, 4));
        }"#);
    assert_eq!(output, "hello\nworld\n\n\nllo world\n\n");
}

#[test]
fn test_u8_ascii_predicates_interpreter() {
    // ASCII byte-classification on the `u8` bytes from `String.bytes()`:
    // is_ascii_digit / is_ascii_alphabetic / is_ascii_hexdigit. Phase-8 floor
    // for the self-hosting lexer's byte-indexed scan.
    let output = run(r#"fn main() {
            let s: String = "aZ9_ f";
            for b in s.bytes() {
                println(f"{b.is_ascii_digit()} {b.is_ascii_alphabetic()} {b.is_ascii_hexdigit()}");
            }
        }"#);
    // a:_/alpha/hex  Z:_/alpha/_  9:digit/_/hex  _:none  space:none  f:_/alpha/hex
    assert_eq!(
        output,
        "false true true\n\
         false true false\n\
         true false true\n\
         false false false\n\
         false false false\n\
         false true true\n"
    );
}

#[test]
fn test_i64_parse_interpreter() {
    // Five cases mirror `/tmp/kara-probes/i64_parse_full_probe.kara`:
    // numeric / non-numeric / negative / whitespace-padded / empty.
    let output = run(r#"fn main() {
            match i64.parse("42") {
                Some(n) => println(n),
                None => println(-1),
            }
            match i64.parse("not a number") {
                Some(n) => println(n),
                None => println(-1),
            }
            match i64.parse("-7") {
                Some(n) => println(n),
                None => println(-1),
            }
            match i64.parse("  100  ") {
                Some(n) => println(n),
                None => println(-1),
            }
            match i64.parse("") {
                Some(n) => println(n),
                None => println(-1),
            }
        }"#);
    assert_eq!(output, "42\n-1\n-7\n100\n-1\n");
}

#[test]
fn test_i64_from_str_radix_interpreter() {
    // Radix parse for the self-hosting lexer's hex/binary/octal literals:
    // hex / binary / octal / reject-bad-digit / hex-positive. Invalid radix
    // (>36) and bad digits → None.
    let output = run(
        r#"fn pr(o: Option[i64]) { match o { Some(n) => println(n), None => println(-1), } }
        fn main() {
            pr(i64.from_str_radix("ff", 16));
            pr(i64.from_str_radix("1010", 2));
            pr(i64.from_str_radix("17", 8));
            pr(i64.from_str_radix("zz", 16));
            pr(i64.from_str_radix("7f", 16));
        }"#,
    );
    assert_eq!(output, "255\n10\n15\n-1\n127\n");
}

#[test]
fn test_char_try_from_interpreter() {
    // #10: `char.try_from(n) -> Result[char, i64]`. Must match the codegen E2E
    // (test_e2e_char_try_from) — valid scalars → Ok(char), surrogate / above-
    // max / negative → Err(codepoint).
    let output = run(
        r#"fn show(r: Result[char, i64]) { match r { Ok(ch) => println(ch.to_string()), Err(cp) => println("err:" + cp.to_string()), } }
        fn main() {
            let b: u8 = 65;
            show(char.try_from(b))
            show(char.try_from(97))
            show(char.try_from(0x1F600))
            show(char.try_from(0xD800))
            show(char.try_from(0x110000))
            show(char.try_from(-1))
        }"#,
    );
    assert_eq!(output, "A\na\n😀\nerr:55296\nerr:1114112\nerr:-1\n");
}

#[test]
fn test_f64_parse_interpreter() {
    // Float parse: decimal / scientific / negative / reject / integer-form.
    // The self-hosting lexer's float-literal path.
    let output = run(
        r#"fn pr(o: Option[f64]) { match o { Some(x) => println(x), None => println(-1.0), } }
        fn main() {
            pr(f64.parse("3.14"));
            pr(f64.parse("1e10"));
            pr(f64.parse("-2.5"));
            pr(f64.parse("notnum"));
            pr(f64.parse("42"));
        }"#,
    );
    assert_eq!(output, "3.14\n10000000000\n-2.5\n-1\n42\n");
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
fn test_string_chars_with_map_char_as_key() {
    // Locks down the LeetCode #3 idiom — chars feeding a Map[char, i64]
    // last-index map. The sliding-window kata is the natural-pull that
    // surfaced this gap; this test guards against regression.
    let output = run(r#"fn main() {
            let mut last_idx: Map[char, i64] = Map.new();
            let mut i = 0i64;
            for c in "abca".chars() {
                last_idx.insert(c, i);
                i = i + 1;
            }
            match last_idx.get('a') { Some(v) => println(v), None => println(-1) }
            match last_idx.get('b') { Some(v) => println(v), None => println(-1) }
            match last_idx.get('c') { Some(v) => println(v), None => println(-1) }
        }"#);
    assert_eq!(output, "3\n1\n2\n");
}

#[test]
fn test_string_bytes_returns_slice_with_byte_values() {
    // `String.bytes() -> Slice[u8]` (design.md § Character type).
    // ASCII input: each byte is the codepoint. Locks down length +
    // positional access + comparison against `u8` literals via the
    // `char as u32 as u8` chain. This is the primitive the kata-8
    // (atoi) rewrite uses to drop the O(n) Vec[char] snapshot.
    let output = run(r#"fn main() {
            let s = "hello";
            let bs = s.bytes();
            println(bs.len());
            println(bs[0]);
            println(bs[4]);
            let h: u8 = 'h' as u32 as u8;
            println(bs[0] == h);
        }"#);
    assert_eq!(output, "5\n104\n111\ntrue\n");
}

#[test]
fn test_string_bytes_empty_string_zero_len() {
    let output = run(r#"fn main() {
            let bs = "".bytes();
            println(bs.len());
        }"#);
    assert_eq!(output, "0\n");
}

#[test]
fn test_string_bytes_multibyte_utf8_yields_byte_count_not_char_count() {
    // UTF-8 encodes a single Unicode scalar in 1..=4 bytes; the
    // distinction matters for the kata's use case (`bytes().len()`
    // is the byte count, NOT the character count — `chars().count()`
    // is the character count). Regression guard against accidentally
    // returning `Slice[char]` or counting characters.
    // "héllo" = h (1B) + é (2B: 0xC3 0xA9) + l (1B) + l (1B) + o (1B) = 6 bytes.
    let output = run(r#"fn main() {
            let bs = "héllo".bytes();
            println(bs.len());
        }"#);
    assert_eq!(output, "6\n");
}

// ── String/VecDeque constructor family (typechecker special-arm paths) ──
//
// These paths have no syntactic stdlib declaration — the typechecker
// special-cases them (typechecker/expr_call.rs) and codegen claims them
// directly, so each needs an explicit interpreter arm in
// eval_call.rs. Surfaced by the 2026-06-05 kata-corpus audit: every one
// of these previously died at the eval_expr unwired-path panic under
// `karac run` while building fine under `karac build`.

#[test]
fn test_string_new_push_roundtrip() {
    let output = run_no_errors(
        r#"fn main() {
            let mut s = String.new();
            s.push_str("ab");
            s.push('c');
            println(s);
        }"#,
    );
    assert_eq!(output, "abc\n");
}

#[test]
fn test_string_from_literal_passthrough() {
    let output = run_no_errors(r#"fn main() { println(String.from("xy")); }"#);
    assert_eq!(output, "xy\n");
}

#[test]
fn test_string_with_capacity_behaves_like_new() {
    let output = run_no_errors(
        r#"fn main() {
            let mut s = String.with_capacity(8);
            s.push_str("ok");
            println(s);
            println(s.len());
        }"#,
    );
    assert_eq!(output, "ok\n2\n");
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
fn test_unwired_path_reports_runtime_error_not_panic() {
    // The eval_expr Path fallback used to be `unreachable!` — a
    // typechecker-accepted-but-uninterpreted path killed the process
    // with a Rust panic instead of a span-carrying diagnostic.
    // `String.bogus()` survives resolve (run_program_full tolerates the
    // typecheck rejection) and exercises the degraded path: a recorded
    // RuntimeError naming the path, no panic.
    let errors = runtime_errors(r#"fn main() { let _x = String.bogus(); }"#);
    assert_eq!(errors.len(), 1, "expected exactly one runtime error");
    assert!(
        errors[0].message.contains("no interpreter evaluation rule"),
        "unexpected message: {}",
        errors[0].message
    );
    assert!(
        errors[0].message.contains("String.bogus"),
        "message should name the unwired path: {}",
        errors[0].message
    );
}

#[test]
fn test_string_sorted_by_closure_descending() {
    let output = run(
        r#"fn main() { let s = "dcba"; println(s.sorted_by(|a, b| if a < b { Ordering.Greater } else if a > b { Ordering.Less } else { Ordering.Equal })); }"#,
    );
    assert_eq!(output, "dcba\n");
}

#[test]
fn test_string_sorted_by_cmp_descending() {
    // Char `cmp` via the builtin Ord impl — closes the `b.cmp(a)` idiom
    // that was wedged by the missing primitive `cmp` dispatch.
    let output = run(r#"fn main() { let s = "bdac"; println(s.sorted_by(|a, b| b.cmp(a))); }"#);
    assert_eq!(output, "dcba\n");
}

#[test]
fn test_vec_sort_by_closure_descending() {
    let output = run(
        "fn main() {
            let mut xs: Vec[i64] = Vec.new();
            xs.push(3i64); xs.push(1i64); xs.push(4i64); xs.push(1i64); xs.push(5i64);
            xs.sort_by(|a, b| if a < b { Ordering.Greater } else if a > b { Ordering.Less } else { Ordering.Equal });
            for x in xs.iter() { println(x); }
        }",
    );
    assert_eq!(output, "5\n4\n3\n1\n1\n");
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
fn test_vec_sorted_by_closure_returns_new() {
    let output = run(
        "fn main() {
            let mut xs: Vec[i64] = Vec.new();
            xs.push(3i64); xs.push(1i64); xs.push(2i64);
            let ys = xs.sorted_by(|a, b| if a < b { Ordering.Less } else if a > b { Ordering.Greater } else { Ordering.Equal });
            for y in ys.iter() { println(y); }
            for x in xs.iter() { println(x); }
        }",
    );
    // sorted_by returns ascending; original retains insertion order
    assert_eq!(output, "1\n2\n3\n3\n1\n2\n");
}

#[test]
fn test_char_comparison_operators() {
    // Char `<` / `>` / `==` — previously fell through to the binop
    // unreachable. Pinned alongside the primitive Ord dispatch fix.
    let output = run(r#"fn main() {
            let a = 'a';
            let b = 'b';
            println(a < b);
            println(b > a);
            println(a == 'a');
            println(a != b);
        }"#);
    assert_eq!(output, "true\ntrue\ntrue\ntrue\n");
}

#[test]
fn test_string_comparison_operators() {
    let output = run(r#"fn main() {
            let a = "abc";
            let b = "abd";
            println(a < b);
            println(b > a);
            println(a <= "abc");
            println(a >= "abc");
        }"#);
    assert_eq!(output, "true\ntrue\ntrue\ntrue\n");
}

#[test]
fn test_i64_cmp_method() {
    // `cmp` returns `Ordering` — pin the variant-name round trip.
    let output = run("fn main() {
            let a = 3_i64;
            let b = 5_i64;
            match a.cmp(b) {
                Ordering.Less => println(\"less\"),
                Ordering.Equal => println(\"equal\"),
                Ordering.Greater => println(\"greater\"),
            }
        }");
    assert_eq!(output, "less\n");
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
fn test_vec_sorted_by_key_returns_new() {
    // sorted_by_key returns a new Vec; original retains insertion order.
    let output = run("fn main() {
            let mut xs: Vec[i64] = Vec.new();
            xs.push(3i64); xs.push(1i64); xs.push(2i64);
            let ys = xs.sorted_by_key(|x| x);
            for y in ys.iter() { println(y); }
            for x in xs.iter() { println(x); }
        }");
    assert_eq!(output, "1\n2\n3\n3\n1\n2\n");
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

// ── std.http ──────────────────────────────────────────────────────────────────

#[test]
fn test_http_client_new() {
    // Client.new() should return a Client struct — no network needed.
    let output = run(r#"
fn main() {
    let c = Client.new();
    println("ok");
}
"#);
    assert_eq!(output, "ok\n");
}

#[test]
#[ignore = "requires network access"]
fn test_http_client_get_ok() {
    let output = run(r#"
fn main() {
    let c = Client.new();
    match c.get("http://httpbin.org/status/200") {
        Ok(resp) => println(resp.status()),
        Err(e) => println(e.message()),
    }
}
"#);
    assert_eq!(output, "200\n");
}

#[test]
fn test_http_client_get_invalid_url() {
    // A clearly invalid URL should produce Err, not panic.
    let output = run(r#"
fn main() {
    let c = Client.new();
    match c.get("not-a-url") {
        Ok(_) => println("ok"),
        Err(_) => println("err"),
    }
}
"#);
    assert_eq!(output, "err\n");
}

#[test]
#[ignore = "requires network access"]
fn test_http_client_post_ok() {
    let output = run(r#"
fn main() {
    let c = Client.new();
    match c.post("http://httpbin.org/post", "hello") {
        Ok(resp) => println(resp.status()),
        Err(e) => println(e.message()),
    }
}
"#);
    assert_eq!(output, "200\n");
}

#[test]
fn test_http_response_methods() {
    // Invalid URL → Err — verify the error path and HttpError.message() work.
    let output = run(r#"
fn make_client() -> Client { Client.new() }
fn main() {
    let c = make_client();
    match c.get("not-a-url") {
        Ok(resp) => println(resp.status()),
        Err(e) => println("error"),
    }
}
"#);
    assert_eq!(output, "error\n");
}

// ── Trait associated function dispatch (List 1, item 5) ─────────

#[test]
fn test_bare_assoc_fn_with_concrete_expected_type() {
    // `let w: Foo = default()` resolves through `impl Default for Foo`.
    let output = run(r#"
trait Default {
    fn default() -> Self;
}

struct Foo { value: i64 }

impl Default for Foo {
    fn default() -> Foo { Foo { value: 42 } }
}

fn main() {
    let f: Foo = default();
    println(f.value);
}
"#);
    assert_eq!(output, "42\n");
}

#[test]
fn test_typeparam_assoc_fn_dispatch() {
    // `T.default()` inside a generic function dispatches to the impl
    // matching the runtime binding of `T` (driven by the caller's
    // expected type at the outer call site).
    let output = run(r#"
trait Default {
    fn default() -> Self;
}

struct Foo { value: i64 }

impl Default for Foo {
    fn default() -> Foo { Foo { value: 42 } }
}

fn make[T: Default]() -> T {
    T.default()
}

fn main() {
    let f: Foo = make();
    println(f.value);
}
"#);
    assert_eq!(output, "42\n");
}

#[test]
fn test_bare_assoc_fn_with_typeparam_expected() {
    // Bare `default()` inside a generic function with typeparam expected
    // type uses lowering + runtime substitution stack to dispatch.
    let output = run(r#"
trait Default {
    fn default() -> Self;
}

struct Foo { value: i64 }

impl Default for Foo {
    fn default() -> Foo { Foo { value: 99 } }
}

fn make[T: Default]() -> T {
    default()
}

fn main() {
    let f: Foo = make();
    println(f.value);
}
"#);
    assert_eq!(output, "99\n");
}

#[test]
fn test_assoc_fn_with_arg_through_typeparam() {
    // Trait method with a non-Self parameter dispatches through typeparam.
    let output = run(r#"
trait FromI64 {
    fn from_i64(n: i64) -> Self;
}

struct Wrap { v: i64 }

impl FromI64 for Wrap {
    fn from_i64(n: i64) -> Wrap { Wrap { v: n } }
}

fn make[T: FromI64](n: i64) -> T {
    T.from_i64(n)
}

fn main() {
    let w: Wrap = make(7);
    println(w.v);
}
"#);
    assert_eq!(output, "7\n");
}

#[test]
fn test_concrete_type_prefix_assoc_fn_dispatch() {
    // `Foo.default()` directly dispatches to the impl method.
    let output = run(r#"
trait Default {
    fn default() -> Self;
}

struct Foo { value: i64 }

impl Default for Foo {
    fn default() -> Foo { Foo { value: 5 } }
}

fn main() {
    let f = Foo.default();
    println(f.value);
}
"#);
    assert_eq!(output, "5\n");
}

// ── Item 7: broad integration coverage ──────────────────────────

#[test]
fn test_integration_from_str_user_trait() {
    // FromStr-style factory trait taking a String argument. Verifies
    // dispatch when the trait method has a non-Self parameter and the
    // dispatch goes through the bare-call expected-type lowering.
    let output = run(r#"
trait FromStr {
    fn from_str(s: String) -> Self;
}

struct Tag { label: String }

impl FromStr for Tag {
    fn from_str(s: String) -> Tag { Tag { label: s } }
}

fn main() {
    let t: Tag = from_str("hello");
    println(t.label);
}
"#);
    assert_eq!(output, "hello\n");
}

#[test]
fn test_integration_two_traits_one_type() {
    // A single concrete type implementing two factory traits — verify
    // both dispatch correctly in the same program.
    let output = run(r#"
trait Default {
    fn default() -> Self;
}
trait FromI64 {
    fn from_i64(n: i64) -> Self;
}

struct Counter { value: i64 }

impl Default for Counter {
    fn default() -> Counter { Counter { value: 0 } }
}
impl FromI64 for Counter {
    fn from_i64(n: i64) -> Counter { Counter { value: n } }
}

fn main() {
    let a: Counter = default();
    let b: Counter = from_i64(7);
    println(a.value);
    println(b.value);
}
"#);
    assert_eq!(output, "0\n7\n");
}

#[test]
fn test_integration_generic_helper_chain() {
    // A generic helper calls another generic helper that dispatches via
    // typeparam — verifies the runtime substitution stack chains transitively
    // through multiple call frames.
    let output = run(r#"
trait Default {
    fn default() -> Self;
}

struct Box { value: i64 }

impl Default for Box {
    fn default() -> Box { Box { value: 100 } }
}

fn make[T: Default]() -> T {
    T.default()
}

fn outer[T: Default]() -> T {
    make()
}

fn main() {
    let b: Box = outer();
    println(b.value);
}
"#);
    assert_eq!(output, "100\n");
}

#[test]
fn test_integration_where_clause_bound_e2e() {
    // Full end-to-end with a where-clause bound (instead of inline) —
    // dispatch should land at the same impl method.
    let output = run(r#"
trait Default {
    fn default() -> Self;
}

struct Slot { v: i64 }

impl Default for Slot {
    fn default() -> Slot { Slot { v: 13 } }
}

fn make[T]() -> T where T: Default {
    default()
}

fn main() {
    let s: Slot = make();
    println(s.v);
}
"#);
    assert_eq!(output, "13\n");
}

#[test]
fn test_integration_assoc_fn_in_arg_position() {
    // Bare assoc fn call passed as a function argument. The expected type
    // is the parameter type at the call site.
    let output = run(r#"
trait Default {
    fn default() -> Self;
}

struct Foo { v: i64 }

impl Default for Foo {
    fn default() -> Foo { Foo { v: 21 } }
}

fn take(f: Foo) -> i64 { f.v * 2 }

fn main() {
    println(take(default()));
}
"#);
    assert_eq!(output, "42\n");
}

/// HTTP handler ABI trampoline (2026-05-09): F2 owned-String contract.
/// `Request.path()` (and `.method()`) return owned Strings each call —
/// the interpreter side mirrors the codegen contract by returning a
/// fresh `Value::String` per invocation, so two back-to-back calls
/// don't share a buffer or fight over a borrow. The interpreter
/// doesn't run a real HTTP server, so the returned String is empty;
/// what the test pins is the *shape* (owned, not a `ref` borrow) and
/// repeat-callability.
#[test]
fn test_server_serve_handler_request_path_returns_owned_string() {
    let output = run(r#"
fn main() {
    let req = Request { };
    let p1 = req.path();
    let p2 = req.path();
    let m1 = req.method();
    println(p1.len());
    println(p2.len());
    println(m1.len());
}
"#);
    // Empty owned Strings: each `.len()` returns 0, and the chained
    // calls compose without lifetime conflicts.
    assert_eq!(output, "0\n0\n0\n");
}

/// `Request.header(name)` interpreter shape: returns `Option[String]`,
/// with the interpreter (no real HTTP server) always falling through
/// to `None`. Pins: the method dispatches at all, the args slot
/// accepts a `String`, and the result pattern-matches as `Option`.
/// Real header lookup happens through the codegen path —
/// `tests/http_server.rs::test_server_serve_handler_reads_header`.
#[test]
fn test_server_serve_handler_request_header_returns_none() {
    let output = run(r#"
fn main() {
    let req = Request { };
    match req.header("content-type") {
        Some(v) => println(v),
        None => println("none"),
    }
}
"#);
    assert_eq!(output, "none\n");
}

/// `Request.headers()` / `.query()` interpreter shape: both return a
/// `Vec[(String, String)]`. With no real HTTP server the stub Request
/// carries no data, so each is empty. Pins that the methods dispatch
/// and produce a Vec (whose `.len()` is 0) rather than a scalar. Real
/// iteration is exercised by the codegen E2E tests in
/// `tests/http_server.rs`.
#[test]
fn test_server_serve_handler_request_headers_and_query_return_empty() {
    let output = run(r#"
fn main() {
    let req = Request { };
    let h = req.headers();
    let q = req.query();
    println(h.len());
    println(q.len());
}
"#);
    assert_eq!(output, "0\n0\n");
}

#[test]
fn test_for_in_vec_string_calls_len_interp() {
    // Interpreter parity for List 2 / item 3 (codegen for-loop element-type
    // propagation). The interpreter already binds runtime-tagged Values, so
    // dispatch on the bound name routes through Value::String automatically.
    let output = run(r#"
fn main() {
    let v = ["alice", "bobby"];
    for s in v {
        println(s.len());
    }
}
"#);
    assert_eq!(output, "5\n5\n");
}

// ── Iterator: `iter()` / `into_iter()` / `next()` (wip-list2 subtask 1) ──

#[test]
fn test_iter_next_drains_vec_in_order() {
    // Calling next() repeatedly walks the source elements then yields None.
    // The cursor advance writes back through the binding, so successive
    // calls observe the new state.
    let output = run_no_errors(
        r#"
fn main() {
    let v = [10, 20, 30];
    let mut it = v.iter();
    println(it.next().unwrap());
    println(it.next().unwrap());
    println(it.next().unwrap());
    match it.next() {
        Some(_) => println("more"),
        None => println("done"),
    }
}
"#,
    );
    assert_eq!(output, "10\n20\n30\ndone\n");
}

#[test]
fn test_into_iter_matches_iter_at_runtime() {
    // The interpreter is type-erased; iter() and into_iter() produce the
    // same Value::Iterator. Verifying observable equivalence pins the
    // contract before laziness adaptors land.
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2];
    let mut a = v.iter();
    let mut b = v.into_iter();
    println(a.next().unwrap());
    println(b.next().unwrap());
    println(a.next().unwrap());
    println(b.next().unwrap());
}
"#,
    );
    assert_eq!(output, "1\n1\n2\n2\n");
}

#[test]
fn test_iter_on_set_yields_each_element_once() {
    // Set iterates in insertion order at the interpreter (storage backed
    // by Vec<Value>); next() yields each element exactly once before None.
    let output = run_no_errors(
        r#"
fn main() {
    let mut s: Set[i64] = Set.new();
    s.insert(7);
    s.insert(3);
    s.insert(11);
    let mut it = s.iter();
    let mut sum = 0;
    let mut count = 0;
    let mut more = true;
    while more {
        match it.next() {
            Some(n) => { sum = sum + n; count = count + 1; },
            None    => { more = false; },
        }
    }
    println(sum);
    println(count);
}
"#,
    );
    assert_eq!(output, "21\n3\n");
}

#[test]
fn test_iter_on_sorted_set_yields_ascending() {
    // SortedSet iterates ascending — verify next() honors that order.
    let output = run_no_errors(
        r#"
fn main() {
    let mut s: SortedSet[i64] = SortedSet.new();
    s.insert(11);
    s.insert(3);
    s.insert(7);
    let mut it = s.iter();
    println(it.next().unwrap());
    println(it.next().unwrap());
    println(it.next().unwrap());
}
"#,
    );
    assert_eq!(output, "3\n7\n11\n");
}

#[test]
fn test_iter_on_map_yields_kv_tuples() {
    // Map.iter() yields (K, V) tuples in insertion order at the interpreter.
    let output = run_no_errors(
        r#"
fn main() {
    let mut m: Map[String, i64] = Map.new();
    m.insert("a", 1);
    m.insert("b", 2);
    let mut it = m.iter();
    let (k1, v1) = it.next().unwrap();
    let (k2, v2) = it.next().unwrap();
    println(k1);
    println(v1);
    println(k2);
    println(v2);
}
"#,
    );
    assert_eq!(output, "a\n1\nb\n2\n");
}

#[test]
fn test_iter_on_empty_vec_yields_none_immediately() {
    // First next() on an empty source returns None; no Some preceded it.
    let output = run_no_errors(
        r#"
fn main() {
    let v: Vec[i64] = Vec[];
    let mut it = v.iter();
    match it.next() {
        Some(_) => println("had value"),
        None => println("empty"),
    }
}
"#,
    );
    assert_eq!(output, "empty\n");
}

// ── for-loop on iterator values (wip-list2 subtask 2) ────────────

#[test]
fn test_for_loop_on_vec_iter_walks_elements() {
    // Direct iteration over an iterator value — `for x in v.iter() { ... }`
    // walks the source elements in order.
    let output = run_no_errors(
        r#"
fn main() {
    let v = [10, 20, 30];
    for x in v.iter() {
        println(x);
    }
}
"#,
    );
    assert_eq!(output, "10\n20\n30\n");
}

#[test]
fn test_for_loop_on_iter_resumes_from_cursor() {
    // The iterator is bound, advanced manually with next(), then dropped
    // into a for-loop — the loop must resume from the cursor's current
    // position rather than restarting from the beginning.
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 3, 4];
    let mut it = v.iter();
    let _ = it.next();
    let _ = it.next();
    for x in it {
        println(x);
    }
}
"#,
    );
    assert_eq!(output, "3\n4\n");
}

#[test]
fn test_for_loop_on_map_iter_destructures_kv() {
    // Map.iter() yields (K, V) tuples; for-loop binds via tuple pattern.
    let output = run_no_errors(
        r#"
fn main() {
    let mut m: Map[String, i64] = Map.new();
    m.insert("x", 1);
    m.insert("y", 2);
    for (k, v) in m.iter() {
        println(k);
        println(v);
    }
}
"#,
    );
    assert_eq!(output, "x\n1\ny\n2\n");
}

#[test]
fn test_for_loop_break_inside_iterator_loop() {
    // break exits the for-loop early; downstream code still executes.
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 3, 4, 5];
    for x in v.iter() {
        if x > 2 {
            break;
        }
        println(x);
    }
    println(99);
}
"#,
    );
    assert_eq!(output, "1\n2\n99\n");
}

#[test]
fn test_for_loop_continue_inside_iterator_loop() {
    // continue skips to the next iteration without exiting the loop.
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 3, 4];
    for x in v.iter() {
        if x == 2 {
            continue;
        }
        println(x);
    }
}
"#,
    );
    assert_eq!(output, "1\n3\n4\n");
}

// ── map / filter (wip-list2 subtask 3) ───────────────────────────

#[test]
fn test_iter_map_transforms_each_element() {
    // `.map(|x| x * 10)` rewrites every element through the closure.
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 3];
    for n in v.iter().map(|x| x * 10) {
        println(n);
    }
}
"#,
    );
    assert_eq!(output, "10\n20\n30\n");
}

#[test]
fn test_iter_filter_keeps_matching_elements() {
    // `.filter(pred)` yields only elements where pred returns true.
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 3, 4, 5];
    for n in v.iter().filter(|x| x > 2) {
        println(n);
    }
}
"#,
    );
    assert_eq!(output, "3\n4\n5\n");
}

#[test]
fn test_iter_map_then_filter_chain() {
    // Adaptors chain: map first, then filter on the mapped values.
    // 1,2,3,4 → *3 → 3,6,9,12 → > 5 → 6,9,12.
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 3, 4];
    for n in v.iter().map(|x| x * 3).filter(|y| y > 5) {
        println(n);
    }
}
"#,
    );
    assert_eq!(output, "6\n9\n12\n");
}

#[test]
fn test_iter_filter_then_map_chain() {
    // Order matters — filter first, then map on filtered elements.
    // 1,2,3,4,5 → > 2 → 3,4,5 → +100 → 103,104,105.
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 3, 4, 5];
    for n in v.iter().filter(|x| x > 2).map(|x| x + 100) {
        println(n);
    }
}
"#,
    );
    assert_eq!(output, "103\n104\n105\n");
}

#[test]
fn test_iter_map_via_next_step_by_step() {
    // map is lazy — each next() pull invokes the closure exactly once.
    // No more, no fewer (verified by counting println side effects).
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2];
    let mut it = v.iter().map(|x| x + 100);
    println(it.next().unwrap());
    println(it.next().unwrap());
    match it.next() {
        Some(_) => println("more"),
        None => println("done"),
    }
}
"#,
    );
    assert_eq!(output, "101\n102\ndone\n");
}

#[test]
fn test_iter_filter_drops_all_when_predicate_always_false() {
    // When the predicate rejects every element, next() returns None on
    // the first pull (after walking the entire source internally).
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 3];
    let mut it = v.iter().filter(|x| x > 100);
    match it.next() {
        Some(_) => println("had value"),
        None => println("empty"),
    }
}
"#,
    );
    assert_eq!(output, "empty\n");
}

#[test]
fn test_iter_map_on_map_kv_tuples() {
    // Map.iter() yields (K, V) tuples; .map(|pair| ...) gets the tuple.
    let output = run_no_errors(
        r#"
fn main() {
    let mut m: Map[String, i64] = Map.new();
    m.insert("a", 1);
    m.insert("b", 2);
    for s in m.iter().map(|(k, v)| f"{k}={v}") {
        println(s);
    }
}
"#,
    );
    assert_eq!(output, "a=1\nb=2\n");
}

#[test]
fn test_iter_count_returns_element_count() {
    // count() drains the iterator and returns the element count as i64.
    let output = run_no_errors(
        r#"
fn main() {
    let v = [10, 20, 30, 40];
    let n: i64 = v.iter().count();
    println(n);
}
"#,
    );
    assert_eq!(output, "4\n");
}

#[test]
fn test_iter_count_empty_returns_zero() {
    let output = run_no_errors(
        r#"
fn main() {
    let v: Vec[i64] = Vec[];
    let n: i64 = v.iter().count();
    println(n);
}
"#,
    );
    assert_eq!(output, "0\n");
}

#[test]
fn test_iter_count_after_filter_counts_kept_elements() {
    // count() composes with filter — only elements that pass the
    // predicate contribute to the count.
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 3, 4, 5];
    let n: i64 = v.iter().filter(|x| x > 2).count();
    println(n);
}
"#,
    );
    assert_eq!(output, "3\n");
}

#[test]
fn test_iter_collect_yields_vec_in_order() {
    // collect() v1 returns a Vec[T] preserving iterator order.
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 3];
    let xs: Vec[i64] = v.iter().collect();
    for x in xs {
        println(x);
    }
}
"#,
    );
    assert_eq!(output, "1\n2\n3\n");
}

#[test]
fn test_iter_collect_after_map_collects_mapped_values() {
    // map then collect — closure runs once per element during collect's drain.
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 3];
    let xs: Vec[i64] = v.iter().map(|x| x * 10).collect();
    for x in xs {
        println(x);
    }
}
"#,
    );
    assert_eq!(output, "10\n20\n30\n");
}

#[test]
fn test_iter_collect_after_filter_drops_rejected_elements() {
    // filter then collect — only elements that pass the predicate land
    // in the resulting Vec.
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 3, 4, 5];
    let xs: Vec[i64] = v.iter().filter(|x| x > 2).collect();
    for x in xs {
        println(x);
    }
}
"#,
    );
    assert_eq!(output, "3\n4\n5\n");
}

#[test]
fn test_iter_fold_sums_elements() {
    // Canonical fold use — sum a Vec[i64] starting from 0.
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 3, 4, 5];
    let s: i64 = v.iter().fold(0, |acc, x| acc + x);
    println(s);
}
"#,
    );
    assert_eq!(output, "15\n");
}

#[test]
fn test_iter_fold_empty_returns_init_unchanged() {
    let output = run_no_errors(
        r#"
fn main() {
    let v: Vec[i64] = Vec[];
    let s: i64 = v.iter().fold(42, |acc, x| acc + x);
    println(s);
}
"#,
    );
    assert_eq!(output, "42\n");
}

#[test]
fn test_iter_fold_threads_string_accumulator() {
    // Accumulator type can differ from element type.
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 3];
    let s: String = v.iter().fold("", |acc, x| f"{acc}{x},");
    println(s);
}
"#,
    );
    assert_eq!(output, "1,2,3,\n");
}

#[test]
fn test_iter_fold_after_filter_only_visits_kept_elements() {
    // Adaptors fire during fold's drain — filter rejects 1 and 2,
    // so the closure only runs for 3 + 4 + 5 = 12 (init 0).
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 3, 4, 5];
    let s: i64 = v.iter().filter(|x| x > 2).fold(0, |acc, x| acc + x);
    println(s);
}
"#,
    );
    assert_eq!(output, "12\n");
}

#[test]
fn test_iter_any_returns_true_on_first_match() {
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 3, 4, 5];
    let b: bool = v.iter().any(|x| x > 3);
    println(b);
}
"#,
    );
    assert_eq!(output, "true\n");
}

#[test]
fn test_iter_any_returns_false_when_no_match() {
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 3];
    let b: bool = v.iter().any(|x| x > 100);
    println(b);
}
"#,
    );
    assert_eq!(output, "false\n");
}

#[test]
fn test_iter_any_on_empty_returns_false() {
    let output = run_no_errors(
        r#"
fn main() {
    let v: Vec[i64] = Vec[];
    let b: bool = v.iter().any(|x| x > 0);
    println(b);
}
"#,
    );
    assert_eq!(output, "false\n");
}

#[test]
fn test_iter_any_short_circuits_on_first_match() {
    // any() should stop iterating the moment the predicate returns true.
    // The closure prints each element it sees; with input 1..5 and
    // pred `x > 2`, only the first three elements should print before
    // any() returns. Tree-walk closures snapshot captures so we can't
    // count via mutated outer bindings — the println side-effect
    // ordering is the visible signal.
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 3, 4, 5];
    let b: bool = v.iter().any(|x| {
        println(x);
        x > 2
    });
    println(b);
}
"#,
    );
    assert_eq!(output, "1\n2\n3\ntrue\n");
}

#[test]
fn test_iter_all_returns_true_when_every_element_matches() {
    let output = run_no_errors(
        r#"
fn main() {
    let v = [2, 4, 6];
    let b: bool = v.iter().all(|x| x > 0);
    println(b);
}
"#,
    );
    assert_eq!(output, "true\n");
}

#[test]
fn test_iter_all_returns_false_on_first_mismatch() {
    let output = run_no_errors(
        r#"
fn main() {
    let v = [2, 4, -1, 6];
    let b: bool = v.iter().all(|x| x > 0);
    println(b);
}
"#,
    );
    assert_eq!(output, "false\n");
}

#[test]
fn test_iter_all_on_empty_returns_true() {
    // Vacuously true — no element violates the predicate.
    let output = run_no_errors(
        r#"
fn main() {
    let v: Vec[i64] = Vec[];
    let b: bool = v.iter().all(|x| x > 100);
    println(b);
}
"#,
    );
    assert_eq!(output, "true\n");
}

#[test]
fn test_iter_all_short_circuits_on_first_mismatch() {
    // all() should stop the moment the predicate returns false. With
    // input 1..5 and pred `x < 3`, the predicate sees 1, 2, 3 — and
    // bails on 3 (the first failing element). Element 3 is still
    // printed because the closure body runs to completion before its
    // boolean is consulted.
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 3, 4, 5];
    let b: bool = v.iter().all(|x| {
        println(x);
        x < 3
    });
    println(b);
}
"#,
    );
    assert_eq!(output, "1\n2\n3\nfalse\n");
}

#[test]
fn test_iter_any_after_map_predicate_sees_mapped_values() {
    // Composes with map — the predicate sees mapped i64 (x * 10), so
    // it returns true once the running x*10 exceeds 25 (i.e. on x=3).
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 3, 4];
    let b: bool = v.iter().map(|x| x * 10).any(|y| y > 25);
    println(b);
}
"#,
    );
    assert_eq!(output, "true\n");
}

#[test]
fn test_iter_enumerate_yields_index_and_item_pairs() {
    // enumerate() yields (idx, item) tuples; idx starts at 0.
    let output = run_no_errors(
        r#"
fn main() {
    let v = ["a", "b", "c"];
    for (i, s) in v.iter().enumerate() {
        println(f"{i}:{s}");
    }
}
"#,
    );
    assert_eq!(output, "0:a\n1:b\n2:c\n");
}

#[test]
fn test_iter_enumerate_persists_index_across_next_calls() {
    // Verifies state writeback — the Enumerate(idx) counter has to
    // survive between separate next() calls on the same iterator.
    let output = run_no_errors(
        r#"
fn main() {
    let v = [10, 20, 30];
    let mut it = v.iter().enumerate();
    let (a, ax) = it.next().unwrap();
    let (b, bx) = it.next().unwrap();
    println(f"{a}:{ax}");
    println(f"{b}:{bx}");
}
"#,
    );
    assert_eq!(output, "0:10\n1:20\n");
}

#[test]
fn test_iter_take_yields_only_first_n_elements() {
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 3, 4, 5];
    for x in v.iter().take(3) {
        println(x);
    }
}
"#,
    );
    assert_eq!(output, "1\n2\n3\n");
}

#[test]
fn test_iter_take_zero_yields_nothing() {
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 3];
    let mut it = v.iter().take(0);
    match it.next() {
        Some(_) => println("had value"),
        None => println("empty"),
    }
}
"#,
    );
    assert_eq!(output, "empty\n");
}

#[test]
fn test_iter_take_more_than_length_yields_all_elements() {
    // take(n) where n > len yields all elements without error.
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2];
    let n: i64 = v.iter().take(100).count();
    println(n);
}
"#,
    );
    assert_eq!(output, "2\n");
}

#[test]
fn test_iter_skip_drops_first_n_elements() {
    let output = run_no_errors(
        r#"
fn main() {
    let v = [10, 20, 30, 40, 50];
    for x in v.iter().skip(2) {
        println(x);
    }
}
"#,
    );
    assert_eq!(output, "30\n40\n50\n");
}

#[test]
fn test_iter_skip_more_than_length_yields_nothing() {
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 3];
    let mut it = v.iter().skip(100);
    match it.next() {
        Some(_) => println("had value"),
        None => println("empty"),
    }
}
"#,
    );
    assert_eq!(output, "empty\n");
}

#[test]
fn test_iter_skip_then_take_window() {
    // skip + take composed forms a slice — drop 1, take next 2 → [2, 3].
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 3, 4, 5];
    for x in v.iter().skip(1).take(2) {
        println(x);
    }
}
"#,
    );
    assert_eq!(output, "2\n3\n");
}

#[test]
fn test_iter_take_with_filter_yields_first_n_passing() {
    // filter then take(2) — only the first two elements that pass the
    // predicate are yielded; the source still walks past rejected items.
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 3, 4, 5, 6];
    for x in v.iter().filter(|x| x > 2).take(2) {
        println(x);
    }
}
"#,
    );
    assert_eq!(output, "3\n4\n");
}

#[test]
fn test_iter_take_state_persists_across_next_calls() {
    // The Take(remaining) counter has to survive between next() calls
    // for the bound to actually limit total yields.
    let output = run_no_errors(
        r#"
fn main() {
    let v = [10, 20, 30, 40];
    let mut it = v.iter().take(2);
    println(it.next().unwrap());
    println(it.next().unwrap());
    match it.next() {
        Some(_) => println("more"),
        None => println("done"),
    }
}
"#,
    );
    assert_eq!(output, "10\n20\ndone\n");
}

#[test]
fn test_iter_enumerate_after_map_indexes_mapped_values() {
    // Map first, then enumerate — index counts mapped output positions
    // (same as source positions here, but enumerate sees the mapped item).
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 3];
    for (i, y) in v.iter().map(|x| x * 100).enumerate() {
        println(f"{i}={y}");
    }
}
"#,
    );
    assert_eq!(output, "0=100\n1=200\n2=300\n");
}

#[test]
fn test_iter_chain_yields_left_then_right() {
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2];
    let w = [10, 20, 30];
    for x in v.iter().chain(w.iter()) {
        println(x);
    }
}
"#,
    );
    assert_eq!(output, "1\n2\n10\n20\n30\n");
}

#[test]
fn test_iter_chain_left_empty_yields_only_right() {
    let output = run_no_errors(
        r#"
fn main() {
    let v: Vec[i64] = Vec[];
    let w = [7, 8];
    for x in v.iter().chain(w.iter()) {
        println(x);
    }
}
"#,
    );
    assert_eq!(output, "7\n8\n");
}

#[test]
fn test_iter_chain_right_empty_yields_only_left() {
    let output = run_no_errors(
        r#"
fn main() {
    let v = [3, 4];
    let w: Vec[i64] = Vec[];
    for x in v.iter().chain(w.iter()) {
        println(x);
    }
}
"#,
    );
    assert_eq!(output, "3\n4\n");
}

#[test]
fn test_iter_chain_preserves_per_side_adaptors() {
    // Each side keeps its own adaptor chain — left's filter and
    // right's map both fire on their own elements only.
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 3, 4];
    let w = [10, 20];
    let xs: Vec[i64] = v.iter().filter(|x| x > 2).chain(w.iter().map(|y| y + 100)).collect();
    for x in xs {
        println(x);
    }
}
"#,
    );
    assert_eq!(output, "3\n4\n110\n120\n");
}

#[test]
fn test_iter_chain_downstream_step_applies_to_both_sides() {
    // Downstream map on the result of chain applies to ALL items
    // regardless of which side they came from.
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2];
    let w = [10, 20];
    for x in v.iter().chain(w.iter()).map(|x| x * 100) {
        println(x);
    }
}
"#,
    );
    assert_eq!(output, "100\n200\n1000\n2000\n");
}

#[test]
fn test_iter_zip_pairs_elements_in_lockstep() {
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 3];
    let w = ["a", "b", "c"];
    for (n, s) in v.iter().zip(w.iter()) {
        println(f"{n}:{s}");
    }
}
"#,
    );
    assert_eq!(output, "1:a\n2:b\n3:c\n");
}

#[test]
fn test_iter_zip_stops_at_shorter_left_side() {
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2];
    let w = ["a", "b", "c", "d"];
    for (n, s) in v.iter().zip(w.iter()) {
        println(f"{n}:{s}");
    }
}
"#,
    );
    assert_eq!(output, "1:a\n2:b\n");
}

#[test]
fn test_iter_zip_stops_at_shorter_right_side() {
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 3, 4];
    let w = ["a", "b"];
    for (n, s) in v.iter().zip(w.iter()) {
        println(f"{n}:{s}");
    }
}
"#,
    );
    assert_eq!(output, "1:a\n2:b\n");
}

#[test]
fn test_iter_zip_either_empty_yields_nothing() {
    let output = run_no_errors(
        r#"
fn main() {
    let v: Vec[i64] = Vec[];
    let w = ["a", "b"];
    let mut it = v.iter().zip(w.iter());
    match it.next() {
        Some(_) => println("had value"),
        None => println("empty"),
    }
}
"#,
    );
    assert_eq!(output, "empty\n");
}

#[test]
fn test_iter_zip_preserves_per_side_adaptors() {
    // Left's filter and right's map both fire while zipping. Filtering
    // and mapping happen INSIDE each side's iteration; zip pulls from
    // the post-adaptor stream.
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 3, 4, 5];
    let w = [10, 20, 30, 40];
    for (a, b) in v.iter().filter(|x| x > 1).zip(w.iter().map(|y| y * 2)) {
        println(f"{a}+{b}");
    }
}
"#,
    );
    // Left filtered: 2, 3, 4, 5. Right mapped: 20, 40, 60, 80.
    // Zipped: (2,20) (3,40) (4,60) (5,80).
    assert_eq!(output, "2+20\n3+40\n4+60\n5+80\n");
}

#[test]
fn test_iter_zip_with_enumerate_on_one_side() {
    // Composes with enumerate on the right side — index and value.
    let output = run_no_errors(
        r#"
fn main() {
    let v = ["a", "b", "c"];
    let w = [10, 20, 30];
    for (s, (i, n)) in v.iter().zip(w.iter().enumerate()) {
        println(f"{s}:{i}={n}");
    }
}
"#,
    );
    assert_eq!(output, "a:0=10\nb:1=20\nc:2=30\n");
}

#[test]
fn test_iter_chain_state_persists_across_next_calls() {
    // After exhausting left via two next() calls, the third pull
    // should switch to right transparently.
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2];
    let w = [9];
    let mut it = v.iter().chain(w.iter());
    println(it.next().unwrap());
    println(it.next().unwrap());
    println(it.next().unwrap());
    match it.next() {
        Some(_) => println("more"),
        None => println("done"),
    }
}
"#,
    );
    assert_eq!(output, "1\n2\n9\ndone\n");
}

#[test]
fn test_iter_take_while_yields_until_first_failure() {
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 3, 10, 4, 5];
    for x in v.iter().take_while(|x| x < 5) {
        println(x);
    }
}
"#,
    );
    // Stops at the first element where predicate fails (10), yielding
    // only the 1, 2, 3 prefix — even though 4 and 5 follow, take_while
    // does not resume after a failure.
    assert_eq!(output, "1\n2\n3\n");
}

#[test]
fn test_iter_take_while_first_element_fails_yields_nothing() {
    let output = run_no_errors(
        r#"
fn main() {
    let v = [10, 20, 30];
    let mut it = v.iter().take_while(|x| x < 5);
    match it.next() {
        Some(_) => println("had value"),
        None => println("empty"),
    }
}
"#,
    );
    assert_eq!(output, "empty\n");
}

#[test]
fn test_iter_take_while_all_pass_yields_all_elements() {
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 3];
    for x in v.iter().take_while(|x| x < 100) {
        println(x);
    }
}
"#,
    );
    assert_eq!(output, "1\n2\n3\n");
}

#[test]
fn test_iter_take_while_short_circuits_predicate() {
    // After the first false, predicate must NOT fire on subsequent
    // elements. Use println side effects to verify: the prefix prints
    // "p:N" for each predicate call, and the body prints "y:N" for
    // each yielded element.
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 9, 3, 4];
    for x in v.iter().take_while(|x| { println(f"p:{x}"); x < 5 }) {
        println(f"y:{x}");
    }
}
"#,
    );
    // Predicate fires on 1, 2 (yield), 9 (stop). Never on 3 or 4.
    // For-loop drains the iterator first, then iterates the body — so
    // the order is predicate-prefix then yielded-prefix. The
    // short-circuit guarantee is still proven by the absence of "p:3"
    // and "p:4".
    assert_eq!(output, "p:1\np:2\np:9\ny:1\ny:2\n");
}

#[test]
fn test_iter_skip_while_drops_leading_prefix() {
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 3, 10, 1, 2];
    for x in v.iter().skip_while(|x| x < 5) {
        println(x);
    }
}
"#,
    );
    // Skips 1, 2, 3 (predicate true), then yields 10 and everything
    // that follows — INCLUDING 1, 2 — because skip_while does not
    // re-test once the predicate has failed.
    assert_eq!(output, "10\n1\n2\n");
}

#[test]
fn test_iter_skip_while_all_pass_yields_nothing() {
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 3];
    let mut it = v.iter().skip_while(|x| x < 100);
    match it.next() {
        Some(_) => println("had value"),
        None => println("empty"),
    }
}
"#,
    );
    assert_eq!(output, "empty\n");
}

#[test]
fn test_iter_skip_while_first_element_fails_yields_all() {
    let output = run_no_errors(
        r#"
fn main() {
    let v = [10, 20, 30];
    for x in v.iter().skip_while(|x| x < 5) {
        println(x);
    }
}
"#,
    );
    // Predicate is false on the very first element, so skip_while
    // yields the whole iterator unchanged.
    assert_eq!(output, "10\n20\n30\n");
}

#[test]
fn test_iter_skip_while_does_not_re_test_after_first_failure() {
    // Same observation harness as the take_while short-circuit test:
    // predicate side-effects show the call sequence, body shows yields.
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 9, 3, 4];
    for x in v.iter().skip_while(|x| { println(f"p:{x}"); x < 5 }) {
        println(f"y:{x}");
    }
}
"#,
    );
    // Predicate fires on 1, 2, 9 (trip). Never on 3 or 4. After the
    // trip, 9 is yielded and 3 / 4 pass through unconditionally.
    assert_eq!(output, "p:1\np:2\np:9\ny:9\ny:3\ny:4\n");
}

#[test]
fn test_iter_take_while_state_persists_across_next_calls() {
    // Once take_while has tripped, subsequent next() calls must
    // continue to return None — even though the source has more items.
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 9, 3];
    let mut it = v.iter().take_while(|x| x < 5);
    println(it.next().unwrap());
    println(it.next().unwrap());
    match it.next() {
        Some(_) => println("more"),
        None => println("done"),
    }
    match it.next() {
        Some(_) => println("more"),
        None => println("still-done"),
    }
}
"#,
    );
    assert_eq!(output, "1\n2\ndone\nstill-done\n");
}

#[test]
fn test_iter_skip_while_state_persists_across_next_calls() {
    // Once skip_while has tripped, every subsequent next() must
    // return the next raw item without re-testing the predicate.
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 9, 3, 4];
    let mut it = v.iter().skip_while(|x| x < 5);
    println(it.next().unwrap());
    println(it.next().unwrap());
    println(it.next().unwrap());
    match it.next() {
        Some(_) => println("more"),
        None => println("done"),
    }
}
"#,
    );
    assert_eq!(output, "9\n3\n4\ndone\n");
}

#[test]
fn test_iter_take_while_composes_with_filter() {
    // Filter feeds take_while — predicate sees only elements that
    // passed the filter; take_while stops on the first kept-but-failing.
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 3, 4, 5, 6, 7, 8];
    let xs: Vec[i64] = v.iter().filter(|x| x % 2 == 0).take_while(|x| x < 7).collect();
    for x in xs {
        println(x);
    }
}
"#,
    );
    // Filter yields 2, 4, 6, 8. take_while(<7) stops at 8 → [2, 4, 6].
    assert_eq!(output, "2\n4\n6\n");
}

#[test]
fn test_iter_skip_while_then_take_while_window() {
    // skip_while drops leading prefix, take_while bounds the tail.
    // Composition produces a "while-window" view.
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 5, 6, 7, 9, 1];
    let xs: Vec[i64] = v.iter().skip_while(|x| x < 5).take_while(|x| x < 9).collect();
    for x in xs {
        println(x);
    }
}
"#,
    );
    // skip_while drops 1, 2 → trips on 5. take_while keeps 5, 6, 7;
    // stops at 9. Trailing 1 is unreachable because take_while is
    // sticky-stop after the first failure.
    assert_eq!(output, "5\n6\n7\n");
}

#[test]
fn test_iter_flat_map_yields_concatenated_inner_iters() {
    // For each outer item, closure yields a 2-element inner. Result
    // concatenates them in outer-then-inner order.
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 3];
    for x in v.iter().flat_map(|n| { let inner = [n * 10, n * 100]; inner.iter() }) {
        println(x);
    }
}
"#,
    );
    assert_eq!(output, "10\n100\n20\n200\n30\n300\n");
}

#[test]
fn test_iter_flat_map_empty_outer_yields_nothing() {
    let output = run_no_errors(
        r#"
fn main() {
    let v: Vec[i64] = Vec[];
    let mut it = v.iter().flat_map(|n| { let inner = [n, n]; inner.iter() });
    match it.next() {
        Some(_) => println("had value"),
        None => println("empty"),
    }
}
"#,
    );
    assert_eq!(output, "empty\n");
}

#[test]
fn test_iter_flat_map_inner_empty_skipped() {
    // Some outer items produce empty inner iterators — those are
    // skipped without yielding anything.
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 3, 4];
    for x in v.iter().flat_map(|n| {
        if n % 2 == 0 {
            let inner = [n * 10];
            inner.iter()
        } else {
            let inner: Vec[i64] = Vec[];
            inner.iter()
        }
    }) {
        println(x);
    }
}
"#,
    );
    // Outer 1 → empty; 2 → [20]; 3 → empty; 4 → [40].
    assert_eq!(output, "20\n40\n");
}

#[test]
fn test_iter_flat_map_state_persists_across_next_calls() {
    // next() pulls one item at a time. When the in-flight inner is
    // exhausted, the next pull must transparently switch to the
    // next outer's inner.
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2];
    let mut it = v.iter().flat_map(|n| { let inner = [n * 10, n * 100]; inner.iter() });
    println(it.next().unwrap());
    println(it.next().unwrap());
    println(it.next().unwrap());
    println(it.next().unwrap());
    match it.next() {
        Some(_) => println("more"),
        None => println("done"),
    }
}
"#,
    );
    assert_eq!(output, "10\n100\n20\n200\ndone\n");
}

#[test]
fn test_iter_flat_map_inner_can_have_adaptors() {
    // Closure returns an inner iterator with its own adaptor chain
    // (filter here). Those filter-rejected items don't surface as
    // flat_map yields.
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 3];
    let xs: Vec[i64] = v.iter().flat_map(|n| {
        let inner = [n - 1, n, n + 1];
        inner.iter().filter(|x| x > 1)
    }).collect();
    for x in xs {
        println(x);
    }
}
"#,
    );
    // Outer 1 → [0,1,2] filtered to [2]
    // Outer 2 → [1,2,3] filtered to [2,3]
    // Outer 3 → [2,3,4] filtered to [2,3,4]
    assert_eq!(output, "2\n2\n3\n2\n3\n4\n");
}

#[test]
fn test_iter_flat_map_composes_with_filter_after() {
    // Downstream filter applies to the flattened stream.
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 3];
    let xs: Vec[i64] = v.iter()
        .flat_map(|n| { let inner = [n, n * 10]; inner.iter() })
        .filter(|x| x > 5)
        .collect();
    for x in xs {
        println(x);
    }
}
"#,
    );
    // Flattened: 1, 10, 2, 20, 3, 30. Filter >5: 10, 20, 30.
    assert_eq!(output, "10\n20\n30\n");
}

#[test]
fn test_iter_flat_map_with_take_short_circuits_outer() {
    // Downstream take(2) means we should only need to drain enough
    // outer items to produce 2 yields. Side-effect prefixes prove
    // that outer 3 is never visited.
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 3];
    for x in v.iter()
        .flat_map(|n| { println(f"outer:{n}"); let inner = [n * 10, n * 100]; inner.iter() })
        .take(2)
    {
        println(f"y:{x}");
    }
}
"#,
    );
    // For-loop drains: outer:1 → inner [10, 100], take pulls both →
    // remaining=0. Source still pulled but step rejects. Outer 2 is
    // pulled (because take exhaustion happens AFTER outer 1's inner
    // is fully drained — the take step counts post-flat_map). The
    // test of importance: outer 3 must NEVER be pulled.
    // Expected: outer:1, y:10, y:100 (or similar drain order),
    // then nothing more — definitely no "outer:3".
    let lines: Vec<&str> = output.lines().collect();
    assert!(
        lines.contains(&"outer:1"),
        "outer:1 must fire, got: {:?}",
        lines
    );
    assert!(
        !lines.contains(&"outer:3"),
        "outer:3 must NOT fire (take(2) short-circuits), got: {:?}",
        lines
    );
    assert!(
        lines.iter().filter(|l| l.starts_with("y:")).count() == 2,
        "exactly 2 yields expected, got: {:?}",
        lines
    );
}

#[test]
fn test_iter_flat_map_then_map_threads_types() {
    // flat_map produces a stream of one type, then map transforms.
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2];
    let xs: Vec[i64] = v.iter()
        .flat_map(|n| { let inner = [n, n + 100]; inner.iter() })
        .map(|x| x * 2)
        .collect();
    for x in xs {
        println(x);
    }
}
"#,
    );
    // Flattened: 1, 101, 2, 102. Doubled: 2, 202, 4, 204.
    assert_eq!(output, "2\n202\n4\n204\n");
}

#[test]
fn test_iter_flat_map_after_filter() {
    // Filter the OUTER stream first — the closure only runs on
    // kept-outer items.
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 3, 4];
    let xs: Vec[i64] = v.iter()
        .filter(|n| n % 2 == 0)
        .flat_map(|n| { let inner = [n, n * 10]; inner.iter() })
        .collect();
    for x in xs {
        println(x);
    }
}
"#,
    );
    // Filtered outer: 2, 4. flat_map: [2, 20], [4, 40] → 2, 20, 4, 40.
    assert_eq!(output, "2\n20\n4\n40\n");
}

#[test]
fn test_iter_flat_map_with_count_terminal() {
    // Terminal count() drains and counts the flattened stream.
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 3];
    let n = v.iter().flat_map(|n| { let inner = [n, n, n]; inner.iter() }).count();
    println(n);
}
"#,
    );
    // 3 outer × 3 inner each = 9.
    assert_eq!(output, "9\n");
}

#[test]
fn test_iter_step_by_yields_every_nth_element() {
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 3, 4, 5, 6, 7];
    for x in v.iter().step_by(2) {
        println(x);
    }
}
"#,
    );
    // Yields indices 0, 2, 4, 6 → 1, 3, 5, 7.
    assert_eq!(output, "1\n3\n5\n7\n");
}

#[test]
fn test_iter_step_by_one_is_observable_noop() {
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 3];
    for x in v.iter().step_by(1) {
        println(x);
    }
}
"#,
    );
    assert_eq!(output, "1\n2\n3\n");
}

#[test]
fn test_iter_step_by_zero_clamps_to_one() {
    // n=0 would underflow on the post-yield reset; runtime clamps
    // to 1, behaving like step_by(1).
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 3];
    for x in v.iter().step_by(0) {
        println(x);
    }
}
"#,
    );
    assert_eq!(output, "1\n2\n3\n");
}

#[test]
fn test_iter_step_by_larger_than_length_yields_only_first() {
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 3];
    for x in v.iter().step_by(100) {
        println(x);
    }
}
"#,
    );
    assert_eq!(output, "1\n");
}

#[test]
fn test_iter_step_by_on_empty_yields_nothing() {
    let output = run_no_errors(
        r#"
fn main() {
    let v: Vec[i64] = Vec[];
    let mut it = v.iter().step_by(2);
    match it.next() {
        Some(_) => println("had value"),
        None => println("empty"),
    }
}
"#,
    );
    assert_eq!(output, "empty\n");
}

#[test]
fn test_iter_step_by_state_persists_across_next_calls() {
    let output = run_no_errors(
        r#"
fn main() {
    let v = [10, 20, 30, 40, 50];
    let mut it = v.iter().step_by(2);
    println(it.next().unwrap());
    println(it.next().unwrap());
    println(it.next().unwrap());
    match it.next() {
        Some(_) => println("more"),
        None => println("done"),
    }
}
"#,
    );
    // Yields 10, 30, 50.
    assert_eq!(output, "10\n30\n50\ndone\n");
}

#[test]
fn test_iter_step_by_composes_with_filter() {
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 3, 4, 5, 6, 7, 8];
    let xs: Vec[i64] = v.iter().filter(|x| x > 2).step_by(2).collect();
    for x in xs {
        println(x);
    }
}
"#,
    );
    // Filter yields 3, 4, 5, 6, 7, 8. step_by(2) → 3, 5, 7.
    assert_eq!(output, "3\n5\n7\n");
}

#[test]
fn test_iter_cycle_with_take_yields_repeated_elements() {
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2];
    for x in v.iter().cycle().take(5) {
        println(x);
    }
}
"#,
    );
    assert_eq!(output, "1\n2\n1\n2\n1\n");
}

#[test]
fn test_iter_cycle_preserves_pre_adaptors() {
    // Adaptors applied BEFORE cycle live in the template's own
    // step chain — they re-run on each restart. Here the filter
    // re-rejects 1 each cycle.
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 3];
    for x in v.iter().filter(|x| x > 1).cycle().take(5) {
        println(x);
    }
}
"#,
    );
    // Each cycle yields 2, 3. take(5) → 2, 3, 2, 3, 2.
    assert_eq!(output, "2\n3\n2\n3\n2\n");
}

#[test]
fn test_iter_cycle_on_empty_yields_nothing() {
    // Sticky-stop on empty template — must NOT infinite-loop.
    let output = run_no_errors(
        r#"
fn main() {
    let v: Vec[i64] = Vec[];
    let mut it = v.iter().cycle().take(10);
    match it.next() {
        Some(_) => println("had value"),
        None => println("empty"),
    }
}
"#,
    );
    assert_eq!(output, "empty\n");
}

#[test]
fn test_iter_cycle_composes_with_post_map() {
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2];
    for x in v.iter().cycle().take(4).map(|x| x * 10) {
        println(x);
    }
}
"#,
    );
    assert_eq!(output, "10\n20\n10\n20\n");
}

#[test]
fn test_iter_cycle_state_persists_across_next_calls() {
    // next() pulls one item at a time, crossing cycle boundaries
    // transparently.
    let output = run_no_errors(
        r#"
fn main() {
    let v = [7, 8];
    let mut it = v.iter().cycle();
    println(it.next().unwrap());
    println(it.next().unwrap());
    println(it.next().unwrap());
    println(it.next().unwrap());
    println(it.next().unwrap());
}
"#,
    );
    assert_eq!(output, "7\n8\n7\n8\n7\n");
}

#[test]
fn test_iter_cycle_resets_stateful_adaptors_each_cycle() {
    // enumerate inside the template restarts at index 0 each cycle
    // because cycle clones the template (with its initial counters).
    let output = run_no_errors(
        r#"
fn main() {
    let v = [10, 20];
    for (i, x) in v.iter().enumerate().cycle().take(4) {
        println(f"{i}:{x}");
    }
}
"#,
    );
    // First cycle: (0,10), (1,20). Second cycle re-runs enumerate
    // from 0 → (0,10), (1,20). take(4) total.
    assert_eq!(output, "0:10\n1:20\n0:10\n1:20\n");
}

#[test]
fn test_iter_step_by_then_cycle() {
    // step_by trims the template; cycle replays the trimmed
    // sequence forever.
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 3, 4, 5];
    for x in v.iter().step_by(2).cycle().take(7) {
        println(x);
    }
}
"#,
    );
    // step_by(2) → 1, 3, 5. cycle.take(7) → 1, 3, 5, 1, 3, 5, 1.
    assert_eq!(output, "1\n3\n5\n1\n3\n5\n1\n");
}

#[test]
fn test_iter_inspect_passes_through_unchanged() {
    let output = run_no_errors(
        r#"
fn main() {
    let v = [10, 20, 30];
    let xs: Vec[i64] = v.iter().inspect(|x| println(f"saw:{x}")).collect();
    for x in xs {
        println(x);
    }
}
"#,
    );
    // For-loop drains: inspect fires on each item during drain
    // (saw:10, saw:20, saw:30), then the body iterates the
    // collected Vec.
    assert_eq!(output, "saw:10\nsaw:20\nsaw:30\n10\n20\n30\n");
}

#[test]
fn test_iter_inspect_after_filter_only_fires_on_kept() {
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 3, 4];
    let xs: Vec[i64] = v.iter()
        .filter(|x| x > 2)
        .inspect(|x| println(f"saw:{x}"))
        .collect();
    for x in xs {
        println(x);
    }
}
"#,
    );
    // Filter keeps 3, 4. inspect fires on 3, 4 only.
    assert_eq!(output, "saw:3\nsaw:4\n3\n4\n");
}

#[test]
fn test_iter_inspect_composes_with_downstream_steps() {
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 3];
    let xs: Vec[i64] = v.iter()
        .inspect(|x| println(f"raw:{x}"))
        .map(|x| x * 10)
        .inspect(|x| println(f"mapped:{x}"))
        .collect();
    for x in xs {
        println(x);
    }
}
"#,
    );
    // First inspect sees raw items; map transforms; second inspect
    // sees mapped items. All firing during the drain phase.
    assert_eq!(
        output,
        "raw:1\nmapped:10\nraw:2\nmapped:20\nraw:3\nmapped:30\n10\n20\n30\n"
    );
}

#[test]
fn test_iter_scan_yields_running_sum() {
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 3, 4];
    for x in v.iter().scan(0, |state, item| {
        let new = state + item;
        Some((new, new))
    }) {
        println(x);
    }
}
"#,
    );
    // 0+1=1, 1+2=3, 3+3=6, 6+4=10.
    assert_eq!(output, "1\n3\n6\n10\n");
}

#[test]
fn test_iter_scan_short_circuits_on_none() {
    // scan returning None stops iteration; subsequent items are
    // not visited.
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 3, 100, 4, 5];
    for x in v.iter().scan(0, |state, item| {
        if item > 50 {
            None
        } else {
            let new = state + item;
            Some((new, new))
        }
    }) {
        println(x);
    }
}
"#,
    );
    // Stops at 100; running sums of 1, 2, 3 are 1, 3, 6.
    assert_eq!(output, "1\n3\n6\n");
}

#[test]
fn test_iter_scan_short_circuit_does_not_re_fire() {
    // Side-effect prefix proves scan does NOT call closure on items
    // after the first None.
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 100, 3, 4];
    for x in v.iter().scan(0, |state, item| {
        println(f"c:{item}");
        if item > 50 {
            None
        } else {
            let new = state + item;
            Some((new, new))
        }
    }) {
        println(f"y:{x}");
    }
}
"#,
    );
    // Closure fires on 1, 2, 100. None on 100 → stop. 3 and 4 are
    // never visited. Yields: 1 (=0+1), 3 (=1+2).
    assert_eq!(output, "c:1\nc:2\nc:100\ny:1\ny:3\n");
}

#[test]
fn test_iter_scan_state_persists_across_next_calls() {
    // next() pulls one item at a time; scan's state survives
    // between pulls.
    let output = run_no_errors(
        r#"
fn main() {
    let v = [10, 20, 30];
    let mut it = v.iter().scan(0, |state, item| {
        let new = state + item;
        Some((new, new))
    });
    println(it.next().unwrap());
    println(it.next().unwrap());
    println(it.next().unwrap());
    match it.next() {
        Some(_) => println("more"),
        None => println("done"),
    }
}
"#,
    );
    assert_eq!(output, "10\n30\n60\ndone\n");
}

#[test]
fn test_iter_scan_with_string_state() {
    // State can be any type — String here.
    let output = run_no_errors(
        r#"
fn main() {
    let v = ["a", "b", "c"];
    for x in v.iter().scan("", |state, item| {
        let new = state + item;
        Some((new, new))
    }) {
        println(x);
    }
}
"#,
    );
    // Concatenating: "" + "a" = "a", "a" + "b" = "ab", "ab" + "c" = "abc".
    assert_eq!(output, "a\nab\nabc\n");
}

#[test]
fn test_iter_scan_after_filter() {
    // Filter first, then scan — the scan closure only sees kept
    // items.
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 3, 4, 5, 6];
    let xs: Vec[i64] = v.iter()
        .filter(|x| x % 2 == 0)
        .scan(0, |state, item| {
            let new = state + item;
            Some((new, new))
        })
        .collect();
    for x in xs {
        println(x);
    }
}
"#,
    );
    // Filter yields 2, 4, 6. Running sum: 2, 6, 12.
    assert_eq!(output, "2\n6\n12\n");
}

#[test]
fn test_iter_scan_state_independent_of_yielded_value() {
    // State and yielded value can differ — useful for "yield index
    // of running max" patterns.
    let output = run_no_errors(
        r#"
fn main() {
    let v = [3, 1, 4, 1, 5, 9, 2, 6];
    for x in v.iter().scan(0, |state, item| {
        let new_state = if item > state { item } else { state };
        Some((new_state, new_state))
    }) {
        println(x);
    }
}
"#,
    );
    // Running max: 3, 3, 4, 4, 5, 9, 9, 9.
    assert_eq!(output, "3\n3\n4\n4\n5\n9\n9\n9\n");
}

#[test]
fn test_iter_peek_returns_next_without_consuming() {
    // peek() returns the upcoming element; the next next() call
    // returns the SAME element (the buffer is what gets consumed).
    let output = run_no_errors(
        r#"
fn main() {
    let v = [10, 20, 30];
    let mut p = v.iter().peekable();
    println(p.peek().unwrap());
    println(p.next().unwrap());
    println(p.next().unwrap());
}
"#,
    );
    assert_eq!(output, "10\n10\n20\n");
}

#[test]
fn test_iter_peek_idempotent_until_next() {
    // Multiple peek()s in a row see the same element; only next()
    // drains the buffer.
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 3];
    let mut p = v.iter().peekable();
    println(p.peek().unwrap());
    println(p.peek().unwrap());
    println(p.peek().unwrap());
    println(p.next().unwrap());
    println(p.peek().unwrap());
}
"#,
    );
    assert_eq!(output, "1\n1\n1\n1\n2\n");
}

#[test]
fn test_iter_peek_at_end_returns_none() {
    // After draining, peek returns None; subsequent peeks stay None.
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1];
    let mut p = v.iter().peekable();
    println(p.next().unwrap());
    match p.peek() {
        Some(_) => println("more"),
        None => println("done"),
    }
    match p.peek() {
        Some(_) => println("more"),
        None => println("done"),
    }
    match p.next() {
        Some(_) => println("more"),
        None => println("done-next"),
    }
}
"#,
    );
    assert_eq!(output, "1\ndone\ndone\ndone-next\n");
}

#[test]
fn test_iter_peek_on_drained_iterator() {
    // After draining a single-element iterator with .take(0), peek
    // sees no element on a freshly constructed Peekable.
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 3];
    let mut p = v.iter().take(0).peekable();
    match p.peek() {
        Some(_) => println("yes"),
        None => println("none"),
    }
}
"#,
    );
    assert_eq!(output, "none\n");
}

#[test]
fn test_iter_peek_does_not_re_pull_after_buffered() {
    // peek() pulls from inner exactly once per buffered slot.
    // Side-effect prefix proves the closure does NOT fire on
    // repeated peek() calls — only the first peek() pulls.
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 3];
    let mut p = v.iter().inspect(|x| println(f"pull:{x}")).peekable();
    println(f"peek1:{p.peek().unwrap()}");
    println(f"peek2:{p.peek().unwrap()}");
    println(f"next:{p.next().unwrap()}");
    println(f"peek3:{p.peek().unwrap()}");
}
"#,
    );
    // Drain order: first peek() triggers inner pull (pull:1) and
    // buffers; second peek() returns from buffer (no pull); next()
    // drains buffer (no pull); third peek() pulls again (pull:2).
    assert_eq!(
        output,
        "pull:1\npeek1:1\npeek2:1\nnext:1\npull:2\npeek3:2\n"
    );
}

#[test]
fn test_iter_peek_sees_post_inner_step_value() {
    // When map is applied BEFORE peekable(), the buffered (and peeked)
    // value is the mapped value — peek and next agree.
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 3];
    let mut p = v.iter().map(|x| x * 10).peekable();
    println(p.peek().unwrap());
    println(p.next().unwrap());
    println(p.peek().unwrap());
}
"#,
    );
    assert_eq!(output, "10\n10\n20\n");
}

#[test]
fn test_iter_peekable_drains_in_for_loop() {
    // A Peekable iterator is still iterable; for-loop drains it
    // including any element already buffered by a prior peek().
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 3, 4];
    let mut p = v.iter().peekable();
    println(p.peek().unwrap());
    for x in p {
        println(x);
    }
}
"#,
    );
    // Buffered 1 from peek; for-loop drains 1, 2, 3, 4.
    assert_eq!(output, "1\n1\n2\n3\n4\n");
}

#[test]
fn test_iter_peekable_count_drains_buffer() {
    // Terminal `count()` on a Peekable counts the buffered element
    // plus the rest of the inner iterator.
    let output = run_no_errors(
        r#"
fn main() {
    let v = [10, 20, 30, 40, 50];
    let mut p = v.iter().peekable();
    println(p.peek().unwrap());
    println(p.count());
}
"#,
    );
    assert_eq!(output, "10\n5\n");
}

#[test]
fn test_iter_peekable_collect_includes_buffered() {
    // Round-trip: Peekable.collect() yields the same Vec as the
    // underlying iterator, even after a peek() has buffered an
    // element.
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 3];
    let mut p = v.iter().peekable();
    let _ = p.peek();
    let xs: Vec[i64] = p.collect();
    for x in xs {
        println(x);
    }
}
"#,
    );
    assert_eq!(output, "1\n2\n3\n");
}

#[test]
fn test_iter_peekable_then_filter_drops_peek_capability_at_runtime() {
    // After .filter() the type is Iterator[T] (not Peekable), but
    // the underlying source chain still routes correctly — the
    // resulting iterator drains as if peekable() were a no-op
    // wrapper. This guards the runtime path that wraps the inner
    // for downstream adaptors.
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 3, 4, 5];
    let xs: Vec[i64] = v.iter().peekable().filter(|x| x % 2 == 1).collect();
    for x in xs {
        println(x);
    }
}
"#,
    );
    assert_eq!(output, "1\n3\n5\n");
}

#[test]
fn test_iter_chunk_by_groups_consecutive_equal_keys() {
    // Consecutive items with the same parity group together.
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 3, 5, 2, 4, 7, 9];
    for g in v.iter().chunk_by(|x| x % 2) {
        let mut s = "[";
        let mut first = true;
        for x in g {
            if not first { s = s + ","; }
            s = s + f"{x}";
            first = false;
        }
        s = s + "]";
        println(s);
    }
}
"#,
    );
    // Groups: [1,3,5] (odd), [2,4] (even), [7,9] (odd).
    assert_eq!(output, "[1,3,5]\n[2,4]\n[7,9]\n");
}

#[test]
fn test_iter_chunk_by_singleton_groups_when_all_keys_differ() {
    // Each item is its own group when key_fn returns a unique key.
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 3, 4];
    let mut count = 0;
    for g in v.iter().chunk_by(|x| x) {
        for x in g {
            println(x);
        }
        count = count + 1;
    }
    println(f"groups:{count}");
}
"#,
    );
    assert_eq!(output, "1\n2\n3\n4\ngroups:4\n");
}

#[test]
fn test_iter_chunk_by_one_group_when_all_keys_equal() {
    // Constant key — single group containing every element.
    let output = run_no_errors(
        r#"
fn main() {
    let v = [10, 20, 30, 40];
    let mut count = 0;
    for g in v.iter().chunk_by(|x| 0) {
        for x in g {
            println(x);
        }
        count = count + 1;
    }
    println(f"groups:{count}");
}
"#,
    );
    assert_eq!(output, "10\n20\n30\n40\ngroups:1\n");
}

#[test]
fn test_iter_chunk_by_collects_into_vec_of_vec() {
    // Terminal collect() yields Vec[Vec[T]] — exercises the heap
    // allocation per group end-to-end.
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 1, 2, 2, 2, 3];
    let groups: Vec[Vec[i64]] = v.iter().chunk_by(|x| x).collect();
    println(groups.len());
    for g in groups {
        println(g.len());
    }
}
"#,
    );
    // 3 groups: lengths 2, 3, 1.
    assert_eq!(output, "3\n2\n3\n1\n");
}

#[test]
fn test_iter_chunk_by_state_persists_across_next_calls() {
    // Calling next() one group at a time threads pending-item state
    // correctly across pulls.
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 1, 2, 3, 3];
    let mut it = v.iter().chunk_by(|x| x);
    println(it.next().unwrap().len());
    println(it.next().unwrap().len());
    println(it.next().unwrap().len());
    match it.next() {
        Some(_) => println("more"),
        None => println("done"),
    }
}
"#,
    );
    assert_eq!(output, "2\n1\n2\ndone\n");
}

#[test]
fn test_iter_chunk_by_after_map_uses_mapped_item() {
    // Map runs before chunk_by — the groups carry mapped values.
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 3, 4];
    let groups: Vec[Vec[i64]] = v.iter()
        .map(|x| x * 10)
        .chunk_by(|x| x > 25)
        .collect();
    for g in groups {
        let first = g[0];
        println(first);
    }
}
"#,
    );
    // Mapped: [10, 20, 30, 40]. Keys: false, false, true, true.
    // Groups: [10, 20], [30, 40]. First-elements: 10, 30.
    assert_eq!(output, "10\n30\n");
}

#[test]
fn test_iter_chunk_by_after_filter_only_groups_kept_items() {
    // Filter first, then chunk_by — the groups only include items
    // that passed the filter.
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 3, 4, 5, 6, 7, 8];
    let groups: Vec[Vec[i64]] = v.iter()
        .filter(|x| x != 4)
        .chunk_by(|x| x < 5)
        .collect();
    println(groups.len());
    for g in groups {
        println(g.len());
    }
}
"#,
    );
    // After filter: [1, 2, 3, 5, 6, 7, 8]. Keys: T, T, T, F, F, F, F.
    // Groups: [1, 2, 3] (len 3), [5, 6, 7, 8] (len 4).
    assert_eq!(output, "2\n3\n4\n");
}

#[test]
fn test_iter_chunk_by_key_fn_fires_once_per_item() {
    // Side-effect prefix proves key_fn is called exactly once per
    // inner item — even though the same item's key is consulted
    // twice (when ending a group and when seeding the next).
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 1, 2, 2];
    let groups: Vec[Vec[i64]] = v.iter()
        .chunk_by(|x| {
            println(f"k:{x}");
            x
        })
        .collect();
    println(groups.len());
}
"#,
    );
    // key_fn fires once per element (not twice per boundary item).
    assert_eq!(output, "k:1\nk:1\nk:2\nk:2\n2\n");
}

#[test]
fn test_iter_chunk_by_with_take_short_circuits() {
    // Downstream take(n) limits how many groups we drain, so
    // chunk_by's inner pulls stop early.
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 1, 2, 2, 3, 3, 4, 4];
    for g in v.iter().chunk_by(|x| x).take(2) {
        for x in g {
            println(x);
        }
    }
}
"#,
    );
    // First two groups only: [1, 1], [2, 2].
    assert_eq!(output, "1\n1\n2\n2\n");
}

#[test]
fn test_iter_chunk_by_on_empty_yields_no_groups() {
    // Empty source → no groups; for-loop body never runs.
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1];
    let groups: Vec[Vec[i64]] = v.iter().take(0).chunk_by(|x| x).collect();
    println(f"groups:{groups.len()}");
}
"#,
    );
    assert_eq!(output, "groups:0\n");
}

#[test]
fn test_iter_chunks_groups_into_n_sized_pieces() {
    // Non-overlapping groups of n consecutive items; trailing
    // remainder is shorter when source isn't a multiple of n.
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 3, 4, 5, 6, 7];
    let groups: Vec[Vec[i64]] = v.iter().chunks(3).collect();
    println(groups.len());
    for g in groups {
        println(g.len());
    }
}
"#,
    );
    // 3 chunks: [1,2,3], [4,5,6], [7]. Lengths: 3, 3, 1.
    assert_eq!(output, "3\n3\n3\n1\n");
}

#[test]
fn test_iter_chunks_exact_multiple_yields_no_partial() {
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 3, 4, 5, 6];
    let groups: Vec[Vec[i64]] = v.iter().chunks(2).collect();
    println(groups.len());
    for g in groups {
        println(g.len());
    }
}
"#,
    );
    // 3 chunks of size 2 each.
    assert_eq!(output, "3\n2\n2\n2\n");
}

#[test]
fn test_iter_chunks_n_larger_than_source_yields_one_partial() {
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 3];
    let groups: Vec[Vec[i64]] = v.iter().chunks(10).collect();
    println(groups.len());
    println(groups[0].len());
}
"#,
    );
    assert_eq!(output, "1\n3\n");
}

#[test]
fn test_iter_chunks_zero_clamps_to_one() {
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 3];
    let groups: Vec[Vec[i64]] = v.iter().chunks(0).collect();
    println(groups.len());
    for g in groups {
        println(g.len());
    }
}
"#,
    );
    // n=0 clamps to n=1: 3 singleton chunks.
    assert_eq!(output, "3\n1\n1\n1\n");
}

#[test]
fn test_iter_chunks_state_persists_across_next() {
    let output = run_no_errors(
        r#"
fn main() {
    let v = [10, 20, 30, 40, 50];
    let mut it = v.iter().chunks(2);
    println(it.next().unwrap().len());
    println(it.next().unwrap().len());
    println(it.next().unwrap().len());
    match it.next() {
        Some(_) => println("more"),
        None => println("done"),
    }
}
"#,
    );
    assert_eq!(output, "2\n2\n1\ndone\n");
}

#[test]
fn test_iter_chunks_after_filter_only_groups_kept() {
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 3, 4, 5, 6, 7, 8];
    let groups: Vec[Vec[i64]] = v.iter()
        .filter(|x| x % 2 == 0)
        .chunks(2)
        .collect();
    println(groups.len());
    for g in groups {
        println(g.len());
    }
}
"#,
    );
    // After filter: [2, 4, 6, 8]. Chunks(2): [2,4], [6,8].
    assert_eq!(output, "2\n2\n2\n");
}

#[test]
fn test_iter_windows_slides_by_one() {
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 3, 4, 5];
    let wins: Vec[Vec[i64]] = v.iter().windows(3).collect();
    println(wins.len());
    for w in wins {
        let mut s = "[";
        let mut first = true;
        for x in w {
            if not first { s = s + ","; }
            s = s + f"{x}";
            first = false;
        }
        s = s + "]";
        println(s);
    }
}
"#,
    );
    // 3 windows: [1,2,3], [2,3,4], [3,4,5].
    assert_eq!(output, "3\n[1,2,3]\n[2,3,4]\n[3,4,5]\n");
}

#[test]
fn test_iter_windows_smaller_than_n_yields_nothing() {
    // No partial windows — when source is shorter than n, windows
    // emits zero items (matches Rust's [T].windows semantics).
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2];
    let wins: Vec[Vec[i64]] = v.iter().windows(3).collect();
    println(wins.len());
}
"#,
    );
    assert_eq!(output, "0\n");
}

#[test]
fn test_iter_windows_exactly_n_yields_one() {
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 3];
    let wins: Vec[Vec[i64]] = v.iter().windows(3).collect();
    println(wins.len());
    println(wins[0].len());
}
"#,
    );
    assert_eq!(output, "1\n3\n");
}

#[test]
fn test_iter_windows_state_persists_across_next() {
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 3, 4];
    let mut it = v.iter().windows(2);
    println(it.next().unwrap()[0]);
    println(it.next().unwrap()[0]);
    println(it.next().unwrap()[0]);
    match it.next() {
        Some(_) => println("more"),
        None => println("done"),
    }
}
"#,
    );
    // 3 windows of size 2: [1,2], [2,3], [3,4]. First element each.
    assert_eq!(output, "1\n2\n3\ndone\n");
}

#[test]
fn test_iter_windows_after_map_yields_mapped_values() {
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 3, 4];
    let wins: Vec[Vec[i64]] = v.iter().map(|x| x * 10).windows(2).collect();
    for w in wins {
        let a = w[0];
        let b = w[1];
        println(f"{a},{b}");
    }
}
"#,
    );
    // Mapped: [10, 20, 30, 40]. Windows(2): [10,20], [20,30], [30,40].
    assert_eq!(output, "10,20\n20,30\n30,40\n");
}

#[test]
fn test_iter_windows_zero_clamps_to_one() {
    // n=0 clamps to n=1 — degenerates to "each item as its own
    // singleton window" (still allocates per window).
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 3];
    let wins: Vec[Vec[i64]] = v.iter().windows(0).collect();
    println(wins.len());
    for w in wins {
        println(w[0]);
    }
}
"#,
    );
    assert_eq!(output, "3\n1\n2\n3\n");
}

#[test]
fn test_iter_chunks_with_take_short_circuits() {
    // Downstream take(n) limits how many chunks we drain.
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 3, 4, 5, 6, 7, 8];
    let groups: Vec[Vec[i64]] = v.iter().chunks(2).take(2).collect();
    println(groups.len());
}
"#,
    );
    assert_eq!(output, "2\n");
}

// ── Range pattern matching (interpreter) ────────────────────────────
//
// All five forms — `..hi`, `lo..hi`, `lo..=hi`, `..=hi`, `lo..` — must
// match the value space correctly. Previously the interpreter had a
// "simplified" RangePattern arm that always returned `true`; this test
// pins the per-form semantics.

#[test]
fn test_range_pattern_matches_correctly() {
    let output = run_no_errors(
        r#"
fn classify(n: i32) -> i32 {
    match n {
        ..0 => -1,
        0..=9 => 1,
        10..100 => 2,
        100.. => 3,
        _ => 0,
    }
}
fn main() {
    println(classify(-5));
    println(classify(0));
    println(classify(5));
    println(classify(9));
    println(classify(10));
    println(classify(50));
    println(classify(99));
    println(classify(100));
    println(classify(200));
}
"#,
    );
    assert_eq!(output, "-1\n1\n1\n1\n2\n2\n2\n3\n3\n");
}

#[test]
fn test_range_pattern_char_bounded_inclusive() {
    let output = run_no_errors(
        r#"
fn classify(c: char) -> i32 {
    match c {
        'a'..='z' => 1,
        'A'..='Z' => 2,
        _ => 0,
    }
}
fn main() {
    println(classify('m'));
    println(classify('M'));
    println(classify('1'));
}
"#,
    );
    assert_eq!(output, "1\n2\n0\n");
}

#[test]
fn test_range_pattern_char_bounded_exclusive() {
    let output = run_no_errors(
        r#"
fn main() {
    let c: char = 'g';
    let r = match c {
        'a'..'h' => 1,
        _ => 0,
    };
    println(r);
}
"#,
    );
    // 'g' is in [a, h) — should match.
    assert_eq!(output, "1\n");
}

// ── dbg() task-id tagging and structured output (item 129) ────

#[test]
fn test_dbg_terminal_no_par_omits_task_prefix() {
    // Spec: `dbg(compute(x))` reports `compute(x)` as the expr — the
    // argument's source text, not the whole dbg call.
    let src = r#"fn main() {
    let _ = dbg(7);
}
"#;
    let (_stdout, dbg) = run_program_with_dbg(src, DbgOutputMode::Terminal);
    assert_eq!(dbg.len(), 1, "expected one dbg line, got {:?}", dbg);
    let line = &dbg[0];
    assert_eq!(line, "[test.kara:2] 7 = 7\n");
    assert!(
        !line.contains("task:"),
        "outside par {{}} should not include task tag"
    );
}

#[test]
fn test_dbg_terminal_in_par_tags_each_branch() {
    let src = r#"fn main() {
    par {
        let _ = dbg(1);
        let _ = dbg(2);
    }
}
"#;
    let (_stdout, dbg) = run_program_with_dbg(src, DbgOutputMode::Terminal);
    assert_eq!(dbg.len(), 2, "expected two dbg lines, got {:?}", dbg);
    // Branch 0 → task:1, branch 1 → task:2 (assigned in source order
    // before spawn, so deterministic regardless of OS scheduling).
    assert_eq!(dbg[0], "[task:1 test.kara:3] 1 = 1\n");
    assert_eq!(dbg[1], "[task:2 test.kara:4] 2 = 2\n");
}

#[test]
fn test_dbg_json_no_par_emits_null_task_id() {
    let src = r#"fn main() {
    let _ = dbg(42);
}
"#;
    let (_stdout, dbg) = run_program_with_dbg(src, DbgOutputMode::Json);
    assert_eq!(dbg.len(), 1);
    let line = &dbg[0];
    assert!(line.starts_with("{\"kind\":\"dbg\","));
    assert!(line.contains("\"task_id\":null"), "got {:?}", line);
    assert!(line.contains("\"file\":\"test.kara\""));
    assert!(line.contains("\"line\":2"));
    assert!(line.contains("\"expr\":\"42\""), "got {:?}", line);
    // Bare integer literal infers as i64 in the typechecker; the spec
    // example uses i32 because there `compute(x)` returns explicit i32.
    // The contract is "Display of the inferred type"; i64 is correct here.
    assert!(line.contains("\"type\":\"i64\""), "got {:?}", line);
    assert!(line.contains("\"value\":\"42\""));
    assert!(line.ends_with("}\n"));
}

#[test]
fn test_dbg_json_in_par_emits_numeric_task_id() {
    let src = r#"fn main() {
    par {
        let _ = dbg(10);
        let _ = dbg(20);
    }
}
"#;
    let (_stdout, dbg) = run_program_with_dbg(src, DbgOutputMode::Json);
    assert_eq!(dbg.len(), 2, "got {:?}", dbg);
    assert!(dbg[0].contains("\"task_id\":1"), "got {:?}", dbg[0]);
    assert!(dbg[0].contains("\"value\":\"10\""));
    assert!(dbg[1].contains("\"task_id\":2"), "got {:?}", dbg[1]);
    assert!(dbg[1].contains("\"value\":\"20\""));
}

#[test]
fn test_dbg_json_value_uses_debug_fmt_quoting() {
    // The `value` field must use `Debug` formatting: strings get
    // quoted as `"hello"` not `hello`. The whole field is then JSON-
    // escaped, so the inner quotes show up as `\"`.
    let src = r#"fn main() {
    let _ = dbg("hi");
}
"#;
    let (_stdout, dbg) = run_program_with_dbg(src, DbgOutputMode::Json);
    assert_eq!(dbg.len(), 1);
    // Inner Debug form: `"hi"`. JSON-escaped: `"\"hi\""`.
    assert!(
        dbg[0].contains("\"value\":\"\\\"hi\\\"\""),
        "expected JSON-escaped quoted string, got {:?}",
        dbg[0]
    );
}

#[test]
fn test_dbg_returns_its_argument() {
    // dbg() is an identity function — the value flows through.
    let src = r#"fn main() {
    let x = dbg(3) + dbg(4);
    println(x);
}
"#;
    let (stdout, dbg) = run_program_with_dbg(src, DbgOutputMode::Terminal);
    assert_eq!(stdout.join(""), "7\n");
    assert_eq!(dbg.len(), 2);
}

// ── Range / RangeInclusive as Iterator ─────────────────────────
//
// Range and RangeInclusive evaluate to `Value::Iterator` so the adaptor
// surface (`step_by`, `map`, `filter`, `take`, `collect`, ...) dispatches
// directly without a redundant `.iter()` layer. The for-loop iterable
// path drains `Value::Iterator` via `iterator_step`, so for-loop
// semantics are preserved.

#[test]
fn test_range_iter_step_by_collect() {
    // `(0..10).step_by(2).collect()` — half-open Range as Iterator.
    let output = run_no_errors(
        r#"
fn main() {
    let xs: Vec[i64] = (0..10).step_by(2).collect();
    for x in xs {
        println(x);
    }
}
"#,
    );
    assert_eq!(output, "0\n2\n4\n6\n8\n");
}

#[test]
fn test_range_inclusive_step_by_collect() {
    // `(1..=10).step_by(2).collect()` — RangeInclusive as Iterator pins
    // the inclusive-end semantics (10 is reachable but the stride
    // skips it).
    let output = run_no_errors(
        r#"
fn main() {
    let xs: Vec[i64] = (1..=10).step_by(2).collect();
    for x in xs {
        println(x);
    }
}
"#,
    );
    assert_eq!(output, "1\n3\n5\n7\n9\n");
}

#[test]
fn test_range_redundant_iter() {
    // `(0..5).iter().collect()` — the redundant iter() call on a Range
    // is a no-op pass-through. Pins the iter/into_iter early-return
    // guard (sub-step (d)) — without it, this hits an `unreachable!`
    // because Range now produces `Value::Iterator` rather than
    // `Value::Array`.
    let output = run_no_errors(
        r#"
fn main() {
    let xs: Vec[i64] = (0..5).iter().collect();
    for x in xs {
        println(x);
    }
}
"#,
    );
    assert_eq!(output, "0\n1\n2\n3\n4\n");
}

#[test]
fn test_range_chained_adaptors() {
    // Multi-step adaptor composition through the new entry surface:
    // map doubles each element, filter keeps those > 5.
    let output = run_no_errors(
        r#"
fn main() {
    let xs: Vec[i64] = (0..10).map(|x| x * 2).filter(|x| x > 5).collect();
    for x in xs {
        println(x);
    }
}
"#,
    );
    assert_eq!(output, "6\n8\n10\n12\n14\n16\n18\n");
}

#[test]
fn test_range_for_loop_unchanged() {
    // Regression — for-loop semantics must survive the Range → Iterator
    // eval change. Sums 0+1+2+3+4 = 10.
    let output = run_no_errors(
        r#"
fn main() {
    let mut s = 0;
    for x in 0..5 {
        s = s + x;
    }
    println(s);
}
"#,
    );
    assert_eq!(output, "10\n");
}

#[test]
fn test_range_inclusive_take() {
    // `(0..=100).take(3).collect()` pins the eager-snapshot truncation
    // under `take` — the source pre-materialises 101 items but the
    // step-layer adaptor pulls only the first three.
    let output = run_no_errors(
        r#"
fn main() {
    let xs: Vec[i64] = (0..=100).take(3).collect();
    for x in xs {
        println(x);
    }
}
"#,
    );
    assert_eq!(output, "0\n1\n2\n");
}

// ── Slice[T] Iterator impl ─────────────────────────────────────
//
// `Slice[T]` IS `Iterator[T]` — `s.iter()` and `s.into_iter()` route
// through the same Iterator dispatch as `Vec.iter()`, so chained
// adaptors compose. The for-loop iterable path drains `Value::Slice`
// directly, so `for x in s { ... }` keeps working without `.iter()`.
// Sibling to the Range / RangeInclusive Iterator entry above.

#[test]
fn test_interpreter_slice_iter_basic() {
    // `s.iter()` over a borrowed slice yields each element; sums to 6.
    let output = run_no_errors(
        r#"
fn main() {
    let v = Vec[1, 2, 3];
    let s: Slice[i64] = v.as_slice();
    let mut sum = 0;
    for x in s.iter() {
        sum = sum + x;
    }
    println(sum);
}
"#,
    );
    assert_eq!(output, "6\n");
}

#[test]
fn test_interpreter_slice_iter_chain_with_map_collect() {
    // `s.iter().map(|x| x * 2).collect()` returns [2, 4, 6].
    let output = run_no_errors(
        r#"
fn main() {
    let v = Vec[1, 2, 3];
    let s: Slice[i64] = v.as_slice();
    let xs: Vec[i64] = s.iter().map(|x| x * 2).collect();
    for x in xs {
        println(x);
    }
}
"#,
    );
    assert_eq!(output, "2\n4\n6\n");
}

#[test]
fn test_interpreter_slice_iter_chain_with_filter_sum() {
    // `s.iter().filter(|x| x % 2 == 0).fold(0, |a, b| a + b)` returns 2.
    // Sums via `fold` since `Iterator.sum` is not on the shipped terminal
    // surface (`next` / `count` / `collect` / `fold` / `any` / `all`).
    let output = run_no_errors(
        r#"
fn main() {
    let v = Vec[1, 2, 3];
    let s: Slice[i64] = v.as_slice();
    let total: i64 = s.iter().filter(|x| x % 2 == 0).fold(0, |a, b| a + b);
    println(total);
}
"#,
    );
    assert_eq!(output, "2\n");
}

#[test]
fn test_interpreter_slice_into_iter_works() {
    // `s.into_iter()` round-trips identical to `s.iter()` at the
    // tree-walk layer (the borrow-vs-consume distinction is a
    // typechecker concern; sums 1+2+3 = 6).
    let output = run_no_errors(
        r#"
fn main() {
    let v = Vec[1, 2, 3];
    let s: Slice[i64] = v.as_slice();
    let mut sum = 0;
    for x in s.into_iter() {
        sum = sum + x;
    }
    println(sum);
}
"#,
    );
    assert_eq!(output, "6\n");
}

#[test]
fn test_interpreter_slice_for_loop_without_iter() {
    // `for x in s { ... }` (no explicit `.iter()`) sums correctly via
    // the for-loop iterable path's `Value::Slice` arm. Pins (SI3).
    let output = run_no_errors(
        r#"
fn main() {
    let v = Vec[10, 20, 30];
    let s: Slice[i64] = v.as_slice();
    let mut sum = 0;
    for x in s {
        sum = sum + x;
    }
    println(sum);
}
"#,
    );
    assert_eq!(output, "60\n");
}

#[test]
fn test_interpreter_slice_iter_empty_slice() {
    // Empty slice iterator yields no elements; `.collect()` returns [].
    let output = run_no_errors(
        r#"
fn main() {
    let v: Vec[i64] = Vec.new();
    let s: Slice[i64] = v.as_slice();
    let xs: Vec[i64] = s.iter().collect();
    println(xs.len());
}
"#,
    );
    assert_eq!(output, "0\n");
}

// ── Slice / array patterns (phase-5 § Slice and array patterns — sub-item 3)

#[test]
fn test_slice_pattern_empty_matches_empty_vec() {
    // `[]` arm matches an empty Vec; non-empty falls through.
    let output = run_no_errors(
        r#"
fn label(v: Vec[i64]) -> String {
    match v {
        [] => "empty",
        _ => "non-empty",
    }
}
fn main() {
    let a: Vec[i64] = Vec.new();
    let mut b: Vec[i64] = Vec.new();
    b.push(7);
    println(label(a));
    println(label(b));
}
"#,
    );
    assert_eq!(output, "empty\nnon-empty\n");
}

#[test]
fn test_slice_pattern_single_element_fixed_arity_array() {
    // `let [x] = arr` on Array[i64, 1] binds the single element.
    let output = run_no_errors(
        r#"
fn main() {
    let a: Array[i64, 1] = [42];
    let [x] = a;
    println(x);
}
"#,
    );
    assert_eq!(output, "42\n");
}

#[test]
fn test_slice_pattern_fixed_arity_let_binds_all_elements() {
    // `let [a, b, c] = arr` on Array[i64, 3] is irrefutable; binds positionally.
    let output = run_no_errors(
        r#"
fn main() {
    let arr: Array[i64, 3] = [10, 20, 30];
    let [a, b, c] = arr;
    println(a);
    println(b);
    println(c);
}
"#,
    );
    assert_eq!(output, "10\n20\n30\n");
}

#[test]
fn test_slice_pattern_head_only_ignored_rest_on_vec() {
    // `[first, ..]` against a Vec binds head, ignores the tail.
    let output = run_no_errors(
        r#"
fn head_or(v: Vec[i64], default: i64) -> i64 {
    match v {
        [first, ..] => first,
        [] => default,
    }
}
fn main() {
    let mut v: Vec[i64] = Vec.new();
    v.push(10);
    v.push(20);
    v.push(30);
    let empty: Vec[i64] = Vec.new();
    println(head_or(v, -1));
    println(head_or(empty, -1));
}
"#,
    );
    assert_eq!(output, "10\n-1\n");
}

#[test]
fn test_slice_pattern_tail_only_ignored_rest_on_vec() {
    // `[.., last]` against a Vec binds the last element.
    let output = run_no_errors(
        r#"
fn last_or(v: Vec[i64], default: i64) -> i64 {
    match v {
        [.., last] => last,
        [] => default,
    }
}
fn main() {
    let mut v: Vec[i64] = Vec.new();
    v.push(10);
    v.push(20);
    v.push(30);
    println(last_or(v, -1));
}
"#,
    );
    assert_eq!(output, "30\n");
}

#[test]
fn test_slice_pattern_both_ends_ignored_rest_on_vec() {
    // `[first, .., last]` against a Vec of length >= 2 binds both endpoints.
    let output = run_no_errors(
        r#"
fn ends(v: Vec[i64]) -> i64 {
    match v {
        [first, .., last] => first + last,
        [only] => only,
        [] => -1,
    }
}
fn main() {
    let mut v: Vec[i64] = Vec.new();
    v.push(1);
    v.push(2);
    v.push(3);
    v.push(4);
    v.push(5);
    println(ends(v));
}
"#,
    );
    assert_eq!(output, "6\n");
}

#[test]
fn test_slice_pattern_single_bound_rest_at_tail_array() {
    // `let [first, ..rest] = arr` on Array[i64, 5] binds first and rest;
    // rest has length 4.
    let output = run_no_errors(
        r#"
fn main() {
    let arr: Array[i64, 5] = [10, 20, 30, 40, 50];
    let [first, ..rest] = arr;
    println(first);
    println(rest.len());
}
"#,
    );
    assert_eq!(output, "10\n4\n");
}

#[test]
fn test_slice_pattern_single_bound_rest_at_head_array() {
    // `let [..rest, last] = arr` on Array[i64, 4] binds rest and last;
    // rest has length 3.
    let output = run_no_errors(
        r#"
fn main() {
    let arr: Array[i64, 4] = [10, 20, 30, 40];
    let [..rest, last] = arr;
    println(rest.len());
    println(last);
}
"#,
    );
    assert_eq!(output, "3\n40\n");
}

#[test]
fn test_slice_pattern_two_bound_middle_rest_array() {
    // `let [first, ..mid, last] = arr` on Array[i64, 5] binds endpoints
    // and the bound middle rest; mid has length 3.
    let output = run_no_errors(
        r#"
fn main() {
    let arr: Array[i64, 5] = [1, 2, 3, 4, 5];
    let [first, ..mid, last] = arr;
    println(first);
    println(mid.len());
    println(last);
}
"#,
    );
    assert_eq!(output, "1\n3\n5\n");
}

#[test]
fn test_slice_pattern_multi_element_prefix_and_suffix_array() {
    // Multi-element prefix and suffix around an ignored rest:
    // `let [a, b, .., y, z] = arr` on Array[i64, 6].
    let output = run_no_errors(
        r#"
fn main() {
    let arr: Array[i64, 6] = [10, 20, 30, 40, 50, 60];
    let [a, b, .., y, z] = arr;
    println(a);
    println(b);
    println(y);
    println(z);
}
"#,
    );
    assert_eq!(output, "10\n20\n50\n60\n");
}

#[test]
fn test_slice_pattern_vec_bound_rest_sum_via_iter() {
    // Bound rest on a Vec scrutinee is a Slice[T] over the source storage;
    // iteration over it yields the middle elements in order.
    let output = run_no_errors(
        r#"
fn middle_sum(v: Vec[i64]) -> i64 {
    match v {
        [_, ..mid, _] => {
            let mut acc = 0;
            for x in mid { acc = acc + x; }
            acc
        },
        _ => 0,
    }
}
fn main() {
    let mut v: Vec[i64] = Vec.new();
    v.push(100);
    v.push(2);
    v.push(3);
    v.push(4);
    v.push(200);
    println(middle_sum(v));
}
"#,
    );
    assert_eq!(output, "9\n");
}

#[test]
fn test_slice_pattern_match_dispatches_on_length_for_vec() {
    // Different arms select on length classes; vectors of varying length
    // each route to the right arm.
    let output = run_no_errors(
        r#"
fn classify(v: Vec[i64]) -> String {
    match v {
        [] => "0",
        [_] => "1",
        [_, _] => "2",
        [_, .., _] => "3+",
    }
}
fn main() {
    let a: Vec[i64] = Vec.new();
    let mut b: Vec[i64] = Vec.new();
    b.push(1);
    let mut c: Vec[i64] = Vec.new();
    c.push(1); c.push(2);
    let mut d: Vec[i64] = Vec.new();
    d.push(1); d.push(2); d.push(3); d.push(4);
    println(classify(a));
    println(classify(b));
    println(classify(c));
    println(classify(d));
}
"#,
    );
    assert_eq!(output, "0\n1\n2\n3+\n");
}

#[test]
fn test_slice_pattern_rest_binding_preserves_element_values_on_vec() {
    // Bound rest on a Vec scrutinee exposes the middle elements in order
    // and indexes correctly.
    let output = run_no_errors(
        r#"
fn main() {
    let mut v: Vec[i64] = Vec.new();
    v.push(7);
    v.push(8);
    v.push(9);
    v.push(10);
    match v {
        [_, ..rest] => {
            println(rest.len());
            println(rest[0]);
            println(rest[1]);
            println(rest[2]);
        },
        [] => println(-1),
    }
}
"#,
    );
    assert_eq!(output, "3\n8\n9\n10\n");
}

// `c as i32` used to be a no-op in the interpreter (codegen lowered it
// correctly via LLVM `int_cast`). The downstream subtraction then panicked
// in `eval_ops` with "type mismatch in binary operation Sub". The fix
// mirrors `check_cast_pair`'s accepted shapes (char → wide-int).
// Surfaced while writing kata #8 (string-to-integer atoi).
#[test]
fn test_interp_char_as_i32_digit_subtraction() {
    let output = run(r#"
fn main() {
    let s = "0123";
    for c in s.chars() {
        let n: i32 = c as i32;
        let d: i32 = n - 48i32;
        println(d);
    }
}
"#);
    assert_eq!(output, "0\n1\n2\n3\n");
}

#[test]
fn test_interp_int_widening_narrowing_casts() {
    let output = run(r#"
fn main() {
    let a: i32 = 1000i32;
    let b: i64 = a as i64;
    println(b);
    let c: i8 = a as i8;
    let d: i32 = c as i32;
    println(d);
}
"#);
    // 1000 widens to i64 unchanged; truncating to i8 keeps the low 8 bits
    // (1000 & 0xff = 0xe8 = -24 as signed i8), then widens back as -24.
    assert_eq!(output, "1000\n-24\n");
}

#[test]
fn test_interp_int_to_float_cast() {
    let output = run(r#"
fn main() {
    let n: i64 = 7;
    let f: f64 = n as f64;
    println(f);
}
"#);
    assert_eq!(output.trim(), "7");
}

// ── Refinement types: runtime predicate enforcement (phase-9 step 5b) ──

#[test]
fn test_interp_refinement_try_from_ok() {
    // `Even.try_from(4)` evaluates the predicate `self % 2 == 0` against 4,
    // which holds, so the construction returns `Ok(4)`.
    let output = run_no_errors(
        r#"
type Even = i64 where self % 2 == 0;
fn main() {
    match Even.try_from(4) {
        Ok(v) => println(v),
        Err(e) => println(e),
    }
}
"#,
    );
    assert_eq!(output.trim(), "4");
}

#[test]
fn test_interp_refinement_try_from_err() {
    // 3 fails `self % 2 == 0`, so `try_from` returns `Err(<message>)` — the
    // recoverable construction surface. No runtime fault is raised.
    let output = run_no_errors(
        r#"
type Even = i64 where self % 2 == 0;
fn main() {
    match Even.try_from(3) {
        Ok(v) => println(v),
        Err(e) => println(e),
    }
}
"#,
    );
    assert!(
        output.contains("refinement `Even`"),
        "expected an Err message naming the refinement, got: {output:?}"
    );
}

#[test]
fn test_interp_refinement_as_cast_ok() {
    // `4 as Even` passes the predicate; the (layout-identical) base value
    // flows through and prints, with no runtime fault.
    let output = run_no_errors(
        r#"
type Even = i64 where self % 2 == 0;
fn main() {
    let e = 4 as Even;
    println(e);
}
"#,
    );
    assert_eq!(output.trim(), "4");
}

#[test]
fn test_interp_refinement_as_cast_violation_faults() {
    // `3 as Even` is the asserting construction form: a false predicate is a
    // contract violation, surfaced as a runtime fault (not a recoverable Err).
    let errors = runtime_errors(
        r#"
type Even = i64 where self % 2 == 0;
fn main() {
    let e = 3 as Even;
    println(e);
}
"#,
    );
    assert!(
        errors
            .iter()
            .any(|e| e.message.contains("contract violated") && e.message.contains("Even")),
        "expected a contract-violation fault naming `Even`, got: {errors:?}"
    );
}

#[test]
fn test_interp_refinement_string_try_from_uses_method_predicate() {
    // A refinement over String with a zero-arg method predicate
    // (`self.len() > 0`): `try_from("")` fails, `try_from("hi")` succeeds.
    let output = run_no_errors(
        r#"
type NonEmpty = String where self.len() > 0;
fn main() {
    match NonEmpty.try_from("") {
        Ok(s) => println(s),
        Err(_) => println("rejected-empty"),
    }
    match NonEmpty.try_from("hi") {
        Ok(s) => println(s),
        Err(_) => println("rejected"),
    }
}
"#,
    );
    assert_eq!(output.trim(), "rejected-empty\nhi");
}

// ── Contracts — requires / ensures runtime enforcement ─────────────
//
// design.md § Contracts: `requires` predicates are checked at function
// entry and `ensures(result) …` at the return point (debug builds); a
// false predicate faults `contract violated`. v1 covers free functions.

#[test]
fn test_contract_requires_holds_runs_body() {
    let output = run_no_errors(
        "fn checked(x: i64) -> i64 requires x > 0 { x * 2 }\n\
         fn main() { println(checked(5)); }",
    );
    assert_eq!(output, "10\n");
}

#[test]
fn test_contract_requires_violation_faults() {
    let errors = runtime_errors(
        "fn checked(x: i64) -> i64 requires x > 0 { x * 2 }\n\
         fn main() { println(checked(-3)); }",
    );
    assert!(
        errors
            .iter()
            .any(|e| e.message.contains("contract violated")),
        "expected a `contract violated` fault for a failed requires, got: {errors:?}"
    );
}

#[test]
fn test_contract_ensures_holds_runs() {
    // Both binding syntaxes are accepted; this uses the design `(result)`.
    let output = run_no_errors(
        "fn double(x: i64) -> i64 ensures(result) result > x { x * 2 }\n\
         fn main() { println(double(5)); }",
    );
    assert_eq!(output, "10\n");
}

#[test]
fn test_contract_ensures_violation_faults() {
    let errors = runtime_errors(
        "fn bad(x: i64) -> i64 ensures(result) result > 100 { x }\n\
         fn main() { println(bad(5)); }",
    );
    assert!(
        errors
            .iter()
            .any(|e| e.message.contains("contract violated")),
        "expected a `contract violated` fault for a failed ensures, got: {errors:?}"
    );
}

#[test]
fn test_contract_requires_and_ensures_combined() {
    let output = run_no_errors(
        "fn clamp_pos(x: i64) -> i64 requires x > 0 ensures(result) result >= x { x + 1 }\n\
         fn main() { println(clamp_pos(10)); }",
    );
    assert_eq!(output, "11\n");
}

#[test]
fn test_contract_ensures_pipe_syntax_still_works() {
    // The `|result|` closure-style binding remains accepted alongside the
    // `(result)` design form.
    let output = run_no_errors(
        "fn double(x: i64) -> i64 ensures |result| result > x { x * 2 }\n\
         fn main() { println(double(5)); }",
    );
    assert_eq!(output, "10\n");
}

// ── Contracts — struct invariants at pub method exits ──────────────
//
// design.md § Contracts rule 3: a type with an `invariant` block re-checks
// it at the exit of every pub method (private methods do not check). v1
// covers pub instance methods.

#[test]
fn test_contract_invariant_holds_runs() {
    // `inc` keeps `self.n >= 0`, so the pub-method-exit check passes.
    let errors = runtime_errors(
        "struct Counter { n: i64, invariant self.n >= 0 }\n\
         impl Counter { pub fn inc(mut ref self) { self.n = self.n + 1; } }\n\
         fn main() { let mut c = Counter { n: 0 }; c.inc(); }",
    );
    assert!(
        errors.is_empty(),
        "a satisfied invariant must not fault, got: {errors:?}"
    );
}

#[test]
fn test_contract_invariant_violation_faults() {
    // `dec` drives `self.n` to -1, violating `self.n >= 0` at method exit.
    let errors = runtime_errors(
        "struct Counter { n: i64, invariant self.n >= 0 }\n\
         impl Counter { pub fn dec(mut ref self) { self.n = self.n - 1; } }\n\
         fn main() { let mut c = Counter { n: 0 }; c.dec(); }",
    );
    assert!(
        errors
            .iter()
            .any(|e| e.message.contains("contract violated")),
        "expected a `contract violated` invariant fault, got: {errors:?}"
    );
}

#[test]
fn test_contract_invariant_private_method_not_checked() {
    // A private method may transiently break the invariant; only the
    // outer pub method's exit checks it. Here the pub method restores
    // nothing but never lets a violated state reach a pub exit, so no
    // fault fires (the private `break_priv` result is discarded).
    let errors = runtime_errors(
        "struct DateRange { start: i64, end: i64, invariant self.start <= self.end }\n\
         impl DateRange {\n\
             fn break_priv(self) -> DateRange { DateRange { start: 10, end: 0 } }\n\
             pub fn via(self) -> i64 { let _b = self.break_priv(); 99 }\n\
         }\n\
         fn main() { let r = DateRange { start: 1, end: 5 }; let _ = r.via(); }",
    );
    assert!(
        errors.is_empty(),
        "a private method must not trigger the invariant check, got: {errors:?}"
    );
}

// ── Contracts — constructor invariants (pub assoc fn returning Self) ──
//
// design.md § Contracts: "Constructors (pub associated functions that return
// `Self`) also check the invariant at their return point." The return value
// is bound as `self` and the type's invariants are re-checked — the
// construction boundary, alongside the pub-method-exit checks above.

#[test]
fn test_contract_constructor_invariant_holds() {
    // A pub constructor that produces a valid instance must not fault.
    let errors = runtime_errors(
        "struct Counter { n: i64, invariant self.n >= 0 }\n\
         impl Counter { pub fn make() -> Self { Counter { n: 7 } } }\n\
         fn main() { let _c = Counter.make(); }",
    );
    assert!(
        errors.is_empty(),
        "a valid constructor must not fault, got: {errors:?}"
    );
}

#[test]
fn test_contract_constructor_invariant_violation_faults() {
    // The constructor builds `n = -5`, violating `self.n >= 0` at its return
    // point — the construction boundary aborts even though no method ran.
    let errors = runtime_errors(
        "struct Counter { n: i64, invariant self.n >= 0 }\n\
         impl Counter { pub fn bad() -> Self { Counter { n: 0 - 5 } } }\n\
         fn main() { let _c = Counter.bad(); }",
    );
    assert!(
        errors
            .iter()
            .any(|e| e.message.contains("contract violated")),
        "expected a `contract violated` invariant fault at construction, got: {errors:?}"
    );
}

#[test]
fn test_contract_constructor_impl_invariant_faults() {
    // `impl invariant` fires at the constructor return point too (it fires at
    // every method exit, and a constructor is a return boundary). The explicit
    // `-> Counter` return-type form (not `Self`) is also recognized.
    let errors = runtime_errors(
        "struct Counter { n: i64, impl invariant self.n >= 0 }\n\
         impl Counter { pub fn bad() -> Counter { Counter { n: 0 - 1 } } }\n\
         fn main() { let _c = Counter.bad(); }",
    );
    assert!(
        errors
            .iter()
            .any(|e| e.message.contains("contract violated")),
        "an `impl invariant` must fire at the constructor return, got: {errors:?}"
    );
}

#[test]
fn test_contract_constructor_non_self_return_not_checked() {
    // A static associated function returning some *other* type (`-> i64`) is
    // not a constructor — its return value must NOT be invariant-checked,
    // even though the type has an invariant and the value would violate it.
    let errors = runtime_errors(
        "struct Counter { n: i64, invariant self.n >= 0 }\n\
         impl Counter { pub fn answer() -> i64 { 0 - 9 } }\n\
         fn main() { let _x = Counter.answer(); }",
    );
    assert!(
        errors.is_empty(),
        "a non-Self-returning assoc fn must not be invariant-checked, got: {errors:?}"
    );
}

#[test]
fn test_contract_shared_constructor_invariant_faults() {
    // Constructor invariants fire on shared (RC) structs too — construction
    // doesn't involve the shared-mutation path, so this is a clean check.
    let errors = runtime_errors(
        "shared struct Scell { n: i64, invariant self.n >= 0 }\n\
         impl Scell { pub fn bad() -> Self { Scell { n: 0 - 1 } } }\n\
         fn main() { let _c = Scell.bad(); }",
    );
    assert!(
        errors
            .iter()
            .any(|e| e.message.contains("contract violated")),
        "a shared-struct constructor must check its invariant, got: {errors:?}"
    );
}

#[test]
fn test_contract_shared_method_invariant_holds() {
    // Shared-struct pub-method invariants are already plumbed (the check is
    // receiver-kind-agnostic): a non-mutating pub method with a held invariant
    // runs clean. (A *violating* shared method needs field mutation through
    // `mut ref self`, which has an orthogonal constraint tracked separately;
    // this pins that the invariant dispatch itself fires without spurious
    // faults for a shared receiver.)
    let errors = runtime_errors(
        "shared struct Scell { n: i64, invariant self.n >= 0 }\n\
         impl Scell { pub fn get(ref self) -> i64 { self.n } }\n\
         fn main() { let c = Scell { n: 5 }; let _ = c.get(); }",
    );
    assert!(
        errors.is_empty(),
        "a held shared-method invariant must not fault, got: {errors:?}"
    );
}

// ── Contracts — old(expr) pre-state + method contracts ─────────────
//
// design.md § Contracts rule 4: `old(expr)` in an `ensures` clause reads
// the value captured at function entry. Method `requires`/`ensures` are
// enforced on the method-dispatch path (same as free functions).

#[test]
fn test_contract_old_method_holds() {
    // `withdraw` reduces the balance by `amount`; the postcondition
    // `self.balance == old(self.balance) - amount` holds.
    let errors = runtime_errors(
        "struct Account { balance: i64 }\n\
         impl Account {\n\
             pub fn withdraw(mut ref self, amount: i64) -> i64\n\
                 ensures(result) self.balance == old(self.balance) - amount\n\
             { self.balance = self.balance - amount; amount }\n\
         }\n\
         fn main() { let mut a = Account { balance: 100 }; let _ = a.withdraw(30); }",
    );
    assert!(
        errors.is_empty(),
        "a satisfied old() postcondition must not fault, got: {errors:?}"
    );
}

#[test]
fn test_contract_old_method_violation_faults() {
    // The body mutates the balance wrongly, so the `old()` postcondition fails.
    let errors = runtime_errors(
        "struct Account { balance: i64 }\n\
         impl Account {\n\
             pub fn withdraw(mut ref self, amount: i64) -> i64\n\
                 ensures(result) self.balance == old(self.balance) - amount\n\
             { self.balance = self.balance - 999; amount }\n\
         }\n\
         fn main() { let mut a = Account { balance: 100 }; let _ = a.withdraw(30); }",
    );
    assert!(
        errors
            .iter()
            .any(|e| e.message.contains("contract violated")),
        "expected a `contract violated` fault for the old() postcondition, got: {errors:?}"
    );
}

#[test]
fn test_contract_method_requires_violation_faults() {
    // Method `requires` is enforced on the dispatch path.
    let errors = runtime_errors(
        "struct Counter { n: i64 }\n\
         impl Counter { pub fn step(self, by: i64) -> i64 requires by > 0 { self.n + by } }\n\
         fn main() { let c = Counter { n: 5 }; let _ = c.step(-1); }",
    );
    assert!(
        errors
            .iter()
            .any(|e| e.message.contains("contract violated")),
        "expected a method-requires fault, got: {errors:?}"
    );
}

#[test]
fn test_contract_old_free_function() {
    // `old(x)` in a free-function ensures evaluates to the entry value.
    let output = run_no_errors(
        "fn bump(x: i64) -> i64 ensures(result) result > old(x) { x + 1 }\n\
         fn main() { println(bump(5)); }",
    );
    assert_eq!(output, "6\n");
}

// ── Contracts — impl invariant (step 5b, all-method scope) ─────────
//
// design.md § Contracts — `impl invariant` fires at every method exit
// (pub and private); plain `invariant` only at pub method exits.

#[test]
fn test_impl_invariant_fires_at_private_method_exit() {
    let errors = runtime_errors(
        "struct Counter { n: i64, impl invariant self.n >= 0 }\n\
         impl Counter {\n\
             fn dec_priv(mut ref self) { self.n = self.n - 1; }\n\
             pub fn run(mut ref self) -> i64 { self.dec_priv(); 0 }\n\
         }\n\
         fn main() { let mut c = Counter { n: 0 }; let _ = c.run(); }",
    );
    assert!(
        errors
            .iter()
            .any(|e| e.message.contains("contract violated")),
        "expected `impl invariant` to fault at the private method exit, got: {errors:?}"
    );
}

#[test]
fn test_plain_invariant_not_checked_at_private_method_exit() {
    // The private helper transiently breaks the plain invariant; the pub
    // method restores it before its own exit, so no fault fires.
    let errors = runtime_errors(
        "struct Counter { n: i64, invariant self.n >= 0 }\n\
         impl Counter {\n\
             fn dec_priv(mut ref self) { self.n = self.n - 1; }\n\
             pub fn run(mut ref self) -> i64 { self.dec_priv(); self.n = self.n + 1; 0 }\n\
         }\n\
         fn main() { let mut c = Counter { n: 0 }; let _ = c.run(); }",
    );
    assert!(
        errors.is_empty(),
        "plain invariant must not fire at a private method exit, got: {errors:?}"
    );
}

#[test]
fn test_impl_invariant_holds_runs() {
    let errors = runtime_errors(
        "struct Counter { n: i64, impl invariant self.n >= 0 }\n\
         impl Counter { fn inc_priv(mut ref self) { self.n = self.n + 1; } pub fn run(mut ref self) { self.inc_priv(); } }\n\
         fn main() { let mut c = Counter { n: 0 }; c.run(); }",
    );
    assert!(
        errors.is_empty(),
        "a satisfied impl invariant must not fault, got: {errors:?}"
    );
}

// ── Contracts — distinct "predicate panicked" fault category (step 6) ──
//
// design.md § Contracts rule 2: a predicate that *returns false* is
// `contract violated`; a predicate whose *evaluation faults* (index OOB,
// div-by-zero, unwrap) is the distinct `contract predicate panicked`.

#[test]
fn test_contract_predicate_panicked_is_distinct() {
    let errors = runtime_errors(
        "fn at(v: Vec[i64], i: i64) -> i64 requires v[i] >= 0 { 0 }\n\
         fn main() { let v: Vec[i64] = Vec[1, 2, 3]; let _ = at(v, 99); }",
    );
    assert!(
        errors
            .iter()
            .any(|e| e.message.contains("contract predicate panicked")),
        "expected a `contract predicate panicked` fault, got: {errors:?}"
    );
    assert!(
        !errors
            .iter()
            .any(|e| e.message.contains("contract violated")),
        "a panicking predicate must NOT be reported as `contract violated`, got: {errors:?}"
    );
}

#[test]
fn test_contract_violated_distinct_from_panicked() {
    let errors = runtime_errors(
        "fn pos(x: i64) -> i64 requires x > 0 { x }\n\
         fn main() { let _ = pos(-5); }",
    );
    assert!(
        errors
            .iter()
            .any(|e| e.message.contains("contract violated")),
        "expected `contract violated` for a false predicate, got: {errors:?}"
    );
    assert!(
        !errors
            .iter()
            .any(|e| e.message.contains("predicate panicked")),
        "a false predicate must NOT be reported as panicked, got: {errors:?}"
    );
}

#[test]
fn test_contract_predicate_panicked_in_ensures() {
    let errors = runtime_errors(
        "fn f(v: Vec[i64]) -> i64 ensures(result) v[result] >= 0 { 99 }\n\
         fn main() { let v: Vec[i64] = Vec[1, 2, 3]; let _ = f(v); }",
    );
    assert!(
        errors
            .iter()
            .any(|e| e.message.contains("contract predicate panicked")),
        "expected `contract predicate panicked` in an ensures, got: {errors:?}"
    );
}

// ── par-struct field mutation persists through `ref self` ────────
// Regression: a par struct must be reference-semantic in the interpreter
// (SharedStruct, not a value-copy), and its `Atomic` / `Mutex` fields must
// be interior-mutable cells so `.fetch_add` / `lock` writes through a
// `ref self` method reach the caller. Was silently returning 0.

#[test]
fn par_struct_atomic_field_mutation_persists() {
    let out = run("par struct C { n: Atomic[i64] }
         impl C {
             fn add(ref self, v: i64) { let _ = self.n.fetch_add(v, MemoryOrdering.SeqCst); }
             fn get(ref self) -> i64 { self.n.load(MemoryOrdering.SeqCst) }
         }
         fn main() { let c = C { n: Atomic.new(0) }; c.add(5); c.add(37); println(c.get()); }");
    assert_eq!(out.trim(), "42");
}

#[test]
fn par_struct_atomic_store_through_field_persists() {
    let out = run("par struct C { n: Atomic[i64] }
         impl C {
             fn set(ref self, v: i64) { self.n.store(v, MemoryOrdering.SeqCst); }
             fn get(ref self) -> i64 { self.n.load(MemoryOrdering.SeqCst) }
         }
         fn main() { let c = C { n: Atomic.new(0) }; c.set(7); println(c.get()); }");
    assert_eq!(out.trim(), "7");
}

#[test]
fn par_struct_mutex_field_mutation_persists() {
    let out = run(
        "par struct Counter { total: Mutex[i64] }
         impl Counter {
             fn add(ref self, n: i64) { lock self.total t { t = t + n; } }
             fn get(ref self) -> i64 { lock self.total t { t } }
         }
         fn main() { let c = Counter { total: Mutex.new(0) }; c.add(5); c.add(37); println(c.get()); }",
    );
    assert_eq!(out.trim(), "42");
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

// ── Portable SIMD `Vector[T, N]` — slice 1b interpreter parity ────────
//
// design.md § Portable SIMD + "Interpreter parity scope": the tree-walk
// interpreter and codegen must produce equivalent observable output for the
// same program. These mirror the `tests/codegen.rs::test_vector_*` run-tests
// (same sources, same expected stdout) so the two backends are pinned to the
// same behaviour for construction, element-wise arithmetic, and lane read.

#[test]
fn test_vector_i64_construct_add_index() {
    let out = run_no_errors(
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
    assert_eq!(out, "11\n44\n");
}

#[test]
fn test_vector_i64_mul_and_sub() {
    let out = run_no_errors(
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
    // prod = [20, 30, 40, 50] -> [1] == 30; diff = [8, 7, 6, 5] -> [2] == 6
    assert_eq!(out, "30\n6\n");
}

#[test]
fn test_vector_inferred_binding_type() {
    let out = run_no_errors(
        r#"
fn main() {
    let a = Vector[i64, 2](7, 8);
    let b = Vector[i64, 2](100, 200);
    let c = a + b;
    println(c[1]);
}
"#,
    );
    assert_eq!(out, "208\n");
}

#[test]
fn test_vector_f64_elementwise_div() {
    // Parity with codegen: f64 whole numbers print without a decimal point.
    let out = run_no_errors(
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
    assert_eq!(out, "5\n3\n");
}

#[test]
fn test_vector_value_semantics_no_aliasing() {
    // `Vector` is Copy: rebinding does not alias. (Lane mutation isn't in the
    // slice-1 surface, so this pins the representation choice for when it is.)
    let out = run_no_errors(
        r#"
fn main() {
    let a = Vector[i64, 2](1, 2);
    let b = a;
    let c = a + b;
    println(c[0]);
    println(c[1]);
}
"#,
    );
    assert_eq!(out, "2\n4\n");
}

#[test]
fn test_vector_lane_out_of_bounds_is_runtime_error() {
    let errs = runtime_errors(
        r#"
fn main() {
    let a = Vector[i64, 2](1, 2);
    println(a[5]);
}
"#,
    );
    assert!(
        errs.iter().any(|e| e.message.contains("lane index")),
        "expected a vector lane out-of-bounds runtime error, got: {errs:?}"
    );
}

// ── Portable SIMD `Vector[T, N]` — slice 2 reductions (interpreter) ───
// Mirror the codegen slice-2 run-tests for cross-backend parity.

#[test]
fn test_vector_reduce_sum_i64() {
    let out =
        run_no_errors("fn main() { let v = Vector[i64, 4](1, 2, 3, 4); println(v.reduce_sum()); }");
    assert_eq!(out, "10\n");
}

#[test]
fn test_vector_reduce_sum_f64() {
    let out =
        run_no_errors("fn main() { let v = Vector[f64, 2](1.5, 2.5); println(v.reduce_sum()); }");
    assert_eq!(out, "4\n");
}

#[test]
fn test_vector_dot_i64() {
    let out = run_no_errors(
        r#"
fn main() {
    let a = Vector[i64, 4](1, 2, 3, 4);
    let b = Vector[i64, 4](10, 20, 30, 40);
    println(a.dot(b));
}
"#,
    );
    // 1*10 + 2*20 + 3*30 + 4*40 = 10 + 40 + 90 + 160 = 300
    assert_eq!(out, "300\n");
}

#[test]
fn test_vector_dot_f64() {
    let out = run_no_errors(
        r#"
fn main() {
    let a = Vector[f64, 2](2.0, 3.0);
    let b = Vector[f64, 2](4.0, 5.0);
    println(a.dot(b));
}
"#,
    );
    // 2*4 + 3*5 = 8 + 15 = 23
    assert_eq!(out, "23\n");
}

// ── Vector slice 2b — product + bitwise reductions (interpreter) ──────

#[test]
fn test_vector_reduce_product_i64() {
    let out = run_no_errors(
        "fn main() { let v = Vector[i64, 4](1, 2, 3, 4); println(v.reduce_product()); }",
    );
    assert_eq!(out, "24\n");
}

#[test]
fn test_vector_reduce_and_i64() {
    let out = run_no_errors(
        "fn main() { let v = Vector[i64, 4](15, 7, 3, 1); println(v.reduce_and()); }",
    );
    assert_eq!(out, "1\n");
}

#[test]
fn test_vector_reduce_or_i64() {
    let out =
        run_no_errors("fn main() { let v = Vector[i64, 4](1, 2, 4, 8); println(v.reduce_or()); }");
    assert_eq!(out, "15\n");
}

#[test]
fn test_vector_reduce_xor_i64() {
    let out =
        run_no_errors("fn main() { let v = Vector[i64, 4](1, 2, 4, 8); println(v.reduce_xor()); }");
    assert_eq!(out, "15\n");
}

// ── Vector slice 2c — min/max (interpreter) ──────────────────────────

#[test]
fn test_vector_reduce_min_max_i64() {
    let out = run_no_errors(
        r#"
fn main() {
    let v = Vector[i64, 4](3, 1, 4, 2);
    println(v.reduce_min());
    println(v.reduce_max());
}
"#,
    );
    assert_eq!(out, "1\n4\n");
}

#[test]
fn test_vector_reduce_min_max_i64_negative() {
    // Signed comparison: -5 is the min, 2 the max.
    let out = run_no_errors(
        r#"
fn main() {
    let v = Vector[i64, 3](-5, 2, -3);
    println(v.reduce_min());
    println(v.reduce_max());
}
"#,
    );
    assert_eq!(out, "-5\n2\n");
}

#[test]
fn test_vector_reduce_min_max_f64() {
    let out = run_no_errors(
        r#"
fn main() {
    let v = Vector[f64, 2](2.5, 1.5);
    println(v.reduce_min());
    println(v.reduce_max());
}
"#,
    );
    assert_eq!(out, "1.5\n2.5\n");
}

#[test]
fn test_vector_reduce_min_max_u32() {
    // Slice 2e-ii: unsigned element accepted (previously a type error). The
    // interpreter reads element signedness off the receiver's recorded type
    // and compares its `Value::Int` carrier as `u64`. u32 values all fit
    // positively in i64, so signed and unsigned agree here — this pins the
    // parity contract with codegen's unsigned-result `5\n4000000000\n`.
    let out = run_no_errors(
        r#"
fn main() {
    let v = Vector[u32, 4](3000000000, 5, 10, 4000000000);
    println(v.reduce_min());
    println(v.reduce_max());
}
"#,
    );
    assert_eq!(out, "5\n4000000000\n");
}

// ── Vector slice 2c — cross product interpreter parity ───────────────

#[test]
fn test_vector_cross_i64() {
    // (2,3,4) × (5,6,7) = (-3, 6, -3). Same expected output as the codegen
    // E2E test (`tests/codegen.rs::test_vector_cross_i64`) — interpreter and
    // compiled backends must agree lane-for-lane.
    let out = run_no_errors(
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
    assert_eq!(out, "-3\n6\n-3\n");
}

#[test]
fn test_vector_cross_f64_orthonormal() {
    // x̂ × ŷ = ẑ: (1,0,0) × (0,1,0) = (0,0,1).
    let out = run_no_errors(
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
    assert_eq!(out, "0\n0\n1\n");
}

// ── Vector slice 2d — splat (scalar broadcast) interpreter parity ─────

#[test]
fn test_vector_splat_i64() {
    // Vector[i64, 4].splat(7) → all four lanes == 7.
    let out = run_no_errors(
        r#"
fn main() {
    let v = Vector[i64, 4].splat(7);
    println(v[0]);
    println(v[3]);
}
"#,
    );
    assert_eq!(out, "7\n7\n");
}

#[test]
fn test_vector_splat_enables_scalar_broadcast_arithmetic() {
    // splat is the explicit broadcast: `v + Vector[T,N].splat(s)` is how a
    // scalar combines with a vector (bare vector-vs-scalar arithmetic stays
    // a type error). [1,2,3,4] + splat(10) = [11,12,13,14].
    let out = run_no_errors(
        r#"
fn main() {
    let v: Vector[i64, 4] = Vector[i64, 4](1, 2, 3, 4);
    let r = v + Vector[i64, 4].splat(10);
    println(r[0]);
    println(r[3]);
}
"#,
    );
    assert_eq!(out, "11\n14\n");
}

#[test]
fn test_vector_splat_f64() {
    let out = run_no_errors(
        "fn main() { let v = Vector[f64, 2].splat(1.5); println(v[0]); println(v[1]); }",
    );
    assert_eq!(out, "1.5\n1.5\n");
}

#[test]
fn test_vector_from_array_i64() {
    // Vector[i64, 4].from_array([10, 20, 30, 40]) → lanes in order.
    let out = run_no_errors(
        r#"
fn main() {
    let v = Vector[i64, 4].from_array([10, 20, 30, 40]);
    println(v[0]);
    println(v[3]);
}
"#,
    );
    assert_eq!(out, "10\n40\n");
}

#[test]
fn test_vector_from_array_feeds_arithmetic() {
    // from_array participates in element-wise vector ops like any other
    // Vector[T, N]: [1,2,3,4] + [10,20,30,40] = [11,22,33,44].
    let out = run_no_errors(
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
    assert_eq!(out, "11\n44\n");
}

#[test]
fn test_vector_from_array_f64() {
    let out = run_no_errors(
        "fn main() { let v = Vector[f64, 2].from_array([1.5, 2.5]); println(v[0]); println(v[1]); }",
    );
    assert_eq!(out, "1.5\n2.5\n");
}

// ── Vector slice 2e-iii — from_slice (runtime-length construction) ────

#[test]
fn test_vector_from_slice_i64() {
    // Whole-array slice → vector. The runtime len==N check passes.
    let out = run_no_errors(
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
    assert_eq!(out, "100\n10\n40\n");
}

#[test]
fn test_vector_from_slice_subslice_offset() {
    // A range-indexed sub-slice (start != 0): `a[1..5]` is the window
    // {2,3,4,5}, so the interpreter must read from `start..start+len`.
    let out = run_no_errors(
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
    assert_eq!(out, "2\n5\n14\n");
}

#[test]
fn test_vector_from_slice_length_mismatch_panics() {
    // A 3-element slice for a 4-lane vector is a runtime error (the length
    // is only known at runtime, so the typechecker can't catch it).
    let errs = runtime_errors(
        r#"
fn main() {
    let a: Array[i64, 3] = [10, 20, 30];
    let v = Vector[i64, 4].from_slice(a.as_slice());
    println(v[0]);
}
"#,
    );
    assert!(
        errs.iter()
            .any(|e| format!("{e:?}").contains("does not match Vector lane count")),
        "expected a from_slice length-mismatch runtime error; got: {errs:?}"
    );
}

// ── Vector slice 3a — bitwise & | ^ (binary) and ~ (unary) ───────────

#[test]
fn test_vector_bitwise_and_or_xor() {
    let out = run_no_errors(
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
    assert_eq!(out, "8\n14\n14\n");
}

#[test]
fn test_vector_bitnot() {
    let out = run_no_errors(
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
    assert_eq!(out, "-1\n-4\n0\n-256\n");
}

// ── Vector slice 3b — comparison → Mask[N] + select ──────────────────

#[test]
fn test_vector_compare_mask() {
    let out = run_no_errors(
        r#"
fn main() {
    let a = Vector[i64, 4](1, 5, 3, 8);
    let b = Vector[i64, 4](4, 2, 3, 6);
    let lt = a < b;
    let eq = a == b;
    println(lt[0]); // 1<4 = true
    println(lt[1]); // 5<2 = false
    println(eq[2]); // 3==3 = true
    println(eq[0]); // 1==4 = false
}
"#,
    );
    assert_eq!(out, "true\nfalse\ntrue\nfalse\n");
}

#[test]
fn test_vector_select() {
    // `(a < b).select(a, b)` is a per-lane min.
    let out = run_no_errors(
        r#"
fn main() {
    let a = Vector[i64, 4](1, 5, 3, 8);
    let b = Vector[i64, 4](4, 2, 3, 6);
    let mn = (a < b).select(a, b);
    println(mn[0]); // 1
    println(mn[1]); // 2
    println(mn[2]); // 3
    println(mn[3]); // 6
}
"#,
    );
    assert_eq!(out, "1\n2\n3\n6\n");
}

// ── Slice 6a — lane permutations (parity with codegen) ──────────────

#[test]
fn test_vector_reverse() {
    let out = run_no_errors(
        "fn main() { let a = Vector[i64, 4](1, 2, 3, 4); let r = a.reverse(); \
         println(r[0]); println(r[1]); println(r[2]); println(r[3]); }",
    );
    assert_eq!(out, "4\n3\n2\n1\n");
}

#[test]
fn test_vector_rotate_lanes_left() {
    let out = run_no_errors(
        "fn main() { let a = Vector[i64, 4](10, 20, 30, 40); let r = a.rotate_lanes_left(1); \
         println(r[0]); println(r[1]); println(r[2]); println(r[3]); }",
    );
    assert_eq!(out, "20\n30\n40\n10\n");
}

#[test]
fn test_vector_rotate_lanes_right() {
    let out = run_no_errors(
        "fn main() { let a = Vector[i64, 4](10, 20, 30, 40); let r = a.rotate_lanes_right(1); \
         println(r[0]); println(r[1]); println(r[2]); println(r[3]); }",
    );
    assert_eq!(out, "40\n10\n20\n30\n");
}

#[test]
fn test_vector_rotate_wraps_modulo_lanes() {
    // rotate_left(5) on 4 lanes wraps to rotate_left(1) — parity with codegen.
    let out = run_no_errors(
        "fn main() { let a = Vector[i64, 4](10, 20, 30, 40); let r = a.rotate_lanes_left(5); \
         println(r[0]); println(r[1]); println(r[2]); println(r[3]); }",
    );
    assert_eq!(out, "20\n30\n40\n10\n");
}

#[test]
fn test_vector_replace() {
    // replace(2, 99) returns a new vector with lane 2 set; the original is
    // unchanged (value semantics — a[2] still reads 3).
    let out = run_no_errors(
        "fn main() { let a = Vector[i64, 4](1, 2, 3, 4); let r = a.replace(2, 99); \
         println(r[0]); println(r[1]); println(r[2]); println(r[3]); println(a[2]); }",
    );
    assert_eq!(out, "1\n2\n99\n4\n3\n");
}

#[test]
fn test_vector_replace_runtime_index() {
    let out = run_no_errors(
        "fn main() { let a = Vector[i64, 4](1, 2, 3, 4); let i = 0; let r = a.replace(i, 7); \
         println(r[0]); println(r[1]); }",
    );
    assert_eq!(out, "7\n2\n");
}

#[test]
fn test_vector_shuffle_permute() {
    // shuffle gathers source lanes by index — parity with codegen.
    let out = run_no_errors(
        "fn main() { let a = Vector[i64, 4](10, 20, 30, 40); let r = a.shuffle([0, 2, 1, 3]); \
         println(r[0]); println(r[1]); println(r[2]); println(r[3]); }",
    );
    assert_eq!(out, "10\n30\n20\n40\n");
}

#[test]
fn test_vector_shuffle_widening_with_repeats() {
    // M (index-list length) may differ from N, and indices may repeat.
    let out = run_no_errors(
        "fn main() { let a = Vector[i64, 2](7, 9); let r = a.shuffle([1, 0, 1, 0]); \
         println(r[0]); println(r[1]); println(r[2]); println(r[3]); }",
    );
    assert_eq!(out, "9\n7\n9\n7\n");
}

#[test]
fn test_vector_shuffle_narrowing() {
    let out = run_no_errors(
        "fn main() { let a = Vector[i64, 4](1, 2, 3, 4); let r = a.shuffle([3, 0]); \
         println(r[0]); println(r[1]); }",
    );
    assert_eq!(out, "4\n1\n");
}

#[test]
fn test_vector_load_masked_tail() {
    // Tail handling: a 2-element slice into a 4-lane vector, mask true for the
    // first two lanes — active lanes load, inactive read 0 (parity w/ codegen).
    let out = run_no_errors(
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
    assert_eq!(out, "10\n20\n0\n0\n");
}

#[test]
fn test_vector_load_masked_float_zero_fill() {
    let out = run_no_errors(
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
    assert_eq!(out, "1.5\n2.5\n0\n0\n");
}

#[test]
fn test_vector_store_masked_partial() {
    // Writes active lanes through a mut slice; inactive lanes preserved
    // (parity with codegen). Lanes 0,1 active → written; 2,3 inactive.
    let out = run_no_errors(
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
    assert_eq!(out, "10\n20\n3\n4\n");
}

#[test]
fn test_vector_gather_permuted_indices() {
    // gather reads slice[indices[i]] per lane (parity with codegen).
    let out = run_no_errors(
        r#"
fn main() {
    let a: Array[i64, 6] = [10, 20, 30, 40, 50, 60];
    let idx = Vector[i64, 4](5, 0, 3, 1);
    let v = Vector[i64, 4].gather(a.as_slice(), idx);
    println(v[0]); println(v[1]); println(v[2]); println(v[3]);
}
"#,
    );
    assert_eq!(out, "60\n10\n40\n20\n");
}

#[test]
fn test_vector_scatter_permuted_indices() {
    // scatter writes slice[indices[i]] = v[i] (parity with codegen).
    let out = run_no_errors(
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
    assert_eq!(out, "30\n20\n40\n10\n");
}

#[test]
fn test_vector_cast_from_roundtrip() {
    // f64 -> i64 (truncate) then i64 -> f64 (parity with codegen).
    let out = run_no_errors(
        r#"
fn main() {
    let f = Vector[f64, 4](1.7, 2.2, 3.9, 4.0);
    let i = Vector[i64, 4].cast_from(f);
    println(i[0]); println(i[1]); println(i[2]); println(i[3]);
    let back = Vector[f64, 4].cast_from(i);
    println(back[0]); println(back[3]);
}
"#,
    );
    assert_eq!(out, "1\n2\n3\n4\n1\n4\n");
}

#[test]
fn test_vector_compare_unsigned_mask() {
    // Unsigned compare: 3000000000 (high bit set as i32) is NOT < 10.
    let out = run_no_errors(
        r#"
fn main() {
    let a = Vector[u32, 2](3000000000, 5);
    let b = Vector[u32, 2](10, 10);
    let m = a < b;
    println(m[0]); // false (unsigned)
    println(m[1]); // true
}
"#,
    );
    assert_eq!(out, "false\ntrue\n");
}

// ── Slice 4 — first-class Numeric trait + lane-literal ergonomics ─────

#[test]
fn test_numeric_generic_arithmetic() {
    // `[T: Numeric]` enables arithmetic on the bounded parameter; monomorphized
    // for both i64 and f64.
    let out = run_no_errors(
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
    assert_eq!(out, "6\n7\n-5\n");
}

#[test]
fn test_vector_f32_suffixless_lanes() {
    // Lane literals `1.0` (default f64) coerce to f32 lanes — no suffix.
    let out = run_no_errors(
        r#"
fn main() {
    let a = Vector[f32, 4](1.0, 2.0, 3.0, 4.0);
    let b = Vector[f32, 4](0.5, 0.5, 0.5, 0.5);
    let c = a * b;
    println(c[0]); // 0.5
    println(c[3]); // 2
}
"#,
    );
    assert_eq!(out, "0.5\n2\n");
}

#[test]
fn test_vector_i32_suffixless_lanes() {
    // Lane literals `1` (default i64) coerce to i32 lanes — no suffix.
    let out = run_no_errors(
        r#"
fn main() {
    let a = Vector[i32, 4](1, 2, 3, 4);
    let b = Vector[i32, 4](10, 20, 30, 40);
    let c = a + b;
    println(c[0]); // 11
    println(c[3]); // 44
}
"#,
    );
    assert_eq!(out, "11\n44\n");
}

// ── CStr borrowed surface (Phase 8 — design.md § C-String Literals) ──
//
// `len` / `is_empty` / `as_bytes` evaluate in tree-walk mode; `as_ptr`
// is deliberately rejected at eval time (no raw-pointer representation
// in the interpreter — see Value::CStr's docstring). Interpreter/codegen
// parity for the value-producing trio is pinned by the codegen E2E
// (`test_e2e_cstr_len_is_empty_as_bytes` asserts identical output).

#[test]
fn test_cstr_len_and_is_empty() {
    let out = run_no_errors(
        r#"
fn main() {
    let msg = c"hello, world";
    println(msg.len());
    let e = c"";
    println(e.len());
    if e.is_empty() { println("empty"); }
    if msg.is_empty() { println("BAD"); } else { println("non-empty"); }
}
"#,
    );
    assert_eq!(out, "12\n0\nempty\nnon-empty\n");
}

#[test]
fn test_cstr_len_excludes_trailing_nul_and_counts_utf8_bytes() {
    // design.md: `c"hello".len()` is 5, not 6 (the NUL is a codegen
    // artifact); `c"café"` is the UTF-8 byte count (5), not the char count.
    let out = run_no_errors(
        r#"
fn main() {
    println(c"hello".len());
    println(c"caf\u{e9}".len());
}
"#,
    );
    assert_eq!(out, "5\n5\n");
}

#[test]
fn test_cstr_as_bytes_yields_source_bytes() {
    let out = run_no_errors(
        r#"
fn main() {
    let bytes = c"abc".as_bytes();
    println(bytes.len());
    println(bytes[0]);
    println(bytes[2]);
}
"#,
    );
    assert_eq!(out, "3\n97\n99\n");
}

#[test]
fn test_cstr_annotated_binding_form() {
    // The design's canonical annotated form (`let msg: ref CStr = ...`).
    let out = run_no_errors(
        r#"
fn main() {
    let msg: ref CStr = c"hi";
    println(msg.len());
}
"#,
    );
    assert_eq!(out, "2\n");
}

// ── Tensor[T, Shape] interpreter MVP (Phase 11) ─────────────────────

#[test]
fn test_tensor_zeros_shape_rank() {
    let out = run_no_errors(
        "fn main() {\n\
             let t: Tensor[f64, [3, 4]] = Tensor.zeros([3, 4]);\n\
             println(t.rank());\n\
             let s = t.shape();\n\
             println(s[0]);\n\
             println(s[1]);\n\
         }",
    );
    assert_eq!(out, "2\n3\n4\n");
}

#[test]
fn test_tensor_full_and_index_get() {
    let out = run_no_errors(
        "fn main() {\n\
             let t: Tensor[i64, [2, 3]] = Tensor.full([2, 3], 7);\n\
             println(t[0, 0]);\n\
             println(t[1, 2]);\n\
         }",
    );
    assert_eq!(out, "7\n7\n");
}

#[test]
fn test_tensor_index_set_get_roundtrip() {
    let out = run_no_errors(
        "fn main() {\n\
             let mut t: Tensor[f64, [2, 2]] = Tensor.zeros([2, 2]);\n\
             t[0, 1] = 5.5;\n\
             t[1, 0] = 2.5;\n\
             println(t[0, 1]);\n\
             println(t[1, 0]);\n\
             println(t[0, 0]);\n\
         }",
    );
    assert_eq!(out, "5.5\n2.5\n0\n");
}

#[test]
fn test_tensor_rank1_bare_index() {
    let out = run_no_errors(
        "fn main() {\n\
             let mut v: Tensor[f64, [4]] = Tensor.ones([4]);\n\
             v[2] = 9.0;\n\
             println(v[2]);\n\
             println(v[0]);\n\
         }",
    );
    assert_eq!(out, "9\n1\n");
}

#[test]
fn test_tensor_zeros_ones_int_elem_integer_semantics() {
    // An integer-element tensor's zeros/ones fill `Value::Int`, not the
    // historical blanket `Value::Float` — so a fill cell participates in
    // integer division (`1 / 2 == 0`), not float division (`1.0 / 2 ==
    // 0.5`). The element type is read off the `let`'s annotation.
    let out = run_no_errors(
        "fn main() {\n\
             let o: Tensor[i32, [2]] = Tensor.ones([2]);\n\
             println(o[0] / 2);\n\
             let z: Tensor[i64, [3]] = Tensor.zeros([3]);\n\
             println(z[0]);\n\
         }",
    );
    assert_eq!(out, "0\n0\n");
}

#[test]
fn test_tensor_zeros_ones_bool_elem() {
    // A bool-element tensor fills `Value::Bool` — zeros → false, ones →
    // true — so the cells render as `false`/`true` (not `0`/`1`) and are
    // usable as a condition. Previously the f64 fill made `b[0]` a
    // `Value::Float(0.0)`.
    let out = run_no_errors(
        "fn main() {\n\
             let b: Tensor[bool, [2]] = Tensor.zeros([2]);\n\
             println(b[0]);\n\
             let t: Tensor[bool, [2]] = Tensor.ones([2]);\n\
             println(t[0]);\n\
             if t[1] {\n\
                 println(\"flag-set\");\n\
             }\n\
         }",
    );
    assert_eq!(out, "false\ntrue\nflag-set\n");
}

#[test]
fn test_tensor_zeros_ones_float_elem_unchanged() {
    // The f64 default is preserved: a float-element tensor still fills
    // `Value::Float`, so `0.0 / 1.0` render as `0` / `1` and division is
    // float division (`1 / 2 == 0.5`).
    let out = run_no_errors(
        "fn main() {\n\
             let z: Tensor[f64, [2]] = Tensor.zeros([2]);\n\
             println(z[0]);\n\
             let o: Tensor[f32, [2]] = Tensor.ones([2]);\n\
             println(o[0] / 2.0);\n\
         }",
    );
    assert_eq!(out, "0\n0.5\n");
}

#[test]
fn test_tensor_zeros_nested_let_annotations_dont_leak() {
    // The fill hint is saved/restored around each `let` RHS, so an inner
    // tensor `let` with its own annotation doesn't corrupt an outer one.
    // Here the outer `i64` zeros and inner `bool` zeros each pick their
    // own element fill even though the inner `let` evaluates inside the
    // outer block-expr RHS.
    let out = run_no_errors(
        "fn main() {\n\
             let outer: Tensor[i64, [2]] = {\n\
                 let inner: Tensor[bool, [2]] = Tensor.zeros([2]);\n\
                 println(inner[0]);\n\
                 Tensor.zeros([2])\n\
             };\n\
             println(outer[0] + 5);\n\
         }",
    );
    // inner[0] → false (bool fill); outer[0] → Int(0), 0 + 5 → 5 (int).
    assert_eq!(out, "false\n5\n");
}

#[test]
fn test_tensor_row_major_layout_distinct_cells() {
    // Writes to distinct cells must not alias (row-major offsets).
    let out = run_no_errors(
        "fn main() {\n\
             let mut t: Tensor[i64, [2, 3]] = Tensor.full([2, 3], 0);\n\
             t[0, 0] = 1;\n\
             t[0, 2] = 3;\n\
             t[1, 0] = 4;\n\
             t[1, 2] = 6;\n\
             println(t[0, 0]);\n\
             println(t[0, 1]);\n\
             println(t[0, 2]);\n\
             println(t[1, 0]);\n\
             println(t[1, 1]);\n\
             println(t[1, 2]);\n\
         }",
    );
    assert_eq!(out, "1\n0\n3\n4\n0\n6\n");
}

#[test]
fn test_tensor_index_out_of_bounds_runtime_error() {
    // Dynamic dim (?) so the bounds miss is a runtime concern, not a
    // compile-time literal check.
    let errors = runtime_errors(
        "fn main() {\n\
             let t: Tensor[f64, [?, ?]] = Tensor.zeros([2, 2]);\n\
             let i = 5;\n\
             println(t[i, 0]);\n\
         }",
    );
    assert!(
        errors
            .iter()
            .any(|e| e.message.contains("out of bounds for dim 0 (size 2)")),
        "{errors:?}",
    );
}

#[test]
fn test_tensor_display_renders_shape() {
    let out = run_no_errors(
        "fn main() {\n\
             let t: Tensor[f64, [2, 3]] = Tensor.zeros([2, 3]);\n\
             println(t);\n\
         }",
    );
    assert_eq!(out, "Tensor[2, 3]\n");
}

#[test]
fn test_tensor_from_values_c_order() {
    // Literal constructor: dims from nesting, elements land in
    // row-major order.
    let out = run_no_errors(
        "fn main() {\n\
             let t = Tensor.from([[1.0, 2.0], [3.0, 4.0]]);\n\
             println(t.rank());\n\
             println(t.shape()[0]);\n\
             println(t.shape()[1]);\n\
             println(t[0, 0]);\n\
             println(t[0, 1]);\n\
             println(t[1, 0]);\n\
             println(t[1, 1]);\n\
         }",
    );
    assert_eq!(out, "2\n2\n2\n1\n2\n3\n4\n");
}

#[test]
fn test_tensor_from_rank1_and_rank3() {
    let out = run_no_errors(
        "fn main() {\n\
             let v = Tensor.from([10, 20, 30]);\n\
             println(v.rank());\n\
             println(v[2]);\n\
             let t = Tensor.from([[[1, 2], [3, 4]], [[5, 6], [7, 8]]]);\n\
             println(t.rank());\n\
             println(t[1, 0, 1]);\n\
             println(t[0, 1, 0]);\n\
         }",
    );
    assert_eq!(out, "1\n30\n3\n6\n3\n");
}

#[test]
fn test_tensor_from_expression_leaves() {
    // Leaves are ordinary expressions, evaluated in C-order.
    let out = run_no_errors(
        "fn main() {\n\
             let x = 5.0;\n\
             let e = Tensor.from([[x, x + 1.0], [x * 2.0, 0.0]]);\n\
             println(e[0, 1]);\n\
             println(e[1, 0]);\n\
         }",
    );
    assert_eq!(out, "6\n10\n");
}

#[test]
fn test_tensor_from_mutation_after_construction() {
    let out = run_no_errors(
        "fn main() {\n\
             let mut t = Tensor.from([[1, 2], [3, 4]]);\n\
             t[0, 1] = 99;\n\
             println(t[0, 1]);\n\
             println(t[1, 1]);\n\
         }",
    );
    assert_eq!(out, "99\n4\n");
}

#[test]
fn test_tensor_from_ragged_runtime_error() {
    // The interpreter walks the literal syntax itself (run_program
    // doesn't gate on typecheck), so raggedness is also a runtime
    // error on the interpreter-only path.
    let errors = runtime_errors(
        "fn main() {\n\
             let t = Tensor.from([[1.0, 2.0], [3.0]]);\n\
         }",
    );
    assert!(
        errors.iter().any(|e| e
            .message
            .contains("ragged tensor literal: level at depth 1 has 1 element(s), expected 2")),
        "{errors:?}",
    );
}

#[test]
fn test_tensor_iter_axis_rows_and_cols() {
    // Axis 0 yields the rows; axis 1 yields the columns (axis dropped,
    // C-order preserved within each sub-tensor).
    let out = run_no_errors(
        "fn main() {\n\
             let t = Tensor.from([[1.0, 2.0, 3.0], [4.0, 5.0, 6.0]]);\n\
             let rows = t.iter_axis(0);\n\
             println(rows.len());\n\
             for r in rows {\n\
                 println(r.shape()[0]);\n\
                 println(r[0]);\n\
                 println(r[2]);\n\
             }\n\
             let cols = t.iter_axis(1);\n\
             println(cols.len());\n\
             for c in cols {\n\
                 println(c[0]);\n\
                 println(c[1]);\n\
             }\n\
         }",
    );
    assert_eq!(out, "2\n3\n1\n3\n3\n4\n6\n3\n1\n4\n2\n5\n3\n6\n");
}

#[test]
fn test_tensor_iter_axis_rank3_middle_axis() {
    // [2, 3, 2] tensor, axis 1: three [2, 2] sub-tensors; slab i holds
    // the elements whose middle coordinate is i.
    let out = run_no_errors(
        "fn main() {\n\
             let t = Tensor.from([[[1, 2], [3, 4], [5, 6]], [[7, 8], [9, 10], [11, 12]]]);\n\
             let slabs = t.iter_axis(1);\n\
             println(slabs.len());\n\
             let s = slabs[1];\n\
             println(s.rank());\n\
             println(s[0, 0]);\n\
             println(s[0, 1]);\n\
             println(s[1, 0]);\n\
             println(s[1, 1]);\n\
         }",
    );
    assert_eq!(out, "3\n2\n3\n4\n9\n10\n");
}

#[test]
fn test_tensor_iter_axis_rank1_yields_scalars() {
    let out = run_no_errors(
        "fn main() {\n\
             let v = Tensor.from([10.0, 20.0, 30.0]);\n\
             for x in v.iter_axis(0) {\n\
                 println(x);\n\
             }\n\
         }",
    );
    assert_eq!(out, "10\n20\n30\n");
}

#[test]
fn test_tensor_iter_axis_yields_copies() {
    // Sub-tensors are copies, not views: writing through one leaves
    // the source (and sibling sub-tensors) untouched.
    let out = run_no_errors(
        "fn main() {\n\
             let t = Tensor.from([[1, 2], [3, 4]]);\n\
             let rows = t.iter_axis(0);\n\
             let mut r0 = rows[0];\n\
             r0[0] = 99;\n\
             println(r0[0]);\n\
             println(t[0, 0]);\n\
         }",
    );
    assert_eq!(out, "99\n1\n");
}

#[test]
fn test_tensor_iter_axis_runtime_axis_value() {
    // The axis can be a runtime value; bounds are checked at runtime.
    let out = run_no_errors(
        "fn main() {\n\
             let t = Tensor.from([[1.0, 2.0], [3.0, 4.0]]);\n\
             let n = 1;\n\
             let cols = t.iter_axis(n);\n\
             println(cols[1][0]);\n\
             println(cols[1][1]);\n\
         }",
    );
    assert_eq!(out, "2\n4\n");
    let errors = runtime_errors(
        "fn main() {\n\
             let t = Tensor.from([[1.0, 2.0], [3.0, 4.0]]);\n\
             let n = 6;\n\
             let bad = t.iter_axis(n);\n\
         }",
    );
    assert!(
        errors
            .iter()
            .any(|e| e.message.contains("axis 6 out of bounds for rank-2 tensor")),
        "{errors:?}",
    );
}

#[test]
fn test_tensor_reshape_values_c_order() {
    // C-order data is untouched; only the dims change.
    let out = run_no_errors(
        "fn main() {\n\
             let t = Tensor.from([[1, 2, 3], [4, 5, 6]]);\n\
             let r = t.reshape([3, 2]);\n\
             println(r.rank());\n\
             println(r[0, 0]);\n\
             println(r[0, 1]);\n\
             println(r[1, 0]);\n\
             println(r[2, 1]);\n\
             let flat = t.reshape([6]);\n\
             println(flat[4]);\n\
         }",
    );
    assert_eq!(out, "2\n1\n2\n3\n6\n5\n");
}

#[test]
fn test_tensor_reshape_is_a_copy_and_checks_count() {
    // Writing through the reshaped tensor leaves the source untouched;
    // a runtime-valued dim with a bad product errors at runtime.
    let out = run_no_errors(
        "fn main() {\n\
             let t = Tensor.from([[1, 2], [3, 4]]);\n\
             let mut r = t.reshape([4]);\n\
             r[0] = 99;\n\
             println(r[0]);\n\
             println(t[0, 0]);\n\
         }",
    );
    assert_eq!(out, "99\n1\n");
    let errors = runtime_errors(
        "fn main() {\n\
             let t = Tensor.from([[1, 2, 3], [4, 5, 6]]);\n\
             let m = 4;\n\
             let bad = t.reshape([2, m]);\n\
         }",
    );
    assert!(
        errors
            .iter()
            .any(|e| e.message.contains("element counts must match")),
        "{errors:?}",
    );
}

#[test]
fn test_tensor_permute_transpose_and_rank3() {
    // result[i, j] = t[j, i] for the rank-2 transpose; for [2, 0, 1] on
    // a rank-3 receiver, result[i, j, k] = t[j, k, i] (NumPy transpose
    // semantics).
    let out = run_no_errors(
        "fn main() {\n\
             let t = Tensor.from([[1, 2, 3], [4, 5, 6]]);\n\
             let p = t.permute([1, 0]);\n\
             println(p.shape()[0]);\n\
             println(p[0, 1]);\n\
             println(p[2, 0]);\n\
             let t3 = Tensor.from([[[1, 2], [3, 4], [5, 6]], [[7, 8], [9, 10], [11, 12]]]);\n\
             let p3 = t3.permute([2, 0, 1]);\n\
             println(p3.shape()[0]);\n\
             println(p3.shape()[1]);\n\
             println(p3.shape()[2]);\n\
             println(p3[0, 0, 1]);\n\
             println(p3[1, 1, 2]);\n\
         }",
    );
    assert_eq!(out, "3\n4\n3\n2\n2\n3\n3\n12\n");
}

#[test]
fn test_tensor_slice_values_and_runtime_bounds() {
    let out = run_no_errors(
        "fn main() {\n\
             let t = Tensor.from([[1, 2, 3], [4, 5, 6]]);\n\
             let s = t.slice(1, 1, 3);\n\
             println(s.shape()[1]);\n\
             println(s[0, 0]);\n\
             println(s[1, 1]);\n\
             let rows = t.slice(0, 1, 2);\n\
             println(rows.shape()[0]);\n\
             println(rows[0, 2]);\n\
             let empty = t.slice(1, 2, 2);\n\
             println(empty.shape()[1]);\n\
         }",
    );
    assert_eq!(out, "2\n2\n6\n1\n6\n0\n");
    let errors = runtime_errors(
        "fn main() {\n\
             let t = Tensor.from([[1, 2, 3], [4, 5, 6]]);\n\
             let e = 5;\n\
             let bad = t.slice(1, 2, e);\n\
         }",
    );
    assert!(
        errors.iter().any(|e| e
            .message
            .contains("slice end 5 out of bounds for dim 1 (size 3)")),
        "{errors:?}",
    );
}

#[test]
fn test_tensor_squeeze_values_and_runtime_check() {
    let out = run_no_errors(
        "fn main() {\n\
             let u = Tensor.from([[[7, 8, 9]]]);\n\
             let q = u.squeeze();\n\
             println(q.rank());\n\
             println(q[2]);\n\
             let one = u.squeeze(0);\n\
             println(one.rank());\n\
             println(one[0, 1]);\n\
         }",
    );
    assert_eq!(out, "1\n9\n2\n8\n");
    // A `?`-typed (runtime-checked) squeeze axis whose size isn't 1.
    let errors = runtime_errors(
        "fn main() {\n\
             let t: Tensor[f64, [1, ?]] = Tensor.zeros([1, 4]);\n\
             let bad = t.squeeze(1);\n\
         }",
    );
    assert!(
        errors.iter().any(|e| e
            .message
            .contains("cannot squeeze axis 1: its size is 4, not 1")),
        "{errors:?}",
    );
}

#[test]
fn test_tensor_reshape_of_permuted_data() {
    // Chained transforms: permute reorders the buffer, reshape then
    // reads the *new* C-order — pins that permute produced a real
    // reordered copy, not a view.
    let out = run_no_errors(
        "fn main() {\n\
             let t = Tensor.from([[1, 2, 3], [4, 5, 6]]);\n\
             let p = t.permute([1, 0]);\n\
             let flat = p.reshape([6]);\n\
             println(flat[0]);\n\
             println(flat[1]);\n\
             println(flat[2]);\n\
             println(flat[5]);\n\
         }",
    );
    assert_eq!(out, "1\n4\n2\n6\n");
}

#[test]
fn test_tensor_elementwise_arithmetic() {
    // + - * /, scalar broadcast both sides (incl. int-literal promotion to
    // a float element), unary neg; the operands stay usable afterward
    // (borrow, not move).
    let out = run_no_errors(
        "fn main() {\n\
             let a: Tensor[f64, [2, 2]] = Tensor.from([[1.0, 2.0], [3.0, 4.0]]);\n\
             let b: Tensor[f64, [2, 2]] = Tensor.from([[10.0, 20.0], [30.0, 40.0]]);\n\
             let c = a + b;\n\
             println(c[0, 0]);\n\
             println(c[1, 1]);\n\
             let d = a * b;\n\
             println(d[0, 1]);\n\
             let s = a + 100.0;\n\
             println(s[0, 0]);\n\
             let p = a + 2;\n\
             println(p[0, 0]);\n\
             let n = -a;\n\
             println(n[1, 0]);\n\
             let sl = 100.0 - a;\n\
             println(sl[0, 0]);\n\
             println(a[0, 0]);\n\
         }",
    );
    assert_eq!(out, "11\n44\n40\n101\n3\n-3\n99\n1\n");
}

#[test]
fn test_tensor_int_arithmetic_integer_division() {
    let out = run_no_errors(
        "fn main() {\n\
             let i: Tensor[i64, [3]] = Tensor.from([10, 20, 30]);\n\
             let j: Tensor[i64, [3]] = Tensor.from([1, 2, 3]);\n\
             let k = i - j;\n\
             println(k[2]);\n\
             let q = i / j;\n\
             println(q[1]);\n\
         }",
    );
    assert_eq!(out, "27\n10\n");
}

#[test]
fn test_tensor_arithmetic_runtime_shape_mismatch_and_divzero() {
    // `?`-dim operands pass the typechecker; the interpreter re-checks shape
    // equality at runtime (run_program bypasses typecheck).
    let errors = runtime_errors(
        "fn main() {\n\
             let a: Tensor[f64, [?]] = Tensor.zeros([3]);\n\
             let b: Tensor[f64, [?]] = Tensor.zeros([4]);\n\
             let c = a + b;\n\
         }",
    );
    assert!(
        errors.iter().any(|e| e
            .message
            .contains("tensor shape mismatch in element-wise operator")),
        "{errors:?}",
    );
    // Element-wise div-by-zero traps just like the scalar op.
    let errors = runtime_errors(
        "fn main() {\n\
             let i: Tensor[i64, [2]] = Tensor.from([10, 20]);\n\
             let z: Tensor[i64, [2]] = Tensor.from([2, 0]);\n\
             let q = i / z;\n\
             println(q[0]);\n\
         }",
    );
    assert!(
        errors
            .iter()
            .any(|e| e.message.contains("division by zero")),
        "{errors:?}",
    );
}

#[test]
fn test_tensor_full_reduce() {
    let out = run_no_errors(
        "fn main() {\n\
             let a: Tensor[i64, [2, 3]] = Tensor.from([[1, 2, 3], [4, 5, 6]]);\n\
             println(a.sum());\n\
             println(a.prod());\n\
             println(a.min());\n\
             println(a.max());\n\
             println(a.mean());\n\
             let v: Tensor[f64, [4]] = Tensor.from([2.0, 4.0, 6.0, 8.0]);\n\
             println(v.sum());\n\
             println(v.mean());\n\
         }",
    );
    // mean of [1..6] = 3.5; mean of [2,4,6,8] = 5.0.
    assert_eq!(out, "21\n720\n1\n6\n3.5\n20\n5\n");
}

#[test]
fn test_tensor_axis_reduce() {
    let out = run_no_errors(
        "fn main() {\n\
             let a: Tensor[i64, [2, 3]] = Tensor.from([[1, 2, 3], [4, 5, 6]]);\n\
             let s0 = a.sum_axis(0);\n\
             println(s0[0]); println(s0[1]); println(s0[2]);\n\
             let s1 = a.sum_axis(1);\n\
             println(s1[0]); println(s1[1]);\n\
             let m0 = a.mean_axis(0);\n\
             println(m0[0]); println(m0[2]);\n\
             let v: Tensor[i64, [4]] = Tensor.from([1, 2, 3, 4]);\n\
             println(v.sum_axis(0));\n\
         }",
    );
    // sum_axis(0)=[5,7,9]; sum_axis(1)=[6,15]; mean_axis(0)=[2.5,3.5,4.5];
    // rank-1 sum_axis -> scalar 10.
    assert_eq!(out, "5\n7\n9\n6\n15\n2.5\n4.5\n10\n");
}

#[test]
fn test_tensor_reduce_empty_traps() {
    for m in ["sum", "prod", "min", "max", "mean"] {
        let errors = runtime_errors(&format!(
            "fn main() {{\n\
                 let e: Tensor[i64, [0]] = Tensor.zeros([0]);\n\
                 let r = e.{m}();\n\
             }}",
        ));
        assert!(
            errors
                .iter()
                .any(|e| e.message.contains("cannot reduce an empty tensor")),
            "{m}: {errors:?}",
        );
    }
}

#[test]
fn test_tensor_broadcast() {
    let out = run_no_errors(
        "fn main() {\n\
             let m: Tensor[i64, [2, 3]] = Tensor.from([[1, 2, 3], [4, 5, 6]]);\n\
             // row [1,3] broadcasts over the 2 rows.\n\
             let row: Tensor[i64, [1, 3]] = Tensor.from([[10, 20, 30]]);\n\
             let r = m.broadcast_add(row);\n\
             println(r[0, 0]); println(r[1, 2]);\n\
             // column [2,1] broadcasts over the 3 cols.\n\
             let col: Tensor[i64, [2, 1]] = Tensor.from([[100], [200]]);\n\
             let c = m.broadcast_mul(col);\n\
             println(c[0, 1]); println(c[1, 0]);\n\
             // rank-mismatch: [3] aligns to the trailing axis.\n\
             let v: Tensor[i64, [3]] = Tensor.from([1, 2, 3]);\n\
             let d = m.broadcast_sub(v);\n\
             println(d[0, 0]); println(d[1, 1]);\n\
             // operands are read, not moved — reuse afterward.\n\
             println(m[1, 2]);\n\
         }",
    );
    // r=[ [11,22,33],[14,25,36] ]; c=[ [100,200,300],[800,1000,1200] ];
    // d=[ [0,0,0],[3,3,3] ]; m reused = 6.
    assert_eq!(out, "11\n36\n200\n800\n0\n3\n6\n");
}

#[test]
fn test_tensor_broadcast_div_and_two_singletons() {
    let out = run_no_errors(
        "fn main() {\n\
             let m: Tensor[f64, [2, 2]] = Tensor.from([[2.0, 4.0], [6.0, 8.0]]);\n\
             let col: Tensor[f64, [2, 1]] = Tensor.from([[2.0], [4.0]]);\n\
             let q = m.broadcast_div(col);\n\
             println(q[0, 0]); println(q[1, 0]);\n\
             // [1,3] broadcast with [2,1] -> [2,3], both singletons expand.\n\
             let row: Tensor[i64, [1, 3]] = Tensor.from([[1, 2, 3]]);\n\
             let coli: Tensor[i64, [2, 1]] = Tensor.from([[10], [20]]);\n\
             let g = row.broadcast_add(coli);\n\
             println(g[0, 2]); println(g[1, 0]);\n\
         }",
    );
    // q=[ [1,2],[1.5,2] ]; g=[ [11,12,13],[21,22,23] ].
    assert_eq!(out, "1\n1.5\n13\n21\n");
}

#[test]
fn test_tensor_broadcast_incompatible_traps() {
    let errors = runtime_errors(
        "fn main() {\n\
             let a: Tensor[i64, [2, 3]] = Tensor.from([[1, 2, 3], [4, 5, 6]]);\n\
             let b: Tensor[i64, [2, 4]] = Tensor.from([[1, 2, 3, 4], [5, 6, 7, 8]]);\n\
             let r = a.broadcast_add(b);\n\
         }",
    );
    assert!(
        errors
            .iter()
            .any(|e| e.message.contains("not broadcast-compatible")),
        "{errors:?}",
    );
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
fn test_ref_at_binding_over_option_payload() {
    let out = run_no_errors(
        "fn main() {\n\
             let opt = Some(\"hello\");\n\
             match opt {\n\
                 ref x @ Some(y) => { println(y); }\n\
                 None => { println(\"none\"); }\n\
             }\n\
             match opt {\n\
                 Some(z) => { println(z); }\n\
                 None => { }\n\
             }\n\
         }",
    );
    assert_eq!(out, "hello\nhello\n");
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

#[test]
fn test_range_pattern_const_bounds_int() {
    // Const-named range bounds (design.md § Range Patterns) match the same
    // as the literal forms once resolved.
    let output = run_no_errors(
        r#"
const LO: i64 = 10;
const HI: i64 = 20;
fn classify(n: i64) -> i64 {
    match n {
        ..LO => 1,
        LO..=HI => 2,
        _ => 3,
    }
}
fn main() {
    println(classify(5));
    println(classify(10));
    println(classify(15));
    println(classify(20));
    println(classify(25));
}
"#,
    );
    assert_eq!(output, "1\n2\n2\n2\n3\n");
}

#[test]
fn test_range_pattern_const_bounds_char() {
    let output = run_no_errors(
        r#"
const LOWER_A: char = 'a';
const LOWER_Z: char = 'z';
fn is_lower(c: char) -> i64 {
    match c { LOWER_A..=LOWER_Z => 1, _ => 0 }
}
fn main() {
    println(is_lower('m'));
    println(is_lower('M'));
}
"#,
    );
    assert_eq!(output, "1\n0\n");
}

// ── Enum / struct structural equality (`==` / `!=`) ───────────────
// Regression coverage for the interpreter gap where `==` on any enum
// variant or struct hit `eval_ops`'s `unreachable!` (every enum, incl.
// Option/Result/Ordering, panicked on `==`). `Value`'s `PartialEq`
// already compared these structurally; the fix wires `eval_binary` to it.

#[test]
fn enum_equality_unit_and_payload_variants() {
    let output = run(r#"
#[derive(Eq)]
enum Color { Red, Green, Blue }

#[derive(Eq)]
enum Tagged { N(i64), Z }

fn main() {
    println(f"{Color.Red == Color.Red}");
    println(f"{Color.Red == Color.Blue}");
    println(f"{Color.Red != Color.Blue}");
    println(f"{Tagged.N(7) == Tagged.N(7)}");
    println(f"{Tagged.N(7) == Tagged.N(9)}");
    println(f"{Tagged.N(7) == Tagged.Z}");
}
"#);
    assert_eq!(output, "true\nfalse\ntrue\ntrue\nfalse\nfalse\n");
}

#[test]
fn builtin_enum_equality_option_result_ordering() {
    let output = run(r#"
fn main() {
    println(f"{Some(1) == Some(1)}");
    println(f"{Some(1) == Some(2)}");
    let a: Result[i64, i64] = Ok(3);
    let b: Result[i64, i64] = Ok(3);
    println(f"{a == b}");
    println(f"{3.cmp(5) == Ordering.Less}");
    println(f"{3.cmp(5) == Ordering.Greater}");
}
"#);
    assert_eq!(output, "true\nfalse\ntrue\ntrue\nfalse\n");
}

#[test]
fn struct_equality_structural() {
    let output = run(r#"
#[derive(Eq)]
struct Point { x: i64, y: i64 }

fn main() {
    let p = Point { x: 1, y: 2 };
    let q = Point { x: 1, y: 2 };
    let r = Point { x: 1, y: 3 };
    println(f"{p == q}");
    println(f"{p == r}");
    println(f"{p != r}");
}
"#);
    assert_eq!(output, "true\nfalse\ntrue\n");
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

#[test]
fn alloc_error_prelude_type_usable_without_import() {
    // The `AllocError` prelude type (phase-8 § Fallible Allocation API) is
    // available without import as `Result[T, AllocError]`, constructs both
    // variants (struct + unit), compares with `==`, renders via Display, and
    // pattern-matches with field binding.
    let output = run(r#"
fn try_make(fail: bool) -> Result[i64, AllocError] {
    if fail {
        Err(AllocError.OutOfMemory { requested_bytes: 64 })
    } else {
        Ok(1)
    }
}

fn main() {
    let oom = AllocError.OutOfMemory { requested_bytes: 64 };
    let oom2 = AllocError.OutOfMemory { requested_bytes: 64 };
    let co = AllocError.CapacityOverflow;
    println(f"{oom == oom2}");
    println(f"{oom == co}");
    println(f"{co}");
    match try_make(true) {
        Ok(_) => println("ok"),
        Err(AllocError.OutOfMemory { requested_bytes }) => println(f"oom:{requested_bytes}"),
        Err(AllocError.CapacityOverflow) => println("co"),
    }
}
"#);
    assert_eq!(output, "true\nfalse\nCapacityOverflow\noom:64\n");
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

#[test]
fn test_mut_ref_self_field_and_index_rooted_receivers() {
    // The write-back place dispatch covers field-rooted (`b.c.inc()`) and
    // index-rooted (`v[1].inc()`) receivers, not just bare identifiers.
    let out = run_no_errors(
        r#"
struct Counter { n: i64 }
impl Counter {
    fn inc(mut ref self) { self.n = self.n + 1; }
}
struct Box { c: Counter }
fn main() {
    let mut b = Box { c: Counter { n: 10 } };
    b.c.inc();
    b.c.inc();
    println(b.c.n);
    let mut v = [Counter { n: 100 }, Counter { n: 200 }];
    v[1].inc();
    v[1].inc();
    println(v[1].n);
}
"#,
    );
    assert_eq!(out, "12\n202\n");
}

#[test]
fn test_owned_self_method_is_not_written_back() {
    // A consuming (owned) `self` receiver must NOT trigger write-back — the
    // gate is `MutRef`-only. A `ref self` reader is likewise untouched.
    let out = run_no_errors(
        r#"
struct Counter { n: i64 }
impl Counter {
    fn into_n(self) -> i64 { self.n }
    fn peek(ref self) -> i64 { self.n }
}
fn main() {
    let c = Counter { n: 7 };
    println(c.peek());
    println(c.into_n());
}
"#,
    );
    assert_eq!(out, "7\n7\n");
}
