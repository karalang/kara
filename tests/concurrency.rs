// tests/concurrency.rs

use karac::concurrency::*;
use karac::{concurrency_analyze, effectcheck, lower, parse, resolve, typecheck};

// ── Test Helpers ────────────────────────────────────────────────

fn analyze(source: &str) -> ConcurrencyAnalysis {
    let parsed = parse(source);
    assert!(
        parsed.errors.is_empty(),
        "Parse errors: {}",
        parsed
            .errors
            .iter()
            .map(|e| e.to_string())
            .collect::<Vec<_>>()
            .join(", ")
    );
    let effects = effectcheck(&parsed.program);
    concurrency_analyze(&parsed.program, &effects)
}

/// Mirror of `analyze` that runs the typecheck + lowering passes
/// before concurrency analysis. The CLI pipeline lowers primitive
/// operators into trait-method calls before concurrencycheck runs
/// (`src/lowering.rs`), so the reduction recognizer must handle the
/// post-lowering `Call(Path([type, op_method]), [a, b])` shape as
/// well as the parser-shape `Binary { op, left, right }`. Without
/// this lowered-pipeline test, the kata-7 / Parallax CLI surface
/// would silently regress while the parse-shape unit tests pass.
fn analyze_lowered(source: &str) -> ConcurrencyAnalysis {
    let mut parsed = parse(source);
    assert!(
        parsed.errors.is_empty(),
        "Parse errors: {}",
        parsed
            .errors
            .iter()
            .map(|e| e.to_string())
            .collect::<Vec<_>>()
            .join(", ")
    );
    let resolved = resolve(&parsed.program);
    let tc = typecheck(&parsed.program, &resolved);
    lower(&mut parsed.program, &tc);
    let effects = effectcheck(&parsed.program);
    concurrency_analyze(&parsed.program, &effects)
}

fn get_function<'a>(analysis: &'a ConcurrencyAnalysis, name: &str) -> &'a FunctionConcurrency {
    analysis
        .function_decisions
        .get(name)
        .unwrap_or_else(|| panic!("function '{}' not found in analysis", name))
}

// ── Pure independent calls are parallelizable ──────────────────

#[test]
fn test_pure_independent_calls() {
    let analysis = analyze(
        r#"
        fn a() -> i32 { 1 }
        fn b() -> i32 { 2 }
        fn main() {
            let x = a();
            let y = b();
        }
        "#,
    );

    let main_fc = get_function(&analysis, "main");
    assert_eq!(main_fc.total_statements, 2);
    // Both statements should be in a single parallel group
    assert_eq!(main_fc.parallel_groups.len(), 1);
    assert_eq!(main_fc.parallel_groups[0].statement_indices.len(), 2);
    assert!(main_fc.parallel_groups[0].statement_indices.contains(&0));
    assert!(main_fc.parallel_groups[0].statement_indices.contains(&1));
}

// ── Data dependency forces serialization ───────────────────────

#[test]
fn test_data_dependency_serializes() {
    let analysis = analyze(
        r#"
        fn main() {
            let x = 1;
            let y = x + 1;
        }
        "#,
    );

    let main_fc = get_function(&analysis, "main");
    assert_eq!(main_fc.total_statements, 2);
    // No parallel groups because y depends on x
    assert!(
        main_fc.parallel_groups.is_empty(),
        "Expected no parallel groups due to data dependency, got {:?}",
        main_fc.parallel_groups
    );
}

#[test]
fn test_mut_ref_self_calls_serialize() {
    // B-2026-07-09-12: two sequential `let x = self.mut_method()` calls that
    // mutate the receiver (a parser cursor) must NOT be auto-parallelized —
    // they share `self` and have a strict ordering dependency through it. A
    // `mut ref self` method that only advances a plain scalar field carries no
    // `writes(Resource)` effect, so the effect-verb heuristic alone missed the
    // conflict; combined with the `Let` arm not collecting RHS inner-writes, the
    // auto-parallelizer raced three such calls in the self-hosted parser's
    // `parse_if` (SEGV on every control-flow expression). The fix records `self`
    // as written by a `mut ref self` call so the calls conflict and stay serial.
    let analysis = analyze(
        r#"
        struct Cursor { pos: i64 }
        impl Cursor {
            fn step(mut ref self) -> i64 {
                self.pos = self.pos + 1;
                self.pos
            }
            fn run(mut ref self) -> i64 {
                let a = self.step();
                let b = self.step();
                a + b
            }
        }
        fn main() {}
        "#,
    );
    let run_fc = get_function(&analysis, "Cursor.run");
    // Statements 0 (`let a = self.step()`) and 1 (`let b = self.step()`) must
    // never land in the same parallel group.
    assert!(
        run_fc
            .parallel_groups
            .iter()
            .all(|g| { !(g.statement_indices.contains(&0) && g.statement_indices.contains(&1)) }),
        "two mut-ref-self cursor advances must serialize, got groups {:?}",
        run_fc.parallel_groups
    );
}

// ── A map-mutating loop serializes against a later read of the map ──

#[test]
fn test_map_mutating_loop_serializes_against_later_read() {
    // `for x in xs { *m.entry(x).or_insert(0) += 1 }` writes the map `m`
    // through a deref-of-method-chain target; the later `let ks = m.keys()`
    // reads it. That read-after-write must serialize the loop against the read.
    // Pre-fix the loop's writes(m) was INVISIBLE — `collect_assign_target_defines`
    // had no `Deref` / `MethodCall` arm, so a `*chain += …` target recorded no
    // write — and auto-par grouped the loop with `keys()` and raced on the map
    // under `karac build` (B-2026-06-20-16).
    let analysis = analyze_lowered(
        r#"
        fn main() {
            let mut m: Map[i64, i64] = Map.new();
            let xs = [1, 2, 1];
            for x in xs {
                *m.entry(x).or_insert(0_i64) += 1_i64;
            }
            let ks = m.keys();
        }
        "#,
    );
    let main_fc = get_function(&analysis, "main");
    // 0=let m, 1=let xs, 2=for-loop (writes m), 3=let ks (reads m).
    assert_eq!(main_fc.total_statements, 4);
    for g in &main_fc.parallel_groups {
        let si = &g.statement_indices;
        assert!(
            !(si.contains(&2) && si.contains(&3)),
            "map-mutating loop (2) and m.keys() (3) must not be co-parallelized; got {:?}",
            si
        );
    }
}

// ── Effect conflict forces serialization ───────────────────────

#[test]
fn test_effect_conflict_serializes() {
    let analysis = analyze(
        r#"
        effect resource Db;
        fn read_db() reads(Db) { }
        fn write_db() writes(Db) { }
        fn main() {
            read_db();
            write_db();
        }
        "#,
    );

    let main_fc = get_function(&analysis, "main");
    assert_eq!(main_fc.total_statements, 2);
    // reads + writes on same resource = conflict, no parallel group
    assert!(
        main_fc.parallel_groups.is_empty(),
        "Expected no parallel groups due to effect conflict (reads+writes on Db), got {:?}",
        main_fc.parallel_groups
    );
}

// ── console output parallelizes with ordered-output capture ────
// (was B-2026-06-13-18 "output forces serialization"; the blanket
// suppression is reversed now that the runtime captures each branch's output
// and replays it in source order at the join — phase-6-runtime.md "Auto-par
// ordered output". Observable output stays byte-identical to sequential.)

#[test]
fn test_direct_println_parallelizes_with_ordered_output() {
    // Previously (B-2026-06-13-18) consecutive `println`s were forced serial to
    // avoid racing the shared stdout buffer. With ordered-output capture they
    // are independent effect-free statements again and form one parallel group;
    // `karac_par_run` buffers each branch and flushes in source order, so the
    // printed sequence is unchanged. (Codegen may still decline a trivial group
    // via the cost model — that's a separate, downstream decision.)
    let analysis = analyze(
        r#"
        fn main() {
            println(1);
            println(2);
            println(3);
        }
        "#,
    );

    let main_fc = get_function(&analysis, "main");
    assert_eq!(
        main_fc.parallel_groups.len(),
        1,
        "consecutive `println`s should now group (ordered-output capture preserves order), got {:?}",
        main_fc.parallel_groups
    );
}

#[test]
fn test_transitive_println_parallelizes_with_ordered_output() {
    // A fn that prints AND carries a real (non-conflicting) effect is fanned out
    // at its call sites again — effects on different resources don't conflict,
    // and the prints buried inside each branch are captured and replayed in
    // branch order. No transitive-output suppression remains.
    let analysis = analyze(
        r#"
        effect resource A;
        effect resource B;
        fn touch_a() writes(A) {}
        fn touch_b() writes(B) {}
        fn work_a() -> i32 writes(A) { println(1); touch_a(); 10 }
        fn work_b() -> i32 writes(B) { println(2); touch_b(); 20 }
        fn main() {
            let a = work_a();
            let b = work_b();
        }
        "#,
    );

    let main_fc = get_function(&analysis, "main");
    assert_eq!(
        main_fc.parallel_groups.len(),
        1,
        "calls to transitively-printing fns on disjoint resources should now group, got {:?}",
        main_fc.parallel_groups
    );
}

#[test]
fn test_pure_compute_still_parallelizes_alongside_output_guard() {
    // Guard against over-correction: two side-effect-free user calls with no
    // output still form a group (mirrors `test_pure_independent_calls`).
    let analysis = analyze(
        r#"
        fn a() -> i32 { 1 }
        fn b() -> i32 { 2 }
        fn main() {
            let x = a();
            let y = b();
        }
        "#,
    );

    let main_fc = get_function(&analysis, "main");
    assert_eq!(
        main_fc.parallel_groups.len(),
        1,
        "Pure independent calls should still group, got {:?}",
        main_fc.parallel_groups
    );
}

// ── self-mutation forces serialization (self-hosting #8) ───────

#[test]
fn test_self_field_writes_serialize() {
    // Regression for self-hosting blocker #8: the auto-par analyzer dropped a
    // `self` (SelfValue) receiver from both its read collector
    // (`collect_expr_reads`) and its write collector
    // (`collect_assign_target_defines`), so two statements that both touch
    // `self` showed "no data dependency" and got parallelized — racing on the
    // struct's state (nondeterministic). The lexer's
    // `skip_whitespace(); self.start = self.pos` was the live case. Both
    // self-field writes here read+write `self`, so they MUST serialize.
    let analysis = analyze(
        r#"
        struct S { pos: i64, start: i64 }
        impl S {
            fn step(mut ref self) -> i64 {
                self.start = self.pos;
                self.pos = self.start + 1;
                self.pos
            }
        }
        fn main() { let mut s = S { pos: 0, start: 0 }; let n = s.step(); }
        "#,
    );
    let step_fc = get_function(&analysis, "S.step");
    assert!(
        step_fc.parallel_groups.is_empty(),
        "self-field writes must serialize (auto-par #8), got {:?}",
        step_fc.parallel_groups
    );
}

#[test]
fn test_mut_ref_self_method_call_serializes_with_self_read() {
    // The method-call half of #8: a `mut ref self` method call writes `self`
    // (its effects imply receiver mutation), so a following `self.field` read
    // depends on it. Before the fix the receiver's `self` was dropped, so
    // `self.bump(); self.start = self.pos` parallelized and raced.
    let analysis = analyze(
        r#"
        effect resource Tape;
        struct S { pos: i64, start: i64 }
        impl S {
            fn bump(mut ref self) writes(Tape) { self.pos = self.pos + 1; }
            fn step(mut ref self) -> i64 {
                self.bump();
                self.start = self.pos;
                self.start
            }
        }
        fn main() { let mut s = S { pos: 0, start: 0 }; let n = s.step(); }
        "#,
    );
    let step_fc = get_function(&analysis, "S.step");
    assert!(
        step_fc.parallel_groups.is_empty(),
        "a mut-ref-self method call + self read must serialize (auto-par #8), got {:?}",
        step_fc.parallel_groups
    );
}

// ── Independent blocking calls parallelize (A1) ────────────────

#[test]
fn test_independent_blocking_calls_parallelize() {
    // Two independent `blocks` statements (libc `usleep`, ABI "C" → default
    // `blocks` effect). Before A1 the auto-par conflict model wrongly treated
    // blocks+blocks as a conflict and serialized them; A1 lifts that
    // (design.md:5907 — execution verbs answer placement, not conflict), so
    // the runtime overlaps them on the blocking pool via the `par_run`
    // fan-out. See bench/auto_par_io/ for the wall-clock proof.
    let analysis = analyze(
        r#"
        unsafe extern "C" { fn usleep(usecs: u32) -> i32; }
        fn main() {
            usleep(100);
            usleep(100);
        }
        "#,
    );

    let main_fc = get_function(&analysis, "main");
    assert_eq!(main_fc.total_statements, 2);
    assert_eq!(
        main_fc.parallel_groups.len(),
        1,
        "two independent blocking calls should parallelize (A1), got {:?}",
        main_fc.parallel_groups
    );
    assert_eq!(main_fc.parallel_groups[0].statement_indices.len(), 2);
    assert!(main_fc.parallel_groups[0].statement_indices.contains(&0));
    assert!(main_fc.parallel_groups[0].statement_indices.contains(&1));
}

// ── Independent allocating calls parallelize (A3) ──────────────

#[test]
fn test_independent_allocating_calls_parallelize() {
    // Two independent statements that each only `allocates(Heap)` (each calls a
    // Vec-building helper). Before A3 the auto-par conflict model wrongly
    // treated allocates+allocates on the same `Heap` resource as a conflict and
    // serialized them; A3 lifts that — `allocates` is an *informational* verb
    // (design.md: only reads/writes + sends/receives drive conflict), the heap
    // allocator is thread-safe, and the diagnostics-side `effects_conflict`
    // already treats it as non-conflicting. The two calls write disjoint
    // bindings (`a`, `b`) so there is no dataflow dependency either.
    let analysis = analyze(
        r#"
        fn make() -> Vec[i64] {
            let mut v: Vec[i64] = Vec.new();
            v.push(1);
            return v;
        }
        fn main() {
            let a = make();
            let b = make();
        }
        "#,
    );

    let main_fc = get_function(&analysis, "main");
    assert_eq!(main_fc.total_statements, 2);
    assert_eq!(
        main_fc.parallel_groups.len(),
        1,
        "two independent allocating calls should parallelize (A3), got {:?}",
        main_fc.parallel_groups
    );
    let g = &main_fc.parallel_groups[0];
    assert!(g.statement_indices.contains(&0) && g.statement_indices.contains(&1));
}

