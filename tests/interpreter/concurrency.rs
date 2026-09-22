//! par blocks, spawn, tasks, channels, atomics, pools -- fixtures for `tests/interpreter.rs`.
//!
//! Split out of `tests/interpreter.rs` on 2026-09-21. The TEST TARGET is
//! unchanged: this file is a module of that target, so
//! `cargo test --features llvm --test interpreter` still runs everything
//! and CI needs no edit. Run this area alone with:
//!
//!     cargo test --features llvm --test interpreter concurrency::
//!
//! New fixtures about par blocks, spawn, tasks, channels, atomics, pools belong in this file.

use super::*;

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

#[test]
fn test_par_block_join_does_not_shadow_enclosing_bindings() {
    // B-2026-08-07-22 — the SILENT half, and the one worth guarding hardest.
    //
    // The join merges each branch's bindings into the enclosing scope. It used
    // to merge the whole seeded environment snapshot, so every enclosing
    // variable was re-`define`d into the parent's CURRENT scope. At function
    // top level that rewrites bindings in their own scope and nothing is
    // observable — which is exactly why every par test above passed. One block
    // deeper the current scope is the block's, so `n` gained a shadow there and
    // `n = n + 5` updated the shadow, which died with the block: this printed
    // `0` under `--interp` while the AOT twin printed `5`.
    //
    // Two statements in the par block is load-bearing: at one or fewer,
    // `eval_par_block` returns early and never reaches the join.
    let output = run("fn main() {
             let mut n: i64 = 0;
             if 1 < 2 {
                 par { let a = 1; let b = 2; }
                 n = n + 5;
             }
             println(n);
         }");
    assert_eq!(
        output.trim(),
        "5",
        "an assignment after a par join must reach the ENCLOSING binding, not a \
         shadow created in the current block scope"
    );
}

