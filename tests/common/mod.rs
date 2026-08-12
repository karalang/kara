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
    //
    // B-2026-08-01-27 — pins the compiled backends' post-move
    // observations (source frozen at pre-move state) that the
    // interpreter's let-move deep-clone now matches; the reuse IS the
    // divergence surface under test.
    "e2e_let_move_source_frozen",
    // B-2026-08-01-31 — `let x = o.h.r; o.h.r = <new>` (UAM-warned
    // move-then-reinit): pins the deep-chain move-out zeroing + the
    // root-coarse bodies disarm keeping BOTH backends on the same
    // single-fire sequencing. The reuse IS the point.
    "e2e_deep_chain_field_move_then_reassign",
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
    // B-2026-08-08-25 — a live `Option[String]` / user-enum binding matched
    // TWICE, where the arms only READ the payload. `karac check` renders this
    // as a WARNING and exits 0, so production really does compile and run
    // these; the gate's "karac check would reject it" premise does not hold
    // for UseAfterMove. Pre-fix the second read was a use-after-free
    // (valgrind: invalid read of a freed block); the reuse IS the point.
    "test_e2e_match_out_of_option_string_leaves_source_usable",
    "test_e2e_map_twice_over_live_option_string",
    "asan_match_out_of_live_option_string_no_use_after_free",
    // Same row, leg 1 (`.map` and the hand-written `None => None` shape) —
    // identical reasoning: reused after a UseAfterMove *warning*, which
    // production compiles and runs.
    "asan_map_over_live_option_string_leaves_source_owning_its_buffer",
    // Same row, legs 2 and 3 (`Result` and the user-enum channel). These
    // extend the block above rather than introduce a new class: the shapes are
    // the `Result`/user-enum spellings of the same twice-matched live local,
    // and the checker's answer on them is the same inverted one — it flags the
    // READ-ONLY double match, which is safe and is precisely what these legs
    // fixed, while staying silent on the consuming `.map` that genuinely moves
    // the payload out. Deleting the offending case was the right call for
    // leg 1 (it was a duplicate of coverage two other tests already had); here
    // it is not available, because the read-only `Result` match IS the leg-2
    // diagnosis — the scalar `Err` co-arm is what disqualified the classifier.
    // (`test_e2e_consuming_map_over_live_optres_still_open` stood here until
    // those legs closed; the test it named is gone, replaced by the two below.)
    "test_e2e_live_local_match_keeps_the_source_for_result_and_user_enums",
    "asan_live_local_match_over_result_and_user_enum_frees_each_buffer_once",
    // B-2026-08-09-9 — the `Vec[heap-element]` payload split out of leg 3.
    // Same twice-matched live local, same inverted diagnostic.
    "test_e2e_consuming_match_over_live_enum_vec_payload_keeps_the_source",
    "asan_consuming_match_over_live_enum_vec_payload_frees_each_buffer_once",
    // B-2026-08-09-8 — the same two rows' shape with a bare rebind (`let p =
    // o;`) in front, which is what made the caller-retains classifier miss it.
    // Same reasoning again: `karac check` exits 0 on these (the matrix above
    // was measured with `karac check` returning ok for every one), so
    // production compiles and runs them; the reuse IS the point.
    "test_e2e_rebound_option_local_is_read_repeatedly",
    "asan_rebound_option_local_transfers_payload_ownership",
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
/// Triage a `link_executable` failure: soft-skip a genuinely archive-less
/// environment, but PANIC on a stale archive.
///
/// Every E2E harness links against `libkarac_runtime.a` and soft-skips
/// (`.ok()?` → `None`) when linking fails, because a checkout without the
/// archive built is a legitimate skip (CLAUDE.md's documented "skip with a
/// stderr notice" contract). The hole: an archive that EXISTS but is STALE —
/// built before a runtime symbol current codegen emits — also fails to link,
/// and the same soft-skip swallowed it. Because ~960 of the codegen suite's
/// call sites use the tolerant `if let Some(out) = out { assert!(…) }` shape,
/// a stale archive silently voided most of the E2E suite: it reported green
/// while asserting nothing. Only the ~300 strict `assert_eq!(run(…), Some(…))`
/// sites noticed, and there the symptom (`left: None`) hid the linker error.
///
/// The discriminator is the error text. An UNDEFINED-SYMBOL failure means the
/// linker found the archive and it lacked a symbol — always a stale archive
/// (or a genuine missing keep-list entry), never "this environment has no
/// archive". Any other link error (archive absent, no linker, permissions)
/// keeps the soft-skip.
///
/// Returns `Some(())` to continue; `None` to soft-skip (propagate with `?`).
pub fn link_or_skip(result: Result<(), String>) -> Option<()> {
    let Err(e) = result else {
        return Some(());
    };
    let undefined_symbol = e.contains("undefined reference")
        || e.contains("undefined symbol")
        // macOS ld: "Undefined symbols for architecture arm64:"
        || e.contains("Undefined symbols");
    if undefined_symbol {
        panic!(
            "link failed with an UNDEFINED SYMBOL — the runtime archive is STALE \
             (built before a symbol current codegen emits), not missing. This would \
             otherwise soft-skip and silently void most of the E2E suite. Rebuild it \
             (CLAUDE.md § Commands, lean FIRST then full):\n\
             \x20 cargo rustc -p karac-runtime --release --no-default-features \
             --features net --crate-type staticlib\n\
             \x20 cp target/release/libkarac_runtime.a target/release/libkarac_runtime_min.a\n\
             \x20 cargo rustc -p karac-runtime --release --crate-type staticlib\n\n\
             linker error:\n{e}"
        );
    }
    None
}