#[test]
fn test_allocate_then_read_dependency_still_serializes() {
    // A3 only lifts the *effect-level* allocates+allocates conflict; it must NOT
    // weaken dataflow serialization. Here the second statement reads `a`, the
    // value the first produced (a RAW dependency), so the pair must stay serial
    // even though both also `allocates(Heap)`. Pins that the flip is scoped to
    // the effect graph and the data-dependency graph is untouched.
    let analysis = analyze(
        r#"
        fn make() -> Vec[i64] {
            let mut v: Vec[i64] = Vec.new();
            v.push(1);
            return v;
        }
        fn consume(xs: Vec[i64]) -> Vec[i64] {
            let mut v: Vec[i64] = Vec.new();
            v.push(xs.len());
            return v;
        }
        fn main() {
            let a = make();
            let b = consume(a);
        }
        "#,
    );

    let main_fc = get_function(&analysis, "main");
    assert!(
        main_fc.parallel_groups.is_empty(),
        "a RAW data dependency must serialize the pair despite the allocates flip, got {:?}",
        main_fc.parallel_groups
    );
}

// ── Independent panicking calls parallelize (A3b) ──────────────

#[test]
fn test_independent_panicking_calls_parallelize() {
    // Two independent statements that each only `panics` (each calls a helper
    // that divides — `/` infers `panics` via the div-by-zero guard). Before A3b
    // the auto-par conflict model treated panics+panics as a conflict and
    // serialized them; A3b lifts that — `panics` is *informational* (design.md:
    // only reads/writes + sends/receives drive conflict), and a Kāra panic
    // lowers to `exit(1)` (fail-fast process exit, not an unwind), so grouping
    // two panic-capable statements is safe. This is what unblocks auto-par for
    // ordinary arithmetic (the `examples/parallax_lite` `/`/`%` blocker).
    let analysis = analyze(
        r#"
        fn divmod(n: i64, d: i64) -> i64 { return n / d; }
        fn main() {
            let a = divmod(100, 5);
            let b = divmod(200, 4);
        }
        "#,
    );

    let main_fc = get_function(&analysis, "main");
    assert_eq!(main_fc.total_statements, 2);
    assert_eq!(
        main_fc.parallel_groups.len(),
        1,
        "two independent panicking calls should parallelize (A3b), got {:?}",
        main_fc.parallel_groups
    );
    let g = &main_fc.parallel_groups[0];
    assert!(g.statement_indices.contains(&0) && g.statement_indices.contains(&1));
}

#[test]
fn test_panic_then_dependency_still_serializes() {
    // A3b is scoped to the effect graph: a data dependency between two
    // panic-capable statements must still serialize them. Here the second div
    // consumes the first's result (RAW), so the pair stays serial despite the
    // panics flip.
    let analysis = analyze(
        r#"
        fn divmod(n: i64, d: i64) -> i64 { return n / d; }
        fn main() {
            let a = divmod(100, 5);
            let b = divmod(a, 2);
        }
        "#,
    );

    let main_fc = get_function(&analysis, "main");
    assert!(
        main_fc.parallel_groups.is_empty(),
        "a RAW data dependency must serialize the pair despite the panics flip, got {:?}",
        main_fc.parallel_groups
    );
}

// ── Timer suspends parallelizes; other suspends stays serial (A2b) ─

#[test]
fn test_independent_timer_suspends_parallelize() {
    // A2b: a standalone `sleep_ms` timer wait is the one `suspends` form proven
    // independent (a bare timer park, no by-value `Drop` params, no
    // happens-before with a sibling). Two of them are an execution-verb pair
    // (placement, not conflict — like `blocks`, A1) and overlap via the par
    // thread-block path.
    let analysis = analyze(
        r#"
        fn main() {
            sleep_ms(100);
            sleep_ms(100);
        }
        "#,
    );

    let main_fc = get_function(&analysis, "main");
    assert_eq!(
        main_fc.parallel_groups.len(),
        1,
        "two independent sleep_ms timer waits should parallelize (A2b), got {:?}",
        main_fc.parallel_groups
    );
    let g = &main_fc.parallel_groups[0];
    assert!(g.statement_indices.contains(&0) && g.statement_indices.contains(&1));
}

#[test]
fn test_nontimer_suspend_calls_serialize() {
    // A2b is conservative: at the effect level a channel `recv`, a network park,
    // and a user `with suspends` fn all seed a bare `suspends` — indistinguish-
    // able from a timer wait, but NOT provably independent. Only a direct
    // `sleep_ms` is exempted; everything else stays serial. This pins that a
    // non-`sleep_ms` suspending call (here a user `with suspends` fn) does NOT
    // parallelize — the guard that prevents a channel-`recv`-behind-a-call from
    // being lifted into a `__par_branch` worker and deadlocking (regression
    // for the channel producer/consumer hang found while building A2b).
    let analysis = analyze(
        r#"
        fn work() -> i64 with suspends { return 1; }
        fn main() {
            let a = work();
            let b = work();
        }
        "#,
    );

    let main_fc = get_function(&analysis, "main");
    assert!(
        main_fc.parallel_groups.is_empty(),
        "a non-timer suspending call must stay serial (A2b conservative gate), got {:?}",
        main_fc.parallel_groups
    );
}

// ── A2b-2 Phase 1: ephemeral send/recv network calls fan out ──────

#[test]
fn test_a2b2_ephemeral_network_sends_receives_fanout() {
    // A2b-2 Phase 1 flagship. Two borrow-free free-fn network calls that
    // `sends(Network)` AND `receives(Network)` — the real `http_get("a");
    // http_get("b")` shape, not the synthetic `reads(Network)` one — now fan
    // out. Both are *ephemeral* network fan-outs (borrow-free callees ⇒ each
    // opens its own private connection), so `statements_conflict` relaxes the
    // `Network`↔`Network` conflict that previously kept them serial (via
    // `(Sends,Sends)`/`(Receives,Receives)`). Before Phase 1 this exact case
    // stayed serial (the old `test_network_boundary_calls_still_serialize`).
    // Contrast `test_a2b2_borrow_param_network_sends_receives_stays_serial`,
    // which pins that a *borrow-param* send/recv pair (a possibly-shared
    // connection) still serializes.
    let analysis = analyze(
        r#"
        fn get_a() -> i64 with sends(Network) receives(Network) { return 1; }
        fn get_b() -> i64 with sends(Network) receives(Network) { return 2; }
        fn main() {
            let x = get_a();
            let y = get_b();
        }
        "#,
    );

    let main_fc = get_function(&analysis, "main");
    assert_eq!(
        main_fc.parallel_groups.len(),
        1,
        "two ephemeral send/recv network calls should fan out (A2b-2 Phase 1), got {:?}",
        main_fc.parallel_groups
    );
    let g = &main_fc.parallel_groups[0];
    assert!(g.statement_indices.contains(&0) && g.statement_indices.contains(&1));
}

// ── Borrow-param network calls STILL serialize (shared-conn soundness) ──

#[test]
fn test_a2b2_borrow_param_network_sends_receives_stays_serial() {
    // The Phase 1 `Network`-conflict relaxation is gated on *ephemeral* calls
    // — a borrow-free callee that cannot be handed a shared connection. A
    // callee that BORROWS a parameter (`ref Conn`) could be operating on the
    // same connection object as its sibling; overlapping two `sends`/`receives`
    // on one socket races. This pass carries no connection-identity info to
    // tell same-conn from different-conn, so a borrow-param send/recv pair is
    // NOT ephemeral and stays serial — the `(Sends,Sends)`/`(Receives,Receives)`
    // conflict is not relaxed. (Distinguishing distinct borrowed connections is
    // the Phase 2 parameterized-`Network` follow-up.)
    let analysis = analyze(
        r#"
        fn send_on(c: ref Conn) -> i64 with sends(Network) receives(Network) { return 1; }
        fn main() {
            let conn = 0;
            let x = send_on(conn);
            let y = send_on(conn);
        }
        "#,
    );

    let main_fc = get_function(&analysis, "main");
    // Statements 1 and 2 are the two `send_on(conn)` calls (after the `let
    // conn`). They must NOT co-group — the callee borrows, so the relaxation
    // does not apply and the send/recv conflict serializes them.
    let calls_grouped = main_fc
        .parallel_groups
        .iter()
        .any(|g| g.statement_indices.contains(&1) && g.statement_indices.contains(&2));
    assert!(
        !calls_grouped,
        "borrow-param send/recv network calls must stay serial (shared-conn soundness), got {:?}",
        main_fc.parallel_groups
    );
}

// ── A2b-2: independent network calls fan out (arg-safe shape) ─────

#[test]
fn test_a2b2_independent_network_reads_parallelize() {
    // A2b-2: two independent network fetches — `reads(Network) suspends`, the
    // canonical shape — now overlap. The coroutine-boundary gate excluded ALL
    // suspends/Network calls; A2b-2 lifts it for the ARG-SAFE shape (literal /
    // no-argument calls that move no owned binding into the coroutine, so the
    // `__par_branch` coroutine-owned-param double-drop cannot fire).
    // `reads(Network)` + `reads(Network)` do not conflict (`(Reads,Reads) =>
    // false`), so once past the gate they group. Flagship "effects
    // auto-parallelize independent I/O".
    let analysis = analyze(
        r#"
        fn fetch_a() -> i64 with reads(Network) suspends { return 1; }
        fn fetch_b() -> i64 with reads(Network) suspends { return 2; }
        fn main() {
            let x = fetch_a();
            let y = fetch_b();
        }
        "#,
    );
    let main_fc = get_function(&analysis, "main");
    assert_eq!(
        main_fc.parallel_groups.len(),
        1,
        "two independent arg-safe network reads should parallelize (A2b-2), got {:?}",
        main_fc.parallel_groups
    );
    let g = &main_fc.parallel_groups[0];
    assert!(g.statement_indices.contains(&0) && g.statement_indices.contains(&1));
}

#[test]
fn test_a2b2_network_call_with_owned_arg_stays_serial() {
    // The A2b-2 exemption is FAIL-CLOSED on arguments: a network call that moves
    // an owned binding into itself (`fetch(a)` where `a` is a named `String`)
    // could hit the coroutine-owned-param double-drop, so it is NOT admitted —
    // only literal/const-arg calls are. The `Identifier` argument disqualifies,
    // so these stay serial. (Copy/borrow args are also excluded for now — this
    // pass carries no type info to tell them from an owned move; admitting them
    // is the A2b-2 follow-up.)
    let analysis = analyze(
        r#"
        fn fetch(u: String) -> i64 with reads(Network) suspends { return 1; }
        fn main() {
            let a = "http://a";
            let b = "http://b";
            let x = fetch(a);
            let y = fetch(b);
        }
        "#,
    );
    let main_fc = get_function(&analysis, "main");
    // The two `fetch(...)` calls are statements 2 and 3 (after the two `let`
    // URL bindings). They must NOT be co-grouped — the owned-`String` identifier
    // arg fails the fail-closed exemption. (The two trivial constant-init `let`
    // bindings at 0,1 may form their own is_trivial group — a codegen no-op —
    // which is not what this pins.)
    let fetches_grouped = main_fc
        .parallel_groups
        .iter()
        .any(|g| g.statement_indices.contains(&2) && g.statement_indices.contains(&3));
    assert!(
        !fetches_grouped,
        "two network calls each moving an owned binding in must stay serial \
         (fail-closed A2b-2), got {:?}",
        main_fc.parallel_groups
    );
}

#[test]
fn test_a2b2_network_call_with_ref_param_arg_parallelizes() {
    // A2b-2 variable-arg reach: a network call whose parameter BORROWS
    // (`ref String`) does not move its argument, so passing an owned binding
    // there is fan-out-safe (no coroutine-owned-param double-drop). The two
    // `fetch(a)` / `fetch(b)` calls (statements 2 and 3, distinct URLs) now
    // group — the identifier args are admitted because the callee param is
    // `ref`. Contrast `test_a2b2_network_call_with_owned_arg_stays_serial`,
    // where the same arg shape into an OWNED `String` param stays serial.
    let analysis = analyze(
        r#"
        fn fetch(u: ref String) -> i64 with reads(Network) suspends { return 1; }
        fn main() {
            let a = "http://a";
            let b = "http://b";
            let x = fetch(a);
            let y = fetch(b);
        }
        "#,
    );
    let main_fc = get_function(&analysis, "main");
    let fetches_grouped = main_fc
        .parallel_groups
        .iter()
        .any(|g| g.statement_indices.contains(&2) && g.statement_indices.contains(&3));
    assert!(
        fetches_grouped,
        "ref-param network calls with owned-binding args should parallelize \
         (A2b-2 variable-arg), got {:?}",
        main_fc.parallel_groups
    );
}

// ── Different resources are parallelizable ─────────────────────

#[test]
fn test_different_resources_parallelizable() {
    let analysis = analyze(
        r#"
        effect resource Db;
        effect resource Cache;
        fn read_db() reads(Db) { }
        fn read_cache() reads(Cache) { }
        fn main() {
            read_db();
            read_cache();
        }
        "#,
    );

    let main_fc = get_function(&analysis, "main");
    assert_eq!(main_fc.total_statements, 2);
    assert_eq!(main_fc.parallel_groups.len(), 1);
    assert_eq!(main_fc.parallel_groups[0].statement_indices.len(), 2);
}

// ── reads+reads on same resource is safe ───────────────────────

#[test]
fn test_reads_reads_same_resource_safe() {
    let analysis = analyze(
        r#"
        effect resource Db;
        fn read1() reads(Db) { }
        fn read2() reads(Db) { }
        fn main() {
            read1();
            read2();
        }
        "#,
    );

    let main_fc = get_function(&analysis, "main");
    assert_eq!(main_fc.total_statements, 2);
    assert_eq!(main_fc.parallel_groups.len(), 1);
    assert_eq!(main_fc.parallel_groups[0].statement_indices.len(), 2);
}

// ── writes+writes on same resource conflicts ───────────────────

