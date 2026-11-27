# core-semantics §10 and §11: sentence map with lane verdicts

One row per normative sentence of `docs/core-semantics.md` §10 (panics and
errors) and §11 (concurrency), numbered as in `corpus/CORE_SENTENCE_MAP.md`
(cs.730 to cs.930; headings, list numerals and `Pin:` lines left out). Each
sentence that can be run has a cell in this directory, run by
`conformance/cases.py` on every lane:

```bash
CONF_LANES=interp,jit,build,asan python3 conformance/cases.py conformance/core_c8_c9
CONF_LANES=tsan python3 conformance/cases.py conformance/core_c8_c9   # task cells; 3 runs each, KARAC_THREADS=4
```

`jit` is `karac run` (through `karac_jit_runner`), `interp` is `karac run --interp`,
`build` a `karac build` binary, `asan`/`tsan` the sanitizer builds. A `✓` means the
cell's `.out`/`.err`/`.exit` matched (a `.refused` cell matched its diagnostic).
Rows with no cell are covered by the existing pins `CORE_SENTENCE_MAP.md` names, or
make no runnable claim (rationale, history, scheduling).

The directory holds 68 cells. The commit that added it (285a9875fc) says 71
in its message; that count is wrong.

## Findings

- **A. §11.8 cs.843 vs the effect checker.** The sentence says `send`, `recv`, a
  join and a `Mutex` lock "carry the `suspends` effect"; the checker infers
  `blocks` for them (only `sleep` carries both), and `docs/library/concurrency.md`
  declares `blocks`. So cs.851's promise is unwritable: a public
  `fn take(rx) -> Option[i64] with suspends receives(rx) { rx.recv() }` is refused
  E0400 "performs blocks" (`t_suspends_alone_admits_recv`). Resolved for the spec
  (review ruling): B-2026-11-27-10 moved those calls, the socket waits and the
  HTTP client's request to `suspends`, and the cell now runs.
- **B. A `Never`-typed block in arithmetic.** `let a = { return 7; }; a + 1` is
  refused E0208 "type 'Never' does not implement trait Add" (met writing the
  §11.7 cells). Fixed under B-2026-11-27-11: a `Never` operand coerces.