/// Does this ownership kind actually stop a `karac build` / fail `karac check`?
///
/// B-2026-08-10-21 — this MUST mirror `cli.rs`'s `is_fatal_ownership_kind`,
/// which is the production gate. The two advisory kinds are excluded there
/// deliberately (`RcFallbackNote` is a perf note; `UseAfterMove` carries a
/// machine-applicable `.clone()` fix and compiles by design), so `karac check`
/// exits 0 and `karac build` succeeds on them.
///
/// The gate below used to fail on EVERY kind, justified by "`karac check` would
/// reject it, so codegen is being fed input it never sees in production". That
/// premise is exactly backwards for the advisory kinds: production not only
/// sees them, it is documented to accept them. The effect was that the
/// ~1000-fixture memory suite structurally could not contain a single test of
/// the defensive-copy mechanism whose correctness `UseAfterMove`'s non-fatal
/// status depends on — which is how B-2026-08-10-21 (that copy never existed
/// for any heap type) survived undetected.
///
/// `RcFallbackNote` is listed for symmetry with the production gate; it rides
/// `notes` rather than `errors` today, so it is not reachable here.
fn ownership_kind_blocks_production(kind: &karac::ownership::OwnershipErrorKind) -> bool {
    use karac::ownership::OwnershipErrorKind as K;
    !matches!(kind, K::RcFallbackNote | K::UseAfterMove)
}