#[test]
fn test_writes_writes_same_resource_conflicts() {
    let analysis = analyze(
        r#"
        effect resource Db;
        fn write1() writes(Db) { }
        fn write2() writes(Db) { }
        fn main() {
            write1();
            write2();
        }
        "#,
    );

    let main_fc = get_function(&analysis, "main");
    assert_eq!(main_fc.total_statements, 2);
    assert!(
        main_fc.parallel_groups.is_empty(),
        "Expected no parallel groups due to writes+writes conflict, got {:?}",
        main_fc.parallel_groups
    );
}

// ── seq {} forces sequential ───────────────────────────────────

#[test]
fn test_seq_forces_sequential() {
    let analysis = analyze(
        r#"
        fn a() -> i32 { 1 }
        fn b() -> i32 { 2 }
        fn main() {
            seq {
                let x = a();
                let y = b();
            };
        }
        "#,
    );

    let main_fc = get_function(&analysis, "main");
    // The seq block is a single Expr statement wrapping the block
    // The inner statements within seq are forced sequential
    // but main only has 1 top-level statement (the seq expression)
    assert_eq!(main_fc.total_statements, 1);
    // With only 1 statement, no parallel groups possible
    assert!(main_fc.parallel_groups.is_empty());
}

// ── Cross-category effects don't conflict ──────────────────────

#[test]
fn test_cross_category_no_conflict() {
    let analysis = analyze(
        r#"
        effect resource Db;
        fn read_db() reads(Db) { }
        fn send_db() sends(Db) { }
        fn main() {
            read_db();
            send_db();
        }
        "#,
    );

    let main_fc = get_function(&analysis, "main");
    assert_eq!(main_fc.total_statements, 2);
    // reads + sends = different categories = no conflict
    assert_eq!(main_fc.parallel_groups.len(), 1);
    assert_eq!(main_fc.parallel_groups[0].statement_indices.len(), 2);
}

// ── sends+sends on same resource conflicts ─────────────────────

#[test]
fn test_sends_sends_same_resource_conflicts() {
    let analysis = analyze(
        r#"
        effect resource Chan;
        fn send1() sends(Chan) { }
        fn send2() sends(Chan) { }
        fn main() {
            send1();
            send2();
        }
        "#,
    );

    let main_fc = get_function(&analysis, "main");
    assert_eq!(main_fc.total_statements, 2);
    assert!(
        main_fc.parallel_groups.is_empty(),
        "Expected no parallel groups due to sends+sends conflict"
    );
}

// ── Empty function ─────────────────────────────────────────────

#[test]
fn test_empty_function() {
    let analysis = analyze("fn main() { }");
    let main_fc = get_function(&analysis, "main");
    assert_eq!(main_fc.total_statements, 0);
    assert!(main_fc.parallel_groups.is_empty());
}

// ── Single statement — no parallelism possible ─────────────────

#[test]
fn test_single_statement() {
    let analysis = analyze(
        r#"
        fn main() {
            let x = 1;
        }
        "#,
    );
    let main_fc = get_function(&analysis, "main");
    assert_eq!(main_fc.total_statements, 1);
    assert!(main_fc.parallel_groups.is_empty());
}

// ── Multiple independent pure statements ───────────────────────

#[test]
fn test_multiple_independent_pure() {
    let analysis = analyze(
        r#"
        fn main() {
            let a = 1;
            let b = 2;
            let c = 3;
        }
        "#,
    );

    let main_fc = get_function(&analysis, "main");
    assert_eq!(main_fc.total_statements, 3);
    assert_eq!(main_fc.parallel_groups.len(), 1);
    assert_eq!(main_fc.parallel_groups[0].statement_indices.len(), 3);
}

// ── Chain dependency: a -> b -> c ──────────────────────────────

#[test]
fn test_chain_dependency() {
    let analysis = analyze(
        r#"
        fn main() {
            let a = 1;
            let b = a + 1;
            let c = b + 1;
        }
        "#,
    );

    let main_fc = get_function(&analysis, "main");
    assert_eq!(main_fc.total_statements, 3);
    // `a` and `c` have no direct edge between them (c reads b, not a),
    // but `b` sits between them with a hard dep on both. The
    // contiguous-only grouping rule rejects [0, 2]: codegen emits a
    // single `karac_par_run` fan-out at the group's min_idx, so
    // skipping over a dependent middle stmt would either drop stmt 1
    // entirely or produce a branch that reads a binding the analyzer
    // can't guarantee is in scope. So no parallel group fires here.
    assert_eq!(main_fc.parallel_groups.len(), 0);
}

// ── Full chain: every statement reads previous ─────────────────

#[test]
fn test_full_chain_no_parallelism() {
    let analysis = analyze(
        r#"
        fn main() {
            let a = 1;
            let b = a;
            let c = a + b;
        }
        "#,
    );

    let main_fc = get_function(&analysis, "main");
    assert_eq!(main_fc.total_statements, 3);
    // b reads a, c reads both a and b — no independent pair
    assert!(main_fc.parallel_groups.is_empty());
}

// ── Diamond dependency pattern ─────────────────────────────────

#[test]
fn test_diamond_dependency() {
    let analysis = analyze(
        r#"
        fn main() {
            let a = 1;
            let b = a + 1;
            let c = a + 2;
            let d = b + c;
        }
        "#,
    );

    let main_fc = get_function(&analysis, "main");
    assert_eq!(main_fc.total_statements, 4);
    // b and c both depend on a, but are independent of each other
    // d depends on both b and c
    // So b and c can be parallel
    assert!(!main_fc.parallel_groups.is_empty());
    // Find the group containing b (index 1) and c (index 2)
    let bc_group = main_fc
        .parallel_groups
        .iter()
        .find(|g| g.statement_indices.contains(&1) && g.statement_indices.contains(&2));
    assert!(
        bc_group.is_some(),
        "Expected b and c to be in a parallel group"
    );
}

// ── Transitive effect inheritance ──────────────────────────────

#[test]
fn test_transitive_effect_inheritance() {
    let analysis = analyze(
        r#"
        effect resource Db;
        fn helper() writes(Db) { }
        fn wrapper() { helper(); }
        fn reader() reads(Db) { }
        fn main() {
            wrapper();
            reader();
        }
        "#,
    );

    let main_fc = get_function(&analysis, "main");
    assert_eq!(main_fc.total_statements, 2);
    // wrapper() transitively writes(Db), reader() reads(Db) -> conflict
    assert!(
        main_fc.parallel_groups.is_empty(),
        "Expected no parallel groups due to transitive effect conflict"
    );
}

// ── Parallel group reason descriptions ─────────────────────────

#[test]
fn test_reason_pure_computations() {
    let analysis = analyze(
        r#"
        fn a() -> i32 { 1 }
        fn b() -> i32 { 2 }
        fn main() {
            let x = a();
            let y = b();
        }
        "#,
    );

    let main_fc = get_function(&analysis, "main");
    assert!(!main_fc.parallel_groups.is_empty());
    assert_eq!(main_fc.parallel_groups[0].reason, "pure computations");
}

#[test]
fn test_reason_concurrent_reads() {
    let analysis = analyze(
        r#"
        effect resource Db;
        fn read1() reads(Db) { }
        fn read2() reads(Db) { }
        fn main() {
            read1();
            read2();
        }
        "#,
    );

    let main_fc = get_function(&analysis, "main");
    assert!(!main_fc.parallel_groups.is_empty());
    assert_eq!(
        main_fc.parallel_groups[0].reason,
        "concurrent reads on same resource"
    );
}

#[test]
fn test_reason_different_resources() {
    let analysis = analyze(
        r#"
        effect resource Db;
        effect resource Cache;
        fn read_db() reads(Db) { }
        fn read_cache() reads(Cache) { }
        fn main() {
            read_db();
            read_cache();
        }
        "#,
    );

    let main_fc = get_function(&analysis, "main");
    assert!(!main_fc.parallel_groups.is_empty());
    assert_eq!(
        main_fc.parallel_groups[0].reason,
        "independent reads on different resources"
    );
}

// ── Cost-model gate: zero-parallelism shapes are marked trivial ─

#[test]
fn test_cost_model_one_expensive_plus_lets_marked_trivial() {
    // One effectful stmt + N constant-init lets has zero structural
    // parallelism: one par branch holds all the work, the others
    // idle. Pre-fix the analyzer still emitted the group as
    // non-trivial and the codegen paid `karac_par_run` spawn cost
    // (~70μs/dispatch on macOS) for no speedup. Post-fix the
    // cost-model gate routes these through `is_trivial = true` so
    // codegen skips the par dispatch.
    let analysis = analyze(
        r#"
        effect resource R;
        fn worker() writes(R) {}
        fn main() {
            let mut x = 0i64;
            worker();
            let mut y = 0i64;
        }
        "#,
    );
    let main_fc = get_function(&analysis, "main");
    assert_eq!(main_fc.parallel_groups.len(), 1);
    let group = &main_fc.parallel_groups[0];
    assert_eq!(group.statement_indices.len(), 3);
    assert!(
        group.is_trivial,
        "Group with 1 effectful stmt + 2 constant-init lets should be \
         marked trivial (zero structural parallelism)"
    );
}

#[test]
fn test_cost_model_call_plus_literal_collection_marked_trivial() {
    // Auto-par ordered-output corpus probe (2026-06-14): once output
    // suppression was removed, test-harness mains shaped
    // `report(prev); let next = ["..", ".."]; report(next);` paired each
    // substantial `report` call with the adjacent literal-array `let`. Pre-fix
    // that `let` counted as non-constant work, so the group had two
    // "non-constant" stmts → non-trivial → fanned out a par-block overlapping
    // real work with a ~zero-work literal build (no speedup, pure spawn cost +
    // binary growth — measured ~0.5ms/run on kata 722). Recognizing
    // source-bounded collection literals as constant-init drops
    // `non_constant_count` to 1 → the group is trivial → codegen inlines it.
    let analysis = analyze(
        r#"
        effect resource R;
        fn report(v: Vec[String]) writes(R) {}
        fn main() {
            let a: Vec[String] = ["x", "y"];
            report(a);
            let b: Vec[String] = ["z"];
            report(b);
        }
        "#,
    );
    let main_fc = get_function(&analysis, "main");
    let g = main_fc
        .parallel_groups
        .iter()
        .find(|g| g.statement_indices.len() >= 2)
        .expect("expected the report(a) + literal `let b` pair to group");
    assert!(
        g.is_trivial,
        "a substantial call paired only with a source-bounded literal collection \
         init has no balanced parallelism and must be trivial; got {:?}",
        main_fc.parallel_groups
    );
}

#[test]
fn test_cost_model_hot_loop_plus_let_init_marked_trivial() {
    // Distillation of the kata 6 zigzag failure mode: the analyzer
    // groups a hot push loop with a let-init for the next phase's
    // counter (`let mut r2 = 0i64`). Both stmts are independent
    // (no shared vars, no effect conflict on the loop's
    // `allocates(Heap)`), so the analyzer correctly identifies
    // them as a parallelizable pair — but parallelizing yields
    // no speedup since one branch sits on the let-of-literal and
    // the other does all the work. Drove the kata 6 bench's 2.5×
    // gap vs sequential codegen (2026-05-17).
    let analysis = analyze(
        r#"
        fn main() {
            let mut v: Vec[i64] = Vec.new();
            let mut i = 0i64;
            while i < 10 {
                v.push(i);
                i = i + 1;
            }
            let mut r2 = 0i64;
            let last = v.len() - 1;
            println(v[last]);
            println(r2);
        }
        "#,
    );
    let main_fc = get_function(&analysis, "main");
    // Every group that survives the cost-model gate must have
    // 2+ stmts that do real work. The kata 6 shape only produces
    // "one-big + N-cheap" groups, so all must be trivial.
    for group in &main_fc.parallel_groups {
        assert!(
            group.is_trivial,
            "Group {:?} (reason: {:?}) should be marked trivial — \
             only one of its stmts does meaningful work",
            group.statement_indices, group.reason
        );
    }
}

#[test]
fn test_cost_model_byte_lit_let_init_counts_as_constant() {
    // Distillation of the kata-91 bench failure mode. Pre-fix
    // `stmt_is_constant_init` listed Integer/Float/CharLit/StringLit/
    // CStringLit/Bool/Identifier but not `ByteLit`, so `let zero: u8 =
    // b'0';` was mis-classified as non-constant. With one Vec.new()
    // sibling and one byte-lit let in a 4-stmt prologue,
    // `non_constant_count` reached 2 (instead of 1), flipping
    // `is_trivial` to false and emitting a 4-branch par-block for
    // four ~3-instruction stores. Downstream, the captured `let l =
    // 80i64` became a `karac_par_run`-opaque load, breaking LLVM's
    // const-prop into `k % l` (which then lowered as `sdiv` instead
    // of `umulh`-reciprocal). Cost on kata-91's 10M-iter hot loop:
    // ~47 ms — the full gap to rustc-O at the time.
    let analysis = analyze(
        r#"
        fn main() {
            let l: i64 = 80i64;
            let zero: u8 = b'0';
            let mut buf: Vec[u8] = Vec.new();
            let mut j: i64 = 0i64;
            println(l);
            println(zero);
            println(buf.len());
            println(j);
        }
        "#,
    );
    let main_fc = get_function(&analysis, "main");
    // Either no non-trivial group was formed, or the only group is
    // trivial. Both shapes mean codegen skips the par dispatch.
    for group in &main_fc.parallel_groups {
        assert!(
            group.is_trivial,
            "Group {:?} (reason: {:?}) should be marked trivial — \
             three of its four stmts are literal-init lets (Integer / \
             ByteLit / Integer); only Vec.new() does meaningful work",
            group.statement_indices, group.reason
        );
    }
}