#[test]
fn test_par_block_inside_while_loop_terminates() {
    // B-2026-08-07-22 — the loud half of the same defect. When the shadowed
    // binding is a loop counter, `i = i + 1` never reaches the real `i`, the
    // condition stays true forever, and `karac run --interp` hangs on a program
    // whose AOT twin prints instantly.
    //
    // Run on a worker with a deadline: a regression here does not fail, it
    // spins, and an un-timed assertion would hang the whole suite rather than
    // report.
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .stack_size(32 * 1024 * 1024)
        .spawn(move || {
            let out = run("fn main() {
                     let mut total: i64 = 0;
                     let mut i: i64 = 0;
                     while i < 2 {
                         let (a, b) = par {
                             let a = i + 1;
                             let b = i + 2;
                             (a, b)
                         };
                         total = total + a + b;
                         i = i + 1;
                     }
                     println(total);
                 }");
            let _ = tx.send(out);
        })
        .expect("failed to spawn worker");

    match rx.recv_timeout(std::time::Duration::from_secs(60)) {
        Ok(out) => assert_eq!(
            out.trim(),
            "8",
            "par inside a while loop must produce the same value as the AOT build"
        ),
        Err(_) => panic!(
            "`par {{}}` inside a `while` loop did not terminate within 60s — the loop \
             counter's update is being lost to a shadow created by the par join"
        ),
    }
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

#[test]
fn test_atomic_fetch_add_load_explicit_agrees_with_codegen() {
    // The explicit-ordering form is the canonical shape (deferred.md § Atomic
    // Operations; book ch14). This mirrors `examples/mend/.../solution.kara`
    // and the codegen E2E `test_e2e_atomic_fetch_add_load_explicit` in
    // par_codegen.rs — both must print `3`, pinning run/build agreement on the
    // fetch_add + load pair the arity bug split (B-2026-06-30-5).
    let src = "par struct Counter { count: Atomic[i64] }\n\
         fn bump(c: ref Counter) { let _ = c.count.fetch_add(1, MemoryOrdering.Relaxed); }\n\
         fn main() {\n\
             let c = Counter { count: Atomic.new(0) };\n\
             par { bump(c); bump(c); bump(c); }\n\
             println(c.count.load(MemoryOrdering.Relaxed));\n\
         }";
    assert_eq!(run_no_errors(src), "3\n");
}

#[test]
fn test_atomic_implicit_ordering_rejected_by_interpreter() {
    // Interpreter backstop for the run/build divergence (B-2026-06-30-5):
    // codegen rejects the implicit-ordering form, so `karac run` must too.
    // The typechecker catches the owned/self/local shapes (run-fatal
    // `AtomicMissingOrdering`, see tests/typechecker.rs), but a field access
    // through a `ref`/`mut ref` struct param types as `Type::Error`
    // (fields.rs) and slips past typecheck — this receiver shape is exactly
    // the one the guard in the interpreter's atomic arms exists to catch, so
    // `karac run` still rejects rather than silently accepting the count.
    let src = "par struct Counter { count: Atomic[i64] }\n\
         fn bump(c: ref Counter) { let _ = c.count.fetch_add(1); }\n\
         fn main() {\n\
             let c = Counter { count: Atomic.new(0) };\n\
             bump(c);\n\
             println(c.count.load(MemoryOrdering.Relaxed));\n\
         }";
    let errors = runtime_errors(src);
    assert!(
        errors
            .iter()
            .any(|e| e.message.contains("Atomic.fetch_add")
                && e.message.contains("MemoryOrdering")),
        "interpreter must reject implicit-ordering fetch_add through a ref \
         param, got: {:?}",
        errors.iter().map(|e| &e.message).collect::<Vec<_>>()
    );
}

// ── TaskGroup / spawn — run/build agreement (B-2026-06-30-8) ──────
//
// `TaskGroup.new()` / `tg.spawn(closure)` / `handle.join()` and the free
// `spawn(closure)` were accepted by the typechecker and lowered by codegen
// (`karac build`), but the tree-walk interpreter (`karac run`) had no
// evaluation rule — `TaskGroup.new` hit the "not wired in the tree-walk
// interpreter" internal error and free `spawn` panicked as an unresolved
// identifier. Same run/build-divergence class as the Atomic-ordering split
// above (B-2026-06-30-5). The interpreter now runs each spawned child
// eagerly at its spawn site (join-at-spawn), which is observably identical
// to the parallel codegen for the order-independent fan-out/join shape the
// `ScopeLocal` rules permit — see `Value::TaskGroup` in value.rs and
// `eval_spawn_closure` in eval_call.rs for the model + its bounds.

#[test]
fn test_taskgroup_spawn_join_agrees_with_codegen() {
    // Canonical explicit-join fan-out: spawn two children, join each handle,
    // sum the results. Mirrors the codegen E2E
    // `test_e2e_taskgroup_spawn_join` in par_codegen.rs — both must print
    // `60` (worker(10)=20 + worker(20)=40), pinning `karac run` ↔ `karac
    // build` agreement on the TaskGroup surface.
    let src = "fn worker(n: i64) -> i64 { n * 2 }\n\
         fn main() {\n\
             let mut tg = TaskGroup.new();\n\
             let h1: TaskHandle[i64] = tg.spawn(|| worker(10));\n\
             let h2: TaskHandle[i64] = tg.spawn(|| worker(20));\n\
             let r1: i64 = h1.join();\n\
             let r2: i64 = h2.join();\n\
             println(r1 + r2);\n\
         }";
    assert_eq!(run_no_errors(src), "60\n");
}

#[test]
fn test_free_spawn_join_agrees_with_codegen() {
    // Free `spawn(closure)` — the unscoped sibling of `tg.spawn`. Mirrors
    // the codegen E2E `test_e2e_free_spawn_join` in par_codegen.rs; both
    // print `42`. Before B-2026-06-30-8 this panicked in the interpreter
    // ("variable 'spawn' not found") while `karac build` compiled it fine.
    let src = "fn add(a: i64, b: i64) -> i64 { a + b }\n\
         fn main() {\n\
             let h: TaskHandle[i64] = spawn(|| add(40, 2));\n\
             let r: i64 = h.join();\n\
             println(r);\n\
         }";
    assert_eq!(run_no_errors(src), "42\n");
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

#[test]
fn test_match_ok_unit_payload_selects_correct_arm() {
    // `match Result[(), E] { Ok(()) => .., Err(_) => .. }` — the `Ok(())`
    // unit-payload pattern matches `Ok` and the `Err` value routes to the
    // Err arm. Parity peer to the codegen E2E regression for the
    // exhaustiveness / unit-tuple-pattern fix.
    let output = run_no_errors(
        "fn classify(r: Result[(), String]) -> i64 {\n\
             match r { Ok(()) => 10, Err(e) => 20 }\n\
         }\n\
         fn main() {\n\
             println(classify(Err(\"x\")));\n\
             println(classify(Ok(())));\n\
         }",
    );
    assert_eq!(output, "20\n10\n");
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

// ── Semaphore — application-layer backpressure primitive ───────────

#[test]
fn test_semaphore_and_rate_limiter_codegen_parity() {
    // The exact program tests/codegen.rs::e2e_backpressure_semaphore_and_rate_limiter
    // compiles — pinned here so run==build stays honest for the whole
    // Semaphore + RateLimiter surface.
    let output = run(r#"fn main() {
         let sem = Semaphore.new(2i64);
         match sem.acquire(0i64) { Ok(u) => { println("a1"); }, Err(e) => { println("t1"); }, }
         match sem.acquire(0i64) { Ok(u) => { println("a2"); }, Err(e) => { println("t2"); }, }
         match sem.acquire(0i64) { Ok(u) => { println("a3"); }, Err(e) => { println("t3"); }, }
         sem.release();
         match sem.acquire(0i64) { Ok(u) => { println("a4"); }, Err(e) => { println("t4"); }, }
         let rl = RateLimiter.new_token_bucket(1i64, 3i64);
         println(rl.try_acquire("k1"));
         println(rl.try_acquire("k1"));
         println(rl.try_acquire("k1"));
         println(rl.try_acquire("k1"));
         println(rl.try_acquire("k2"));
     }"#);
    assert_eq!(output, "a1\na2\nt3\na4\ntrue\ntrue\ntrue\nfalse\ntrue\n");
}

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

// ── OnceLock[T] / OnceCell[T] write-once cells ─────────────────────

#[test]
fn test_oncelock_set_get_is_set_roundtrip() {
    // The basic write-once lifecycle: empty cell reports `is_set() ==
    // false` and `get() == None`; after a successful `set`, `is_set() ==
    // true` and `get() == Some(v)`.
    let output = run(r#"fn main() {
             let cell: OnceLock[i64] = OnceLock.new();
             println(cell.is_set());
             match cell.get() {
                 Some(v) => println(v),
                 None => println(-1),
             }
             match cell.set(42) {
                 Ok(_) => println(1),
                 Err(_) => println(0),
             }
             println(cell.is_set());
             match cell.get() {
                 Some(v) => println(v),
                 None => println(-1),
             }
         }"#);
    assert_eq!(output, "false\n-1\n1\ntrue\n42\n");
}

#[test]
fn test_oncelock_double_set_returns_already_set_error() {
    // `set` on a filled cell fails with `AlreadySetError` carrying the
    // *rejected* value, and leaves the stored value untouched.
    let output = run(r#"fn main() {
             let cell: OnceLock[i64] = OnceLock.new();
             let _ = cell.set(42);
             match cell.set(99) {
                 Ok(_) => println(-1),
                 Err(e) => println(e.rejected),
             }
             match cell.get() {
                 Some(v) => println(v),
                 None => println(-2),
             }
         }"#);
    // 99 = the rejected value handed back; 42 = the cell is unchanged.
    assert_eq!(output, "99\n42\n");
}

#[test]
fn test_oncelock_get_or_init_skipped_when_already_set() {
    // A cell filled by `set` is not re-initialized by a later
    // `get_or_init` — the init closure never runs and the `set` value
    // wins.
    let output = run(r#"fn main() {
             let cell: OnceLock[String] = OnceLock.new();
             let _ = cell.set("hello");
             let s = cell.get_or_init(|| "world");
             println(s);
         }"#);
    assert_eq!(output, "hello\n");
}

#[test]
fn test_oncecell_set_get_parallel_surface() {
    // `OnceCell[T]` carries the identical method surface to `OnceLock[T]`
    // (the difference is single-task vs. cross-task safety, enforced at
    // typecheck time — not a runtime difference).
    let output = run(r#"fn main() {
             let cell: OnceCell[i64] = OnceCell.new();
             println(cell.is_set());
             match cell.set(5) {
                 Ok(_) => println(1),
                 Err(_) => println(0),
             }
             println(cell.is_set());
             match cell.get() {
                 Some(v) => println(v),
                 None => println(-1),
             }
         }"#);
    assert_eq!(output, "false\n1\ntrue\n5\n");
}

#[test]
fn test_oncelock_string_payload_roundtrips() {
    // Heap-payload element type: a `String` round-trips through the cell
    // (the slot is just a `Value`, so T erases cleanly).
    let output = run(r#"fn main() {
             let cell: OnceLock[String] = OnceLock.new();
             let _ = cell.set("greetings-from-the-cell");
             match cell.get() {
                 Some(v) => println(v),
                 None => println("empty"),
             }
         }"#);
    assert_eq!(output, "greetings-from-the-cell\n");
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

#[test]
fn test_dataframe_select_subset_and_reorder() {
    // select picks a column subset in the given order (a reorder here);
    // the result is a fresh frame whose columns view the source buffers.
    let out = run_no_errors(
        "fn main() {\n\
             let mut df: DataFrame = DataFrame.new();\n\
             df.insert(\"a\", Column.from_vec([1i64, 2i64]));\n\
             df.insert(\"b\", Column.from_vec([3i64, 4i64]));\n\
             df.insert(\"c\", Column.from_vec([5i64, 6i64]));\n\
             let sub: DataFrame = df.select([\"c\", \"a\"]);\n\
             println(sub.width());\n\
             println(sub.height());\n\
             for n in sub.column_names() { println(n); }\n\
             let c0: Column[i64] = sub.column(\"c\");\n\
             match c0[1] { Some(v) => { println(v); } None => { println(-1i64); } }\n\
             println(df.width());\n\
         }",
    );
    // sub: 2 cols, 2 rows, names [c, a]; c[1] == 6; source df still 3 wide.
    assert_eq!(out, "2\n2\nc\na\n6\n3\n");
}

#[test]
fn test_dataframe_select_missing_column_traps() {
    let errors = runtime_errors(
        "fn main() {\n\
             let mut df: DataFrame = DataFrame.new();\n\
             df.insert(\"a\", Column.from_vec([1i64]));\n\
             let _ = df.select([\"a\", \"zzz\"]);\n\
         }",
    );
    assert!(
        errors
            .iter()
            .any(|e| e.message.contains("no column named 'zzz'")),
        "{errors:?}",
    );
}

#[test]
fn test_lazyframe_limit_clamps_and_no_select_keeps_all_columns() {
    // A negative limit clamps to 0; a limit past the height keeps every
    // row; with no select the collect keeps every source column (`*`).
    let out = run_no_errors(
        "fn main() {\n\
             let mut df: DataFrame = DataFrame.new();\n\
             df.insert(\"a\", Column.from_vec([1i64, 2i64]));\n\
             let zero = df.lazy().limit(-3).collect();\n\
             println(zero.height());\n\
             let all = df.lazy().limit(99).collect();\n\
             println(all.height()); println(all.width());\n\
             println(df.lazy().explain());\n\
         }",
    );
    assert_eq!(
        out,
        "0\n2\n1\n\
         == logical plan ==\n\
         SCAN [a]\n\
         == optimized ==\n\
         SCAN cols=[*]\n",
        "limit must clamp; empty plan must scan everything",
    );
}

#[test]
fn test_lazyframe_join_inner_collision_suffix_and_explain() {
    // Phase-11 LazyDataFrame slice 5: inner `join` — the plan becomes a
    // tree (the right side is a nested sub-plan, rendered compactly on
    // the JOIN line). Output schema = left columns, then right minus the
    // keys with `_right` suffixed onto collisions. Unmatched rows on
    // either side drop; left row order is preserved.
    let out = run_no_errors(
        "fn main() {\n\
             let mut people: DataFrame = DataFrame.new();\n\
             people.insert(\"id\", Column.from_vec([1i64, 2i64, 3i64, 4i64]));\n\
             people.insert(\"name\", Column.from_vec([\"ada\", \"bob\", \"cyd\", \"dee\"]));\n\
             people.insert(\"city\", Column.from_vec([\"rome\", \"oslo\", \"rome\", \"lima\"]));\n\
             let mut cities: DataFrame = DataFrame.new();\n\
             cities.insert(\"city\", Column.from_vec([\"rome\", \"oslo\", \"kiev\"]));\n\
             cities.insert(\"pop\", Column.from_vec([60i64, 80i64, 30i64]));\n\
             cities.insert(\"name\", Column.from_vec([\"Roma\", \"Oslo\", \"Kyiv\"]));\n\
             let plan = people.lazy().join(cities.lazy(), vec![\"city\"]);\n\
             println(plan.explain());\n\
             let out = plan.collect();\n\
             println(out.width()); println(out.height());\n\
             for n in out.column_names() { println(n); }\n\
             let names: Column[String] = out.column(\"name\");\n\
             let rn: Column[String] = out.column(\"name_right\");\n\
             let pops: Column[i64] = out.column(\"pop\");\n\
             for i in 0..out.height() {\n\
                 match names[i] { Some(v) => println(v), None => println(\"null\") }\n\
                 match rn[i] { Some(v) => println(v), None => println(\"null\") }\n\
                 match pops[i] { Some(v) => println(v), None => println(-1i64) }\n\
             }\n\
         }",
    );
    assert_eq!(
        out,
        "== logical plan ==\n\
         JOIN on=[city] right=(SCAN [city, pop, name])\n\
         \x20 SCAN [id, name, city]\n\
         == optimized ==\n\
         JOIN on=[city] right=(SCAN cols=[*])\n\
         \x20 SCAN cols=[*]\n\
         5\n3\nid\nname\ncity\npop\nname_right\n\
         ada\nRoma\n60\nbob\nOslo\n80\ncyd\nRoma\n60\n",
        "inner join must suffix collisions, drop unmatched rows, keep left order",
    );
}

#[test]
fn test_lazyframe_join_pipeline_nulls_and_fanout() {
    // Ops compose around a join: a right-side select narrows the right
    // sub-plan's scan; filter/select after the join see the joined
    // schema; a left select BEFORE the join renders as an explicit
    // SELECT step under the JOIN (the fold applies it at the join
    // boundary — the rendering must not pretend the scan keeps all
    // columns). NULL keys join nothing; duplicate right matches fan out
    // in left-row-then-right-match order.
    let out = run_no_errors(
        "import std.lazy.{col};\n\
         fn main() {\n\
             let mut people: DataFrame = DataFrame.new();\n\
             people.insert(\"id\", Column.from_vec([1i64, 2i64, 3i64, 4i64]));\n\
             people.insert(\"name\", Column.from_vec([\"ada\", \"bob\", \"cyd\", \"dee\"]));\n\
             people.insert(\"city\", Column.from_vec([\"rome\", \"oslo\", \"rome\", \"lima\"]));\n\
             let mut cities: DataFrame = DataFrame.new();\n\
             cities.insert(\"city\", Column.from_vec([\"rome\", \"oslo\", \"kiev\"]));\n\
             cities.insert(\"pop\", Column.from_vec([60i64, 80i64, 30i64]));\n\
             cities.insert(\"name\", Column.from_vec([\"Roma\", \"Oslo\", \"Kyiv\"]));\n\
             let plan = people.lazy()\n\
                 .join(cities.lazy().select(vec![\"city\", \"pop\"]), vec![\"city\"])\n\
                 .filter(col(\"pop\").gt(50))\n\
                 .select(vec![\"name\", \"pop\"]);\n\
             println(plan.explain());\n\
             println(plan.collect().height());\n\
             let pre = people.lazy().select(vec![\"name\", \"city\"]).join(cities.lazy(), vec![\"city\"]);\n\
             println(pre.explain());\n\
             println(pre.collect().width());\n\
             let mut left: DataFrame = DataFrame.new();\n\
             let lk: Vec[Option[i64]] = vec![Some(1i64), None, Some(3i64)];\n\
             left.insert(\"k\", Column.from_iter_nullable(lk));\n\
             let mut right: DataFrame = DataFrame.new();\n\
             let rk: Vec[Option[i64]] = vec![Some(1i64), None];\n\
             right.insert(\"k\", Column.from_iter_nullable(rk));\n\
             right.insert(\"w\", Column.from_vec([100i64, 200i64]));\n\
             let nj = left.lazy().join(right.lazy(), vec![\"k\"]).collect();\n\
             println(nj.height());\n\
             let ws: Column[i64] = nj.column(\"w\");\n\
             match ws[0] { Some(v) => println(v), None => println(-1i64) }\n\
             let mut dup: DataFrame = DataFrame.new();\n\
             dup.insert(\"city\", Column.from_vec([\"rome\", \"rome\"]));\n\
             dup.insert(\"tag\", Column.from_vec([\"a\", \"b\"]));\n\
             let fan = people.lazy().select(vec![\"name\", \"city\"]).join(dup.lazy(), vec![\"city\"]).collect();\n\
             println(fan.height());\n\
             let n4: Column[String] = fan.column(\"name\");\n\
             let t4: Column[String] = fan.column(\"tag\");\n\
             for i in 0..fan.height() {\n\
                 match n4[i] { Some(v) => println(v), None => println(\"null\") }\n\
                 match t4[i] { Some(v) => println(v), None => println(\"null\") }\n\
             }\n\
         }",
    );
    assert_eq!(
        out,
        "== logical plan ==\n\
         SELECT [name, pop]\n\
         \x20 FILTER (pop > 50)\n\
         \x20   JOIN on=[city] right=(SELECT [city, pop] <- SCAN [city, pop, name])\n\
         \x20     SCAN [id, name, city]\n\
         == optimized ==\n\
         SELECT [name, pop]\n\
         \x20 FILTER (pop > 50)\n\
         \x20   JOIN on=[city] right=(SCAN cols=[city, pop])\n\
         \x20     SCAN cols=[*]\n\
         3\n\
         == logical plan ==\n\
         JOIN on=[city] right=(SCAN [city, pop, name])\n\
         \x20 SELECT [name, city]\n\
         \x20   SCAN [id, name, city]\n\
         == optimized ==\n\
         JOIN on=[city] right=(SCAN cols=[*])\n\
         \x20 SELECT [name, city]\n\
         \x20   SCAN cols=[*]\n\
         4\n\
         1\n100\n\
         4\nada\na\nada\nb\ncyd\na\ncyd\nb\n",
        "join must compose with select/filter, drop NULL keys, fan out duplicates",
    );
}

#[test]
fn test_lazyframe_join_errors() {
    // Missing keys validate at collect (RIGHT side checked against the
    // right sub-plan's FINAL schema; LEFT side against the columns
    // visible at the join step); incompatible key types across the two
    // sides are a loud error, not a silent empty join.
    let out = run_no_errors(
        "fn main() {\n\
             let mut l: DataFrame = DataFrame.new();\n\
             l.insert(\"a\", Column.from_vec([1i64]));\n\
             let mut r: DataFrame = DataFrame.new();\n\
             r.insert(\"b\", Column.from_vec([1i64]));\n\
             let bad = l.lazy().join(r.lazy(), vec![\"a\"]);\n\
             println(\"built-ok\");\n\
             println(bad.explain());\n\
         }",
    );
    assert!(
        out.contains("built-ok")
            && out.contains("INVALID PLAN: LazyFrame.join: no column named 'a' on the RIGHT side"),
        "got: {out}",
    );
    let errors = runtime_errors(
        "fn main() {\n\
             let mut l: DataFrame = DataFrame.new();\n\
             l.insert(\"a\", Column.from_vec([1i64]));\n\
             let mut r: DataFrame = DataFrame.new();\n\
             r.insert(\"b\", Column.from_vec([1i64]));\n\
             let _ = l.lazy().join(r.lazy(), vec![\"b\"]).collect();\n\
         }",
    );
    assert!(
        errors.iter().any(|e| e
            .message
            .contains("LazyFrame.join: no column named 'b' on the LEFT side")),
        "{errors:?}",
    );
    let errors = runtime_errors(
        "fn main() {\n\
             let mut l: DataFrame = DataFrame.new();\n\
             l.insert(\"k\", Column.from_vec([1i64, 2i64]));\n\
             let mut r: DataFrame = DataFrame.new();\n\
             r.insert(\"k\", Column.from_vec([\"1\", \"2\"]));\n\
             r.insert(\"w\", Column.from_vec([10i64, 20i64]));\n\
             let _ = l.lazy().join(r.lazy(), vec![\"k\"]).collect();\n\
         }",
    );
    assert!(
        errors.iter().any(|e| e
            .message
            .contains("LazyFrame.join: key 'k' has incompatible types on the two sides")),
        "{errors:?}",
    );
}

/// B-2026-08-02-11 — interpreter twin of `tests/codegen.rs`'s
/// `e2e_tuple_literal_expected_elem_threading`, same source and expected
/// string (the typecheck fix unblocks both spellings for both backends).
#[test]
fn test_tuple_literal_expected_elem_threading() {
    assert_eq!(
        run("struct Sw { t: (Vec[i64], i64) }\n\
             fn main() {\n\
                 let mut t: (Vec[i64], i64) = (Vec.new(), 3);\n\
                 t.0.push(10);\n\
                 println(f\"a {t.0.len()} {t.0[0]} {t.1}\");\n\
                 let mut o = Sw { t: (Vec.new(), 3) };\n\
                 o.t.0.push(7);\n\
                 println(f\"b {o.t.0.len()} {o.t.0[0]}\");\n\
                 println(\"end\");\n\
             }\n"),
        "a 1 10 3\nb 1 7\nend\n"
    );
}

/// B-2026-08-01-5 — interpreter twin of `tests/codegen.rs`'s
/// `e2e_fresh_recv_temp_drop_semantics`, same source and expected string.
/// Pre-fix the interpreter fired NO receiver-temp body ever (the free-fn
/// arg hook had no method-receiver sibling); the
/// `run_fresh_recv_temp_drop` hook fires a fresh receiver's body at
/// statement end and stays silent for a borrow-returning method (the
/// result aliases the receiver).
///
/// Cell `b` was pinned SILENT here and read "the callee consumed the
/// value" — which is true of the value and false of the BODIES: codegen
/// treats a by-value `self` as caller-retained, so the callee ran no body
/// either and `mk(2).eat()` destroyed a `Res` without ever running its
/// destructor. B-2026-09-04-30 is that lost body, and `drop 2 r2` is the
/// pin moving to match. Cells `a`/`c` (`ref self`) and `d` (a passthrough
/// whose result binding owns the body) are unchanged, and `d` is now also
/// the guard cell: `me(self) -> Res` returns a type that runs a user
/// `Drop`, so the caller still stands down and the body comes from `m`.
#[test]
fn test_fresh_recv_temp_drop_semantics() {
    assert_eq!(
        run("struct Res { id: i64, name: String }\n\
             impl Drop for Res {\n\
                 fn drop(mut ref self) {\n\
                     println(f\"drop {self.id} {self.name}\")\n\
                 }\n\
             }\n\
             impl Res {\n\
                 fn ident(ref self) -> i64 {\n\
                     return self.id;\n\
                 }\n\
                 fn eat(self) -> i64 {\n\
                     return self.id + 100;\n\
                 }\n\
                 fn me(self) -> Res {\n\
                     return self;\n\
                 }\n\
             }\n\
             fn mk(n: i64) -> Res {\n\
                 return Res { id: n, name: f\"r{n}\" };\n\
             }\n\
             fn main() {\n\
                 println(\"a: ref-method fresh struct receiver\");\n\
                 let x = mk(1).ident();\n\
                 println(f\"x={x}\");\n\
                 println(\"b: owned-self consumed receiver\");\n\
                 let y = mk(2).eat();\n\
                 println(f\"y={y}\");\n\
                 println(\"c: bare ref-method statement\");\n\
                 mk(3).ident();\n\
                 println(\"d: passthrough result owned by binding\");\n\
                 let m = mk(4).me();\n\
                 println(f\"m={m.id}\");\n\
                 println(\"end\");\n\
             }\n"),
        "a: ref-method fresh struct receiver\ndrop 1 r1\nx=1\n\
         b: owned-self consumed receiver\ndrop 2 r2\ny=102\n\
         c: bare ref-method statement\ndrop 3 r3\n\
         d: passthrough result owned by binding\nm=4\ndrop 4 r4\nend\n"
    );
}

/// B-2026-07-30-11 (owning-temp arm channel) — interpreter twin of
/// `tests/codegen.rs`'s `e2e_arm_channel_owning_temp_vs_borrow_scrutinees`,
/// same source and expected string. The pre-fix interpreter never stashed
/// fresh-temp scrutinees (match-over-pop was silent), and the first if-let
/// gate admitted borrow accessors — `m.get(k)` / `v.first()` double-fired.
#[test]
fn test_arm_channel_owning_temp_vs_borrow_scrutinees() {
    assert_eq!(
        run("struct Res { id: i64 }\n\
             impl Drop for Res {\n\
                 fn drop(mut ref self) {\n\
                     println(f\"drop {self.id}\")\n\
                 }\n\
             }\n\
             fn main() {\n\
                 let mut v: Vec[Res] = Vec.new();\n\
                 v.push(Res { id: 61 });\n\
                 println(\"a\");\n\
                 match v.pop() {\n\
                     Option.Some(r) => { println(f\"got {r.id}\"); }\n\
                     Option.None => { println(\"none\"); }\n\
                 }\n\
                 println(\"b\");\n\
                 let mut w: Vec[Res] = Vec.new();\n\
                 w.push(Res { id: 62 });\n\
                 if let Option.Some(r) = w.pop() {\n\
                     println(f\"iflet got {r.id}\");\n\
                 }\n\
                 println(\"c\");\n\
                 let mut u: Vec[Res] = Vec.new();\n\
                 u.push(Res { id: 63 });\n\
                 while let Option.Some(r) = u.pop() {\n\
                     println(f\"wlet got {r.id}\");\n\
                 }\n\
                 println(\"d\");\n\
                 let mut m: Map[i64, Res] = Map.new();\n\
                 m.insert(1, Res { id: 81 });\n\
                 if let Option.Some(r) = m.get(1) {\n\
                     println(f\"see {r.id}\");\n\
                 }\n\
                 println(\"e\");\n\
                 let mut f: Vec[Res] = Vec.new();\n\
                 f.push(Res { id: 82 });\n\
                 if let Option.Some(r) = f.first() {\n\
                     println(f\"first {r.id}\");\n\
                 }\n\
                 println(\"end\");\n\
             }\n"),
        "a\ngot 61\ndrop 61\nb\niflet got 62\ndrop 62\nc\nwlet got 63\ndrop 63\nd\n\
         see 81\ndrop 81\ne\nfirst 82\ndrop 82\nend\n"
    );
}

#[test]
fn test_bare_stdlib_variant_pattern_selects_the_right_arm() {
    // B-2026-08-22-2 — baked-stdlib enum variants are registered in the
    // interpreter's env ONLY under their qualified path, so a BARE pattern
    // name missed the lookup and fell through to "true binding — matches
    // anything": the first arm always won. `IoError` is prelude and matching
    // on it is ordinary code, so this printed `NotFound` for a
    // `PermissionDenied` under `--interp` while both compiled backends
    // printed the right answer — the interpreter, which is the A/B oracle,
    // was the wrong one.
    let out = run("fn main() {\n\
            let e = IoError.PermissionDenied;\n\
            match e {\n\
                NotFound => println(\"NotFound\"),\n\
                PermissionDenied => println(\"PermissionDenied\"),\n\
                _ => println(\"other\"),\n\
            }\n\
        }");
    assert_eq!(out, "PermissionDenied\n");
}

// ── channel receiver liveness (B-2026-08-22-24) ──────────────────
//
// design.md's `send` ("panics if all receivers are dropped") and `try_send`'s
// `SendError.Closed` both need to know whether any receiver is still alive.
// Neither was implemented, because the interpreter's channel was ONE `Arc`
// shared by both ends: it could not tell a dropped receiver from a live one,
// and wiring only the compiled side made an orphaned sender print `closed`
// under `karac build` and `sent` here.
//
// The fix is a per-END count on the endpoint handles, whose `Clone` and `Drop`
// are the only things that move them. That is enough — a tree-walk evaluator
// drops the `Receiver` value when its scope goes, which is exactly the moment
// the compiled backend drops its own end.
//
// These pin the INTERPRETER half. The compiled twin lives in tests/codegen.rs;
// the two must agree, which is the whole point of the row.

#[test]
fn channel_try_send_reports_closed_when_the_receiver_is_gone() {
    // `rx` is local to `orphan`, so it dies when `orphan` returns and the
    // sender that escapes has no peer left.
    let out = run_no_errors(
        "fn orphan() -> Sender[i64] {\n\
         \x20   let (tx, rx): (Sender[i64], Receiver[i64]) = Channel.new();\n\
         \x20   return tx;\n\
         }\n\
         fn main() {\n\
         \x20   match orphan().try_send(9) {\n\
         \x20       Ok(u) => println(\"sent\"),\n\
         \x20       Err(SendError.Closed(v)) => println(f\"closed {v}\"),\n\
         \x20       Err(SendError.Full(v)) => println(f\"full {v}\"),\n\
         \x20   }\n\
         }",
    );
    assert_eq!(out.trim(), "closed 9", "orphaned sender must report Closed");
}

#[test]
fn channel_try_send_is_ok_while_a_receiver_is_alive() {
    // The control that keeps the test above honest: a count that was simply
    // always zero would satisfy it and break every working program.
    let out = run_no_errors(
        "fn main() {\n\
         \x20   let (tx, rx): (Sender[i64], Receiver[i64]) = Channel.new();\n\
         \x20   match tx.try_send(9) {\n\
         \x20       Ok(u) => println(\"sent\"),\n\
         \x20       Err(SendError.Closed(v)) => println(f\"closed {v}\"),\n\
         \x20       Err(SendError.Full(v)) => println(f\"full {v}\"),\n\
         \x20   }\n\
         \x20   match rx.try_recv() { Some(v) => println(v), None => println(\"empty\") }\n\
         }",
    );
    assert_eq!(
        out.trim(),
        "sent\n9",
        "a live receiver must accept the send"
    );
}

#[test]
fn channel_send_errors_when_every_receiver_is_gone() {
    // design.md: `send` panics if all receivers are dropped. `send` returns
    // unit, so an error is the only channel a failure has — the same argument
    // the full-bounded case makes.
    let errors = runtime_errors(
        "fn orphan() -> Sender[i64] {\n\
         \x20   let (tx, rx): (Sender[i64], Receiver[i64]) = Channel.new();\n\
         \x20   return tx;\n\
         }\n\
         fn main() {\n\
         \x20   orphan().send(9);\n\
         \x20   println(\"unreachable\");\n\
         }",
    );
    assert!(
        errors
            .iter()
            .any(|e| e.message.contains("no live receiver")),
        "expected a no-live-receiver error, got: {errors:?}"
    );
}

#[test]
fn channel_sender_clone_keeps_the_receiver_count_independent() {
    // The counts are PER END, so cloning a `Sender` must not make the channel
    // look like it has a receiver. One handle type for both ends would pass
    // the tests above and fail this one.
    let out = run_no_errors(
        "fn orphan() -> Sender[i64] {\n\
         \x20   let (tx, rx): (Sender[i64], Receiver[i64]) = Channel.new();\n\
         \x20   let extra = tx.clone();\n\
         \x20   return extra;\n\
         }\n\
         fn main() {\n\
         \x20   match orphan().try_send(9) {\n\
         \x20       Ok(u) => println(\"sent\"),\n\
         \x20       Err(SendError.Closed(v)) => println(f\"closed {v}\"),\n\
         \x20       Err(SendError.Full(v)) => println(f\"full {v}\"),\n\
         \x20   }\n\
         }",
    );
    assert_eq!(
        out.trim(),
        "closed 9",
        "a cloned SENDER must not count as a receiver"
    );
}

/// The rule is SYNTACTIC and applies at the FUNCTION tail only — both of which
/// are properties of codegen's detector that the interpreter now shares rather
/// than re-derives, so the two cannot drift.
///
/// Two consequences are pinned here because they look like bugs until you know
/// the rule: an `if` whose taken branch evaluates to `Err` does NOT fire
/// (the tail is an `If`, not an `Err(...)`), and an `Err(...)` at the tail of a
/// NESTED block does not fire the enclosing function's errdefer (the function's
/// tail is the block). Both backends agree on both, so neither is a run/build
/// divergence — but a "fix" that classified by the runtime VALUE instead of the
/// syntax would change these and introduce one in the opposite direction.
#[test]
fn test_tail_error_exit_is_syntactic_and_function_scoped() {
    // `if` tail that evaluates to Err — no errdefer.
    assert_eq!(
        run("fn body(c: bool) -> Result[i64, String] {\n\
                 defer { print(\"d\"); }\n\
                 errdefer { print(\"e\"); }\n\
                 if c { Err(\"boom\") } else { Ok(1) }\n\
             }\n\
             fn main() { let _ = body(true); }"),
        "d",
        "the tail is an `if`, not a syntactic `Err(...)` — codegen does not fire \
         here either, and the interpreter must match it"
    );
    // Nested-block tail holding the Err — the FUNCTION's tail is the block.
    assert_eq!(
        run("fn body() -> Result[i64, String] {\n\
                 defer { print(\"d\"); }\n\
                 errdefer { print(\"e\"); }\n\
                 { print(\"b\"); Err(\"boom\") }\n\
             }\n\
             fn main() { let _ = body(); }"),
        "bd"
    );
}

// ---------------------------------------------------------------------------
// A runtime error inside a `par {}` branch reaches the parent — B-2026-08-24-4
// ---------------------------------------------------------------------------
//
// A branch runs on its OWN `Interpreter`, and `record_runtime_error` pushes the
// message / span / trace onto THAT interpreter's vecs. The join used to harvest
// only `defined_vars`, the console segments, the dbg lines and `cf_result`, so
// the diagnostic died with the branch: the CLI found an empty `runtime_errors`,
// printed nothing, and exited 0. A program that died halfway reported SUCCESS,
// which is the worst shape a divergence can take — nothing signals that the
// answer should be doubted.
//
// ONE failing branch per test, deliberately: `par {}` is fail-fast, so with two
// failing branches which one records an error depends on which thread wins the
// cancellation race (measured 3/8 vs 5/8 over eight runs). Asserting on both
// would be flaky by construction.

#[test]
fn test_par_branch_runtime_error_reaches_the_parent() {
    let (_out, errors, trace, _trunc) = run_program_full(
        "fn boom(n: i64) -> i64 { if n == 1 { panic(\"branch died\") } n * 2 }\n\
         fn main() {\n\
             let (a, b) = par { let a = boom(0); let b = boom(1); (a, b) };\n\
             println(f\"joined {a} {b}\");\n\
         }\n",
    );
    assert!(
        errors.iter().any(|e| e.message.contains("branch died")),
        "the branch's error must reach the parent, got: {:?}",
        errors.iter().map(|e| &e.message).collect::<Vec<_>>()
    );
    // The RETURN TRACE is a second vec on the branch interpreter and was lost
    // the same way. Harvesting only the message would leave the par diagnostic
    // missing the `Error return trace:` section that the identical error
    // outside `par` prints.
    assert!(
        !trace.is_empty(),
        "the branch's error return trace must reach the parent too"
    );
}

#[test]
fn test_par_branch_error_matches_the_same_error_outside_par() {
    // The bar is parity with the SAME fault outside a `par {}`, not some new
    // par-specific rendering — that is what makes the join a transport
    // question rather than a formatting one.
    let par_errors = runtime_errors(
        "fn boom(n: i64) -> i64 { if n == 1 { panic(\"same text\") } n * 2 }\n\
         fn main() { let (a, b) = par { let a = boom(0); let b = boom(1); (a, b) }; }\n",
    );
    let plain_errors = runtime_errors(
        "fn boom() -> i64 { panic(\"same text\") }\n\
         fn main() { let x = boom(); }\n",
    );
    assert_eq!(
        par_errors.first().map(|e| e.message.clone()),
        plain_errors.first().map(|e| e.message.clone()),
        "a fault inside par must report the same message as outside it"
    );
    assert!(!par_errors.is_empty(), "par error must exist");
}

#[test]
fn test_par_branch_index_out_of_bounds_reaches_the_parent() {
    // A second, unrelated fault shape: the row measured three (explicit
    // `panic`, a Vec index, a failed `assert`) and all three were swallowed
    // identically, so the harvest must not be keyed to one of them.
    let errors = runtime_errors(
        "fn boom(n: i64) -> i64 { let v = Vec[1, 2]; if n == 1 { let x = v[99]; return x } n * 2 }\n\
         fn main() { let (a, b) = par { let a = boom(0); let b = boom(1); (a, b) }; }\n",
    );
    assert!(
        errors.iter().any(|e| e.message.contains("out of bounds")),
        "got: {:?}",
        errors.iter().map(|e| &e.message).collect::<Vec<_>>()
    );
}

#[test]
fn test_clean_par_records_no_runtime_error() {
    // The retraction must not manufacture errors for a healthy par block —
    // the inverse failure, and the one that would turn every parallel program
    // into a false alarm.
    let errors = runtime_errors(
        "fn ok(n: i64) -> i64 { n * 2 }\n\
         fn main() {\n\
             let (a, b) = par { let a = ok(1); let b = ok(2); (a, b) };\n\
             println(f\"joined {a} {b}\");\n\
         }\n",
    );
    assert!(
        errors.is_empty(),
        "clean par must record nothing: {errors:?}"
    );
}
