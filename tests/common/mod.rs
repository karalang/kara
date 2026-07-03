//! Shared spawn-watchdog helper for integration tests.
//!
//! Lifted from `tests/codegen.rs` (commit `62af025`) and reused across the
//! other test files that spawn user-compiled binaries: `tests/par_codegen.rs`,
//! `tests/parallax.rs`, `tests/parallax_lite.rs`, `tests/cli.rs`. The original
//! incident was a `cargo test --features llvm --test codegen` hang of 30+ min
//! traced to a concurrent `Command::output()` deadlock under `cargo test`
//! parallelism — the four files above have the same structural exposure
//! (parallel `.output()` invocations on child binaries sharing pipe fds).
//! Tracker entry that filed this mirror: `phase-7-codegen.md` § *Mirror
//! `output_with_hang_watchdog` into ...*.
//!
//! Per-file timeout calibration (the slice plan's only twist):
//! - 15 s for short-lived helpers (`codegen.rs`, `cli.rs`)
//! - 60 s for parallel-workload binaries (`par_codegen.rs`, `parallax.rs`,
//!   `parallax_lite.rs`) — they intentionally run 5-15 s of real work and
//!   need headroom above that to distinguish "slow under load" from "hung".
//!
//! Rust's integration-test layout requires this module live at
//! `tests/common/mod.rs` (not `tests/common.rs`), otherwise cargo treats it
//! as another test binary. `mod common;` from each test file picks it up.

#![allow(dead_code)]

use std::process::{Command, Output, Stdio};
use std::sync::mpsc;
use std::time::Duration;

/// Spawn `cmd`, capture stdout/stderr, and kill the child if it hasn't
/// finished within `timeout`. Returns `None` if the spawn itself failed
/// (so callers can soft-skip), panics if the child was killed for hanging
/// (so a CI run surfaces the hang clearly instead of silently passing on
/// a partial output).
///
/// Same shape as the original inline helper in `tests/codegen.rs` pre-
/// extraction: stdin redirected from /dev/null, stdout+stderr piped,
/// `kill -9 <pid>` on timeout, watchdog thread joined before return so
/// the kill is observable on stderr.
pub fn output_with_hang_watchdog(mut cmd: Command, timeout: Duration) -> Option<Output> {
    // Bound each child binary's auto-par worker pool so a suite-wide run does
    // not oversubscribe the machine. `cargo test` runs ~`num_cpus` test threads
    // in parallel, and since the 2026-06-14 auto-par ordered-output change far
    // more E2E programs now spawn the runtime's work-stealing pool (output-
    // bearing mains are no longer suppressed). Left uncapped, each child spins
    // `available_parallelism()` (~18) workers → `test_threads × 18` threads
    // thrash a `num_cpus`-core box, and child binaries miss the watchdog's
    // timeout (slow-under-load read as "hung"). Two workers still exercises the
    // real multi-branch par_run path (queue + work-helping join + ordered-
    // output capture/replay) while keeping the total thread count bounded.
    // Honors an explicit caller override (e.g. a wall-clock benchmark that
    // wants full width) — only sets the default when unset.
    let workers_key = std::ffi::OsStr::new("KARAC_PAR_WORKERS");
    if !cmd.get_envs().any(|(k, _)| k == workers_key) {
        cmd.env("KARAC_PAR_WORKERS", "2");
    }
    let child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .ok()?;
    let pid = child.id();

    let (tx, rx) = mpsc::channel::<()>();
    let watchdog = std::thread::spawn(move || {
        if rx.recv_timeout(timeout).is_err() {
            eprintln!(
                "FATAL: test child (pid {pid}) hung for >{}s — killing. \
                 Likely a concurrent Command::output() deadlock under \
                 cargo test parallelism. Re-run with `--test-threads=1` \
                 to isolate, or run the failing test alone.",
                timeout.as_secs(),
            );
            let _ = Command::new("kill").args(["-9", &pid.to_string()]).status();
            true
        } else {
            false
        }
    });

    let result = child.wait_with_output().ok();
    let _ = tx.send(());
    let killed = watchdog.join().unwrap_or(false);

    if killed {
        panic!("test child binary hung — see stderr above for diagnostics");
    }
    result
}

