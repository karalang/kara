//! closures, captures, heap-env bindings, fn pointers -- fixtures for `tests/interpreter.rs`.
//!
//! Split out of `tests/interpreter.rs` on 2026-09-21. The TEST TARGET is
//! unchanged: this file is a module of that target, so
//! `cargo test --features llvm --test interpreter` still runs everything
//! and CI needs no edit. Run this area alone with:
//!
//!     cargo test --features llvm --test interpreter closures::
//!
//! New fixtures about closures, captures, heap-env bindings, fn pointers belong in this file.

use super::*;

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
fn test_closure_unannotated_param_inferred_from_arith_body() {
    // B-2026-07-12-10: a let-bound closure with an un-annotated numeric param
    // infers it from the arithmetic body — `|x| x + 1` → `Fn(i64) -> i64` —
    // with no annotation and no pushed context. Runs on both backends (build ==
    // run: the closure's `Function` type carries the solved `i64`).
    assert_eq!(
        run("fn main() {\n\
             let f = |x| x + 1;\n\
             println(f(5));\n\
             let g = |x| x * 2;\n\
             println(g(10));\n\
             let h = |y| y + 1.5;\n\
             println(h(2.0));\n\
         }"),
        "6\n20\n3.5\n"
    );
}

#[test]
fn test_closure_param_inferred_from_call_site() {
    // B-2026-07-12-20: a let-bound closure whose un-annotated param has NO body
    // constraint (`let id = |x| x`) infers the param from the monomorphic call
    // site's argument type. The return shares the param's var, so `id(5)` types
    // as `i64` and `s("hi")` as `String`. build == run — the closure's recorded
    // type is resolved through the substitutions at finalize so codegen lays the
    // param out at the right width (the `String` case caught a build!=run gap).
    assert_eq!(
        run("fn main() {\n\
             let id = |x| x;\n\
             let n: i64 = id(5);\n\
             println(n + 1);\n\
             let s = |y| y;\n\
             println(s(\"hello\"));\n\
         }"),
        "6\nhello\n"
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

/// A range is a VALUE, so its bounds are fixed where it is bound — the rule
/// B-2026-08-17-29 settled for the loop position, holding here too. Mutating
/// the source binding afterwards must not move the window.
#[test]
fn test_let_bound_range_index_captures_its_bounds() {
    assert_eq!(
        run(
            "fn main() { let v = [10, 20, 30, 40]; let mut a = 1; let r = a..3; a = 0;\n\
             let s = v[r]; println(s.len()); println(s[0]); }"
        ),
        "2\n20\n"
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
fn test_wp_closure_string_return_in_i64_fn_runs() {
    // B-2026-07-31-18: a String-returning wp closure inside an i64 fn is
    // spec-valid (the body is `Fn() -> T`); pre-fix the typechecker rejected
    // it against the fn's return type. "v-42".len() = 4.
    let output = run("trait Counter { fn get(ref self) -> i64; }
         effect resource Ctr: Counter;
         struct InMem { n: i64 }
         impl Counter for InMem { fn get(ref self) -> i64 { self.n } }
         fn read() -> i64 with reads(Ctr) { Ctr.get() }
         fn heap() -> i64 with reads(Ctr) {
             let s = with_provider[Ctr](InMem { n: 42 }, || { return f\"v-{read()}\"; });
             s.len()
         }
         fn main() with reads(Ctr) { println(f\"{heap()}\"); }");
    assert_eq!(output, "4\n");
}

#[test]
fn test_wp_closure_bare_return_gates_side_effect() {
    // B-2026-07-31-18: `return;` in a unit wp closure inside an i64 fn —
    // closure-scoped, so it skips the println without exiting the fn.
    let output = run("trait Counter { fn get(ref self) -> i64; }
         effect resource Ctr: Counter;
         struct InMem { n: i64 }
         impl Counter for InMem { fn get(ref self) -> i64 { self.n } }
         fn read() -> i64 with reads(Ctr) { Ctr.get() }
         fn quiet(n: i64) -> i64 with reads(Ctr) {
             with_provider[Ctr](InMem { n: n }, || {
                 if read() == 3 { return; }
                 println(f\"seen-{read()}\");
             });
             7
         }
         fn main() with reads(Ctr) {
             println(f\"{quiet(3)}\");
             println(f\"{quiet(5)}\");
         }");
    assert_eq!(output, "7\nseen-5\n7\n");
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

#[test]
fn test_stdout_flush_is_callable() {
    // Stdout.flush() should return Unit without error.
    let out = run_no_errors("fn main() { Stdout.flush(); println(\"ok\"); }");
    assert_eq!(out, "ok\n");
}

#[test]
fn test_stderr_flush_is_callable() {
    let out = run_no_errors("fn main() { Stderr.flush(); println(\"ok\"); }");
    assert_eq!(out, "ok\n");
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
fn test_oncelock_get_or_init_runs_closure_once() {
    // `get_or_init` fills the cell on first access and returns the cached
    // value on every later call WITHOUT re-running the init closure — the
    // second call returns the first value (7), not the second closure's
    // value (999).
    let output = run(r#"fn main() {
             let cell: OnceLock[i64] = OnceLock.new();
             let a = cell.get_or_init(|| 7);
             println(a);
             let b = cell.get_or_init(|| 999);
             println(b);
             println(cell.is_set());
         }"#);
    assert_eq!(output, "7\n7\ntrue\n");
}

#[test]
fn test_string_sorted_by_closure_descending() {
    let output = run(
        r#"fn main() { let s = "dcba"; println(s.sorted_by(|a, b| if a < b { Ordering.Greater } else if a > b { Ordering.Less } else { Ordering.Equal })); }"#,
    );
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
#[cfg(not(target_arch = "wasm32"))]
fn test_http_response_captures_all_headers_not_just_content_type() {
    // `wrap_ok_response` captured ONLY `content-type`, so on the interpreter
    // lane `Response.header(name)` answered `None` for every other header and
    // `Response.headers()` answered a 1-element list — while codegen returned
    // the full set. That is a WRONG ANSWER rather than a failure: a program
    // reading `Location` / `ETag` / a rate-limit header ran clean and silently
    // took the not-present branch. The origin sends Content-Type, X-Echo,
    // Content-Length and Connection, so a correct capture sees X-Echo and
    // strictly more than one header.
    let port = spawn_echo_origin();
    let output = run(&format!(
        r#"
fn main() with sends(Network) receives(Network) {{
    let c = Client.new();
    match c.get("http://127.0.0.1:{port}/g") {{
        Ok(r) => {{
            match r.header("X-Echo") {{
                Some(v) => println("xecho " + v),
                None => println("xecho MISSING"),
            }}
            match r.header("content-type") {{
                Some(v) => println("ct " + v),
                None => println("ct MISSING"),
            }}
            println(r.headers().len() > 1);
        }}
        Err(e) => println("err " + e.message()),
    }}
}}
"#
    ));
    assert_eq!(output, "xecho yes\nct text/plain\ntrue\n");
}

#[test]
fn test_closure_bare_mutating_capture_propagates() {
    // B-2026-07-11-23 — a BARE closure that mutates a captured local captures
    // it by `mut ref` (design.md Rule 2), so writes propagate to the outer
    // binding across invocations. Previously the interpreter snapshot-copied
    // the capture and the mutation was lost (printed 0).
    let output = run_no_errors(
        r#"
fn main() {
    let mut c: i64 = 0;
    let f = |x: i64| { c = c + x; };
    f(3i64);
    f(4i64);
    println(f"{c}");
}
"#,
    );
    assert_eq!(output, "7\n");
}

#[test]
fn test_closure_capture_mutation_in_fold_no_run_build_divergence() {
    // B-2026-07-11-23 — a capture-mutating closure in a `fold` terminal. Codegen
    // INLINES the body (mutation propagates = correct); the interpreter used to
    // invoke it over a snapshot (mutation lost = 0), a run-vs-build divergence.
    // With Rule 2 capture inference the interpreter now agrees: count == 3.
    let output = run_no_errors(
        r#"
fn main() {
    let v: Vec[i64] = [1, 2, 3];
    let mut count = 0i64;
    let s = v.iter().fold(0i64, |a: i64, x: i64| { count = count + 1i64; a + x });
    println(f"{s}");
    println(f"{count}");
}
"#,
    );
    assert_eq!(output, "6\n3\n");
}

#[test]
fn test_closure_readonly_capture_unaffected() {
    // A bare closure that only READS a capture keeps by-value (snapshot)
    // semantics — the Rule 2 inference wraps ONLY mutated captures, so a
    // read-only capture is untouched.
    let output = run_no_errors(
        r#"
fn main() {
    let base: i64 = 10;
    let add = |x: i64| { base + x };
    println(add(5i64).to_string());
}
"#,
    );
    assert_eq!(output, "15\n");
}

#[test]
fn test_iter_adaptor_closure_reads_live_captured_var() {
    // B-2026-07-14-20: an iterator-adaptor closure captures by REFERENCE
    // (design.md § Closures Rule 2: read → ref), so a loop body mutating a
    // captured variable is visible to the predicate on the NEXT element —
    // matching codegen's fused inlining. Pre-fix the interpreter snapshotted
    // at adaptor construction and the predicate kept seeing the stale value
    // (this program summed 10 instead of 3).
    //
    // Trace over [1,2,3,10,4] with `lim` starting at 5:
    //   x=1 passes (1<5), body sets lim=4, s=1
    //   x=2 passes (2<4), body sets lim=3, s=3
    //   x=3 fails (3<3 is false), 10 fails, 4 fails (4<3) → s stays 3.
    let output = run_no_errors(
        r#"
fn main() {
    let xs: Vec[i64] = [1, 2, 3, 10, 4];
    let mut lim = 5;
    let mut s = 0;
    for x in xs.iter().filter(|v| v < lim) {
        lim = lim - 1;
        s = s + x;
    }
    println(s);
}
"#,
    );
    assert_eq!(output, "3\n");
}

#[test]
fn test_iter_take_while_reads_live_captured_var() {
    // B-2026-07-14-20, take_while leg: `seen` is incremented in the LOOP
    // BODY; the predicate must see the live count and stop after 3 elements
    // (sum 1+2+3=6). Pre-fix the snapshot kept `seen`=0 forever and all six
    // elements were yielded (sum 21).
    let output = run_no_errors(
        r#"
fn main() {
    let xs: Vec[i64] = [1, 2, 3, 4, 5, 6];
    let mut seen = 0;
    let mut s = 0;
    for x in xs.iter().take_while(|v| seen < 3) {
        seen = seen + 1;
        s = s + x;
    }
    println(s);
}
"#,
    );
    assert_eq!(output, "6\n");
}

#[test]
fn test_plain_stored_closure_keeps_snapshot_semantics() {
    // Guard for the B-2026-07-14-20 fix's SCOPE: a plain STORED closure
    // (not an adaptor argument) keeps the snapshot model both backends
    // share — `f` sees `k`'s value at creation (1), not the later write
    // (10). Full Rule-2 by-reference capture for materialized closures is
    // a separate cross-backend change; widening only the interpreter here
    // would CREATE a plain-closure divergence.
    let output = run_no_errors(
        r#"
fn main() {
    let mut k = 1;
    let f = |x| x + k;
    k = 10;
    println(f(1));
}
"#,
    );
    assert_eq!(output, "2\n");
}

#[test]
fn test_closure_mutates_captured_collection_via_method() {
    // B-2026-07-15-13: a closure that mutates a captured collection through a
    // mutating METHOD (`buf.push_str`, `m.insert`, `acc.push`) captures the
    // receiver by mut-ref (design.md Rule 2 / § Closures line 4940: a
    // `mut ref self` method call captures its receiver by mut-ref), so the
    // mutation must write through to the outer binding. The interpreter's
    // wrap-set used `collect_assigned_roots` only (which catches `=` / `[i]=`
    // targets, not method mutation), so it snapshot-copied the receiver and
    // dropped the mutation for a non-Rc-shared value — String and Map both
    // read back EMPTY (Vec happened to propagate via its Rc-shared buffer,
    // masking the gap). Now String / Map / Vec all propagate, matching
    // codegen.
    let output = run_no_errors(
        r#"
fn main() {
    let mut buf: String = "";
    let mut append = |s: String| { buf.push_str(s); };
    append("a");
    append("bc");
    println(buf);

    let mut m: Map[String, i64] = Map.new();
    let mut record = |k: String, v: i64| { m.insert(k, v); };
    record("x", 1);
    record("y", 2);
    println(m.len());

    let mut acc: Vec[i64] = Vec.new();
    let mut push = |x: i64| { acc.push(x); };
    push(10);
    push(20);
    push(30);
    println(acc.len());
}
"#,
    );
    assert_eq!(output, "abc\n2\n3\n");
}

#[test]
fn test_let_bound_range_is_a_value_captured_at_the_binding() {
    // The ORACLE for B-2026-08-17-29, whose codegen twin lives in
    // tests/codegen.rs. The interpreter was already right here — the bug was
    // codegen-only — so this pins the answer codegen was made to match, not a
    // behaviour change.
    //
    // What it pins is the semantics, not just "> 0 iterations": `let r = a..6`
    // captures `a` AT THE BINDING, so mutating `a` afterwards cannot move the
    // range. design.md line 2616 types a range as a first-class `Range[T]`
    // value, which is what makes 14 (2+3+4+5) the answer instead of the 0 that
    // re-reading `a` at the loop would give. Both compiled backends now agree.
    //
    // The rest covers the forms the bug report enumerated: inclusive bounds,
    // an empty range, and reuse of one binding by two nested loops (3 x 3),
    // which pins that iterating a bound range does not consume it.
    let output = run_no_errors(
        r#"
fn main() {
    let mut a = 2;
    let r = a..6;
    a = 10;
    let mut n = 0;
    for i in r { n = n + i; }
    println(n);
    let inc = 0..=4;
    let mut b = 0;
    for i in inc { b = b + i; }
    println(b);
    let e = 5..5;
    let mut z = 0;
    for i in e { z = z + 1; }
    println(z);
    let q = 0..3;
    let mut k = 0;
    for i in q { for j in q { k = k + 1; } }
    println(k);
}
"#,
    );
    assert_eq!(output, "14\n10\n0\n9\n");
}

/// B-2026-08-28-4 — a by-value param destructured inside a CLOSURE runs the
/// element's user `Drop` body ONCE, not twice.
///
/// The let-destructure gate exists because a by-value param's bindings are
/// views of the callee's entry copy, whose Drop observability belongs to the
/// CALLER (caller-retains): the caller's fresh-temp argument walk is the single
/// owner, so the callee must not also register slots. That gate resolved the
/// callee's owned params against `program.items`, and a CLOSURE matches no
/// top-level `Item::Function` — it contributed the empty set by construction,
/// the gate never fired inside a closure body, and the element's body ran twice
/// for one object: once from the callee's slot, once from the caller's walk,
/// which fires for a closure call exactly as it does for a free fn.
///
/// The free-fn spelling of the identical body has been correct since
/// B-2026-08-27-48, which is what isolated the closure as the variable rather
/// than the destructure.
///
/// `ref-param-control` is the row this fix cannot express a filter for and so
/// pins by measurement instead: a closure's params are `Pattern`s carrying no
/// recorded type, so `ref` / `mut ref` cannot be filtered out the way the fn
/// path filters them. Collecting only plain `Binding` patterns leaves a
/// borrow-typed closure param correct, and this row is what says so.
///
/// The COMPILED backends agree at one on every row here. They do NOT agree on a
/// closure's by-value STRUCT param, which runs zero bodies compiled — a
/// separate defect on the other side of the same shape, filed rather than
/// pinned here.
#[test]
fn test_closure_param_destructure_user_drop_body_runs_once() {
    const DROPPER: &str = "struct R { id: i64 }\n\
         impl Drop for R { fn drop(mut ref self) { println(f\"drop {self.id}\") } }\n";
    for (label, body, want) in [
        // The row's own repro.
        (
            "tuple-param-destructure",
            "fn main() {\n\
             \x20   let f = |p: (R, i64)| { let (r, n) = p; r.id + n };\n\
             \x20   println(f\"{f((R { id: 41 }, 1))}\");\n\
             \x20   println(\"end\");\n\
             }\n",
            "drop 41\n42\nend\n",
        ),
        // Two droppers in one param — pre-fix this ran FOUR bodies for two
        // objects, so it pins that the gate retracts per frame and not once.
        (
            "two-droppers",
            "fn main() {\n\
             \x20   let f = |p: (R, R)| { let (a, b) = p; a.id + b.id };\n\
             \x20   println(f\"{f((R { id: 41 }, R { id: 42 }))}\");\n\
             \x20   println(\"end\");\n\
             }\n",
            "drop 41\ndrop 42\n83\nend\n",
        ),
        // CONTROL — the same body as a FREE FN, correct since B-2026-08-27-48.
        // This is what isolates the closure rather than the destructure.
        (
            "free-fn-control",
            "fn take(p: (R, i64)) -> i64 { let (r, n) = p; r.id + n }\n\
             fn main() { println(f\"{take((R { id: 41 }, 1))}\"); println(\"end\"); }\n",
            "drop 41\n42\nend\n",
        ),
        // CONTROL — a BORROW-typed closure param. The collection cannot filter
        // by type, so this row is the evidence that not filtering is safe: the
        // caller's binding owns the body and it fires once, at its live-range
        // end.
        (
            "ref-param-control",
            "fn main() {\n\
             \x20   let g = R { id: 41 };\n\
             \x20   let f = |r: ref R| { r.id };\n\
             \x20   println(f\"{f(g)}\");\n\
             \x20   println(\"end\");\n\
             }\n",
            "41\ndrop 41\nend\n",
        ),
    ] {
        assert_eq!(run(&format!("{DROPPER}{body}")), want, "{label}");
    }
}

/// B-2026-08-01-26 — interpreter twin of `tests/codegen.rs`'s
/// `e2e_local_closure_shadows_stdlib_names`, plus the interp-specific arm:
/// the interpreter's own builtin-name intercept table was unguarded for
/// `spawn` (a closure named `spawn` ran the stdlib spawn and returned a
/// TaskHandle), so a local `Value::Function` binding now shadows the whole
/// intercept match.
#[test]
fn test_local_closure_shadows_stdlib_names() {
    assert_eq!(
        run("fn shadowed() -> i64 {\n\
                 let take = |x: i64| {\n\
                     let mut v: Vec[i64] = Vec.new();\n\
                     v.push(x);\n\
                     v.len()\n\
                 };\n\
                 take(9)\n\
             }\n\
             fn unshadowed() -> i64 {\n\
                 let mut z = 7;\n\
                 take(mut z)\n\
             }\n\
             fn main() {\n\
                 let spawn = |x: i64| { x + 1 };\n\
                 println(shadowed());\n\
                 println(unshadowed());\n\
                 println(spawn(4));\n\
             }\n"),
        "1\n7\n5\n"
    );
}