#[test]
fn test_cost_model_empty_vec_new_counts_as_constant() {
    // Distillation of the kata #405 `to_hex` auto-par blowup (B-2026-07-09-14).
    // `Vec.new()` / `String.new()` materialize an empty `{ptr, len, cap}`
    // descriptor with NO heap allocation (the first push/grow allocates), so
    // they do ~zero work — but pre-fix `expr_is_constant_init` recognized only
    // literals, not empty constructors. In a hot function's prologue
    // `let hexd = ".."; let n = num & M; let buf = Vec.new();` that left
    // `non_constant_count` = 2 (the `& M` bitand AND `Vec.new()`), clearing the
    // `<= 1` trivial gate and fanning out a ~70μs-spawn par group PER CALL.
    // Over the millions of `to_hex` calls in the #405 render loop, that blew the
    // default (auto-par) build up to 40–66× its `KARAC_AUTO_PAR=0` seq-lane
    // instructions, non-deterministically (503B vs 9.5B). Unlike the byte-lit
    // sibling above, this shape is DISCRIMINATING: without recognizing the empty
    // `Vec.new()` as constant-init the group has two non-constant stmts and is
    // NOT trivial, so this test fails pre-fix and passes post-fix.
    let analysis = analyze(
        r#"
        fn to_hex(num: i64) -> i64 {
            let hexd: String = "0123456789abcdef";
            let mut n = num & 0xffffffffi64;
            let mut buf: Vec[i64] = Vec.new();
            buf.push(n + hexd.len());
            buf.len()
        }
        "#,
    );
    let fc = get_function(&analysis, "to_hex");
    for group in &fc.parallel_groups {
        assert!(
            group.is_trivial,
            "Group {:?} (reason: {:?}) should be trivial — its only non-constant \
             stmt is the `num & M` bitand; the String literal and the empty \
             `Vec.new()` are both zero-work constant-init, so fanning out buys \
             only spawn overhead, never speedup (B-2026-07-09-14)",
            group.statement_indices, group.reason
        );
    }
}

#[test]
fn test_cost_model_multi_string_lit_counts_as_constant() {
    // Sibling to the ByteLit test. `MultiStringLit` (Kāra's multi-line
    // string literal form) is textual data with no runtime work, parity
    // with `StringLit`. Pre-fix the heuristic listed `StringLit` but
    // not `MultiStringLit`, so a let-of-multi-string would also drive
    // the non-constant count past 1 and emit a wasteful par-group.
    // Without a concrete bench failure for this one, this test is the
    // forward-compat guard: the analyzer should treat both string-
    // literal forms identically for the cost-model gate.
    let analysis = analyze(
        r#"
        fn main() {
            let banner: String = """hello
                                    world""";
            let mut buf: Vec[u8] = Vec.new();
            let n: i64 = 0i64;
            println(banner);
            println(buf.len());
            println(n);
        }
        "#,
    );
    let main_fc = get_function(&analysis, "main");
    for group in &main_fc.parallel_groups {
        assert!(
            group.is_trivial,
            "Group {:?} (reason: {:?}) should be marked trivial — \
             MultiStringLit is a literal, only Vec.new() does real work",
            group.statement_indices, group.reason
        );
    }
}

#[test]
fn test_cost_model_two_effectful_calls_still_parallelized() {
    // Control case: two effectful calls on independent resources
    // have real structural parallelism. The cost-model gate must
    // NOT mark them trivial — codegen should still dispatch the
    // par_run so both calls run concurrently.
    let analysis = analyze(
        r#"
        effect resource R1;
        effect resource R2;
        fn w1() writes(R1) {}
        fn w2() writes(R2) {}
        fn main() {
            w1();
            w2();
        }
        "#,
    );
    let main_fc = get_function(&analysis, "main");
    assert_eq!(main_fc.parallel_groups.len(), 1);
    let group = &main_fc.parallel_groups[0];
    assert_eq!(group.statement_indices.len(), 2);
    assert!(
        !group.is_trivial,
        "Two effectful calls on independent resources have real \
         structural parallelism — must not be marked trivial"
    );
}

#[test]
fn test_cost_model_let_with_effectful_rhs_counts_as_work() {
    // Control case: a `let x = call()` stmt where the RHS is a
    // function call (not a literal/identifier) counts as work.
    // Two such lets in a group have real parallelism and must
    // not be filtered out.
    let analysis = analyze(
        r#"
        effect resource R1;
        effect resource R2;
        fn compute1() -> i64 writes(R1) { 0 }
        fn compute2() -> i64 writes(R2) { 0 }
        fn main() {
            let x = compute1();
            let y = compute2();
        }
        "#,
    );
    let main_fc = get_function(&analysis, "main");
    assert_eq!(main_fc.parallel_groups.len(), 1);
    let group = &main_fc.parallel_groups[0];
    assert_eq!(group.statement_indices.len(), 2);
    assert!(
        !group.is_trivial,
        "Two let-bindings whose RHS calls effectful functions have \
         work-bearing RHS expressions — must not be marked trivial"
    );
}

// ── CLI query test ─────────────────────────────────────────────

#[test]
fn test_cli_query_concurrency() {
    use std::io::Write;
    use std::process::Command;

    // Write a temp .kara file
    let dir = std::env::temp_dir();
    let file_path = dir.join("test_concurrency_query.kara");
    {
        let mut f = std::fs::File::create(&file_path).unwrap();
        writeln!(
            f,
            r#"
fn a() -> i32 {{ 1 }}
fn b() -> i32 {{ 2 }}
fn main() {{
    let x = a();
    let y = b();
}}
"#
        )
        .unwrap();
    }

    // Use the binary that cargo already built for this test run
    let karac_bin = env!("CARGO_BIN_EXE_karac");

    // Run karac query concurrency
    let output = Command::new(karac_bin)
        .args([
            "query",
            "concurrency",
            &format!("{}.main", file_path.display()),
        ])
        .output()
        .expect("failed to run karac");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "karac query concurrency failed: {}{}",
        stdout,
        String::from_utf8_lossy(&output.stderr)
    );

    // Verify JSON output contains expected fields
    assert!(
        stdout.contains("\"function\":\"main\""),
        "stdout: {}",
        stdout
    );
    assert!(
        stdout.contains("\"total_statements\":2"),
        "stdout: {}",
        stdout
    );
    assert!(stdout.contains("\"parallel_groups\""), "stdout: {}", stdout);

    // Clean up
    let _ = std::fs::remove_file(&file_path);
}

#[test]
fn test_cli_query_concurrency_whole_program() {
    use std::io::Write;
    use std::process::Command;

    // A bare `<file>.kara` target (no trailing `.function`) emits the
    // whole-program concurrency report: every analyzed function's
    // parallel bands, keyed identically to `query effects <file>` so a
    // consumer can overlay the bands on the effect graph.
    let dir = std::env::temp_dir();
    let file_path = dir.join("test_concurrency_query_whole.kara");
    {
        let mut f = std::fs::File::create(&file_path).unwrap();
        writeln!(
            f,
            r#"
fn a() -> i32 {{ 1 }}
fn b() -> i32 {{ 2 }}
fn main() {{
    let x = a();
    let y = b();
}}
"#
        )
        .unwrap();
    }

    let karac_bin = env!("CARGO_BIN_EXE_karac");
    let output = Command::new(karac_bin)
        .args(["query", "concurrency", file_path.to_str().unwrap()])
        .output()
        .expect("failed to run karac");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "karac query concurrency (whole-program) failed: {}{}",
        stdout,
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(stdout.contains("\"scope\":"), "stdout: {}", stdout);
    assert!(stdout.contains("\"functions\":["), "stdout: {}", stdout);
    assert!(
        stdout.contains("\"function\":\"main\""),
        "stdout: {}",
        stdout
    );
    // `main`'s two independent pure lets form a parallel band.
    assert!(
        stdout.contains("\"parallel_groups\":[{"),
        "main should carry a parallel group; stdout: {}",
        stdout
    );
    assert!(stdout.contains("\"line\":"), "stdout: {}", stdout);
    // Independent statements parallelize, so they carry no serialization points.
    assert!(
        stdout.contains("\"serialization_points\":[]"),
        "independent fns should have empty serialization_points; stdout: {}",
        stdout
    );

    let _ = std::fs::remove_file(&file_path);
}

#[test]
fn test_cli_query_concurrency_serialization_points_attribute_blocking_callee() {
    use std::io::Write;
    use std::process::Command;

    // The whole-program concurrency report names, for every pair of
    // statements that can't parallelize, the cause + the callee whose
    // effect is to blame. Here `twice` calls `emit` (writes Log) twice;
    // the two calls conflict write/write on Log, and the serialization
    // point must attribute that to `emit`. Inverting `blocking_callees`
    // across functions is the Cartographer "which callers does this
    // function block" view.
    let dir = std::env::temp_dir();
    let file_path = dir.join("test_concurrency_serialization_attr.kara");
    {
        let mut f = std::fs::File::create(&file_path).unwrap();
        writeln!(
            f,
            r#"
pub trait Sink {{ fn put(mut ref self); }}
pub effect resource Log: Sink;
pub struct Mem {{}}
impl Mem {{ pub fn new() -> Mem {{ Mem {{}} }} }}
impl Sink for Mem {{ fn put(mut ref self) {{ }} }}

pub fn emit() with writes(Log) {{ Log.put(); }}

pub fn twice() with writes(Log) {{
    emit();
    emit();
}}
"#
        )
        .unwrap();
    }

    let karac_bin = env!("CARGO_BIN_EXE_karac");
    let output = Command::new(karac_bin)
        .args(["query", "concurrency", file_path.to_str().unwrap()])
        .output()
        .expect("failed to run karac");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // `twice`'s two calls to `emit` conflict write/write on Log; the
    // serialization point names the cause, the resource, and `emit` as
    // the blocking callee.
    assert!(
        stdout.contains("\"function\":\"twice\""),
        "stdout: {stdout}"
    );
    assert!(
        stdout.contains("\"reason\":\"writes(Log) conflicts with writes(Log)\""),
        "serialization reason names verb+resource; stdout: {stdout}",
    );
    assert!(
        stdout.contains("\"resource\":\"Log\",\"blocking_callees\":[\"emit\"]"),
        "serialization point attributes the conflict to callee `emit` on Log; stdout: {stdout}",
    );
    // The structured `serialized_by` tag lets a consumer branch on the
    // conflict class without parsing the prose `reason`. Here it is a
    // resource-level effect conflict, both verbs `writes` on `Log`.
    assert!(
        stdout.contains(
            "\"serialized_by\":{\"category\":\"effect_conflict\",\"resource\":\"Log\",\"verbs\":[\"writes\",\"writes\"]}"
        ),
        "serialization point carries a structured effect-conflict tag; stdout: {stdout}",
    );

    let _ = std::fs::remove_file(&file_path);
}

#[test]
fn test_cli_query_concurrency_emits_statement_spans() {
    use std::io::Write;
    use std::process::Command;

    // The concurrency query reports grouped/serialized statements by
    // *ordinal* index. Those ordinals are the stable key, but they are not
    // self-locating — an IDE/LSP layer (or a human report) needs source
    // positions. `statement_spans[i]` locates statement ordinal `i`, making
    // the machine surface self-locating without re-deriving positions by
    // counting statements. See phase-5-diagnostics.md "Self-locating query
    // output".
    let dir = std::env::temp_dir();
    let file_path = dir.join("test_concurrency_statement_spans.kara");
    {
        let mut f = std::fs::File::create(&file_path).unwrap();
        // The two `let`s land on source lines 4 and 5 (the leading `\n` from
        // the raw string is line 1, `fn a` line 2, `fn main {` line 3).
        writeln!(
            f,
            r#"
fn a() -> i32 {{ 1 }}
fn main() {{
    let x = a();
    let y = a();
}}
"#
        )
        .unwrap();
    }

    let karac_bin = env!("CARGO_BIN_EXE_karac");
    let output = Command::new(karac_bin)
        .args([
            "query",
            "concurrency",
            &format!("{}.main", file_path.display()),
        ])
        .output()
        .expect("failed to run karac");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // One span per statement, in ordinal order, carrying file + line/col.
    // The path lands in JSON, so backslashes are escaped on the wire (Windows
    // temp paths like `C:\Users\...` serialize as `C:\\Users\\...`). Escape the
    // expected path the same way — a no-op on `/`-separated platforms.
    let fname = file_path.display().to_string().replace('\\', "\\\\");
    let expected = format!(
        "\"statement_spans\":[{{\"file\":\"{fname}\",\"line\":4,\"column\":5}},\
         {{\"file\":\"{fname}\",\"line\":5,\"column\":5}}]"
    );
    assert!(
        stdout.contains(&expected),
        "statement_spans must locate each ordinal by source line/col; want {expected}; stdout: {stdout}",
    );

    let _ = std::fs::remove_file(&file_path);
}

#[test]
fn test_cli_query_concurrency_serialized_by_data_dependency_raw() {
    use std::io::Write;
    use std::process::Command;

    // Two statements that serialize purely on a *value dependency* (the
    // second reads a binding the first writes) must be distinguishable on
    // the wire from an effect conflict — they imply a different fix (break
    // the dataflow vs split the resource). The structured `serialized_by`
    // tag carries `category: data_dependency`, the direction (`raw` — a
    // read-after-write / true dependency), and the binding. See
    // phase-5-diagnostics.md "Per-statement exclusion-reason attribution".
    let dir = std::env::temp_dir();
    let file_path = dir.join("test_concurrency_serialized_by_raw.kara");
    {
        let mut f = std::fs::File::create(&file_path).unwrap();
        writeln!(
            f,
            r#"
fn a() -> i32 {{ 1 }}
fn chain() {{
    let x = a();
    let y = x + 1;
}}
"#
        )
        .unwrap();
    }

    let karac_bin = env!("CARGO_BIN_EXE_karac");
    let output = Command::new(karac_bin)
        .args([
            "query",
            "concurrency",
            &format!("{}.chain", file_path.display()),
        ])
        .output()
        .expect("failed to run karac");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // Prose reason is preserved; the structured tag distinguishes the axis.
    assert!(
        stdout.contains("\"reason\":\"data dependency on `x`\""),
        "human reason preserved; stdout: {stdout}",
    );
    assert!(
        stdout.contains(
            "\"serialized_by\":{\"category\":\"data_dependency\",\"kind\":\"raw\",\"vars\":[\"x\"]}"
        ),
        "data-dependency serialization carries a structured raw tag on `x`; stdout: {stdout}",
    );

    let _ = std::fs::remove_file(&file_path);
}

// ── Reorder-opportunity advisory ───────────────────────────────