// ── Ownership gate for E2E harnesses ─────────────────────────────────

/// Tests grandfathered past [`assert_ownership_clean`]. Each entry is a
/// bare test-fn name (matched against the test thread's name, which
/// libtest sets to the full `module::test_name` path), or a corpus
/// prefix ending in `*`, and MUST carry a comment naming either the
/// docs/bug-ledger.jsonl entry for the latent compiler bug it pins, or
/// the reason the test can't be made ownership-clean yet. The list
/// exists to shrink: fix the bug (or the test program), remove the
/// entry.
pub const OWNERSHIP_GATE_GRANDFATHERED: &[&str] = &[
    // ── Escaping-closure corpus (RefCaptureEscapesScope, E0508) ─────
    // design.md § Closures Rule 2 sub-case (iv): a ref-captured value
    // escaping its scope IS a compile error by design; `karac build` /
    // `run` tolerate it (CLI advisory-ownership policy, cli.rs
    // `has_fatal_ownership_errors`) and the heap-env epic
    // (B-2026-06-22-2, be2ef68e) made codegen memory-safe on exactly
    // these shapes. The corpus deliberately pins that codegen surface
    // with the bare (inferred-ref) closure spelling, so it stays
    // ownership-red until rewritten against the check-clean `own |..|`
    // spelling (usable now that B-2026-07-02-20 honors the prefix).
    "heap_env_*",
    // Same corpus, ASAN lane (tests/memory_sanitizer.rs).
    "asan_heap_env_*",
    // Same class: the conservative fn-arg-pass escape rule (round
    // 12.39) fires on closures with ref captures passed to Own-mode
    // Fn slots (`with_span`'s OnceFn, `collect_all`/`collect_all_vec`
    // fan-out args) — documented over-rejection;
    // `#[allow(ref_capture_escape)]` is the designed opt-out.
    "e2e_tracing_with_span_stamps_active_span",
    "asan_collect_all_vec_capturing_closures_no_uaf",
    "collect_all_vec_lowers_to_par_run_gather",
    //
    // ── Deliberate reuse-after-move codegen pins ────────────────────
    // These pin no-double-free / RC-fallback-boxing behavior for
    // ownership-RED programs that `karac build` tolerates (advisory
    // policy). The reuse IS the point — do not "fix" the programs.
    "test_e2e_byvalue_aggregate_param_read_then_reused",
    "test_e2e_owned_vec_param_let_move_param_reusable",
    "test_e2e_struct_param_field_move_out",
    "asan_byvalue_aggregate_param_transferred_out_no_double_free",
    "asan_struct_param_field_move_reuse_no_double_free",
    "asan_deep_tuple_index_match_no_double_free",
    "asan_enum_nested_struct_payload_moved_out_no_leak_no_double_free",
    // Deliberately-illegal aliasing program — the test's point is that
    // the runtime path rejects it (`_rejects` suffix).
    "asan_vec_extend_from_slice_self_alias_rejects",
    //
    // ── Designed concurrent-access diagnostics, tolerated-build pins ─
    // `karac check` rejects a shared/plain struct binding reachable
    // from two par branches by design (design.md § 8183,
    // E_CONCURRENT_SHARED_STRUCT / ConcurrentPlainStruct with its
    // `par struct` migration fix_diff); these tests pin the tolerated
    // build's runtime atomicity / par-branch IR shape.
    "test_e2e_arc_binding_runtime_correctness",
    "asan_par_block_arc_promoted_no_double_free",
    "test_ir_par_branch_emits_method_check_for_effectful_callee",
    "test_ir_par_branch_skips_method_check_for_pure_callee",
    //
    // ── B-2026-07-02-23/24/25/26 removed: the four ownership-checker
    // false-positives are FIXED (comparison-borrow, fn-item Copy,
    // disjoint-place partial moves, with_provider borrow slot). Their
    // tests now pass the strict gate; regression coverage lives in
    // tests/ownership.rs (`b23_*` / `b24_*` / `b25_*` / `b26_*`).
    //
    // B-2026-07-02-35 (RESOLVED — checker correct-by-design): the two
    // shared-enum-drop corpus programs above previously carried a read-only
    // owned bare-`T` param reused across call sites (`peek(r); peek(r)`),
    // which `karac check` correctly flags as move-after-move (design.md:
    // bare-`T` is OWNED; ownership modes are declared, not inferred). The
    // programs — not the checker — were the defect: their `peek` helper now
    // takes `ref` (it only reads a field), so both un-grandfather cleanly.
];