pub fn assert_ownership_clean(ownership: &karac::ownership::OwnershipCheckResult, src: &str) {
    // Only the kinds production actually rejects gate the suite — see
    // `ownership_kind_blocks_production`.
    let blocking: Vec<&karac::ownership::OwnershipError> = ownership
        .errors
        .iter()
        .filter(|e| ownership_kind_blocks_production(&e.kind))
        .collect();
    if blocking.is_empty() {
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
            "[ownership-gate] {test_name}: {} blocking ownership error(s) \
             grandfathered — see OWNERSHIP_GATE_GRANDFATHERED in \
             tests/common/mod.rs",
            blocking.len()
        );
        return;
    }
    let mut msg = format!(
        "[ownership-gate] test `{test_name}`: program fails the ownership checker \
         ({} error(s)) — `karac check` would reject it, so codegen is being fed \
         input it never sees in production. Fix the test program, or (for a latent \
         compiler bug) file a docs/bug-ledger.jsonl entry and grandfather the test \
         in tests/common/mod.rs:\n",
        blocking.len()
    );
    for e in &blocking {
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

// ── Check gate for E2E harnesses ─────────────────────────────────────

/// Tests grandfathered past [`assert_check_clean`]. Same contract as
/// [`OWNERSHIP_GATE_GRANDFATHERED`]: a bare test-fn name (matched against
/// the test thread's name) or a `*`-suffixed corpus prefix, each carrying
/// the reason it can't be made check-clean yet. The list exists to shrink.
///
/// Two distinct populations are in here, and they want opposite fixes:
///
///   * **STALE TEST PROGRAM** — the checker is right and the test source
///     predates the rule (call-site `mut` markers, explicit narrowing
///     casts, `#[derive(Eq)]`, the module-binding naming class, refinement
///     narrowing, the float total-order gate). Fix is to update the
///     program; the codegen coverage is unaffected.
///   * **REAL FRONT-END GAP** — codegen implements something the
///     typechecker rejects, so the pinned shape cannot be compiled by any
///     user at all (the `Unit` type, `CStr.from_ptr` pointer mutability).
///     Fix is in the compiler, and each is worth a ledger row.
///
/// B-2026-08-12-8 emptied the front-end-gap population that this list was
/// created to track: `Set.clear`, `Vec.iter_mut` and `Column`/`Tensor`
/// `range` are now registered in the typechecker (all three were already
/// implemented in both backends), and `Vec.collect` turned out to be a
/// DELIBERATE rejection whose test program was simply illegal — fixed in
/// the program, not the compiler. What remains here is the deliberate
/// codegen-layer negative test.
///
/// Measured 2026-08-12 over the whole `tests/codegen.rs` suite (2905
/// tests): 40 programs reached codegen that `karac check` rejects.
pub const CHECK_GATE_GRANDFATHERED: &[&str] = &[
    // ── DELIBERATE NEGATIVE TEST (tests/par_codegen.rs): asserts the
    //    CODEGEN-layer rejection of an atomic op with an implicit ordering.
    //    The typechecker rejects it first, so production never reaches the
    //    codegen arm — which is the point of the test, and gating it would
    //    delete the coverage rather than fix anything.
    "test_atomic_implicit_ordering_rejected_by_codegen",
];

/// Fail loudly when an E2E test program flunks `resolve` or `typecheck`.
///
/// The sibling of [`assert_ownership_clean`], one phase earlier and for
/// the same reason: `karac build` runs both phases and refuses to reach
/// codegen when either reports an error (`cli.rs` `has_fatal_errors`
/// ORs `has_resolve_errors` and `has_type_errors` in), while the E2E
/// harnesses ran them only to feed `lower` and threw the errors away. So
/// the suite could — and did — stay green on programs no user can
/// compile: `test_e2e_set_clear` pinned `Set.clear` codegen while
/// `karac check` answered `no method 'clear' on type 'Set'` and
/// `karac build` exited 1 on the identical source. (That specific case is
/// fixed — B-2026-08-12-8 — but it is the example worth keeping, because
/// it is exactly what this gate exists to catch.)
///
/// It also makes the failure LEGIBLE in the other direction. A test
/// program with a resolve error still reaches codegen today, where the
/// mistyped call it produces trips the LLVM verifier — surfacing as
/// `codegen failed for test program: Module verification failed: Function
/// return type does not match operand type of return inst!`, which reads
/// as a backend bug and sends the reader into codegen rather than into
/// the six lines of test source that actually caused it (B-2026-08-11-34).
///
/// Existing offenders are grandfathered by test name in
/// [`CHECK_GATE_GRANDFATHERED`], so the gate lands strict for new tests
/// while the backlog is triaged incrementally — the same posture, and the
/// same thread-name matching caveat, as the ownership gate.
///
/// Wired at every site that calls [`assert_ownership_clean`] — 24 across
/// `codegen`, `par_codegen`, `memory_sanitizer`, `coro_e2e`,
/// `disjoint_differential` and `http_server` — since that call is already
/// the marker for "this harness feeds codegen production-shaped input".
/// The 40 programs the first measurement found were triaged rather than
/// parked: 32 were stale test sources and are fixed, so the list above is
/// what genuinely remains.
pub fn assert_check_clean(
    resolved: &karac::resolver::ResolveResult,
    typed: &karac::typechecker::TypeCheckResult,
    src: &str,
) {
    if resolved.errors.is_empty() && typed.errors.is_empty() {
        return;
    }
    let thread = std::thread::current();
    let test_name = thread.name().unwrap_or("<unnamed>");
    if CHECK_GATE_GRANDFATHERED.iter().any(|g| {
        if let Some(prefix) = g.strip_suffix('*') {
            let bare = test_name.rsplit("::").next().unwrap_or(test_name);
            bare.starts_with(prefix)
        } else {
            test_name == *g || test_name.ends_with(&format!("::{g}"))
        }
    }) {
        eprintln!(
            "[check-gate] {test_name}: {} resolve + {} type error(s) \
             grandfathered — see CHECK_GATE_GRANDFATHERED in \
             tests/common/mod.rs",
            resolved.errors.len(),
            typed.errors.len()
        );
        return;
    }
    let mut msg = format!(
        "[check-gate] test `{test_name}`: program fails `karac check` \
         ({} resolve + {} type error(s)) — `karac build` exits 1 on this \
         source, so codegen is being fed input it never sees in \
         production. Fix the test program, or (for a front-end gap the \
         backend already implements) file a docs/bug-ledger.jsonl entry \
         and grandfather the test in tests/common/mod.rs:\n",
        resolved.errors.len(),
        typed.errors.len()
    );
    for e in &resolved.errors {
        msg.push_str(&format!(
            "  resolve {}:{}: {}\n",
            e.span.line, e.span.column, e.message
        ));
    }
    for e in &typed.errors {
        msg.push_str(&format!(
            "  type {}:{}: {}\n",
            e.span.line, e.span.column, e.message
        ));
    }
    msg.push_str("program:\n");
    msg.push_str(src);
    panic!("{msg}");
}