#[test]
fn test_reorder_opportunity_flags_interleaved_pipelines() {
    // Two independent dataflow chains written interleaved — the flagship
    // shape the contiguous-only grouper handles suboptimally. Edges: 0→1
    // (`a`) and 2→3 (`c`); 0∥2, 1∥3 are independent. Greedy grouping forms
    // only the straddling pair {1,2}; the ideal is {0,2} then {1,3}. The
    // advisory must surface both reorders deterministically so the agent
    // can act on a sound signal instead of guessing. See
    // phase-5-diagnostics.md "Contiguous-greedy grouping is suboptimal".
    let analysis = analyze(
        r#"
        fn f1() -> i32 { 1 }
        fn g1(x: i32) -> i32 { x + 1 }
        fn f2() -> i32 { 2 }
        fn g2(x: i32) -> i32 { x + 1 }
        fn pipeline() {
            let a = f1();
            let b = g1(a);
            let c = f2();
            let d = g2(c);
        }
        "#,
    );

    let fc = get_function(&analysis, "pipeline");
    let mut ops: Vec<(Vec<usize>, usize)> = fc
        .reorder_opportunities
        .iter()
        .map(|o| (o.statement_indices.clone(), o.movable_statement))
        .collect();
    ops.sort();
    // {0,2} (move the grouped stmt 2 left to the serial stmt 0) and {1,3}
    // (move the grouped stmt 1 right to the serial stmt 3). Together they
    // describe the optimal {0,2},{1,3} regrouping. The straddling-only pair
    // {1,2} is NOT a reorder opportunity (it is already the emitted group),
    // and {0,3} is absent (no single legal slide spans the gap).
    assert_eq!(
        ops,
        vec![(vec![0, 2], 2), (vec![1, 3], 1)],
        "interleaved pipelines must flag exactly the two reorderable pairs; got {:?}",
        fc.reorder_opportunities,
    );
}

#[test]
fn test_reorder_opportunity_silent_on_pure_dependency_chain() {
    // A straight dependency chain has no latent parallelism, so no reorder
    // can expose any — the advisory must stay silent (no false positives).
    let analysis = analyze(
        r#"
        fn f() -> i32 { 1 }
        fn chain() {
            let a = f();
            let b = a + 1;
            let c = b + 1;
            let d = c + 1;
        }
        "#,
    );

    let fc = get_function(&analysis, "chain");
    assert!(
        fc.reorder_opportunities.is_empty(),
        "a pure dependency chain offers no reorder opportunities; got {:?}",
        fc.reorder_opportunities,
    );
}

#[test]
fn test_reorder_opportunity_excludes_console_output_mover() {
    // A `println` is resourceless, so the dependency graph treats it as
    // independent — but relocating it reorders observable output, which the
    // effect surface would not catch. The advisory must therefore never
    // propose a console statement as a mover. Here stmt 0 (`println`) is
    // independent of stmt 2 with an otherwise-legal left/right slide; the
    // console-output guard suppresses the opportunity. (The same shape with
    // a non-console stmt 0 *does* flag — see
    // `test_reorder_opportunity_flags_serial_statement_into_group`.)
    let analysis = analyze(
        r#"
        fn f() -> i32 { 1 }
        fn demo() {
            println("x");
            let a = f();
            let b = a + 1;
        }
        "#,
    );

    let fc = get_function(&analysis, "demo");
    assert!(
        fc.reorder_opportunities.is_empty(),
        "a console-output statement must not be proposed as a reorder mover; got {:?}",
        fc.reorder_opportunities,
    );
}

#[test]
fn test_reorder_opportunity_flags_serial_statement_into_group() {
    // The console-exclusion contrast: identical shape to the test above but
    // with a plain (non-console) statement 0. Stmt 0 is serial and
    // independent of stmt 2; moving it adjacent exposes a parallel pair, so
    // the advisory flags {0,2} with stmt 0 as the mover.
    let analysis = analyze(
        r#"
        fn f() -> i32 { 1 }
        fn demo() {
            let z = f();
            let a = f();
            let b = a + 1;
        }
        "#,
    );

    let fc = get_function(&analysis, "demo");
    assert_eq!(
        fc.reorder_opportunities.len(),
        1,
        "expected one opportunity"
    );
    let op = &fc.reorder_opportunities[0];
    assert_eq!(op.statement_indices, vec![0, 2]);
    assert_eq!(op.movable_statement, 0);
}

#[test]
fn test_cli_query_concurrency_emits_reorder_opportunities() {
    use std::io::Write;
    use std::process::Command;

    // End-to-end: the `reorder_opportunities` array surfaces on the machine
    // query surface, self-locating via the same ordinal→`statement_spans`
    // join as the other concurrency fields.
    let dir = std::env::temp_dir();
    let file_path = dir.join("test_concurrency_reorder_opportunities.kara");
    {
        let mut f = std::fs::File::create(&file_path).unwrap();
        writeln!(
            f,
            r#"
fn f1() -> i32 {{ 1 }}
fn g1(x: i32) -> i32 {{ x + 1 }}
fn f2() -> i32 {{ 2 }}
fn g2(x: i32) -> i32 {{ x + 1 }}
fn pipeline() {{
    let a = f1();
    let b = g1(a);
    let c = f2();
    let d = g2(c);
}}
"#
        )
        .unwrap();
    }

    let karac_bin = env!("CARGO_BIN_EXE_karac");
    let output = Command::new(karac_bin)
        .args([
            "query",
            "concurrency",
            &format!("{}.pipeline", file_path.display()),
        ])
        .output()
        .expect("failed to run karac");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    assert!(
        stdout.contains("\"statements\":[0,2],\"movable_statement\":2"),
        "reorder opportunity {{0,2}} (move stmt 2) must surface; stdout: {stdout}",
    );
    assert!(
        stdout.contains("\"statements\":[1,3],\"movable_statement\":1"),
        "reorder opportunity {{1,3}} (move stmt 1) must surface; stdout: {stdout}",
    );

    let _ = std::fs::remove_file(&file_path);
}

// ── Assign-target dependencies ─────────────────────────────────

#[test]
fn test_assign_creates_dependency() {
    let analysis = analyze(
        r#"
        fn main() {
            let mut x = 1;
            x = 2;
            let y = x;
        }
        "#,
    );

    let main_fc = get_function(&analysis, "main");
    assert_eq!(main_fc.total_statements, 3);
    // All three are linked: x defined, x assigned, x read
    assert!(main_fc.parallel_groups.is_empty());
}

// ── Task granularity heuristics ──────────────────────────────────

#[test]
fn test_pure_group_is_trivial() {
    let analysis = analyze(
        r#"
        fn a() -> i32 { 1 }
        fn b() -> i32 { 2 }
        fn main() {
            let x = a();
            let y = b();
        }
        "#,
    );
    let main_fc = get_function(&analysis, "main");
    assert_eq!(main_fc.parallel_groups.len(), 1);
    assert!(
        main_fc.parallel_groups[0].is_trivial,
        "pure computation group should be marked trivial"
    );
}

#[test]
fn test_effectful_group_not_trivial() {
    let analysis = analyze(
        r#"
        resource Db;
        fn read_a() -> i32 with reads(Db) { 1 }
        fn read_b() -> i32 with reads(Db) { 2 }
        fn main() with reads(Db) {
            let x = read_a();
            let y = read_b();
        }
        "#,
    );
    let main_fc = get_function(&analysis, "main");
    assert_eq!(main_fc.parallel_groups.len(), 1);
    assert!(
        !main_fc.parallel_groups[0].is_trivial,
        "effectful group should NOT be trivial"
    );
}

// ── Polymorphic-effect calls serialize conservatively ─────────

#[test]
fn test_polymorphic_calls_serialize() {
    // Two calls to a `with _` function have unknown runtime effects and must
    // not be parallelized — they might conflict on shared resources that the
    // inferred-effect set cannot see.
    let analysis = analyze(
        r#"
        effect resource Db;
        pub fn poly() with _ { }
        fn main() {
            let x = poly();
            let y = poly();
        }
        "#,
    );
    let main_fc = get_function(&analysis, "main");
    assert_eq!(main_fc.total_statements, 2);
    assert!(
        main_fc.parallel_groups.is_empty(),
        "two polymorphic calls must not be parallelized, got {:?}",
        main_fc.parallel_groups
    );
}

#[test]
fn test_polymorphic_and_pure_can_parallelize() {
    // A polymorphic call and a pure computation can still parallelize — the
    // pure statement has no effects to be disturbed by the polymorphic one.
    let analysis = analyze(
        r#"
        effect resource Db;
        pub fn poly() with _ { }
        fn main() {
            let x = poly();
            let y = 1 + 2;
        }
        "#,
    );
    let main_fc = get_function(&analysis, "main");
    assert_eq!(main_fc.parallel_groups.len(), 1);
    assert_eq!(main_fc.parallel_groups[0].statement_indices.len(), 2);
}

#[test]
fn test_polymorphic_group_not_trivial() {
    // Even when a group contains two parallelizable statements, if one of them
    // transitively calls a `with _` function, the group cannot be dispatched
    // as trivial — the runtime effects are unknown.
    let analysis = analyze(
        r#"
        effect resource Db;
        pub fn poly() with _ { }
        fn main() {
            let x = poly();
            let y = 1 + 2;
        }
        "#,
    );
    let main_fc = get_function(&analysis, "main");
    assert_eq!(main_fc.parallel_groups.len(), 1);
    assert!(
        !main_fc.parallel_groups[0].is_trivial,
        "group containing a polymorphic call must not be marked trivial"
    );
}

// ── Reduction recognition (auto-par slice 1, 2026-05-19) ───────
//
// Tests for the loop-reduction recognizer: each top-level `for` / `while` /
// `loop` whose body's only loop-carried write follows `acc = acc <op> expr`
// (or `acc op= expr`) for op ∈ {+, *, |, &, ^} is tagged with a
// `LoopReduction`. Induction-shape writes (`i = i + const_lit`, `i +=
// const_lit`) are folded alongside as loop-counter steps so explicit
// `while` loops match without the reduction being broken by the counter.

#[test]
fn test_reduction_recognized_for_add_while_loop() {
    // The kata-7 bench shape: `while k < K { sum = sum + ...; k = k + 1; }`.
    let analysis = analyze(
        r#"
        fn main() {
            let mut sum: i64 = 0i64;
            let mut k: i64 = 0i64;
            while k < 100i64 {
                sum = sum + k;
                k = k + 1i64;
            }
        }
        "#,
    );
    let main_fc = get_function(&analysis, "main");
    assert_eq!(
        main_fc.loop_reductions.len(),
        1,
        "expected one reduction, got {:?}",
        main_fc.loop_reductions
    );
    let r = &main_fc.loop_reductions[0];
    assert_eq!(r.accumulator, "sum");
    assert_eq!(r.op, ReductionOp::Add);
}

#[test]
fn test_reduction_recognized_for_compound_add() {
    // `total += x` shape parses to CompoundAssign — must also be recognized.
    let analysis = analyze(
        r#"
        fn main() {
            let mut total: i64 = 0i64;
            let mut k: i64 = 0i64;
            while k < 10i64 {
                total += k;
                k = k + 1i64;
            }
        }
        "#,
    );
    let main_fc = get_function(&analysis, "main");
    assert_eq!(main_fc.loop_reductions.len(), 1);
    assert_eq!(main_fc.loop_reductions[0].accumulator, "total");
    assert_eq!(main_fc.loop_reductions[0].op, ReductionOp::Add);
}

#[test]
fn test_reduction_recognized_for_mul_or_and_xor() {
    // Sweep the four other ops in one program — one loop each, four
    // recognized reductions, each tagged with its op.
    let analysis = analyze(
        r#"
        fn main() {
            let mut p: i64 = 1i64;
            let mut a: i64 = 0i64;
            while a < 5i64 {
                p = p * a;
                a = a + 1i64;
            }
            let mut o: i64 = 0i64;
            let mut b: i64 = 0i64;
            while b < 5i64 {
                o = o | b;
                b = b + 1i64;
            }
            let mut n: i64 = -1i64;
            let mut c: i64 = 0i64;
            while c < 5i64 {
                n = n & c;
                c = c + 1i64;
            }
            let mut x: i64 = 0i64;
            let mut d: i64 = 0i64;
            while d < 5i64 {
                x = x ^ d;
                d = d + 1i64;
            }
        }
        "#,
    );
    let main_fc = get_function(&analysis, "main");
    assert_eq!(main_fc.loop_reductions.len(), 4);
    let by_acc: std::collections::HashMap<_, _> = main_fc
        .loop_reductions
        .iter()
        .map(|r| (r.accumulator.clone(), r.op))
        .collect();
    assert_eq!(by_acc.get("p"), Some(&ReductionOp::Mul));
    assert_eq!(by_acc.get("o"), Some(&ReductionOp::BitOr));
    assert_eq!(by_acc.get("n"), Some(&ReductionOp::BitAnd));
    assert_eq!(by_acc.get("x"), Some(&ReductionOp::BitXor));
}

#[test]
fn test_reduction_commutative_rhs_acc_position() {
    // `sum = k + sum` — accumulator on the right. Allow-list ops are
    // commutative, so this shape is equally valid.
    let analysis = analyze(
        r#"
        fn main() {
            let mut sum: i64 = 0i64;
            let mut k: i64 = 0i64;
            while k < 100i64 {
                sum = k + sum;
                k = k + 1i64;
            }
        }
        "#,
    );
    let main_fc = get_function(&analysis, "main");
    assert_eq!(main_fc.loop_reductions.len(), 1);
    assert_eq!(main_fc.loop_reductions[0].accumulator, "sum");
}

#[test]
fn test_reduction_rejects_subtraction() {
    // `acc -= x` is NOT associative (a - b - c ≠ a - (b - c)) and not in
    // the allow-list. The classifier must reject the loop.
    let analysis = analyze(
        r#"
        fn main() {
            let mut acc: i64 = 0i64;
            let mut k: i64 = 0i64;
            while k < 10i64 {
                acc = acc - k;
                k = k + 1i64;
            }
        }
        "#,
    );
    let main_fc = get_function(&analysis, "main");
    assert!(
        main_fc.loop_reductions.is_empty(),
        "subtraction is not associative; should not be tagged as reduction"
    );
}

