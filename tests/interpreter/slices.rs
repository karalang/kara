//! slices, indexing, windows and chunks -- fixtures for `tests/interpreter.rs`.
//!
//! Split out of `tests/interpreter.rs` on 2026-09-21. The TEST TARGET is
//! unchanged: this file is a module of that target, so
//! `cargo test --features llvm --test interpreter` still runs everything
//! and CI needs no edit. Run this area alone with:
//!
//!     cargo test --features llvm --test interpreter slices::
//!
//! New fixtures about slices, indexing, windows and chunks belong in this file.

use super::*;

#[test]
fn test_shared_vec_field_index_field_read_store_aliasing() {
    // B-2026-07-13-10 oracle pin. A chained read/store through a Vec that is a
    // FIELD of a shared struct (`root.kids[i].val`) is reference-semantics: the
    // pushed element is the SAME RC object as the original handle, so a write
    // through the Vec-field chain is observable via that handle (`a`/`b`). The
    // codegen backends returned the const-0 placeholder for the read and
    // silently dropped the store until fixed; the interpreter has always been
    // correct and is the oracle this locks.
    let src = "shared struct Node { mut val: i64, mut kids: Vec[Node] }
        fn main() {
            let root = Node { val: 1, kids: Vec.new() };
            let a = Node { val: 10, kids: Vec.new() };
            let b = Node { val: 20, kids: Vec.new() };
            root.kids.push(a);
            root.kids.push(b);
            println(f\"{root.kids[0].val}\");
            root.kids[0].val = root.kids[0].val + 5;
            root.kids[1].val = 99;
            println(f\"{root.kids[0].val}\");
            println(f\"{root.kids[1].val}\");
            println(f\"{a.val}\");
            println(f\"{b.val}\");
        }";
    assert_eq!(run_no_errors(src), "10\n15\n99\n15\n99\n");
}

#[test]
fn slice_ordering_in_the_interpreter() {
    // B-2026-08-27-45 -- the last member of the tuple / array / slice family,
    // and the same shape as the other two: `type_supports_ord` carries a
    // `Type::Slice` arm, so `karac check` printed "All checks passed." while
    // the interpreter died on the catch-all whose message claims the
    // typechecker reports this as a hard error. It does not, and did not.
    //
    // `value_compare` has had a `Slice` arm (comparing the viewed ranges) as
    // long as its `Vec` one, so this side needed only the dispatch -- unlike
    // codegen, which needed the `Vec` comparator's header made a parameter.
    // Both order by the same rule, which is what makes this the oracle for
    // `test_e2e_slice_ordering`.
    //
    // The prefix row (`p < a`) pins the length tiebreak after a common
    // prefix compares equal; the `heap` rows pin per-element CONTENT
    // comparison, with element 0 content-equal but separately built so a
    // pointer-word comparison could not pass them by accident.
    //
    // The last two rows are the deliberate NON-fix, pinned so a later widening
    // has to be deliberate and has to move both backends together: a slice of
    // `Vec`s and a slice of bare floats each decline here and on the compiled
    // backend. The float case is the carve-out the tuple row records
    // (`value_compare` orders a float by `total_cmp`, a sort key's order, not
    // `<`'s); the `Vec` case is what the per-element gate exists to decline.
    let src = "fn build(p: String) -> String {
            let mut s = String.new();
            s.push_str(p);
            s.push_str(\"x\");
            return s;
        }
        fn main() {
            let mut v: Vec[i64] = Vec.new();
            v.push(1); v.push(2); v.push(1); v.push(3);
            let a: Slice[i64] = v[0..2];
            let b: Slice[i64] = v[2..4];
            let c: Slice[i64] = v[0..2];
            println(f\"{a < b}\");
            println(f\"{b < a}\");
            println(f\"{a < c}\");
            println(f\"{a <= c}\");
            println(f\"{b > a}\");
            let p: Slice[i64] = v[0..1];
            println(f\"{p < a}\");
            println(f\"{a < p}\");
            let mut w: Vec[String] = Vec.new();
            w.push(build(\"a\")); w.push(\"m\");
            w.push(build(\"a\")); w.push(\"n\");
            let x: Slice[String] = w[0..2];
            let y: Slice[String] = w[2..4];
            println(f\"{x < y}\");
            println(f\"{y < x}\");
        }";
    assert_eq!(
        run_no_errors(src),
        "true\nfalse\nfalse\ntrue\ntrue\n\
         true\nfalse\n\
         true\nfalse\n"
    );
}

#[test]
fn slice_equality_is_content_equality_in_the_interpreter() {
    // B-2026-08-27-24 -- the `Vec` sibling's bug one type over, and unfixed by
    // that work. The interpreter had no `Value::Slice` arm in its binop
    // dispatcher, so `Slice[T] == Slice[T]` fell to the same catch-all, whose
    // message says "this is a type error the typechecker reports as a hard
    // error". False again: `type_supports_partial_eq` has a `Type::Slice` arm
    // right beside the `Vec` one, so `karac check` printed "All checks passed."
    // on the same file.
    //
    // `Value`'s `PartialEq` already compared slices by content, which is why a
    // slice-typed struct field worked and only the bare operator did not.
    //
    // The SAME-BUFFER rows (`c[0..2]` vs `c[1..3]`) carry a second point: two
    // slices of one `Vec` share an `Arc<RwLock<..>>`, and comparing them used
    // to take two read locks on it. `RwLock::read` is not documented to be
    // reentrant, so that pair now takes one lock -- an ordinary comparison
    // must not depend on a recursive read succeeding.
    let src = "fn cmp_slices(a: Slice[i64], b: Slice[i64]) -> bool { return a == b; }
        fn main() {
            let mut a: Vec[i64] = Vec.new();
            a.push(1);
            a.push(2);
            a.push(3);
            let mut c: Vec[i64] = Vec.new();
            c.push(1);
            c.push(2);
            c.push(9);
            let s1: Slice[i64] = a[0..2];
            let s2: Slice[i64] = c[0..2];
            let s3: Slice[i64] = c[1..3];
            let s4: Slice[i64] = a[0..3];
            println(f\"{s1 == s2}\");
            println(f\"{s1 == s3}\");
            println(f\"{s1 != s3}\");
            println(f\"{s1 == s4}\");
            println(f\"{cmp_slices(a[0..2], c[0..2])}\");
            let mut p: Vec[String] = Vec.new();
            p.push(\"hello\");
            let mut q: Vec[String] = Vec.new();
            q.push(\"hello\");
            println(f\"{p[0..1] == q[0..1]}\");
        }";
    assert_eq!(run_no_errors(src), "true\nfalse\ntrue\nfalse\ntrue\ntrue\n");
}

#[test]
fn nested_index_store_from_a_named_local_is_balanced() {
    // B-2026-08-27-20 — a NESTED index store whose RHS is a NAMED LOCAL
    // double-freed under codegen, while the same store from a TEMPORARY was
    // clean and the interpreter was right in both. One binding was the whole
    // difference.
    //
    // The move-suppression that keeps the source from freeing a buffer the
    // container now owns was gated on the target's object being an `Identifier`
    // (`v[i] = x`) or a `FieldAccess` (`h.xs[j] = t`). For `d[0][1] = x` the
    // object is ITSELF an `Index`, so it matched neither arm and never ran: the
    // store released the displaced element and moved `x`'s buffer into the slot,
    // then `x`'s scope-exit cleanup freed that same buffer.
    //
    // `str` is the failing shape. `deep` is the three-level form, which the same
    // arm has to cover because `vec_index_elem_type_expr` peels one Vec layer per
    // index level — a fix that special-cased two levels would pass `str` and
    // still abort here.
    //
    // `int` and `whole` are the controls, and they are why this survived. `int`
    // stores a scalar into `Vec[Vec[i64]]`: nothing is owned, so no suppression
    // is due and over-suppressing would be invisible. `whole` replaces an entire
    // inner Vec (`outer[0] = nb`), which takes the `Identifier` arm that already
    // worked. Between them they pin that the new arm neither under- nor
    // over-reaches.
    //
    // Every payload is built by `mk(n)` rather than written as a literal: two
    // identical string literals can fold to one global, and a double free of a
    // shared global does not necessarily abort, so distinct allocations are what
    // make the defect reachable at all.
    //
    // Codegen twin: `test_e2e_nested_index_store_from_a_named_local_is_balanced`.
    // The interpreter was already right, so this pins it as the oracle.
    let src = r#"
fn mk(n: i64) -> String { return f"v{n}"; }

fn main() {
    let mut r: Vec[String] = Vec.new();
    r.push(mk(1)); r.push(mk(2));
    let mut d: Vec[Vec[String]] = Vec.new();
    d.push(r);
    let x: String = mk(3);
    d[0][1] = x;
    println(f"str={d[0][1]}");

    let mut i0: Vec[i64] = Vec.new();
    i0.push(1); i0.push(2);
    let mut di: Vec[Vec[i64]] = Vec.new();
    di.push(i0);
    let n: i64 = 9;
    di[0][1] = n;
    println(f"int={di[0][1]}");

    let mut inner: Vec[String] = Vec.new();
    inner.push(mk(4));
    let mut mid: Vec[Vec[String]] = Vec.new();
    mid.push(inner);
    let mut top: Vec[Vec[Vec[String]]] = Vec.new();
    top.push(mid);
    let y: String = mk(5);
    top[0][0][0] = y;
    println(f"deep={top[0][0][0]}");

    let mut vv: Vec[String] = Vec.new();
    vv.push(mk(6));
    let mut outer: Vec[Vec[String]] = Vec.new();
    outer.push(vv);
    let mut nb: Vec[String] = Vec.new();
    nb.push(mk(7));
    outer[0] = nb;
    println(f"whole={outer[0][0]}");
}
"#;
    assert_eq!(run_no_errors(src), "str=v3\nint=9\ndeep=v5\nwhole=v7\n");
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
fn test_index_store_into_tuple_element_vec() {
    // B-2026-07-20-3: `t.0[i] = v` (index store into a Vec that lives in a
    // tuple element) was SILENTLY dropped by the interpreter — `set_index`'s
    // target resolver had no `TupleIndex` arm, so the store no-op'd and the
    // element kept its old value. Now the write lands (the Value::Array Arc
    // aliases the tuple element's storage). Read-back confirms scalar and
    // String stores both take.
    assert_eq!(
        run("fn make() -> (Vec[i64], Vec[String]) {\n\
                 let mut a: Vec[i64] = Vec.new();\n\
                 a.push(1); a.push(2); a.push(3);\n\
                 let mut b: Vec[String] = Vec.new();\n\
                 b.push(\"x\"); b.push(\"y\");\n\
                 (a, b)\n\
             }\n\
             fn main() {\n\
                 let mut t = make();\n\
                 t.0[0] = 10;\n\
                 t.0[2] = 30;\n\
                 t.1[1] = \"zebra\";\n\
                 println(t.0[0] + t.0[1] + t.0[2]);\n\
                 println(t.1[0]);\n\
                 println(t.1[1]);\n\
             }"),
        "42\nx\nzebra\n"
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
fn index_assign_out_of_bounds_records_a_runtime_error_like_the_read_does() {
    // B-2026-08-04-14. The READ above was checked from the start; the STORE
    // guarded itself with a bare `if i < len` and fell off the end when that
    // failed, so an out-of-range write left no trace at all — no error, no
    // growth, the store simply vanished. AOT and the JIT both panic on it, so
    // the interpreter was the one backend that let a memory-safety violation
    // through while reporting success.
    for (label, program) in [
        (
            "past the end",
            "fn main() { let mut v: Vec[i64] = Vec.new(); v.push(1); v[100] = 7; }",
        ),
        // Just-past-the-end is the case an off-by-one produces, and the one a
        // `> len` spot check would miss.
        (
            "one past the end",
            "fn main() { let mut v: Vec[i64] = Vec.new(); v.push(1); v[1] = 7; }",
        ),
        (
            "negative",
            "fn main() { let mut v: Vec[i64] = Vec.new(); v.push(1); v[-1] = 7; }",
        ),
    ] {
        let errors = runtime_errors(program);
        assert!(
            errors.iter().any(|e| e.message.contains("out of bounds")),
            "[{label}] an out-of-range index-assign must record a runtime error, got {errors:?}"
        );
    }
    // Control: an in-range store still lands and reports nothing, so the
    // assertions above are about the range check and not about stores at large.
    let errors =
        runtime_errors("fn main() { let mut v: Vec[i64] = Vec.new(); v.push(1); v[0] = 7; }");
    assert!(
        errors.is_empty(),
        "an in-range index-assign must stay silent, got {errors:?}"
    );
}

#[test]
fn test_shared_struct_field_write_through_index_projection() {
    // Same regression via a *container element* projection (`v[0].value = x`):
    // the Vec element is a clone of the same Arc, so the write is visible at
    // the original handle.
    assert_eq!(
        run("shared struct Cell { mut value: i64 }\n\
             fn main() {\n\
                 let c = Cell { value: 1 };\n\
                 let mut v = Vec.new();\n\
                 v.push(c);\n\
                 v[0].value = 42;\n\
                 println(c.value);\n\
             }"),
        "42\n"
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

/// B-2026-09-02-11 (interpreter twin / ORACLE) — the interpreter never clones an
/// indexed element, so a read-only arm over `v[i]` runs the payload's `Drop`
/// body exactly once: at the container's own NLL death, not at the arm's.
///
/// Pinned to the same program and the same string as `tests/codegen.rs`'s
/// `e2e_index_element_clone_does_not_rerun_the_payload_drop_body`, whose doc
/// carries the leg-by-leg rationale. Recording the count on BOTH sides is what
/// stops a future change from re-baselining the codegen twin against whatever
/// the compiled backends happen to print — which is exactly how the `index` leg
/// of the B-2026-08-29-37 fixture came to assert a two-body answer for a
/// one-body source.
#[test]
fn test_index_element_clone_does_not_rerun_the_payload_drop_body() {
    assert_eq!(
        run(r#"struct R { id: i64, v: Vec[i64] }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
enum E { A(R), B }
impl Drop for E { fn drop(mut ref self) { println("dE") } }
struct H { xs: Vec[E] }

struct P { id: i64 }
impl Drop for P { fn drop(mut ref self) { println(f"dP{self.id}") } }
enum Q { A(P), B }
impl Drop for Q { fn drop(mut ref self) { println("dQ") } }

fn mk(n: i64) -> E { let mut v: Vec[i64] = Vec.new(); v.push(n); return E.A(R { id: n, v: v }) }
fn mkq(n: i64) -> Q { return Q.A(P { id: n }) }

fn leg_bare() {
    println("bare");
    let mut v: Vec[E] = Vec.new();
    v.push(mk(1));
    match v[0] { E.A(r) => { println(f"got {r.id + r.v.len()}"); } E.B => { } }
    println("bare end");
}

fn leg_field_rooted() {
    println("field");
    let mut v: Vec[E] = Vec.new();
    v.push(mk(2));
    let h = H { xs: v };
    match h.xs[0] { E.A(r) => { println(f"got {r.id + r.v.len()}"); } E.B => { } }
    println("field end");
}

fn leg_scalar_payload() {
    println("scalar");
    let mut v: Vec[Q] = Vec.new();
    v.push(mkq(3));
    match v[0] { Q.A(p) => { println(f"got {p.id}"); } Q.B => { } }
    println("scalar end");
}

fn leg_unbound() {
    println("wild");
    let mut v: Vec[E] = Vec.new();
    v.push(mk(4));
    match v[0] { E.A(_) => { println("got"); } E.B => { } }
    println("wild end");
}

fn leg_fresh() {
    println("fresh");
    match mk(5) { E.A(r) => { println(f"got {r.id + r.v.len()}"); } E.B => { } }
    println("fresh end");
}

fn leg_local() {
    println("local");
    let e = mk(6);
    match e { E.A(r) => { println(f"got {r.id + r.v.len()}"); } E.B => { } }
    println("local end");
}

fn main() {
    leg_bare();
    leg_field_rooted();
    leg_scalar_payload();
    leg_unbound();
    leg_fresh();
    leg_local();
    println("end");
}
"#),
        r#"bare
got 2
dE
dR1
bare end
field
got 3
dE
dR2
field end
scalar
got 3
dQ
dP3
scalar end
wild
got
dE
dR4
wild end
fresh
got 6
dR5
dE
fresh end
local
got 7
dE
dR6
local end
end
"#
    );
}

/// B-2026-08-27-53 — interpreter twin of `tests/codegen.rs`'s
/// `e2e_slice_param_from_a_borrowed_field_place`, same source and string.
/// The interpreter always ran this shape; codegen could not BUILD it (LLVM
/// module verification rejected a 3-word Vec header at a 2-word `Slice[T]`
/// formal), so this side is the parity target the coercion fix meets.
#[test]
fn test_slice_param_from_a_borrowed_field_place() {
    assert_eq!(
        run(r#"struct Bag[=T] { xs: Vec[T] }
struct Wrap { b: Bag[i64] }
fn head[T](s: Slice[T]) -> T { return s[0]; }
fn via_ref[T](g: ref Bag[T]) -> T { return head(g.xs); }
fn nested(w: ref Wrap) -> i64 { return head(w.b.xs); }
impl[T] Bag[T] {
    fn first(ref self) -> T { return head(self.xs); }
}
fn main() {
    let mut a: Bag[String] = Bag { xs: Vec.new() };
    a.xs.push("aa"); a.xs.push("bb");
    println(a.first());
    println(via_ref(a));
    let mut n: Bag[i64] = Bag { xs: Vec.new() };
    n.xs.push(7); n.xs.push(8);
    println(f"{n.first()}");
    println(f"{via_ref(n)}");
    let w: Wrap = Wrap { b: n };
    println(f"{nested(w)}");
    println("end");
}
"#),
        "aa\naa\n7\n7\n7\nend\n"
    );
}

/// B-2026-08-27-53, write leg — interpreter twin of `tests/codegen.rs`'s
/// `e2e_mut_slice_param_from_a_borrowed_field_place`, same source and string.
#[test]
fn test_mut_slice_param_from_a_borrowed_field_place() {
    assert_eq!(
        run(r#"struct Bag { xs: Vec[i64] }
fn bump(s: mut Slice[i64]) { s[0] = s[0] + 100; }
fn head(s: Slice[i64]) -> i64 { return s[0]; }
fn poke(b: mut ref Bag) { bump(b.xs); }
impl Bag {
    fn go(mut ref self) { bump(mut self.xs); }
    fn peek(ref self) -> i64 { return head(self.xs); }
}
fn main() {
    let mut a: Bag = Bag { xs: Vec.new() };
    a.xs.push(1); a.xs.push(2);
    a.go();
    println(f"{a.peek()}");
    poke(mut a);
    println(f"{a.peek()} {a.xs[1]}");
    println("end");
}
"#),
        "101\n201 2\nend\n"
    );
}

/// B-2026-08-01-22 leg a — interpreter twin of `tests/codegen.rs`'s
/// `e2e_field_rooted_index_assign_displaced_elem_bodies`, same source and
/// expected string.
#[test]
fn test_field_rooted_index_assign_displaced_elem_bodies() {
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
                 h.xs.push(Res { id: 9, name: f\"z{9}\" });\n\
                 h.xs[0] = Res { id: 5, name: f\"y{5}\" };\n\
                 println(f\"held {h.xs[0].id}\");\n\
                 println(\"end\");\n\
             }\n"),
        "a\ndrop 9 z9\nheld 5\ndrop 5 y5\nend\n"
    );
}

/// B-2026-08-01-21 — interpreter twin of `tests/codegen.rs`'s
/// `e2e_index_assign_displaced_elem_bodies`, same source and expected
/// string (the interp leg is bodies-only; memory is GC'd).
#[test]
fn test_index_assign_displaced_elem_bodies() {
    assert_eq!(
        run("struct Res { id: i64, name: String }\n\
             impl Drop for Res {\n\
                 fn drop(mut ref self) {\n\
                     println(f\"drop {self.id} {self.name}\")\n\
                 }\n\
             }\n\
             fn main() {\n\
                 println(\"a\");\n\
                 let mut v: Vec[Res] = Vec.new();\n\
                 v.push(Res { id: 9, name: f\"z{9}\" });\n\
                 v[0] = Res { id: 5, name: f\"y{5}\" };\n\
                 println(f\"held {v[0].id}\");\n\
                 println(\"end\");\n\
             }\n"),
        "a\ndrop 9 z9\nheld 5\ndrop 5 y5\nend\n"
    );
}

/// B-2026-08-14-8 — interpreter twin of `tests/codegen.rs`'s
/// `test_e2e_slice_read_accessors_have_codegen`, same source and expected
/// string.
///
/// The interpreter ran all of these correctly the whole time — the gap was
/// codegen-only — so this twin is the oracle the routed implementation had to
/// match, and the guard against the two drifting as more slice methods land.
#[test]
fn test_slice_read_accessors_match_codegen() {
    assert_eq!(
        run("fn main() {\n\
                 let mut v: Vec[i64] = Vec.new();\n\
                 v.push(3); v.push(1); v.push(2);\n\
                 let s: Slice[i64] = v.as_slice();\n\
                 println(s.contains(2));\n\
                 println(s.contains(9));\n\
                 match s.first() { Some(x) => { println(x); } None => { println(-1); } }\n\
                 match s.last() { Some(x) => { println(x); } None => { println(-1); } }\n\
                 match s.get(1i64) { Some(x) => { println(x); } None => { println(-1); } }\n\
                 match s.get(9i64) { Some(x) => { println(x); } None => { println(-1); } }\n\
                 let mut w: Vec[String] = Vec.new();\n\
                 let mut a = String.new(); a.push_str(\"alpha\"); w.push(a);\n\
                 let mut b = String.new(); b.push_str(\"beta\"); w.push(b);\n\
                 let t: Slice[String] = w.as_slice();\n\
                 match t.first() { Some(x) => { println(x); } None => { println(\"none\"); } }\n\
                 match t.get(1i64) { Some(x) => { println(x); } None => { println(\"none\"); } }\n\
                 let mut cs = String.new(); cs.push_str(\"beta\");\n\
                 println(t.contains(cs));\n\
                 println(v.len());\n\
                 println(w.len());\n\
             }"),
        "true\nfalse\n3\n2\n1\n-1\nalpha\nbeta\ntrue\n3\n2\n"
    );
}

/// B-2026-08-13-16, the other direction: the widening must NOT reach the two
/// things that are shared on purpose.
///
/// A `shared struct` has reference semantics by design, so a mutation through a
/// rebinding IS visible through the original — `deep_clone_value` Arc-bumps it,
/// and this asserts that stays true. A `Slice` is a VIEW, and rebinding one
/// copies the window rather than materializing an owned buffer on every
/// backend; the guard excludes it explicitly, and a deep clone there would
/// change the value's shape, not just its identity.
///
/// This one passes BEFORE the fix as well, and is meant to — it is an
/// over-reach guard, not a regression guard. The regression guard is the test
/// above, which fails on all seven of its lines against the pre-fix
/// interpreter.
#[test]
fn test_let_rebinding_preserves_shared_and_slice_aliasing() {
    assert_eq!(
        run("shared struct Counter { mut n: i64 }\n\
             fn main() {\n\
                 let c = Counter { n: 0 };\n\
                 let d = c;\n\
                 d.n = 5;\n\
                 println(c.n);\n\
                 let mut v: Vec[i64] = Vec.new();\n\
                 v.push(1);\n\
                 v.push(2);\n\
                 let s = v.as_slice_mut();\n\
                 let s2 = s;\n\
                 s2[0] = 9;\n\
                 println(v[0]);\n\
             }"),
        "5\n9\n"
    );
}

/// B-2026-08-12-33 — interpreter twin of `tests/codegen.rs`'s
/// `test_e2e_index_assign_call_rhs_carrying_container_heap_values_survive`,
/// same source and expected string.
///
/// The codegen leg is where the row lives: it frees the displaced element
/// between the call and the store, so the values it prints are evidence that
/// what got stored was the callee's own copy. The interpreter has no such
/// ordering to get wrong (values are copied and memory is GC'd), which is
/// exactly why it belongs here — it fixes the ORACLE those values are checked
/// against, so a future widening of the guard that quietly changed a result
/// would have to disagree with this file to land.
#[test]
fn test_index_assign_call_rhs_carrying_container_heap_values() {
    assert_eq!(
        run("struct Pair { word: String, n: i64 }\n\
             fn passthru(p: Pair) -> Pair { p }\n\
             fn takes(s: String) -> Pair { Pair { word: s + \"!\", n: 1 } }\n\
             fn join(a: String, b: String) -> Pair { Pair { word: a + b, n: 2 } }\n\
             fn peek(p: ref Pair) -> String { p.word }\n\
             fn main() {\n\
                 let k = 1;\n\
                 let mut ps: Vec[Pair] = Vec.new();\n\
                 ps.push(Pair { word: f\"a{k}\", n: 7 });\n\
                 ps.push(Pair { word: f\"b{k}\", n: 8 });\n\
                 ps[0] = passthru(ps[0]);\n\
                 println(f\"{ps[0].word} {ps[0].n}\");\n\
                 ps[0] = takes(ps[0].word);\n\
                 println(f\"{ps[0].word} {ps[0].n}\");\n\
                 ps[0] = join(ps[0].word, ps[1].word);\n\
                 println(f\"{ps[0].word} {ps[1].word}\");\n\
                 ps[0] = Pair { word: peek(ps[0]), n: 9 };\n\
                 println(f\"{ps[0].word} {ps[0].n}\");\n\
             }\n"),
        "a1 7\na1! 1\na1!b1 b1\na1!b1 9\n"
    );
}

/// B-2026-08-01-30 leg B — interpreter twin of `tests/codegen.rs`'s
/// `e2e_computed_index_assign_displaced_elem_bodies`, same source and
/// expected string. The typechecker desugars `base - 1` into
/// `i64.sub(base, 1)`, so the purity gate accepts the primitive arithmetic
/// intrinsics over pure operands (pre-fix: silent).
#[test]
fn test_computed_index_assign_displaced_elem_bodies() {
    assert_eq!(
        run("struct Res { id: i64, name: String }\n\
             impl Drop for Res {\n\
                 fn drop(mut ref self) {\n\
                     println(f\"drop {self.id} {self.name}\")\n\
                 }\n\
             }\n\
             fn main() {\n\
                 println(\"a\");\n\
                 let mut v: Vec[Res] = Vec.new();\n\
                 v.push(Res { id: 9, name: f\"z{9}\" });\n\
                 v.push(Res { id: 8, name: f\"w{8}\" });\n\
                 let base = 1;\n\
                 v[base - 1] = Res { id: 5, name: f\"y{5}\" };\n\
                 println(f\"held {v[0].id}\");\n\
                 println(\"end\");\n\
             }\n"),
        "a\ndrop 9 z9\nheld 5\ndrop 5 y5\ndrop 8 w8\nend\n"
    );
}

/// B-2026-08-02-15 leg 2 — the interpreter evaluated an indexed receiver's
/// SUBSCRIPT TWICE for a projection field store. `set_field`'s value-type arm
/// is a read-modify-write-back: it evaluates the receiver to read the struct
/// copy, then hands the SAME expression to `assign_to_place`, which evaluates
/// it again to store. `v[f()].field = x` therefore called `f` twice, where
/// codegen calls it once (leg 1 of the same entry). A non-pure subscript is now
/// materialized into a literal before the read, so both halves address the same
/// element and the call runs exactly once.
///
/// Twin of `tests/codegen.rs`'s
/// `e2e_indexed_field_store_non_pure_index_runs_once` — identical source and
/// expected string, because the whole point is that the two backends agree.
#[test]
fn test_indexed_field_store_non_pure_index_runs_once() {
    assert_eq!(
        run("struct P { mut id: i64, mut name: String }\n\
             struct O { mut hs: Vec[P] }\n\
             fn idx() -> i64 {\n\
                 println(\"eval\");\n\
                 return 0;\n\
             }\n\
             fn main() {\n\
                 let mut v: Vec[P] = Vec.new();\n\
                 v.push(P { id: 1, name: \"a\" });\n\
                 v[idx()].id = 9;\n\
                 println(f\"v={v[0].id}\");\n\
                 let mut o = O { hs: Vec.new() };\n\
                 o.hs.push(P { id: 1, name: \"a\" });\n\
                 o.hs[idx()].name = f\"b\";\n\
                 println(f\"o={o.hs[0].name}\");\n\
             }\n"),
        // One `eval` per store — never two.
        "eval\nv=9\neval\no=b\n"
    );
}

/// B-2026-08-02-5 — interpreter twin of `tests/codegen.rs`'s
/// `e2e_tuple_index_assignment_targets`, same source and expected string.
/// Pre-fix the direct TupleIndex targets hit the Assign arm's
/// `unreachable!` — an ICE with a Rust backtrace on an accepted program —
/// and the chained forms silently no-op'd; `assign_to_place`'s TupleIndex
/// arm now mutates the element and writes the tuple back up the place
/// chain.
#[test]
fn test_tuple_index_assignment_targets() {
    assert_eq!(
        run("struct Pu { id: i64, name: String }\n\
             struct Ow { t: (Pu, i64) }\n\
             fn main() {\n\
                 let mut t = (1, f\"a{2}\");\n\
                 t.0 = 5;\n\
                 println(f\"t0 {t.0}\");\n\
                 let mut u = (Pu { id: 9, name: f\"z{9}\" }, 3);\n\
                 u.0.id = 6;\n\
                 println(f\"u {u.0.id} {u.0.name}\");\n\
                 let mut o = Ow { t: (Pu { id: 9, name: f\"z{9}\" }, 3) };\n\
                 o.t.0 = Pu { id: 7, name: f\"y{5}\" };\n\
                 o.t.0.id = 8;\n\
                 println(f\"o {o.t.0.id} {o.t.0.name}\");\n\
                 println(\"end\");\n\
             }\n"),
        "t0 5\nu 6 z9\no 8 y5\nend\n"
    );
}

/// B-2026-08-02-8 — interpreter twin of `tests/codegen.rs`'s
/// `e2e_tuple_index_compound_and_vec_elem_targets`, same source and
/// expected string (the interp applied both writes already — this pins
/// the parity codegen now shares).
#[test]
fn test_tuple_index_compound_and_vec_elem_targets() {
    assert_eq!(
        run("fn main() {\n\
                 let mut t = (10, f\"a{2}\");\n\
                 t.0 += 5;\n\
                 println(f\"t0 {t.0}\");\n\
                 let mut v: Vec[(i64, String)] = Vec.new();\n\
                 v.push((9, f\"z{9}\"));\n\
                 v[0].0 = 5;\n\
                 println(f\"held {v[0].0} {v[0].1}\");\n\
                 println(\"end\");\n\
             }\n"),
        "t0 15\nheld 5 z9\nend\n"
    );
}

/// B-2026-08-05-40 — the ORACLE half. The interpreter has always accepted a
/// PLACE argument where a `Slice[T]` parameter is expected; codegen had no
/// place arm in `coerce_to_slice`, so `f(g.a)` / `f(t.0)` / `f(vv[0])` /
/// `f(o.q.a)` failed LLVM module verification and the program did not build.
///
/// Passes before and after the fix, which is its job: it is the reference
/// answer the codegen twin `e2e_slice_param_from_a_place_argument` is measured
/// against. Seed is the literal 1 here rather than `env.args().len()` for the
/// usual reason — an in-process interpreter test would see the TEST binary's
/// argv.
#[test]
fn test_slice_param_from_a_place_argument() {
    assert_eq!(
        run(r#"struct P { a: Vec[i64], tag: String }
struct Inner { a: Vec[i64] }
struct Outer { q: Inner }

fn mkv(k: i64) -> Vec[i64] {
    let mut v: Vec[i64] = Vec.new();
    v.push(k);
    v.push(k + 1i64);
    v.push(k + 2i64);
    return v;
}

fn total(s: Slice[i64]) -> i64 {
    let mut t: i64 = 0;
    let mut i: i64 = 0;
    while i < s.len() { t = t + s[i]; i = i + 1i64; }
    return t;
}

fn rtotal(s: ref Slice[i64]) -> i64 {
    let mut t: i64 = 0;
    let mut i: i64 = 0;
    while i < s.len() { t = t + s[i]; i = i + 1i64; }
    return t;
}

fn bumpall(s: mut Slice[i64]) {
    let mut i: i64 = 0;
    while i < s.len() { s[i] = s[i] + 1i64; i = i + 1i64; }
}

fn main() {
    let n: i64 = 1i64;
    let mut acc: i64 = 0;
    let mut i: i64 = 0;
    while i < n + 199i64 {
        let mut g: P = P { a: mkv(i), tag: f"tag-{i}-payload" };
        bumpall(mut g.a);
        acc = acc + total(g.a);
        if g.tag.contains("payload") { acc = acc + 1i64; }
        let mut g2: P = P { a: mkv(i), tag: f"tag2-{i}-payload" };
        bumpall(mut g2.a);
        acc = acc + rtotal(g2.a);
        let mut t: (Vec[i64], i64) = (mkv(i), 0i64);
        bumpall(mut t.0);
        acc = acc + total(t.0);
        let mut vv: Vec[Vec[i64]] = Vec.new();
        vv.push(mkv(i));
        bumpall(mut vv[0i64]);
        acc = acc + total(vv[0i64]);
        let mut o: Outer = Outer { q: Inner { a: mkv(i) } };
        bumpall(mut o.q.a);
        acc = acc + total(o.q.a);
        i = i + 1i64;
    }
    println(acc);
}"#),
        "304700\n"
    );
}

/// B-2026-08-14-38 oracle twin — `<method>()[i]` under the tree-walk backend.
///
/// This is the side that always worked: indexing a Vec-returning method call
/// ran here while `karac build` rejected it outright ("Index operator applied
/// to non-array type"), which is what made the bug a run-vs-build divergence
/// rather than a wrong answer. These are the bytes codegen's new
/// materialize-a-temp path has to reproduce.
#[test]
fn test_index_of_method_returned_vec() {
    assert_eq!(
        run("struct Bag { items: Vec[i64] }\n\
             impl Bag {\n\
                 pub fn copy_items(ref self) -> Vec[i64] { return self.items.clone(); }\n\
             }\n\
             fn pick[T](v: Vec[T], i: i64) -> T { return v.clone()[i]; }\n\
             fn main() {\n\
                 let v: Vec[i64] = [1, 2, 3];\n\
                 println(f\"{v.clone()[1]}\");\n\
                 let nums: Vec[i64] = [10, 20, 30, 40];\n\
                 println(f\"{nums[1..3].to_vec()[0]}\");\n\
                 let b = Bag { items: [7, 8] };\n\
                 println(f\"{b.copy_items()[1]}\");\n\
                 let names: Vec[String] = [\"ann\", \"bo\", \"cy\"];\n\
                 println(names.clone()[2]);\n\
                 println(f\"{pick(v, 0)}\");\n\
                 println(pick(names, 1));\n\
             }\n"),
        "2\n20\n8\ncy\n1\nbo\n"
    );
}

#[test]
fn test_field_store_through_mut_slice_param_oracle() {
    // Oracle twin of `tests/codegen.rs`'s
    // `test_e2e_field_store_through_mut_slice_param_reaches_caller`
    // (B-2026-08-15-21). The interpreter has always applied this write; only
    // the compiled backends dropped it, so `karac check` was clean and a
    // Mend-loop oracle run under `--interp` passed while the shipped binary
    // was wrong. Pinning the values here keeps the parity asserted from the
    // side that was already correct.
    let out = run("\n\
         struct P { x: i64, y: i64 }\n\
         fn bump_slice(s: mut Slice[P]) { s[0].x = s[0].x + 1; println(f\"inside={s[0].x}\"); }\n\
         fn bump_at(s: mut Slice[P], i: i64) { s[i].y = 99; }\n\
         fn bump_vec(v: mut ref Vec[P]) { v[0].x = v[0].x + 100; }\n\
         fn set_whole(s: mut Slice[P]) { s[1] = P { x: 70, y: 80 }; }\n\
         fn bump_scalar(s: mut Slice[i64]) { s[0] = s[0] + 1; }\n\
         fn main() {\n\
             let mut ps: Vec[P] = Vec.new();\n\
             ps.push(P { x: 3, y: 7 });\n\
             ps.push(P { x: 5, y: 11 });\n\
             bump_slice(mut ps);\n\
             println(f\"{ps[0].x}\");\n\
             bump_at(mut ps, 1);\n\
             println(f\"{ps[1].y}\");\n\
             bump_vec(mut ps);\n\
             println(f\"{ps[0].x}\");\n\
             set_whole(mut ps);\n\
             println(f\"{ps[1].x} {ps[1].y}\");\n\
             let mut ns: Vec[i64] = Vec.new();\n\
             ns.push(41);\n\
             bump_scalar(mut ns);\n\
             println(f\"{ns[0]}\");\n\
         }\n");
    assert_eq!(out, "inside=4\n4\n99\n104\n70 80\n42\n");
}

#[test]
fn test_tuple_elem_store_through_mut_slice_param_oracle() {
    // Oracle twin of `tests/codegen.rs`'s
    // `test_e2e_tuple_elem_store_through_mut_slice_param_reaches_caller`
    // (B-2026-08-15-24). The interpreter has always applied these writes;
    // codegen refused to lower them at all (a hard build error, not a wrong
    // answer), so this pins the values the build side now has to match.
    let out = run("\n\
         fn bump_first(s: mut Slice[(i64, i64)]) { s[0].0 = s[0].0 + 1; }\n\
         fn bump_second(s: mut Slice[(i64, i64)]) { s[0].1 = 99; }\n\
         fn bump_at(s: mut Slice[(i64, i64)], i: i64) { s[i].0 = 70; }\n\
         fn bump_all(s: mut Slice[(i64, i64, i64)]) {\n\
             let mut i = 0;\n\
             while i < s.len() { s[i].2 = i * 10; i = i + 1; }\n\
         }\n\
         fn swap_text(s: mut Slice[(String, i64)]) { s[0].0 = \"replaced\"; }\n\
         fn bump_vec(v: mut ref Vec[(i64, i64)]) { v[0].0 = v[0].0 + 100; }\n\
         fn main() {\n\
             let mut ps: Vec[(i64, i64)] = Vec.new();\n\
             ps.push((3, 7));\n\
             ps.push((4, 8));\n\
             bump_first(mut ps);\n\
             println(f\"{ps[0].0}\");\n\
             bump_second(mut ps);\n\
             println(f\"{ps[0].1}\");\n\
             bump_at(mut ps, 1);\n\
             println(f\"{ps[1].0} {ps[0].0}\");\n\
             bump_vec(mut ps);\n\
             println(f\"{ps[0].0}\");\n\
             let mut ts: Vec[(i64, i64, i64)] = Vec.new();\n\
             ts.push((0, 0, 0));\n\
             ts.push((0, 0, 0));\n\
             ts.push((0, 0, 0));\n\
             bump_all(mut ts);\n\
             println(f\"{ts[0].2} {ts[1].2} {ts[2].2}\");\n\
             let mut ss: Vec[(String, i64)] = Vec.new();\n\
             ss.push((\"original\", 5));\n\
             swap_text(mut ss);\n\
             println(f\"{ss[0].0} {ss[0].1}\");\n\
         }\n");
    assert_eq!(out, "4\n99\n70 4\n104\n0 10 20\nreplaced 5\n");
}

// ── nested subscript whose INNER index faults ───────────────────

/// B-2026-08-18-37 — `m[missing_key][0]` panicked the interpreter with the same
/// internal `unreachable!()` B-2026-08-18-3 reached: "index expression at
/// 4:13: obj=Value::Unit, index=Value::Unit". A faulting subscript calls
/// `record_runtime_error`, which returns `Value::Unit` and sets `pending_cf`;
/// the OUTER subscript then read that `Unit` as a real operand and fell
/// through every container arm to the catch-all.
///
/// So the user got an internal-error backtrace stacked on top of their own
/// correct "key not found in map" diagnostic, on a program `karac check` had
/// just passed — and both compiled backends faulted cleanly on the identical
/// source, making it a run-vs-build divergence as well.
///
/// Not map-specific: any nested subscript works, because the fault is in how
/// the outer index treats a failed inner one. The out-of-bounds Vec case below
/// ICE'd identically before the fix.
#[test]
fn test_nested_index_with_a_faulting_inner_index_reports_the_real_error() {
    // The row's repro: missing key, map through a struct FIELD.
    let errors = runtime_errors(
        "struct Holder { m: Map[i64, Vec[i64]] }\n\
         fn main() {\n\
             let mut h = Holder { m: Map.new() };\n\
             h.m.insert(1, [43, 7]);\n\
             println(h.m[99][0]);\n\
         }\n",
    );
    assert!(
        errors
            .iter()
            .any(|e| e.message.contains("key not found in map: 99")),
        "expected the real map-miss error, got: {errors:?}"
    );

    // Same miss through a LOCAL — the path the fix does not touch at all,
    // pinned so the two spellings cannot drift apart.
    let errors = runtime_errors(
        "fn main() {\n\
             let mut m: Map[i64, Vec[i64]] = Map.new();\n\
             m.insert(1, [43, 7]);\n\
             println(m[99][0]);\n\
         }\n",
    );
    assert!(
        errors
            .iter()
            .any(|e| e.message.contains("key not found in map: 99")),
        "expected the real map-miss error, got: {errors:?}"
    );

    // Not map-specific: a nested Vec whose OUTER index is out of bounds.
    let errors =
        runtime_errors("fn main() { let v: Vec[Vec[i64]] = [[1, 2], [3, 4]]; println(v[9][0]); }");
    assert!(
        errors
            .iter()
            .any(|e| e.message.contains("index 9 out of bounds")),
        "expected the real bounds error, got: {errors:?}"
    );

    // The error is reported ONCE, not compounded by the outer subscript
    // re-reporting against the `Unit` placeholder.
    let errors = runtime_errors(
        "fn main() {\n\
             let mut m: Map[i64, Vec[i64]] = Map.new();\n\
             m.insert(1, [43, 7]);\n\
             println(m[99][0]);\n\
         }\n",
    );
    assert_eq!(
        errors.len(),
        1,
        "expected exactly one error, got: {errors:?}"
    );

    // Anti-vacuity: the same shapes with a PRESENT key / in-range index still
    // evaluate, so the short-circuit cannot be passing by refusing everything.
    assert_eq!(
        run("struct Holder { m: Map[i64, Vec[i64]] }\n\
             fn main() {\n\
                 let mut h = Holder { m: Map.new() };\n\
                 h.m.insert(1, [43, 7]);\n\
                 println(h.m[1][0]);\n\
                 println(h.m[1][1]);\n\
             }\n"),
        "43\n7\n"
    );
    assert_eq!(
        run("fn main() { let v: Vec[Vec[i64]] = [[1, 2], [3, 4]]; println(v[1][0]); }"),
        "3\n"
    );
}

#[test]
fn to_ne_bytes_feeds_a_slice_parameter() {
    // The shape design.md's `Hasher` default bodies are written in.
    let out = run("fn total(b: ref Slice[u8]) -> i64 {\n\
        let mut s = 0i64;\n\
        let mut i = 0i64;\n\
        while i < b.len() { s = s + b[i] as i64; i = i + 1i64; }\n\
        s\n\
    }\n\
    fn main() {\n\
        let w: u32 = 16909060u32;\n\
        println(total(w.to_ne_bytes()));\n\
    }");
    assert_eq!(out, "10\n");
}

/// B-2026-09-16-2 — an index-assign runs the DISPLACED element's `Drop` body
/// for a tuple, nested-array or nested-`Vec` element, on the tree-walk backend.
///
/// The interpreter twin of the codegen E2E
/// (`e2e_index_store_runs_the_displaced_aggregate_elements_drop_body`), which
/// lives behind `--features llvm` and so is invisible to the DEFAULT leg. Both
/// backends declined here, so the row moved both in one commit; this keeps the
/// interpreter half under the gate CI actually runs.
///
/// This side is two sites, both index-assign displacement blocks — the
/// field-rooted (`h.xs[0] = ..`) and identifier-rooted (`a[0] = ..`) paths,
/// which are structurally identical and each had the same `_ => {}` hole for an
/// `Array`/`Tuple` old value. `value_runs_user_drop` could not be the route: it
/// classifies a bare Tuple/Array as false at top level BY DESIGN, to keep the
/// container walkers the sole firers for DIRECT BINDINGS. A displacement is not
/// one — the slot is overwritten, so no scope-exit walk ever visits the old
/// value, and design.md line 866 puts the body at the live-range end.
///
/// The relocation guard stays in force: `expr_mentions_name_deep(value, vname)`
/// makes the swap idiom skip the whole block, and `a[i] = a[j]` on a non-`Copy`
/// element does not typecheck anyway. `v.swap(0, 1)` is a cell below at exactly
/// two bodies for two values (B-2026-08-26-21's five-for-two is the regression
/// shape).
#[test]
fn test_index_store_runs_the_displaced_aggregate_elements_drop_body() {
    const H: &str = "struct D { s: String, id: i64 }\n\
         impl Drop for D { fn drop(mut ref self) { println(f\"dD{self.id}\") } }\n\
         fn mkd(n: i64) -> D { return D { s: f\"heap-{n}\", id: n }; }\n\
         struct Hh { xs: Array[(D, i64), 2] }\n";
    for (label, body, want) in [
        (
            "a TUPLE element",
            "let mut a: Array[(D, i64), 2] = [(mkd(1), 10), (mkd(2), 20)];\n\
             a[0] = (mkd(3), 30);",
            "dD1\ndD3\ndD2\nmid\n",
        ),
        (
            "a NESTED ARRAY element",
            "let mut a: Array[Array[D, 1], 2] = [[mkd(1)], [mkd(2)]];\n\
             a[0] = [mkd(3)];",
            "dD1\ndD3\ndD2\nmid\n",
        ),
        (
            "a nested Vec element",
            "let mut v: Vec[Vec[D]] = [[mkd(1)], [mkd(2)]];\n\
             v[0] = [mkd(3)];",
            "dD1\ndD3\ndD2\nmid\n",
        ),
        (
            "the Vec container leg with a tuple element",
            "let mut v: Vec[(D, i64)] = [(mkd(1), 10), (mkd(2), 20)];\n\
             v[0] = (mkd(3), 30);",
            "dD1\ndD3\ndD2\nmid\n",
        ),
        (
            "the FIELD-rooted spelling",
            "let mut h: Hh = Hh { xs: [(mkd(1), 10), (mkd(2), 20)] };\n\
             h.xs[0] = (mkd(3), 30);",
            "dD1\ndD3\ndD2\nmid\n",
        ),
        (
            "control: the flat named-struct element",
            "let mut a: Array[D, 2] = [mkd(1), mkd(2)];\n\
             a[0] = mkd(3);",
            "dD1\ndD3\ndD2\nmid\n",
        ),
        (
            "control: relocation via v.swap — two bodies for two values",
            "let mut v: Vec[D] = [mkd(1), mkd(2)];\n\
             v.swap(0, 1);",
            "dD2\ndD1\nmid\n",
        ),
        (
            "control: a non-Drop tuple element runs nothing",
            "let mut a: Array[(i64, i64), 2] = [(1, 10), (2, 20)];\n\
             a[0] = (3, 30);\n\
             println(f\"v:{a[0].0}\");",
            "v:3\nmid\n",
        ),
    ] {
        assert_eq!(
            run(&format!("{H}fn main() {{\n{body}\nprintln(\"mid\");\n}}\n")),
            want,
            "{label}"
        );
    }
}