- **C. §11.1 cs.769 was stale** ("v1 has no automatic parallelisation of
  statements"); review rewrote it at 59758d2a86.

## Map

| id | § | sentence | cell(s) | lanes |
|---|---|---|---|---|
| cs.731 | §10.1 | - `panic(...)`; | `p_panic_call` | interp ✓ jit ✓ build ✓ asan ✓ |
| cs.732 | §10.1 | - a failed bounds check, including an index read `m[k]` of a key a `Map` does not hold (kind `index_out_of_bounds`); | `p_bounds`, `p_map_missing_key` | interp ✓ jit ✓ build ✓ asan ✓ |
| cs.733 | §10.1 | - integer overflow in checked arithmetic, which is the default; | `p_overflow` | interp ✓ jit ✓ build ✓ asan ✓ |
| cs.734 | §10.1 | - division by zero; | `p_div_zero` | interp ✓ jit ✓ build ✓ asan ✓ |
| cs.735 | §10.1 | - `unwrap` / `expect` on `None` or `Err`; | `p_unwrap_none`, `p_expect_err` | interp ✓ jit ✓ build ✓ asan ✓ |
| cs.736 | §10.1 | - a shared borrow-flag conflict (§6.2); | — | existing pin (CORE_SENTENCE_MAP.md) |
| cs.737 | §10.1 | - a failed assertion; | `p_assert` | interp ✓ jit ✓ build ✓ asan ✓ |
| cs.738 | §10.1 | - allocation failure; | `p_alloc_failure` | interp ✓ jit ✓ build ✓ asan ✓ |
| cs.739 | §10.1 | - a channel `recv` that can never complete because no other task can run (deadlock, on a single-threaded target). | `p_deadlock_recv` | interp ✓ jit ✓ build ✓ asan ✓ |
| cs.740 | §10.1 | Stack exhaustion is not a panic: it aborts the process with no panic record and no panic handler. | — | not a runnable claim |
| cs.741 | §10.1 | Whether a given recursion exhausts the stack is not specified, since an optimizer may remove frames (review-thread ruling 2026-11-16, B-2… | — | not a runnable claim |
| cs.742 | §10.1 | Because the process aborts, output still buffered in `stdout` is lost; a program that must keep its output flushes before deep recursion … | — | not a runnable claim |
| cs.745 | §10.2 | It writes the message, the source location and the error-return trace to stderr. | `p_panic_call`, `p_bounds` | interp ✓ jit ✓ build ✓ asan ✓ |
| cs.747 | §10.2 | It runs the program's custom panic handler (`#[panic_handler]`), if there is one. | `p_handler_runs_then_101` | interp ✓ jit ✓ build ✓ asan ✓ |
| cs.748 | §10.2 | A panic inside the handler ends the process at once. | `p_panic_in_handler_ends_at_once` | interp ✓ jit ✓ build ✓ asan ✓ |
| cs.750 | §10.2 | It ends the process with exit code **101**. | `p_panic_call`, `p_handler_runs_then_101` | interp ✓ jit ✓ build ✓ asan ✓ |
| cs.751 | §10.2 | **No** `Drop` body, `defer` or `errdefer` runs, and every other task ends with the process. | `p_no_drop_defer_errdefer`, `p_other_tasks_end` | interp ✓ jit ✓ build ✓ asan ✓ tsan ✓ |
| cs.752 | §10.2 | `catch_panic`, `panic = "unwind"` and `extern "C-unwind"` do not exist. | `p_catch_panic_absent`, `p_c_unwind_absent` | interp ✓ jit ✓ build ✓ asan ✓ |
| cs.754 | §10.2 | `panic_runs_no_drops`. | — | existing pin (CORE_SENTENCE_MAP.md) |
| cs.755 | §10.2 | (The note that the interpreter ran the `defer` first is deleted: no lane does; review-thread ruling 2026-11-16.) | — | existing pin (CORE_SENTENCE_MAP.md) |
| cs.756 | §10.3 | **10.3 Errors are values.** `Result` with `?` returns through the normal path, so drops, `defer`s and `errdefer`s run (§7.6–§7.7). | `e_question_runs_drops` | interp ✓ jit ✓ build ✓ asan ✓ |
| cs.757 | §10.3 | A `main` that returns `Err(e)` is an ordinary error exit: | `e_main_err_exits_1` | interp ✓ jit ✓ build ✓ asan ✓ |
| cs.758 | §10.3 | `main`'s drops, `defer`s and `errdefer`s run first, then `e` is written to stderr and the process exits with code **1**. | `e_main_err_exits_1` | interp ✓ jit ✓ build ✓ asan ✓ |
| cs.759 | §10.3 | Code 101 stays reserved for a panic, so a script can tell the two apart. | `e_main_err_exits_1` | interp ✓ jit ✓ build ✓ asan ✓ |
| cs.760 | §10.4 | **10.4 Tests.** `karac test` runs each test in its own process, so a panicking test fails alone. | — | existing pin (CORE_SENTENCE_MAP.md) |
| cs.763 | §11.1 | - `par { e1, e2, … }` runs its comma-separated branch expressions concurrently; its value is the tuple of their values. | `t_par_tuple_and_block_branch` | interp ✓ jit ✓ build ✓ asan ✓ tsan ✓ |
| cs.764 | §11.1 | A branch may be a block. | `t_par_tuple_and_block_branch` | interp ✓ jit ✓ build ✓ asan ✓ tsan ✓ |
| cs.765 | §11.1 | - `par for x in it { body }` runs one branch per element; its value is the `Vec` of the bodies' values, in iteration order. | `t_par_for_vec_in_order` | interp ✓ jit ✓ build ✓ asan ✓ tsan ✓ |
| cs.766 | §11.1 | It may take a limit on the number of branches in flight (`design.md`). | — | existing pin (CORE_SENTENCE_MAP.md) |
| cs.767 | §11.1 | - `TaskGroup` runs tasks that join when the group drops (§9.5). | `t_taskgroup_joins_on_drop` | interp ✓ jit ✓ build ✓ asan ✓ tsan ✓ |
| cs.768 | §11.1 | - There is no free `spawn`. | `t_no_free_spawn` | interp ✓ jit ✓ build ✓ asan ✓ tsan ✓ |
| cs.769 | §11.1 | - v1 has no automatic parallelisation of statements; it returns in M4 under §11.5's rule (redesign amendment, 2026-10-07). | — | was stale; rewritten at 59758d2a86 (old text shown) |
| cs.770 | §11.1 | - **Progress between tasks.** "Concurrently" means interleaved, not necessarily in parallel. | `t_progress_at_suspension_one_worker` | interp ✓ jit ✓ build ✓ asan ✓ tsan ✓ |
| cs.771 | §11.1 | A task is guaranteed to let its siblings run only at a suspension point: a channel `send` or `recv`, a `Mutex` lock that waits, a join, I… | `t_progress_at_suspension_one_worker` | interp ✓ jit ✓ build ✓ asan ✓ tsan ✓ |
| cs.772 | §11.1 | A target may also run tasks on separate workers, and native does by default, one per core (`KARAC_THREADS=1` runs one; wasm has one); no … | — | not a runnable claim |
| cs.773 | §11.1 | So a loop that waits for a sibling without suspending, such as `while not flag.load() {}`, is not guaranteed to terminate on any target. | — | not a runnable claim |
| cs.774 | §11.1 | Wait on a channel or a join instead. | — | not a runnable claim |
| cs.775 | §11.1 | This is the same rule `wasm_wasi` has always had (design.md §16), now stated for every target (review-thread ruling on B-2026-10-21-69). | — | not a runnable claim |
| cs.776 | §11.1 | - **No order between tasks.** Nothing orders two `TaskGroup` tasks' effects except a join, a channel message or a lock: a task may start … | `t_tasks_print_unordered` | interp ✓ jit ✓ build ✓ asan ✓ tsan ✓ |
| cs.777 | §11.1 | A program, test or corpus expectation that depends on that order is wrong. | `t_tasks_print_unordered` | interp ✓ jit ✓ build ✓ asan ✓ tsan ✓ |
| cs.778 | §11.1 | Order the output through a join or a channel, or print only from one task (review-thread ruling 2026-10-26). | — | existing pin (CORE_SENTENCE_MAP.md) |
| cs.779 | §11.1 | A test that pins the one-worker executor's order runs with `KARAC_THREADS=1` and says so. | — | existing pin (CORE_SENTENCE_MAP.md) |
| cs.780 | §11.1 | Two `par {}` branches cannot both print: that is a conflict (§11.2). | `t_two_par_branches_print_refused` | interp ✓ jit ✓ build ✓ asan ✓ tsan ✓ |
| cs.782 | §11.2 | - Two branches conflict when one writes, moves, mutably borrows or drops a place or resource that another reads or writes. | `t_par_write_read_conflict_refused` | interp ✓ jit ✓ build ✓ asan ✓ tsan ✓ |
| cs.783 | §11.2 | - Resources come from effects (§12). | — | existing pin (CORE_SENTENCE_MAP.md) |
| cs.784 | §11.2 | - `println` has `writes(Stdout)`, so two printing branches conflict. | `t_two_par_branches_print_refused` | interp ✓ jit ✓ build ✓ asan ✓ tsan ✓ |
| cs.785 | §11.2 | - Two branches that both `sends` to the same resource conflict, and so do two that both `receives` from it, because messages on one chann… | `t_par_two_sends_conflict_refused` | interp ✓ jit ✓ build ✓ asan ✓ tsan ✓ |
| cs.786 | §11.2 | A branch that sends and one that receives on the same resource do not conflict. | `t_par_send_and_recv_ok` | interp ✓ jit ✓ build ✓ asan ✓ tsan ✓ |
| cs.787 | §11.2 | - The iterations of a `par for` are branches of one block, so two iterations conflict by the same rule. | `t_par_for_iterations_conflict_refused` | interp ✓ jit ✓ build ✓ asan ✓ tsan ✓ |
| cs.788 | §11.2 | - A conflict is an error that names both accesses. | `t_par_write_read_conflict_refused` | interp ✓ jit ✓ build ✓ asan ✓ tsan ✓ |
| cs.789 | §11.2 | - **Elements of one collection are one place in v1** (ruling 2026-11-02, B-2026-11-01-40). | `t_par_vec_elements_one_place_refused` | interp ✓ jit ✓ build ✓ asan ✓ tsan ✓ |
| cs.790 | §11.2 | There is no index-disjointness analysis yet, so two branches of a `par {}`, or two iterations of a `par for`, that write elements of the … | `t_par_vec_elements_one_place_refused`, `t_par_for_iter_mut_ok` | interp ✓ jit ✓ build ✓ asan ✓ tsan ✓ |
| cs.791 | §11.2 | Split the collection first and give each branch its own part: | `t_par_split_at_mut_ok` | interp ✓ jit ✓ build ✓ asan ✓ tsan ✓ |
| cs.792 | §11.2 | `let (a, b) = v.split_at_mut(mid);`. | `t_par_split_at_mut_ok` | interp ✓ jit ✓ build ✓ asan ✓ tsan ✓ |
| cs.793 | §11.2 | Proving disjoint index ranges is scheduled in `deferred.md` § Auto-Parallel Loops over Provably-Disjoint Indexed Writes; when it lands, r… | — | existing pin (CORE_SENTENCE_MAP.md) |
| cs.795 | §11.2 | `err_par_conflict` (already an error today, E0418). | — | existing pin (CORE_SENTENCE_MAP.md) |
| cs.797 | §11.3 | - owned values (moved in); | `t_owned_moves_into_task` | interp ✓ jit ✓ build ✓ asan ✓ tsan ✓ |
| cs.798 | §11.3 | - `frozen` and `sync` handles; | — | existing pin (CORE_SENTENCE_MAP.md) |
| cs.799 | §11.3 | - views, into `par {}` branches and `TaskGroup` tasks only (§5.7, §9.5). | `t_par_split_at_mut_ok` | interp ✓ jit ✓ build ✓ asan ✓ tsan ✓ |
| cs.800 | §11.3 | Shared handles may not. | `t_shared_may_not_enter_refused` | interp ✓ jit ✓ build ✓ asan ✓ tsan ✓ |
| cs.801 | §11.4 | **11.4 Panics** in a task end the process (§10), except in an isolating `TaskGroup` (§11.12). | `t_task_panic_ends_process`, `t_isolating_others_run_on` | interp ✓ jit ✓ build ✓ asan ✓ tsan ✓ |
| cs.802 | §11.5 | **11.5 Automatic parallelisation** of loops, and of statements from M4, is an optimisation, and it may change nothing observable: | `t_autopar_float_sum_bitwise`, `t_par_order_free_collect` | interp ✓ jit ✓ build ✓ asan ✓ tsan ✓ |
| cs.803 | §11.5 | - the same output; | `t_autopar_float_sum_bitwise` | interp ✓ jit ✓ build ✓ asan ✓ tsan ✓ |
| cs.804 | §11.5 | - the same values, bit for bit, including floating point; | `t_autopar_float_sum_bitwise` | interp ✓ jit ✓ build ✓ asan ✓ tsan ✓ |
| cs.805 | §11.5 | - the same panics, both whether and where. | `t_autopar_int_sum_overflow_panics_same` | interp ✓ jit ✓ build ✓ asan ✓ tsan ✓ |
| cs.806 | §11.5 | It applies only where the compiler proves all of that. | — | existing pin (CORE_SENTENCE_MAP.md) |
| cs.807 | §11.5 | In particular, it does not reassociate floating-point reductions. | `t_autopar_float_sum_bitwise` | interp ✓ jit ✓ build ✓ asan ✓ tsan ✓ |
| cs.808 | §11.5 | It does not parallelise a checked integer reduction whose prefix sums could overflow where the parallel partial sums would not. | `t_autopar_int_sum_overflow_panics_same` | interp ✓ jit ✓ build ✓ asan ✓ tsan ✓ |
| cs.809 | §11.6 | **11.6 Data races** cannot happen in a program that compiles: §11.2–§11.3 plus the field rules of `sync` types guarantee it. | — | not a runnable claim |
| cs.810 | §11.7 | **11.7 Failure in `par {}`** (2026-10-07, D4). | — | existing pin (CORE_SENTENCE_MAP.md) |
| cs.811 | §11.7 | - **`?` in a branch** keeps `design.md`'s block-piercing meaning. | `t_par_question_returns_error_after_block` | interp ✓ jit ✓ build ✓ asan ✓ tsan ✓ |
| cs.812 | §11.7 | It ends that branch, running the branch's drops, `defer`s and `errdefer`s as an error exit (§7.6–§7.7). | `t_par_question_returns_error_after_block` | interp ✓ jit ✓ build ✓ asan ✓ tsan ✓ |
| cs.813 | §11.7 | The error is returned from the enclosing function once the block is done. | `t_par_question_returns_error_after_block` | interp ✓ jit ✓ build ✓ asan ✓ tsan ✓ |
| cs.814 | §11.7 | - **`return` in a branch** is the same early exit as `?`, with any value: | `t_par_return_in_branch` | interp ✓ jit ✓ build ✓ asan ✓ tsan ✓ |
| cs.815 | §11.7 | `?` is a `return` of the error. | `t_par_return_in_branch` | interp ✓ jit ✓ build ✓ asan ✓ tsan ✓ |
| cs.816 | §11.7 | It ends that branch, running the branch's drops and `defer`s, and its `errdefer`s if the value is an `Err` (§7.6–§7.7), and the enclosing… | `t_par_return_in_branch` | interp ✓ jit ✓ build ✓ asan ✓ tsan ✓ |
| cs.817 | §11.7 | `break` and `continue` cannot leave a branch; aiming one at a loop outside the block is an error (review-thread ruling 2026-10-22, B-2026… | `t_par_break_out_refused` | interp ✓ jit ✓ build ✓ asan ✓ tsan ✓ |
| cs.818 | §11.7 | - **The branches after it are cancelled** (§11.9, from M4 item 5). | `t_par_later_branch_cancelled` | interp ✓ jit ✓ build ✓ asan ✓ tsan ✓ |
| cs.819 | §11.7 | When a branch exits early, every branch after it in source order (iteration order for `par for`) is cancelled; it stops at its next suspe… | `t_par_later_branch_cancelled` | interp ✓ jit ✓ build ✓ asan ✓ tsan ✓ |
| cs.820 | §11.7 | The branches before it run to completion, because one of them may still be the source-earliest exit. | `t_par_earliest_exit_wins` | interp ✓ jit ✓ build ✓ asan ✓ tsan ✓ |
| cs.821 | §11.7 | - **When the block is done:** | — | existing pin (CORE_SENTENCE_MAP.md) |
| cs.822 | §11.7 | - If any branch exited early (`?` or `return`), the enclosing function returns the **source-earliest** one's value (for `par for`, the ea… | `t_par_earliest_exit_wins` | interp ✓ jit ✓ build ✓ asan ✓ tsan ✓ |
| cs.823 | §11.7 | That holds whichever exited first, so the result is deterministic. | `t_par_earliest_exit_wins` | interp ✓ jit ✓ build ✓ asan ✓ tsan ✓ |
| cs.824 | §11.7 | A branch cancelled after it cannot change that result. | `t_par_later_branch_cancelled` | interp ✓ jit ✓ build ✓ asan ✓ tsan ✓ |
| cs.825 | §11.7 | The block's other values, errors included, drop in reverse source order as the block is left (§7.6); a branch torn down by cancellation h… | — | existing pin (CORE_SENTENCE_MAP.md) |
| cs.826 | §11.7 | - Otherwise the block has its value, as `design.md` defines it. | `t_par_tuple_and_block_branch` | interp ✓ jit ✓ build ✓ asan ✓ tsan ✓ |
| cs.827 | §11.7 | - **A panic** in any branch ends the process (§11.4). | `t_par_panic_ends_process` | interp ✓ jit ✓ build ✓ asan ✓ tsan ✓ |
| cs.828 | §11.7 | - **Removed.** `Cancellable`, `#[derive(Cancellable)]`, `collect_all` and `collect_all_vec` are removed. | `t_cancellable_removed_refused`, `t_collect_all_removed_refused` | interp ✓ jit ✓ build ✓ asan ✓ tsan ✓ |
| cs.829 | §11.7 | A block whose branches produce `Result` values without `?` already returns every result, in source order. | `t_par_results_without_question` | interp ✓ jit ✓ build ✓ asan ✓ tsan ✓ |
| cs.831 | §11.7 | `par_error_runs_siblings`, `par_early_exit_cancels_later` (B-2026-11-07-38). | — | existing pin (CORE_SENTENCE_MAP.md) |
| cs.832 | §11.7 | `karac` checks the branches' results as they join, in source order, so it cancels the later branches once every branch before the exiting… | — | existing pin (CORE_SENTENCE_MAP.md) |
| cs.833 | §11.7 | **The M4a rules (§11.8–§11.12).** Decided 2026-10-24: | — | existing pin (CORE_SENTENCE_MAP.md) |
| cs.834 | §11.7 | Gowtham chose teardown at the wait for cancellation and opt-in isolating groups for panics, and the rest are review-thread rulings (`revi… | — | existing pin (CORE_SENTENCE_MAP.md) |
| cs.835 | §11.7 | Each rule takes effect when its M4 plan item lands (`review/m4-plan-2026-10-24.md`): §11.8 with items 2–4, §11.9 and §11.10 with item 5, … | — | existing pin (CORE_SENTENCE_MAP.md) |
| cs.836 | §11.7 | Until then `karac` cancels nothing, and every panic ends the process. | — | existing pin (CORE_SENTENCE_MAP.md) |
| cs.837 | §11.7 | Each rule gets its pin in `corpus/core/` when it lands. | — | existing pin (CORE_SENTENCE_MAP.md) |
| cs.839 | §11.8 | - Every program runs as tasks. | `t_taskgroup_joins_on_drop` | interp ✓ jit ✓ build ✓ asan ✓ tsan ✓ |
| cs.840 | §11.8 | `main` is the root task, and `par {}` branches, `par for` iterations and `TaskGroup.spawn` closures are tasks. | — | not a runnable claim |
| cs.841 | §11.8 | - A call **suspends** its task when it waits on a timer, a channel (`send`, `recv`), a join, a held `Mutex`, or a socket (`accept`, `conn… | `t_cancel_every_wait_kind` | interp ✓ jit ✓ build ✓ asan ✓ tsan ✓ |
| cs.842 | §11.8 | While the task is suspended, other tasks run on the same worker. | `t_progress_at_suspension_one_worker` | interp ✓ jit ✓ build ✓ asan ✓ tsan ✓ |
| cs.843 | §11.8 | These calls carry the `suspends` effect. | — | spec conflict: send/recv/join/lock infer `blocks`, not `suspends` (row filed) |
| cs.844 | §11.8 | They are the **suspension points** of §11.1, §11.9 and §11.11. | — | existing pin (CORE_SENTENCE_MAP.md) |
| cs.845 | §11.8 | - Every other waiting call **blocks** its worker: file I/O, any `extern` call, and a process wait or pipe read until item 2 moves them to… | — | existing pin (CORE_SENTENCE_MAP.md) |
| cs.846 | §11.8 | These carry `blocks`. | — | existing pin (CORE_SENTENCE_MAP.md) |
| cs.847 | §11.8 | With one worker, a blocking call stops every task until it returns. | — | existing pin (CORE_SENTENCE_MAP.md) |
| cs.848 | §11.8 | With several workers (M4 item 10), the runtime may start another worker to make up for it. | — | existing pin (CORE_SENTENCE_MAP.md) |
| cs.849 | §11.8 | - **`blocks` admits `suspends`.** Wherever a declared effect clause is a ceiling (a public function's `with`, a trait method's clause, an… | `t_blocks_admits_suspends` | interp ✓ jit ✓ build ✓ asan ✓ tsan ✓ |
| cs.850 | §11.8 | A public function that declares `blocks` may call a `suspends` callee, and a trait method declared `blocks` may have an impl that suspends. | `t_blocks_admits_suspends` | interp ✓ jit ✓ build ✓ asan ✓ tsan ✓ |
| cs.851 | §11.8 | Declaring `suspends` alone is the narrower promise: the function never holds its worker while it waits, so it may not call a `blocks` cal… | `t_suspends_alone_admits_recv`, `t_suspends_alone_refuses_file_io` | admits_recv: interp, jit, build (B-2026-11-27-10, FINDING-A); refuses_file_io: refused (E0400) |
| cs.852 | §11.8 | - **Every body that may suspend is a coroutine** (the stackless transform, `docs/spikes/coroutines.md`), not only a function with network… | — | existing pin (CORE_SENTENCE_MAP.md) |
| cs.853 | §11.8 | This supersedes deferred.md's network-boundary-only lowering. | — | existing pin (CORE_SENTENCE_MAP.md) |
| cs.854 | §11.8 | - **No rule about `ref` across a wait.** A task's frame never moves, and no frame is a value (§9.5), so a `ref` held across a suspension … | `t_ref_across_wait` | interp ✓ jit ✓ build ✓ asan ✓ tsan ✓ |
| cs.855 | §11.8 | - **Foreign calls do not suspend.** An `extern` call runs to completion on the worker that made it. | — | not a runnable claim |
| cs.856 | §11.8 | Whether a task may resume on a different worker after a wait, and so whether a foreign library's thread-local state survives a wait, is s… | — | not a runnable claim |
| cs.858 | §11.9 | - **Who cancels.** | — | existing pin (CORE_SENTENCE_MAP.md) |
| cs.859 | §11.9 | - An early exit in `par {}` or `par for` cancels the branches after it (§11.7). | `t_par_later_branch_cancelled` | interp ✓ jit ✓ build ✓ asan ✓ tsan ✓ |
| cs.860 | §11.9 | - `TaskGroup.cancel(self)` cancels every task in the group that has not finished. | `t_cancel_every_wait_kind` | interp ✓ jit ✓ build ✓ asan ✓ tsan ✓ |
| cs.861 | §11.9 | - A deadline that passes cancels the work it covers (§11.10). | `t_timeout_ok_and_err` | interp ✓ jit ✓ build ✓ asan ✓ tsan ✓ |
| cs.862 | §11.9 | - Cancelling a task cancels the `par`, `par for` and `TaskGroup` scopes running inside it. | `t_cancel_nested_scope` | interp ✓ jit ✓ build ✓ asan ✓ tsan ✓ |
| cs.863 | §11.9 | - **When it takes effect.** Only at a suspension point (§11.8), never in pure computation. | `t_cancel_loop_without_wait_completes` | interp ✓ jit ✓ build ✓ asan ✓ tsan ✓ |
| cs.864 | §11.9 | A task that is already past its last wait completes and keeps its result: completion wins. | `t_cancel_completion_wins` | interp ✓ jit ✓ build ✓ asan ✓ tsan ✓ |
| cs.865 | §11.9 | A loop with no wait inside it cannot be cancelled. | `t_cancel_loop_without_wait_completes` | interp ✓ jit ✓ build ✓ asan ✓ tsan ✓ |
| cs.866 | §11.9 | - **How the task stops: it is torn down at the wait.** The suspended call does not return. | `t_cancel_teardown_innermost_first` | interp ✓ jit ✓ build ✓ asan ✓ tsan ✓ |
| cs.867 | §11.9 | The task ends there, as an error exit from every frame on its chain, innermost first: each frame's drops, `defer`s and `errdefer`s run as… | `t_cancel_teardown_innermost_first` | interp ✓ jit ✓ build ✓ asan ✓ tsan ✓ |
| cs.868 | §11.9 | No other code of the task runs after the wait. | `t_cancel_teardown_innermost_first` | interp ✓ jit ✓ build ✓ asan ✓ tsan ✓ |
| cs.869 | §11.9 | - An `errdefer` without a binding runs on teardown. | `t_cancel_teardown_innermost_first` | interp ✓ jit ✓ build ✓ asan ✓ tsan ✓ |
| cs.870 | §11.9 | An `errdefer(e)` is skipped, because there is no error value for it to name: cleanup that must also run on cancellation goes in a plain `… | `t_cancel_teardown_innermost_first` | interp ✓ jit ✓ build ✓ asan ✓ tsan ✓ |
| cs.871 | §11.9 | - Every suspension point can be cancelled, including `sleep`, `recv`, `join` and a `Mutex` lock, none of which returns a `Result`. | `t_cancel_every_wait_kind` | interp ✓ jit ✓ build ✓ asan ✓ tsan ✓ |
| cs.872 | §11.9 | - A cancelled `par` branch or `TaskGroup` task has no value, and its scope ignores it. | `t_join_of_cancelled_panics` | interp ✓ jit ✓ build ✓ asan ✓ tsan ✓ |
| cs.873 | §11.9 | `try_join` on its handle returns `Err(Cancelled)`; plain `join` on a cancelled task of an ordinary group panics, since there is no value … | `t_join_of_cancelled_panics`, `t_isolating_try_join_on_ordinary_group` | interp ✓ jit ✓ build ✓ asan ✓ tsan ✓ |
| cs.874 | §11.9 | - No value of type `Cancelled` reaches the cancelled task's own code, and no library signature changes for cancellation. | — | existing pin (CORE_SENTENCE_MAP.md) |
| cs.875 | §11.9 | - The teardown runs the drops an error exit already runs, so no rule restricts what may be held across a wait. deferred.md's `CancelSafe`… | — | existing pin (CORE_SENTENCE_MAP.md) |
| cs.876 | §11.9 | - **What is not deterministic.** How far a cancelled branch got before it stopped depends on timing, so its effects up to that point may … | — | existing pin (CORE_SENTENCE_MAP.md) |
| cs.877 | §11.9 | The block's value does not depend on timing (§11.7). | — | existing pin (CORE_SENTENCE_MAP.md) |
| cs.879 | §11.10 | - `std.time.timeout(d: | — | existing pin (CORE_SENTENCE_MAP.md) |
| cs.880 | §11.10 | Duration, f: | — | existing pin (CORE_SENTENCE_MAP.md) |
| cs.881 | §11.10 | OnceFn() -> T) -> Result[T, TimedOut]` runs `f` as a task scope. | `t_timeout_ok_and_err` | interp ✓ jit ✓ build ✓ asan ✓ tsan ✓ |
| cs.882 | §11.10 | If `d` passes first, the scope is cancelled (§11.9) and the call returns `Err(TimedOut {})`. | `t_timeout_ok_and_err` | interp ✓ jit ✓ build ✓ asan ✓ tsan ✓ |
| cs.883 | §11.10 | Otherwise it returns `Ok` of `f`'s value. | `t_timeout_ok_and_err` | interp ✓ jit ✓ build ✓ asan ✓ tsan ✓ |
| cs.884 | §11.10 | A `?` inside `f` behaves as in any closure. | — | existing pin (CORE_SENTENCE_MAP.md) |
| cs.885 | §11.10 | - `TaskGroup.with_deadline(own self, d: | — | existing pin (CORE_SENTENCE_MAP.md) |
| cs.886 | §11.10 | Duration) -> TaskGroup` returns the group with a deadline: once `d` has passed, the group is cancelled. | `t_deadlines_nest_earliest` | interp ✓ jit ✓ build ✓ asan ✓ tsan ✓ |
| cs.887 | §11.10 | - A socket's `set_read_timeout` also bounds a suspended read, which then returns `Err(TimedOut)`, the existing `NetError` arm. | — | existing pin (CORE_SENTENCE_MAP.md) |
| cs.888 | §11.10 | - Deadlines nest, and the earliest deadline covering a wait is the one that fires. | `t_deadlines_nest_earliest` | interp ✓ jit ✓ build ✓ asan ✓ tsan ✓ |
| cs.890 | §11.11 | - The syntax and meaning are as in deferred.md § `select` Across Channels. | — | existing pin (CORE_SENTENCE_MAP.md) |
| cs.891 | §11.11 | Arms are `recv`, `send` or `after(d)`, with an optional `default`. | — | existing pin (CORE_SENTENCE_MAP.md) |
| cs.892 | §11.11 | The first arm to become ready runs, and the block's value is that arm's value. | `t_select_after_counts_from_entry` | interp ✓ jit ✓ build ✓ asan ✓ tsan ✓ |
| cs.893 | §11.11 | - Each arm's pattern is irrefutable, as in `let`. | — | existing pin (CORE_SENTENCE_MAP.md) |
| cs.894 | §11.11 | It binds the operation's own result: | — | existing pin (CORE_SENTENCE_MAP.md) |
| cs.895 | §11.11 | `Option[T]` for `recv`, `Result[(), SendError[T]]` for `send`, `()` for `after`. | `t_select_closed_recv_ready` | interp ✓ jit ✓ build ✓ asan ✓ tsan ✓ |
| cs.896 | §11.11 | The channel, value and duration expressions are evaluated once, in source order, when the `select` is entered, and `after(d)` counts from… | `t_select_after_counts_from_entry` | interp ✓ jit ✓ build ✓ asan ✓ tsan ✓ |
| cs.897 | §11.11 | - **Choosing an arm performs its operation.** The arm that runs has received or sent, and no other arm has. | `t_select_unchosen_send_value_drops` | interp ✓ jit ✓ build ✓ asan ✓ tsan ✓ |
| cs.898 | §11.11 | A send arm that is not chosen still owns its value, which drops as the `select` ends. | `t_select_unchosen_send_value_drops` | interp ✓ jit ✓ build ✓ asan ✓ tsan ✓ |
| cs.899 | §11.11 | A closed channel makes its arm ready: | `t_select_closed_recv_ready` | interp ✓ jit ✓ build ✓ asan ✓ tsan ✓ |
| cs.900 | §11.11 | `recv` gives `None` and `send` gives `Err(SendError.Closed(v))`, as the plain calls do. | `t_select_closed_recv_ready` | interp ✓ jit ✓ build ✓ asan ✓ tsan ✓ |
| cs.901 | §11.11 | - **When several arms are ready at once, the source-first ready arm wins.** That is deterministic, unlike Go's random choice. | `t_select_source_first_ready_wins` | interp ✓ jit ✓ build ✓ asan ✓ tsan ✓ |
| cs.902 | §11.11 | The fairness cost is accepted: order the arms to suit. | `t_select_source_first_ready_wins` | interp ✓ jit ✓ build ✓ asan ✓ tsan ✓ |
| cs.903 | §11.11 | - A `select` with no `default` is a suspension point, and so a cancellation point (§11.9). | `t_select_is_a_cancel_point` | interp ✓ jit ✓ build ✓ asan ✓ tsan ✓ |
| cs.904 | §11.11 | - `Receiver.recv_timeout(self, d: | `conformance/concurrency/timeouts` | interp ✓ jit ✓ build ✓ asan ✓ (concurrency suite, 896 passed) |
| cs.905 | §11.11 | Duration) -> Result[T, RecvTimeoutError]` and `Sender.send_timeout(self, value: own T, d: | `conformance/concurrency/timeouts` | interp ✓ jit ✓ build ✓ asan ✓ (concurrency suite, 896 passed) |
| cs.906 | §11.11 | Duration) -> Result[(), SendTimeoutError[T]]` arrive with it. | `conformance/concurrency/timeouts` | interp ✓ jit ✓ build ✓ asan ✓ (concurrency suite, 896 passed) |
| cs.907 | §11.11 | Each behaves as a two-arm `select` of the operation and `after(d)`. | `conformance/concurrency/timeouts` | interp ✓ jit ✓ build ✓ asan ✓ (concurrency suite, 896 passed) |
| cs.908 | §11.11 | `RecvTimeoutError` is `Closed` or `TimedOut`. | `conformance/concurrency/timeouts` | interp ✓ jit ✓ build ✓ asan ✓ (concurrency suite, 896 passed) |
| cs.909 | §11.11 | `SendTimeoutError[T]` is `Closed(T)` or `TimedOut(T)`: | `conformance/concurrency/timeouts` | interp ✓ jit ✓ build ✓ asan ✓ (concurrency suite, 896 passed) |
| cs.910 | §11.11 | `send_timeout` hands the value back either way (review-thread ruling; this replaces deferred.md's `Option[T]` and `Result[(), T]` shapes,… | `conformance/concurrency/timeouts` | interp ✓ jit ✓ build ✓ asan ✓ (concurrency suite, 896 passed) |
| cs.912 | §11.12 | - `TaskGroup.isolating(; limit: i64 = i64.MAX) -> IsolatingGroup` makes a group that contains its tasks' panics. | `t_isolating_others_run_on` | interp ✓ jit ✓ build ✓ asan ✓ tsan ✓ |
| cs.913 | §11.12 | It has `TaskGroup`'s methods, except that `spawn` returns `TaskHandle[Result[T, TaskFailed]]`, so `join` on its tasks returns `Result[T, … | `t_isolating_others_run_on` | interp ✓ jit ✓ build ✓ asan ✓ tsan ✓ |
| cs.914 | §11.12 | - `TaskFailed` is `Panicked { message, location }` or `Cancelled`. | `t_isolating_others_run_on` | interp ✓ jit ✓ build ✓ asan ✓ tsan ✓ |
| cs.915 | §11.12 | On an ordinary group, `TaskHandle.try_join(own self) -> Result[T, TaskFailed]` gives the same view, where only `Cancelled` can occur. | `t_isolating_try_join_on_ordinary_group` | interp ✓ jit ✓ build ✓ asan ✓ tsan ✓ |
| cs.916 | §11.12 | - A task that panics in such a group is torn down like a cancelled task (§11.9): its drops, `defer`s and plain `errdefer`s run, an `errde… | `t_isolating_others_run_on` | interp ✓ jit ✓ build ✓ asan ✓ tsan ✓ |
| cs.917 | §11.12 | Pinned by `corpus/core/isolating_panic_ends_its_task`. | — | existing pin (CORE_SENTENCE_MAP.md) |
| cs.918 | §11.12 | - The group's other tasks run on. | `t_isolating_others_run_on` | interp ✓ jit ✓ build ✓ asan ✓ tsan ✓ |
| cs.919 | §11.12 | - An allocation failure (§10.1's `alloc_failure`) is such a panic when it is a size the program asks for: | `t_isolating_alloc_failure_ends_task` | interp ✓ jit ✓ build ✓ asan ✓ tsan ✓ |
| cs.920 | §11.12 | `with_capacity`, `reserve` and `reserve_exact` on `Vec` and `String`, `Vec.filled`, `Vec.resize` and `String.repeat`, with a count no buf… | — | existing pin (CORE_SENTENCE_MAP.md) |
| cs.921 | §11.12 | The task's handle holds `Err(Panicked { .. })` with that message, so a request that asks for a huge buffer ends its own task. | `t_isolating_alloc_failure_ends_task` | interp ✓ jit ✓ build ✓ asan ✓ tsan ✓ |
| cs.922 | §11.12 | Memory running out anywhere else, in a vector's ordinary growth or a library native's own work, still ends the process: by then the heap … | — | existing pin (CORE_SENTENCE_MAP.md) |
| cs.923 | §11.12 | Pinned by `tests/isolating_alloc_failure.rs`. | — | existing pin (CORE_SENTENCE_MAP.md) |
| cs.924 | §11.12 | - Everywhere else (`par {}`, `par for`, an ordinary `TaskGroup` and `main`), a panic still ends the process as §10.2 says. | `t_task_panic_ends_process`, `t_par_panic_ends_process` | interp ✓ jit ✓ build ✓ asan ✓ tsan ✓ |
| cs.925 | §11.12 | - A server writes one isolating group per accept loop, so a bad request ends its own connection's task, not the server. | — | existing pin (CORE_SENTENCE_MAP.md) |
| cs.926 | §11.12 | - In a program that makes an isolating group, every call that may panic gets the same exit a cancel point has, and the teardown leaves by… | — | existing pin (CORE_SENTENCE_MAP.md) |
| cs.927 | §11.12 | A call that cannot panic (by the effects analysis) needs none. | — | existing pin (CORE_SENTENCE_MAP.md) |
| cs.928 | §11.12 | - As of 2026-10-25 this runs on the MIR interpreter (c2f859dacc) and in built binaries (M4 item 9's compiled half). | — | existing pin (CORE_SENTENCE_MAP.md) |
| cs.929 | §11.12 | A panic inside a library native's work (a bounds check, a comparator `sort_by` calls) ends the native there, and its call leaves by the e… | `t_isolating_native_comparator_panic` | interp ✓ jit ✓ build ✓ asan ✓ tsan ✓ |