#[test]
fn test_reduction_rejects_division() {
    // Division is neither associative nor commutative.
    let analysis = analyze(
        r#"
        fn main() {
            let mut acc: i64 = 100i64;
            let mut k: i64 = 1i64;
            while k < 5i64 {
                acc /= k;
                k = k + 1i64;
            }
        }
        "#,
    );
    let main_fc = get_function(&analysis, "main");
    assert!(main_fc.loop_reductions.is_empty());
}

#[test]
fn test_reduction_rejects_multiple_distinct_accumulators() {
    // Two distinct accumulators in the same loop — slice 1 only handles
    // single-accumulator reductions, so the loop is rejected entirely.
    let analysis = analyze(
        r#"
        fn main() {
            let mut a: i64 = 0i64;
            let mut b: i64 = 1i64;
            let mut k: i64 = 0i64;
            while k < 10i64 {
                a = a + k;
                b = b * k;
                k = k + 1i64;
            }
        }
        "#,
    );
    let main_fc = get_function(&analysis, "main");
    assert!(
        main_fc.loop_reductions.is_empty(),
        "two-accumulator loop should not match slice-1 recognition"
    );
}

#[test]
fn test_reduction_recognized_for_nested_conditional_acc_update() {
    // An inner `if cond { acc = acc + k; }` was conservatively rejected
    // under slice 1. After the 2026-05-20 conditional-acc-update slice
    // landed (see [`conditional_acc_update_shape`] in src/concurrency.rs),
    // this shape is recognized: it's semantically equivalent to
    // `acc = acc + (if cond { k } else { 0 })`, the per-iter contribution
    // is order-independent, and `cond` here (`k > 5i64`) doesn't read the
    // accumulator. The "if { acc = ... } rejected" conservative-default
    // tests survive in the rejects-when-cond-reads-acc / rejects-with-
    // nonempty-else / rejects-with-extra-stmt-in-then siblings below.
    let analysis = analyze(
        r#"
        fn main() {
            let mut acc: i64 = 0i64;
            let mut k: i64 = 0i64;
            while k < 10i64 {
                if k > 5i64 {
                    acc = acc + k;
                }
                k = k + 1i64;
            }
        }
        "#,
    );
    let main_fc = get_function(&analysis, "main");
    assert_eq!(main_fc.loop_reductions.len(), 1);
    assert_eq!(main_fc.loop_reductions[0].accumulator, "acc");
    assert_eq!(main_fc.loop_reductions[0].op, ReductionOp::Add);
}

#[test]
fn test_reduction_recognized_for_loop() {
    // `for k in 0..K { acc = acc + ... }` — no explicit induction, the
    // for-binding is fresh per-iter. Body has a single loop-carried
    // write; recognized cleanly.
    let analysis = analyze(
        r#"
        fn main() {
            let mut acc: i64 = 0i64;
            for k in 0..100i64 {
                acc = acc + k;
            }
        }
        "#,
    );
    let main_fc = get_function(&analysis, "main");
    assert_eq!(main_fc.loop_reductions.len(), 1);
    assert_eq!(main_fc.loop_reductions[0].accumulator, "acc");
    assert_eq!(main_fc.loop_reductions[0].op, ReductionOp::Add);
}

#[test]
fn test_reduction_recognized_after_lowering() {
    // CLI surface regression check (slice 1, 2026-05-19): the same
    // shape as `test_reduction_recognized_for_add_while_loop` but run
    // through resolve + typecheck + lower before concurrency. The
    // lowering pass rewrites `sum + k` into a `Call(Path(["i64",
    // "add"]), [sum, k])` shape that the recognizer must also match.
    let analysis = analyze_lowered(
        r#"
        fn main() {
            let mut sum: i64 = 0i64;
            let mut k: i64 = 0i64;
            while k < 100i64 {
                sum = sum + k;
                k = k + 1i64;
            }
        }
        "#,
    );
    let main_fc = get_function(&analysis, "main");
    assert_eq!(
        main_fc.loop_reductions.len(),
        1,
        "post-lowering Call shape must be recognized, got {:?}",
        main_fc.loop_reductions
    );
    assert_eq!(main_fc.loop_reductions[0].accumulator, "sum");
    assert_eq!(main_fc.loop_reductions[0].op, ReductionOp::Add);
}

#[test]
fn test_no_reduction_when_loop_has_no_accumulator() {
    // A loop that only steps the counter — no accumulator, no reduction.
    let analysis = analyze(
        r#"
        fn main() {
            let mut k: i64 = 0i64;
            while k < 100i64 {
                k = k + 1i64;
            }
        }
        "#,
    );
    let main_fc = get_function(&analysis, "main");
    assert!(main_fc.loop_reductions.is_empty());
}

// ── Min/Max recognition (combined slice, 2026-05-20) ────────────────
// Both direct-call (`m = i64.min(m, x)`) and conditional-assign
// (`if x < m { m = x; }`) shapes are recognized as Min/Max reductions
// over a single accumulator. The kata-153 linear_scan bench is the
// validation workload — its `find_min`'s `if x < m { m = x; }` inner
// loop drives the conditional-assign branch.

#[test]
fn test_reduction_recognized_for_conditional_min() {
    // The kata-153 shape: `if x < m { m = x; }` inside an inner loop.
    let analysis = analyze(
        r#"
        fn main() {
            let mut m: i64 = 1000i64;
            let mut i: i64 = 0i64;
            while i < 100i64 {
                let x: i64 = i * 7i64;
                if x < m {
                    m = x;
                }
                i = i + 1i64;
            }
        }
        "#,
    );
    let main_fc = get_function(&analysis, "main");
    assert_eq!(
        main_fc.loop_reductions.len(),
        1,
        "expected one Min reduction, got {:?}",
        main_fc.loop_reductions
    );
    assert_eq!(main_fc.loop_reductions[0].accumulator, "m");
    assert_eq!(main_fc.loop_reductions[0].op, ReductionOp::Min);
}

#[test]
fn test_reduction_recognized_for_conditional_max() {
    // Mirror of the Min shape: `if x > m { m = x; }`.
    let analysis = analyze(
        r#"
        fn main() {
            let mut m: i64 = -1000i64;
            let mut i: i64 = 0i64;
            while i < 100i64 {
                let x: i64 = i * 7i64;
                if x > m {
                    m = x;
                }
                i = i + 1i64;
            }
        }
        "#,
    );
    let main_fc = get_function(&analysis, "main");
    assert_eq!(main_fc.loop_reductions.len(), 1);
    assert_eq!(main_fc.loop_reductions[0].accumulator, "m");
    assert_eq!(main_fc.loop_reductions[0].op, ReductionOp::Max);
}

#[test]
fn test_reduction_recognized_for_conditional_minmax_commutative() {
    // Commutative form: `if m > x { m = x; }` is Min, `if m < x { m = x; }` is Max.
    let analysis = analyze(
        r#"
        fn main() {
            let mut lo: i64 = 1000i64;
            let mut hi: i64 = -1000i64;
            let mut i: i64 = 0i64;
            while i < 100i64 {
                let x: i64 = i * 7i64;
                if lo > x {
                    lo = x;
                }
                i = i + 1i64;
            }
            let mut j: i64 = 0i64;
            while j < 100i64 {
                let y: i64 = j * 7i64;
                if hi < y {
                    hi = y;
                }
                j = j + 1i64;
            }
        }
        "#,
    );
    let main_fc = get_function(&analysis, "main");
    assert_eq!(main_fc.loop_reductions.len(), 2);
    let by_acc: std::collections::HashMap<_, _> = main_fc
        .loop_reductions
        .iter()
        .map(|r| (r.accumulator.clone(), r.op))
        .collect();
    assert_eq!(by_acc.get("lo"), Some(&ReductionOp::Min));
    assert_eq!(by_acc.get("hi"), Some(&ReductionOp::Max));
}

#[test]
fn test_reduction_rejects_conditional_with_else_branch() {
    // `if x < m { m = x; } else { m = x + 1; }` is not a clean Min step
    // — the else branch also writes the accumulator, recognition rejects.
    let analysis = analyze(
        r#"
        fn main() {
            let mut m: i64 = 1000i64;
            let mut i: i64 = 0i64;
            while i < 100i64 {
                let x: i64 = i * 7i64;
                if x < m {
                    m = x;
                } else {
                    m = x + 1i64;
                }
                i = i + 1i64;
            }
        }
        "#,
    );
    let main_fc = get_function(&analysis, "main");
    assert!(
        main_fc.loop_reductions.is_empty(),
        "if-with-else should not be Min-recognized, got {:?}",
        main_fc.loop_reductions
    );
}

#[test]
fn test_reduction_rejects_conditional_when_cond_unrelated_to_value() {
    // `if y < z { m = x; }` — cond compares y/z but assigns x to m.
    // Doesn't fit the `value < acc` Min shape; no recognition.
    let analysis = analyze(
        r#"
        fn main() {
            let mut m: i64 = 0i64;
            let mut i: i64 = 0i64;
            while i < 100i64 {
                let x: i64 = i * 7i64;
                let y: i64 = i * 11i64;
                let z: i64 = i * 13i64;
                if y < z {
                    m = x;
                }
                i = i + 1i64;
            }
        }
        "#,
    );
    let main_fc = get_function(&analysis, "main");
    assert!(main_fc.loop_reductions.is_empty());
}

#[test]
fn test_reduction_recognized_for_conditional_min_after_lowering() {
    // End-to-end through the lowering pipeline — the cond's `<` becomes
    // a `Call(Path(["i64", "lt"]), [a, b])` shape; recognition handles both.
    let analysis = analyze_lowered(
        r#"
        fn main() {
            let mut m: i64 = 1000i64;
            let mut i: i64 = 0i64;
            while i < 100i64 {
                let x: i64 = i * 7i64;
                if x < m {
                    m = x;
                }
                i = i + 1i64;
            }
        }
        "#,
    );
    let main_fc = get_function(&analysis, "main");
    assert_eq!(
        main_fc.loop_reductions.len(),
        1,
        "post-lowering Call(lt) shape must be recognized, got {:?}",
        main_fc.loop_reductions
    );
    assert_eq!(main_fc.loop_reductions[0].accumulator, "m");
    assert_eq!(main_fc.loop_reductions[0].op, ReductionOp::Min);
}

#[test]
fn test_reduction_recognized_for_conditional_min_for_range_after_lowering() {
    // kata-153 find_min shape: `for i in 1..n { let x = nums[i]; if x < m { m = x; } }`.
    // Same conditional-assign pattern as the while form, but inside an
    // impl-level free function with a Slice parameter and the for-range
    // loop construct. Validates the analyzer recognizes the kata's
    // actual shape end-to-end through the CLI pipeline.
    let analysis = analyze_lowered(
        r#"
        fn find_min(nums: Slice[i64]) -> i64 {
            let n = nums.len();
            let mut m = nums[0];
            for i in 1..n {
                let x = nums[i];
                if x < m {
                    m = x;
                }
            }
            m
        }
        fn main() { }
        "#,
    );
    let find_min_fc = get_function(&analysis, "find_min");
    assert_eq!(
        find_min_fc.loop_reductions.len(),
        1,
        "expected one Min reduction on `m`, got {:?}",
        find_min_fc.loop_reductions
    );
    assert_eq!(find_min_fc.loop_reductions[0].accumulator, "m");
    assert_eq!(find_min_fc.loop_reductions[0].op, ReductionOp::Min);
}

// ── Conditional accumulator-update recognition (slice: conditional-acc-update, 2026-05-20) ──
// `if cond { acc = acc + delta; }` (and `if cond { acc OP= delta; }`)
// is semantically equivalent to `acc = acc + (if cond { delta } else { 0 })`
// for any associative+commutative op with a known identity, so the
// pattern is a reduction step. Surfaced by kata-65 bench (count of
// truthy `is_number(...)` results); pre-fix the analyzer reported
// `<no parallelization opportunities detected>` and the workload ran
// single-threaded (User ≈ wall, no parallelism).

#[test]
fn test_reduction_recognized_for_conditional_acc_update_assign() {
    // The kata-65 bench shape: `if cond { sum = sum + 1i64; }`.
    let analysis = analyze(
        r#"
        fn main() {
            let mut sum: i64 = 0i64;
            let mut k: i64 = 0i64;
            while k < 100i64 {
                let cond: bool = k > 5i64;
                if cond { sum = sum + 1i64; }
                k = k + 1i64;
            }
        }
        "#,
    );
    let main_fc = get_function(&analysis, "main");
    assert_eq!(
        main_fc.loop_reductions.len(),
        1,
        "expected one reduction, got {:?}",
        main_fc.loop_reductions
    );
    let r = &main_fc.loop_reductions[0];
    assert_eq!(r.accumulator, "sum");
    assert_eq!(r.op, ReductionOp::Add);
}

#[test]
fn test_reduction_recognized_for_conditional_acc_update_compound() {
    // CompoundAssign form: `if cond { sum += 1i64; }`. The unconditional
    // `acc += const_lit` is reserved as the loop-counter shape; under
    // a conditional wrap the matcher recognizes it as the "count of
    // truthy iterations" reduction.
    let analysis = analyze(
        r#"
        fn main() {
            let mut sum: i64 = 0i64;
            let mut k: i64 = 0i64;
            while k < 100i64 {
                let cond: bool = k > 5i64;
                if cond { sum += 1i64; }
                k = k + 1i64;
            }
        }
        "#,
    );
    let main_fc = get_function(&analysis, "main");
    assert_eq!(main_fc.loop_reductions.len(), 1);
    assert_eq!(main_fc.loop_reductions[0].accumulator, "sum");
    assert_eq!(main_fc.loop_reductions[0].op, ReductionOp::Add);
}

#[test]
fn test_reduction_recognized_for_conditional_acc_update_with_empty_else() {
    // `if cond { acc = acc + 1 } else { }` — explicit empty else parses
    // as an If with else_branch = Some(Block{stmts:[], final_expr:None})
    // and is semantically identical to the no-else form.
    let analysis = analyze(
        r#"
        fn main() {
            let mut sum: i64 = 0i64;
            let mut k: i64 = 0i64;
            while k < 100i64 {
                let cond: bool = k > 5i64;
                if cond { sum = sum + 1i64; } else { }
                k = k + 1i64;
            }
        }
        "#,
    );
    let main_fc = get_function(&analysis, "main");
    assert_eq!(main_fc.loop_reductions.len(), 1);
    assert_eq!(main_fc.loop_reductions[0].accumulator, "sum");
    assert_eq!(main_fc.loop_reductions[0].op, ReductionOp::Add);
}