/// Fail loudly when an E2E test program flunks the ownership checker.
///
/// The E2E harnesses run `karac::ownershipcheck` only to feed codegen's
/// RC-fallback surface and historically ignored the returned errors — so
/// the suite stayed green on programs `karac check` rejects, and codegen
/// ran on input it is never given in production (B-2026-07-01-10 hid
/// behind exactly this: a test consumed the same Vec four times and
/// passed for weeks). Every harness that threads ownership into codegen
/// calls this right after `ownershipcheck`.
///
/// Only `ownership.errors` gates; `notes` (RC-fallback perf notes) are
/// non-blocking in `karac check` and stay non-blocking here.
///
/// Existing offenders are grandfathered by test name in
/// [`OWNERSHIP_GATE_GRANDFATHERED`] so the gate lands strict for new
/// tests while the backlog is triaged incrementally. Name matching uses
/// the test thread's name; under a runner that doesn't name test
/// threads (e.g. `--test-threads=1` runs on the main thread) the
/// allowlist can't match and grandfathered tests fail too — run the
/// suite with the default parallel runner.
pub fn assert_ownership_clean(ownership: &karac::ownership::OwnershipCheckResult, src: &str) {
    if ownership.errors.is_empty() {
        return;
    }
    let thread = std::thread::current();
    let test_name = thread.name().unwrap_or("<unnamed>");
    if OWNERSHIP_GATE_GRANDFATHERED.iter().any(|g| {
        // A trailing `*` marks a corpus-prefix entry: it matches any
        // test whose bare name starts with the prefix.
        if let Some(prefix) = g.strip_suffix('*') {
            let bare = test_name.rsplit("::").next().unwrap_or(test_name);
            bare.starts_with(prefix)
        } else {
            test_name == *g || test_name.ends_with(&format!("::{g}"))
        }
    }) {
        eprintln!(
            "[ownership-gate] {test_name}: {} ownership error(s) grandfathered — \
             see OWNERSHIP_GATE_GRANDFATHERED in tests/common/mod.rs",
            ownership.errors.len()
        );
        return;
    }
    let mut msg = format!(
        "[ownership-gate] test `{test_name}`: program fails the ownership checker \
         ({} error(s)) — `karac check` would reject it, so codegen is being fed \
         input it never sees in production. Fix the test program, or (for a latent \
         compiler bug) file a docs/bug-ledger.jsonl entry and grandfather the test \
         in tests/common/mod.rs:\n",
        ownership.errors.len()
    );
    for e in &ownership.errors {
        msg.push_str(&format!(
            "  {}:{}: {} [{:?}]\n",
            e.span.line, e.span.column, e.message, e.kind
        ));
        if let Some(s) = &e.suggestion {
            msg.push_str(&format!("      suggestion: {s}\n"));
        }
    }
    msg.push_str("program:\n");
    msg.push_str(src);
    panic!("{msg}");
}