#[test]
fn test_reduction_recognized_for_conditional_acc_update_mul() {
    // Non-Add op variant — `if cond { p = p * 2i64; }` over a Mul accumulator.
    let analysis = analyze(
        r#"
        fn main() {
            let mut p: i64 = 1i64;
            let mut k: i64 = 0i64;
            while k < 10i64 {
                let cond: bool = k > 2i64;
                if cond { p = p * 2i64; }
                k = k + 1i64;
            }
        }
        "#,
    );
    let main_fc = get_function(&analysis, "main");
    assert_eq!(main_fc.loop_reductions.len(), 1);
    assert_eq!(main_fc.loop_reductions[0].accumulator, "p");
    assert_eq!(main_fc.loop_reductions[0].op, ReductionOp::Mul);
}

#[test]
fn test_reduction_rejects_conditional_acc_update_when_cond_reads_acc() {
    // `if sum > 100 { sum = sum + 1 }` — the condition reads the
    // accumulator, so the per-iter decision depends on accumulator
    // state from earlier iterations. Not order-independent; reject.
    let analysis = analyze(
        r#"
        fn main() {
            let mut sum: i64 = 0i64;
            let mut k: i64 = 0i64;
            while k < 100i64 {
                if sum > 100i64 { sum = sum + 1i64; }
                k = k + 1i64;
            }
        }
        "#,
    );
    let main_fc = get_function(&analysis, "main");
    assert!(
        main_fc.loop_reductions.is_empty(),
        "cond reading acc must not be recognized, got {:?}",
        main_fc.loop_reductions
    );
}

#[test]
fn test_reduction_recognized_for_recursive_self_call_in_body() {
    // A backtracking counter: `if legal { total = total + count(...deeper...) }`
    // is a valid `+` reduction whose delta recurses into the enclosing function.
    // It IS recognized and lowered — the runtime bounds fan-out depth
    // (`KARAC_PAR_MAX_FORK_DEPTH`, default 1), so only the outermost level
    // parallelizes and deeper levels run sequentially, which is safe (no runaway
    // nesting) AND useful (the search parallelizes at its top-level branches).
    // This previously declined under B-2026-07-03-14's conservative guard; the
    // runtime fork-depth cap replaced it (shallow-depth-parallel-reduction).
    let analysis = analyze(
        r#"
        fn count(n: i64, row: i64) -> i64 {
            if row == n { return 1i64; }
            let mut total: i64 = 0i64;
            let mut c: i64 = 0i64;
            while c < n {
                if c >= 0i64 { total = total + count(n, row + 1i64); }
                c = c + 1i64;
            }
            total
        }
        fn main() { let x: i64 = count(4i64, 0i64); }
        "#,
    );
    let count_fc = get_function(&analysis, "count");
    assert_eq!(
        count_fc.loop_reductions.len(),
        1,
        "a recursive-delta reduction should be recognized (runtime caps fan-out \
         depth), got {:?}",
        count_fc.loop_reductions
    );
    assert_eq!(count_fc.loop_reductions[0].accumulator, "total");
    assert_eq!(count_fc.loop_reductions[0].op, ReductionOp::Add);
}

#[test]
fn test_reduction_still_recognized_with_nonrecursive_call_in_body() {
    // A reduction whose delta calls a plain non-recursive function is a
    // legitimate parallel reduction — recognized and lowered as normal.
    let analysis = analyze(
        r#"
        fn work(x: i64) -> i64 { x * 2i64 }
        fn sum(n: i64) -> i64 {
            let mut total: i64 = 0i64;
            let mut c: i64 = 0i64;
            while c < n {
                total = total + work(c);
                c = c + 1i64;
            }
            total
        }
        fn main() { let x: i64 = sum(10i64); }
        "#,
    );
    let sum_fc = get_function(&analysis, "sum");
    assert_eq!(sum_fc.loop_reductions.len(), 1);
    assert_eq!(sum_fc.loop_reductions[0].accumulator, "total");
    assert_eq!(sum_fc.loop_reductions[0].op, ReductionOp::Add);
}

#[test]
fn test_reduction_recognized_for_two_arm_acc_update_same_op() {
    // 2026-05-20 slice extension: `if cond { acc = acc + a } else { acc
    // = acc + b }` is semantically equivalent to `acc = acc + (if cond
    // { a } else { b })` and recognizable as a `+` reduction. The
    // matcher accepts when both arms target the same accumulator with
    // the same op; mixed accumulators or mixed ops are rejected
    // (see siblings below).
    let analysis = analyze(
        r#"
        fn main() {
            let mut sum: i64 = 0i64;
            let mut k: i64 = 0i64;
            while k < 100i64 {
                let cond: bool = k > 5i64;
                if cond { sum = sum + 1i64; } else { sum = sum + 2i64; }
                k = k + 1i64;
            }
        }
        "#,
    );
    let main_fc = get_function(&analysis, "main");
    assert_eq!(
        main_fc.loop_reductions.len(),
        1,
        "two-arm same-acc same-op must be recognized, got {:?}",
        main_fc.loop_reductions
    );
    let r = &main_fc.loop_reductions[0];
    assert_eq!(r.accumulator, "sum");
    assert_eq!(r.op, ReductionOp::Add);
}

#[test]
fn test_reduction_recognized_for_two_arm_acc_update_compound() {
    // CompoundAssign in both arms with different deltas — same shape as
    // the canonical "hit/miss tally" workload (e.g., `if hit { right +=
    // 1 } else { right += 0 }` — though if both arms had identical
    // deltas the unconditional form would be simpler).
    let analysis = analyze(
        r#"
        fn main() {
            let mut tally: i64 = 0i64;
            let mut k: i64 = 0i64;
            while k < 100i64 {
                let hit: bool = (k % 3i64) == 0i64;
                if hit { tally += 3i64; } else { tally += 1i64; }
                k = k + 1i64;
            }
        }
        "#,
    );
    let main_fc = get_function(&analysis, "main");
    assert_eq!(main_fc.loop_reductions.len(), 1);
    assert_eq!(main_fc.loop_reductions[0].accumulator, "tally");
    assert_eq!(main_fc.loop_reductions[0].op, ReductionOp::Add);
}

#[test]
fn test_reduction_recognized_for_two_arm_acc_update_with_variable_deltas() {
    // Both arms have non-literal delta expressions. The
    // `reduction_binary_shape` machinery still requires acc to appear
    // exactly once on each RHS, so non-acc operands are acc-free by
    // construction — both arms recognize as `+`-step contributions.
    let analysis = analyze(
        r#"
        fn main() {
            let mut sum: i64 = 0i64;
            let mut k: i64 = 0i64;
            while k < 100i64 {
                let even: bool = (k % 2i64) == 0i64;
                if even { sum = sum + (k * 2i64); } else { sum = sum + k; }
                k = k + 1i64;
            }
        }
        "#,
    );
    let main_fc = get_function(&analysis, "main");
    assert_eq!(main_fc.loop_reductions.len(), 1);
    assert_eq!(main_fc.loop_reductions[0].accumulator, "sum");
    assert_eq!(main_fc.loop_reductions[0].op, ReductionOp::Add);
}

#[test]
fn test_reduction_rejects_two_arm_acc_update_with_different_accumulators() {
    // `if cond { a = a + 1 } else { b = b + 1 }` — different
    // accumulators per branch. Each arm IS a valid 1-arm shape on its
    // own, but the if-block as a whole writes two distinct names and
    // doesn't fit the single-accumulator fan-out model.
    let analysis = analyze(
        r#"
        fn main() {
            let mut a: i64 = 0i64;
            let mut b: i64 = 0i64;
            let mut k: i64 = 0i64;
            while k < 100i64 {
                let cond: bool = k > 5i64;
                if cond { a = a + 1i64; } else { b = b + 1i64; }
                k = k + 1i64;
            }
        }
        "#,
    );
    let main_fc = get_function(&analysis, "main");
    assert!(
        main_fc.loop_reductions.is_empty(),
        "mixed-accumulator two-arm must not be recognized, got {:?}",
        main_fc.loop_reductions
    );
}

#[test]
fn test_reduction_rejects_two_arm_acc_update_with_mixed_ops() {
    // `if cond { acc = acc + 1 } else { acc = acc * 2 }` — same acc,
    // but different ops. The fan-out + combine model commutes only
    // within a single op, so the contribution-as-`+` and contribution-
    // as-`*` forms can't be unified into one reduction.
    let analysis = analyze(
        r#"
        fn main() {
            let mut acc: i64 = 1i64;
            let mut k: i64 = 0i64;
            while k < 100i64 {
                let cond: bool = k > 5i64;
                if cond { acc = acc + 1i64; } else { acc = acc * 2i64; }
                k = k + 1i64;
            }
        }
        "#,
    );
    let main_fc = get_function(&analysis, "main");
    assert!(
        main_fc.loop_reductions.is_empty(),
        "mixed-op two-arm must not be recognized, got {:?}",
        main_fc.loop_reductions
    );
}

#[test]
fn test_reduction_rejects_conditional_acc_update_with_extra_stmt_in_then() {
    // Then-block has two stmts — not the single-stmt shape the
    // recognizer accepts. The trailing `let local = ...` doesn't
    // touch the accumulator but the shape constraint still rejects.
    let analysis = analyze(
        r#"
        fn main() {
            let mut sum: i64 = 0i64;
            let mut k: i64 = 0i64;
            while k < 100i64 {
                let cond: bool = k > 5i64;
                if cond {
                    sum = sum + 1i64;
                    let _local: i64 = k * 2i64;
                }
                k = k + 1i64;
            }
        }
        "#,
    );
    let main_fc = get_function(&analysis, "main");
    assert!(
        main_fc.loop_reductions.is_empty(),
        "multi-stmt then-block must not be recognized, got {:?}",
        main_fc.loop_reductions
    );
}

// ── Collect-style reduction recognition (slice: par-unordered Phase 2, 2026-05-20) ──
// `#[par_unordered] while ... { ...acc.push(x)... }` — the analyzer
// recognizes `acc.push(x)` (bare) and `if cond { acc.push(x); }`
// (conditional) shapes as `ReductionOp::Collect` only when the
// enclosing loop carries the `#[par_unordered]` attribute. Without the
// opt-in, the same shape falls through to "no parallelization
// opportunities detected" because per-worker partial-Vec concat
// produces worker-order output, not iteration-order — a semantic
// surprise the user must opt into explicitly.

#[test]
fn test_reduction_recognized_for_bare_push_when_par_unordered() {
    let analysis = analyze(
        r#"
        fn main() {
            let mut results: Vec[i64] = Vec.new();
            let mut k: i64 = 0i64;
            #[par_unordered]
            while k < 100i64 {
                results.push(k);
                k = k + 1i64;
            }
        }
        "#,
    );
    let main_fc = get_function(&analysis, "main");
    assert_eq!(
        main_fc.loop_reductions.len(),
        1,
        "bare push with par_unordered must be recognized, got {:?}",
        main_fc.loop_reductions
    );
    let r = &main_fc.loop_reductions[0];
    assert_eq!(r.accumulator, "results");
    assert_eq!(r.op, ReductionOp::Collect);
}

#[test]
fn test_reduction_recognized_for_conditional_push_when_par_unordered() {
    let analysis = analyze(
        r#"
        fn main() {
            let mut results: Vec[i64] = Vec.new();
            let mut k: i64 = 0i64;
            #[par_unordered]
            while k < 100i64 {
                if k > 5i64 {
                    results.push(k);
                }
                k = k + 1i64;
            }
        }
        "#,
    );
    let main_fc = get_function(&analysis, "main");
    assert_eq!(
        main_fc.loop_reductions.len(),
        1,
        "conditional push with par_unordered must be recognized, got {:?}",
        main_fc.loop_reductions
    );
    let r = &main_fc.loop_reductions[0];
    assert_eq!(r.accumulator, "results");
    assert_eq!(r.op, ReductionOp::Collect);
}

#[test]
fn test_reduction_recognized_for_conditional_push_with_empty_else() {
    // Empty else passes through the same matcher path as the no-else
    // case (mirror of conditional_acc_update_shape's empty-else
    // acceptance).
    let analysis = analyze(
        r#"
        fn main() {
            let mut results: Vec[i64] = Vec.new();
            let mut k: i64 = 0i64;
            #[par_unordered]
            while k < 100i64 {
                if k > 5i64 { results.push(k); } else { }
                k = k + 1i64;
            }
        }
        "#,
    );
    let main_fc = get_function(&analysis, "main");
    assert_eq!(main_fc.loop_reductions.len(), 1);
    assert_eq!(main_fc.loop_reductions[0].accumulator, "results");
    assert_eq!(main_fc.loop_reductions[0].op, ReductionOp::Collect);
}

#[test]
fn test_reduction_rejects_bare_push_without_par_unordered() {
    // Same source as the bare-push-recognized test above but with the
    // attribute removed — the same `results.push(k)` body that
    // *would* be recognized under opt-in must fall through to "no
    // reduction" without the attribute. This is the key safety
    // property: collect-style auto-par requires explicit user opt-in.
    let analysis = analyze(
        r#"
        fn main() {
            let mut results: Vec[i64] = Vec.new();
            let mut k: i64 = 0i64;
            while k < 100i64 {
                results.push(k);
                k = k + 1i64;
            }
        }
        "#,
    );
    let main_fc = get_function(&analysis, "main");
    assert!(
        main_fc.loop_reductions.is_empty(),
        "bare push without par_unordered must not be recognized, got {:?}",
        main_fc.loop_reductions
    );
}

#[test]
fn test_reduction_rejects_conditional_push_without_par_unordered() {
    let analysis = analyze(
        r#"
        fn main() {
            let mut results: Vec[i64] = Vec.new();
            let mut k: i64 = 0i64;
            while k < 100i64 {
                if k > 5i64 { results.push(k); }
                k = k + 1i64;
            }
        }
        "#,
    );
    let main_fc = get_function(&analysis, "main");
    assert!(
        main_fc.loop_reductions.is_empty(),
        "conditional push without par_unordered must not be recognized, got {:?}",
        main_fc.loop_reductions
    );
}

#[test]
fn test_reduction_rejects_push_on_let_introduced_acc() {
    // `let mut local: Vec[i64] = Vec.new();` *inside* the loop body
    // creates a body-local accumulator — pushing into it isn't loop-
    // carried; same shape that's already rejected for scalar
    // reductions (see `test_reduction_recognized_for_two_arm_acc_update_same_op`'s
    // `let_introduced` guard). Even with the par_unordered opt-in,
    // body-local accumulators don't fan out across workers.
    let analysis = analyze(
        r#"
        fn main() {
            let mut k: i64 = 0i64;
            #[par_unordered]
            while k < 100i64 {
                let mut local: Vec[i64] = Vec.new();
                local.push(k);
                k = k + 1i64;
            }
        }
        "#,
    );
    let main_fc = get_function(&analysis, "main");
    assert!(
        main_fc.loop_reductions.is_empty(),
        "body-local push must not be recognized, got {:?}",
        main_fc.loop_reductions
    );
}

#[test]
fn test_reduction_rejects_mixed_push_and_scalar_accumulator() {
    // Two distinct accumulators per iter — one Collect, one Add. The
    // single-accumulator contract is preserved: the matcher returns
    // None when reductions of different kinds appear in the same loop.
    // The scalar accumulator uses `total = total + k` (sum of loop
    // indices) rather than `total += 1i64`, because the `acc + const_lit`
    // form is special-cased upstream as the loop-counter (induction-step)
    // shape and is *ignored* by the matcher rather than treated as a
    // competing reduction — that case doesn't actually exercise the
    // mixed-acc rejection path.
    let analysis = analyze(
        r#"
        fn main() {
            let mut results: Vec[i64] = Vec.new();
            let mut total: i64 = 0i64;
            let mut k: i64 = 0i64;
            #[par_unordered]
            while k < 100i64 {
                results.push(k);
                total = total + k;
                k = k + 1i64;
            }
        }
        "#,
    );
    let main_fc = get_function(&analysis, "main");
    assert!(
        main_fc.loop_reductions.is_empty(),
        "mixed Collect+Add must not be recognized as a single reduction, got {:?}",
        main_fc.loop_reductions
    );
}

// ── Nested-binary chain recognition (slice: chain recognizer, 2026-05-20) ──
// `sum = sum + a + b` parses left-associatively as
// `Binary(+, Binary(+, sum, a), b)`. Today's `reduction_binary_shape`
// only checks the outer Binary's direct operands for the acc identifier
// — neither child of the outer matches, so the chain falls through to
// "rejected." Slice extends recognition: count acc occurrences across
// the same-op chain; recognize iff acc appears exactly once.

#[test]
fn test_reduction_recognized_for_chain_of_two() {
    // `sum = sum + a + b` — kata-5-outer shape.
    let analysis = analyze(
        r#"
        fn main() {
            let mut sum: i64 = 0i64;
            let mut k: i64 = 0i64;
            while k < 100i64 {
                let a: i64 = k * 2i64;
                let b: i64 = k * 3i64;
                sum = sum + a + b;
                k = k + 1i64;
            }
        }
        "#,
    );
    let main_fc = get_function(&analysis, "main");
    assert_eq!(
        main_fc.loop_reductions.len(),
        1,
        "expected one Add reduction on `sum`, got {:?}",
        main_fc.loop_reductions
    );
    assert_eq!(main_fc.loop_reductions[0].accumulator, "sum");
    assert_eq!(main_fc.loop_reductions[0].op, ReductionOp::Add);
}

#[test]
fn test_reduction_recognized_for_chain_of_four() {
    // Longer chain `sum + a + b + c + d` — pins that the chain walker
    // recursion handles arbitrary depth, not just 2 levels.
    let analysis = analyze(
        r#"
        fn main() {
            let mut sum: i64 = 0i64;
            let mut k: i64 = 0i64;
            while k < 100i64 {
                let a: i64 = k * 2i64;
                let b: i64 = k * 3i64;
                let c: i64 = k * 5i64;
                let d: i64 = k * 7i64;
                sum = sum + a + b + c + d;
                k = k + 1i64;
            }
        }
        "#,
    );
    let main_fc = get_function(&analysis, "main");
    assert_eq!(main_fc.loop_reductions.len(), 1);
    assert_eq!(main_fc.loop_reductions[0].accumulator, "sum");
    assert_eq!(main_fc.loop_reductions[0].op, ReductionOp::Add);
}

#[test]
fn test_reduction_recognized_for_chain_with_acc_in_middle() {
    // `a + sum + b` parses as `Binary(+, Binary(+, a, sum), b)`.
    // Commutativity makes this equivalent to `sum + a + b`; the chain
    // walker counts occurrences without caring about position.
    let analysis = analyze(
        r#"
        fn main() {
            let mut sum: i64 = 0i64;
            let mut k: i64 = 0i64;
            while k < 100i64 {
                let a: i64 = k * 2i64;
                let b: i64 = k * 3i64;
                sum = a + sum + b;
                k = k + 1i64;
            }
        }
        "#,
    );
    let main_fc = get_function(&analysis, "main");
    assert_eq!(main_fc.loop_reductions.len(), 1);
    assert_eq!(main_fc.loop_reductions[0].accumulator, "sum");
    assert_eq!(main_fc.loop_reductions[0].op, ReductionOp::Add);
}

#[test]
fn test_reduction_recognized_chain_does_not_recurse_into_mixed_ops() {
    // `sum + a * b` — outer is `+`, inner is `*`. The walker stops at
    // the inner `*` (not same op as target `+`), treating `a * b` as a
    // single leaf. `sum + (a * b)` matches the direct `acc + expr`
    // shape, so the reduction IS recognized — and correctly so, since
    // `(a * b)` is just an opaque value combined with acc once per iter.
    let analysis = analyze(
        r#"
        fn main() {
            let mut sum: i64 = 0i64;
            let mut k: i64 = 0i64;
            while k < 100i64 {
                let a: i64 = k * 2i64;
                let b: i64 = k * 3i64;
                sum = sum + a * b;
                k = k + 1i64;
            }
        }
        "#,
    );
    let main_fc = get_function(&analysis, "main");
    assert_eq!(
        main_fc.loop_reductions.len(),
        1,
        "outer `acc + (a*b)` should match direct shape"
    );
    assert_eq!(main_fc.loop_reductions[0].op, ReductionOp::Add);
}

#[test]
fn test_reduction_rejects_chain_with_acc_twice() {
    // `sum + sum + a` — acc appears twice in the chain. NOT a valid
    // reduction step: per-iter combine `sum := sum + sum + a` is
    // `2*sum + a`, but partials initialized to identity (0) wouldn't
    // compose correctly under the standard fan-out + Add-combine model.
    // Chain walker counts acc=2; recognizer rejects.
    let analysis = analyze(
        r#"
        fn main() {
            let mut sum: i64 = 0i64;
            let mut k: i64 = 0i64;
            while k < 100i64 {
                let a: i64 = k;
                sum = sum + sum + a;
                k = k + 1i64;
            }
        }
        "#,
    );
    let main_fc = get_function(&analysis, "main");
    assert!(
        main_fc.loop_reductions.is_empty(),
        "double-acc chain should reject; got {:?}",
        main_fc.loop_reductions
    );
}

#[test]
fn test_reduction_recognized_for_chain_after_lowering() {
    // End-to-end through the lowering pipeline — the chain becomes
    // `Call(Path([T, "add"]), [Call(Path([T, "add"]), [sum, a]), b])`.
    // Chain walker recurses through the Call shape too.
    let analysis = analyze_lowered(
        r#"
        fn main() {
            let mut sum: i64 = 0i64;
            let mut k: i64 = 0i64;
            while k < 100i64 {
                let a: i64 = k * 2i64;
                let b: i64 = k * 3i64;
                sum = sum + a + b;
                k = k + 1i64;
            }
        }
        "#,
    );
    let main_fc = get_function(&analysis, "main");
    assert_eq!(
        main_fc.loop_reductions.len(),
        1,
        "post-lowering chain Call shape must be recognized, got {:?}",
        main_fc.loop_reductions
    );
    assert_eq!(main_fc.loop_reductions[0].accumulator, "sum");
    assert_eq!(main_fc.loop_reductions[0].op, ReductionOp::Add);
}

// ── mut-ref call arguments create write dependencies ────────────
//
// Kata-22 regression pair (2026-06-06). `collect_expr_inner_writes`
// had a MethodCall receiver-mutation arm but NO Call arm, so a free-
// function call passing `mut out` recorded zero writes: the analyzer
// saw `add_one(mut out)` and `println(out.len())` as two reads of
// `out`, grouped them, and `compile_par_groups` captured `out` into
// the par env BY VALUE — the callee's Vec-header writeback (len/cap/
// data after push-grow) landed in the env copy and the caller's
// binding stayed a stale empty Vec. Both detection paths are pinned:
// the call-site `mut` marker, and the callee's declared `mut ref`
// param mode (unmarked forwarding).

#[test]
fn test_mut_marked_call_arg_serializes() {
    let analysis = analyze(
        r#"
        fn add_one(out: mut ref Vec[i64]) {
            out.push(7);
        }
        fn main() {
            let mut out: Vec[i64] = Vec.new();
            add_one(mut out);
            println(out.len());
        }
        "#,
    );
    let main_fc = get_function(&analysis, "main");
    assert!(
        main_fc.parallel_groups.is_empty(),
        "mut-marked call arg must serialize against the subsequent read, got {:?}",
        main_fc.parallel_groups
    );
}

#[test]
fn test_mut_ref_forwarding_call_serializes() {
    // No call-site marker — `out` is already `mut ref` in scope, so the
    // forwarding form is unmarked (design.md Feature 4 Part 1½). The
    // write is derived from the CALLEE's declared param mode instead.
    let analysis = analyze(
        r#"
        fn add_one(out: mut ref Vec[i64]) {
            out.push(7);
        }
        fn helper(out: mut ref Vec[i64]) {
            add_one(out);
            println(out.len());
        }
        "#,
    );
    let helper_fc = get_function(&analysis, "helper");
    assert!(
        helper_fc.parallel_groups.is_empty(),
        "unmarked mut-ref forwarding must serialize against the subsequent read, got {:?}",
        helper_fc.parallel_groups
    );
}

// ── Sparse conflict-graph scale guards ─────────────────────────
// The conflict graph is built from inverted indices, not a dense O(n²)
// adjacency matrix (phase-5-diagnostics.md: a 49K-statement function used
// to allocate ~2.4 GB of bools + an all-pairs scan). These two tests pin
// the two extremes of that build at a size that would be pathological for
// the old dense path: an all-independent body (zero edges — the sparse win)
// and a resource-round-robin body (dense edges, exercised via the
// resource inverted index). Both must analyze quickly AND stay correct.

#[test]
fn test_scale_all_independent_form_one_group() {
    // 4000 statements, each writing its OWN distinct resource → no two
    // conflict → the contiguous grouper folds them into a single parallel
    // group. With the inverted index every resource list has length 1, so
    // zero candidate pairs are generated (the sparse best case).
    const N: usize = 4000;
    let mut src = String::new();
    for i in 0..N {
        src.push_str(&format!("effect resource R{i};\n"));
        src.push_str(&format!("fn touch{i}() writes(R{i}) {{}}\n"));
    }
    src.push_str("fn main() {\n");
    for i in 0..N {
        src.push_str(&format!("    touch{i}();\n"));
    }
    src.push_str("}\n");

    let analysis = analyze(&src);
    let main_fc = get_function(&analysis, "main");
    assert_eq!(main_fc.total_statements, N);
    assert!(
        main_fc.serialization_points.is_empty(),
        "all-independent body must have no serialization points, got {}",
        main_fc.serialization_points.len()
    );
    assert_eq!(
        main_fc.parallel_groups.len(),
        1,
        "all-independent statements must fold into one parallel group"
    );
    assert_eq!(main_fc.parallel_groups[0].statement_indices.len(), N);
}

#[test]
fn test_scale_round_robin_resources_group_in_runs() {
    // 2048 statements writing 8 resources round-robin. Statements 0..7 touch
    // distinct resources (independent → one group), but statement 8 (R0)
    // write-conflicts with statement 0 (R0), so the contiguous grouper breaks
    // there: the body splits into runs of 8. Exercises real edges built via
    // the resource inverted index at scale.
    const GROUPS: usize = 256;
    const N: usize = GROUPS * 8;
    let mut src = String::new();
    for r in 0..8 {
        src.push_str(&format!("effect resource R{r};\n"));
        src.push_str(&format!("fn touch{r}() writes(R{r}) {{}}\n"));
    }
    src.push_str("fn main() {\n");
    for i in 0..N {
        src.push_str(&format!("    touch{}();\n", i % 8));
    }
    src.push_str("}\n");

    let analysis = analyze(&src);
    let main_fc = get_function(&analysis, "main");
    assert_eq!(main_fc.total_statements, N);
    assert_eq!(
        main_fc.parallel_groups.len(),
        GROUPS,
        "round-robin-over-8 must break into runs of 8"
    );
    for (g, group) in main_fc.parallel_groups.iter().enumerate() {
        assert_eq!(
            group.statement_indices,
            (g * 8..g * 8 + 8).collect::<Vec<_>>(),
            "group {g} should be the contiguous run [{}..{}]",
            g * 8,
            g * 8 + 8
        );
    }
    // Every write-write pair on the same resource within reach of the grouper
    // is a serialization point; at minimum the straddling conflicts exist and
    // are emitted in ascending (later, earlier) order.
    assert!(
        !main_fc.serialization_points.is_empty(),
        "round-robin body must report serialization points"
    );
    let idxs: Vec<&Vec<usize>> = main_fc
        .serialization_points
        .iter()
        .map(|sp| &sp.statement_indices)
        .collect();
    for w in idxs.windows(2) {
        assert!(
            (w[0][1], w[0][0]) <= (w[1][1], w[1][0]),
            "serialization points must stay in (later, earlier) ascending order"
        );
        assert!(w[0][0] < w[0][1], "each pair is stored low-index first");
    }
}
