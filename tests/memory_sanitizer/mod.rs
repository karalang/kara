//! Shared harness for the `tests/memory_sanitizer.rs` fixtures.
//!
//! This is the module that used to be written inline in
//! `tests/memory_sanitizer.rs` as `mod memory_sanitizer_tests`. It keeps the helpers; the
//! fixtures themselves live in the area modules declared below.

// Re-exported so the area modules keep resolving `super::common::…`
// exactly as they did when every fixture was in one file.
pub(crate) use crate::common;
pub(crate) use karac::codegen::{
    compile_to_object, compile_to_object_sequential_lane, link_executable_with_sanitizer,
};
pub(crate) use std::path::Path;
pub(crate) use std::process::Command;
pub(crate) use std::sync::OnceLock;

// ── Area modules ─────────────────────────────
// Each file's header states which fixtures belong in it.
mod arrays;
mod attrs_ir;
mod borrows;
mod closures;
mod concurrency;
mod control;
mod drop_order;
mod enums;
mod frames;
mod generics;
mod gpu_tensor;
mod io_runtime;
mod iter_range;
mod map_set;
mod misc;
mod moves;
mod numerics;
mod option_result;
mod patterns;
mod rc_shared;
mod slices;
mod strings;
mod structs;
mod vecs;

/// Returns true if the host toolchain can produce an ASAN-linked executable.
/// Probed once per test binary run. Skipping is preferred over failing so
/// developers on hosts without a sanitizer-capable `cc` still get a green
/// `cargo test` run.
fn asan_available() -> bool {
    static AVAILABLE: OnceLock<bool> = OnceLock::new();
    *AVAILABLE.get_or_init(|| {
        if std::env::var("KARAC_SKIP_ASAN_TESTS").is_ok() {
            return false;
        }
        let probe_c = "/tmp/karac_asan_probe.c";
        let probe_exe = "/tmp/karac_asan_probe";
        if std::fs::write(probe_c, "int main(void){return 0;}\n").is_err() {
            return false;
        }
        let link_ok = Command::new("cc")
            .args(["-fsanitize=address", probe_c, "-o", probe_exe])
            .output()
            .ok()
            .map(|o| o.status.success())
            .unwrap_or(false);
        let run_ok = link_ok
            && Command::new(probe_exe)
                .output()
                .ok()
                .map(|o| o.status.success())
                .unwrap_or(false);
        let _ = std::fs::remove_file(probe_c);
        let _ = std::fs::remove_file(probe_exe);
        run_ok
    })
}

/// Compile `src`, link with ASAN, run the binary, and return both stdout
/// and the process exit status. `None` if the setup failed (runtime library
/// missing, no ASAN-capable `cc`, etc.) — tests should skip rather than fail
/// in those cases to keep the harness robust on varied hosts. A program that
/// does not parse, type-check or compile is not missing setup and panics
/// (B-2026-09-23-33, B-2026-08-08-5, B-2026-08-05-35).
///
/// B-2026-08-08-16 — AUTO-PAR IS ON, matching `karac build`'s default. The
/// harness used to pass `None` for the concurrency analysis, so ~1000
/// leak / use-after-free / double-free fixtures — the codebase's primary
/// memory gate — exercised SEQUENTIAL codegen only, while the shipped
/// compiler parallelizes. B-2026-08-08-15 existed in shipped codegen
/// precisely because of that hole, and flipping it surfaced four more
/// defects (-17, -18, -19, and the `while let` capture miss fixed
/// alongside this flip), every one of them reproducing with plain
/// `karac build`.
///
/// Leak detection is always on (Linux LSan) — the steady-state default for
/// clean-run assertions. Panic-path assertions use
/// [`run_under_asan_no_leak_check`] instead: a program that `emit_panic`s
/// aborts mid-operation, so the in-flight allocations LSan would flag are
/// abort-time, not steady-state leaks, and would spuriously flip the
/// process exit code from `emit_panic`'s 101 to LSan's 23.
fn run_under_asan(src: &str, label: &str) -> Option<(String, std::process::ExitStatus)> {
    run_under_asan_opts(src, label, true, false, true).map(|(out, _err, st)| (out, st))
}

/// Like [`run_under_asan`] but turns on ASAN's exit-time allocation stats
/// and returns stderr alongside stdout, so the caller can assert the
/// program actually allocated. See
/// [`assert_clean_asan_run_min_allocs`] for why that matters.
fn run_under_asan_counting(
    src: &str,
    label: &str,
) -> Option<(String, String, std::process::ExitStatus)> {
    run_under_asan_opts(src, label, true, true, true)
}

/// Pull ASAN's exit-time malloc count out of a `print_stats=1` stderr dump.
/// The line reads `Stats: 0M malloced (0M for red zones) by 9 calls`.
/// `None` when the runtime printed no such line (an ASAN build without
/// stats support), which callers treat as "cannot measure" rather than
/// "measured zero".
fn asan_malloc_calls(stderr: &str) -> Option<u64> {
    stderr
        .lines()
        .find(|l| l.contains("malloced") && l.contains(" by "))
        .and_then(|l| l.rsplit_once(" by "))
        .and_then(|(_, tail)| tail.split_whitespace().next())
        .and_then(|n| n.parse::<u64>().ok())
}

/// The ASAN allocation FLOOR for this host and lane — what a program that
/// does nothing already costs, before any fixture's own heap work.
///
/// B-2026-09-07-26. Every allocation predicate in this file used to compare
/// ASAN's RAW process-wide malloc count against a threshold calibrated on
/// one host, and the floor is not portable: measured with this harness,
/// **macOS 26.6 / Apple M5 reports 199 where arm64 Linux reports 10**, a
/// constant +189 the fixture never asked for. Apple's ASAN runtime does
/// more of its own start-up allocation, and none of it is the program under
/// test.
///
/// That broke the predicates in BOTH directions, and the quiet direction
/// was the worse one:
///
///   * [`assert_clean_asan_run_max_allocs`] — the by-value struct-param
///     transfer ceiling of 180 is BELOW the macOS floor alone, so the test
///     could not pass on this host however well the transfer worked. It
///     didn't: measured here, the transfer halves the fixture's
///     per-iteration allocations on macOS exactly as it does on Linux
///     (3/iteration with it, 6 without, on both). The 320-vs-131
///     disagreement that opened B-2026-09-06-68 was the floor, start to
///     finish.
///   * [`assert_clean_asan_run_min_allocs`] — the vacuous-fixture guard,
///     and this is the direction that mattered. Its thresholds run from 1
///     to 3000 and 169 of the 191 numeric ones sit at or below 199, so a
///     fixture whose payload LLVM had deleted outright still cleared its
///     floor on start-up allocations alone. That is precisely the failure
///     mode B-2026-08-04-17 built this guard to catch, disarmed without a
///     symptom on the primary development host.
///
/// Subtracting a measured floor makes every threshold mean the same thing
/// everywhere: allocations THE PROGRAM PERFORMED. Cached per lane per test
/// process, because it costs a full compile + ASAN run.
///
/// The two lanes are kept separate on principle rather than on evidence —
/// the auto-par lane could start a worker pool. Measured on macOS it does
/// NOT: both lanes floor at 199, because a program that only prints never
/// dispatches anything and the pool is built lazily. They are still
/// measured independently so that a future eager pool is absorbed instead
/// of silently inflating every auto-par fixture's count.
///
/// The floor program prints, because every fixture does; what is being
/// removed is the fixed cost of "an ASAN process that got as far as
/// `println`", not of an empty `main`. A host where the count is
/// unavailable yields 0, which leaves the old raw-count behaviour intact
/// rather than inventing a subtraction.
fn asan_alloc_floor(auto_par: bool) -> u64 {
    static SEQ: OnceLock<u64> = OnceLock::new();
    static PAR: OnceLock<u64> = OnceLock::new();
    let cell = if auto_par { &PAR } else { &SEQ };
    *cell.get_or_init(|| {
        run_under_asan_opts(
            "fn main() { println(\"floor\") }\n",
            "asan-alloc-floor",
            true,
            true,
            auto_par,
        )
        .and_then(|(_, stderr, _)| asan_malloc_calls(&stderr))
        .unwrap_or(0)
    })
}

/// Variant of [`run_under_asan`] that disables LeakSanitizer for the run.
/// Used by [`assert_asan_panics_with`]: an `emit_panic` exit aborts the
/// program partway through an operation (e.g. the `extend_from_slice`
/// source-alias guard fires after the destination Vec has grown but before
/// the old buffer is reclaimed), leaving abort-time allocations that LSan
/// reports as leaks — flipping the exit code from the expected 101 to 23
/// and masking the panic the test is actually asserting. Panic-path
/// cleanup is the OS's job at process death; steady-state leaks are
/// covered by every `assert_clean_asan_run` case.
///
/// Returns stderr alongside stdout: the panic line is on stderr
/// (B-2026-08-23-17), and the caller asserts against it.
fn run_under_asan_no_leak_check(
    src: &str,
    label: &str,
) -> Option<(String, String, std::process::ExitStatus)> {
    run_under_asan_opts(src, label, false, false, true)
}

/// `auto_par`: run the concurrency analysis and hand it to codegen, so the
/// auto-parallelizer actually fires.
///
/// B-2026-08-08-15 — WITHOUT THIS THE HARNESS PASSES `None` FOR
/// CONCURRENCY, WHICH DISABLES AUTO-PAR ENTIRELY. Every other fixture in
/// this file therefore exercises sequential codegen only, even though
/// `karac build` parallelizes by default and the header comment above
/// claims this harness "mirrors the real CLI pipeline" — on this axis it
/// does not. That is exactly why an auto-par ownership-transfer defect (a
/// leak in one shape, a use-after-free in another) lived in shipped
/// codegen with ~1000 memory fixtures green: the class was invisible here.
///
/// Opt-in rather than always-on deliberately: flipping the whole suite
/// changes what a thousand existing fixtures compile, which is a coverage
/// change to land and measure on its own, not a rider on a bug fix. The
/// broader hole is filed separately.
fn run_under_asan_opts(
    src: &str,
    label: &str,
    detect_leaks: bool,
    count_allocs: bool,
    auto_par: bool,
) -> Option<(String, String, std::process::ExitStatus)> {
    run_under_asan_lane(src, label, detect_leaks, count_allocs, auto_par, false)
}

/// [`run_under_asan_opts`] plus the SEQUENTIAL-LANE switch
/// (B-2026-09-07-21). `seq_lane` compiles through
/// `compile_to_object_sequential_lane`, which is what `KARAC_AUTO_PAR=0`
/// actually does: the analysis still runs and only the fan-out EMISSION is
/// gated. Passing `auto_par: false` is a DIFFERENT configuration — it
/// withholds the analysis altogether — and cannot reach the shapes whose
/// codegen is keyed on a populated `concurrency_decisions`.
fn run_under_asan_lane(
    src: &str,
    label: &str,
    detect_leaks: bool,
    count_allocs: bool,
    auto_par: bool,
    seq_lane: bool,
) -> Option<(String, String, std::process::ExitStatus)> {
    // Compile on a FAT-STACK thread, matching how `karac` actually runs.
    //
    // This harness drives the compiler phases IN-PROCESS, and a cargo test
    // thread gets ~2 MB. That is the one place in the repo where the
    // compiler runs on a small stack: `main.rs` puts the whole CLI on 16 MB
    // and `lib.rs`'s `run_on_interp_thread` does the same for the library
    // entry points, both because — in `main.rs`'s words — "the compiler
    // phases are deeply recursive" and the default is a platform lottery.
    // This harness never got that treatment, so its real ceiling was ~2 MB
    // while every user-facing path had 16.
    //
    // Measured (B-2026-08-26-24): compiling a `PriorityQueue[String]`
    // program overflowed at 2 MB and passed at 3, taking 0.89s — deep but
    // finite, and only a little over the line. Lowering a `T: Ord`
    // comparison to `a.cmp(b).is_lt()` added the frames that crossed it, and
    // any future change of that shape would have crossed it again. 16 MB
    // matches the two production entry points rather than inventing a third
    // number.
    // The spawned thread INHERITS THE TEST'S NAME. Several gates this
    // harness runs through — the ownership gate in `tests/common/mod.rs`
    // most sharply — identify the current test by `thread::current().name()`
    // and match it against a grandfather list. An unnamed worker reports
    // itself as `<unnamed>`, no entry matches, and two deliberately
    // grandfathered fixtures started failing with the gate's own "fix the
    // test program" message. Measured, not guessed at.
    let thread_name = std::thread::current()
        .name()
        .unwrap_or("asan-harness")
        .to_string();
    std::thread::scope(|scope| {
        std::thread::Builder::new()
            .name(thread_name)
            .stack_size(16 * 1024 * 1024)
            .spawn_scoped(scope, || {
                run_under_asan_opts_inner(
                    src,
                    label,
                    detect_leaks,
                    count_allocs,
                    auto_par,
                    seq_lane,
                )
            })
            .expect("failed to spawn asan-harness compile thread")
            .join()
            .unwrap_or_else(|payload| std::panic::resume_unwind(payload))
    })
}

/// B-2026-09-23-33 — a PARSE error in the program under test fails the
/// fixture instead of skipping it. It used to return `None`, which every
/// `assert_clean_asan_run*` helper reads as missing setup (`setup failed —
/// skipping`), so a fixture with a typo — `&&` where Kāra spells `and` — reported
/// `ok` while asserting nothing. Typecheck (B-2026-08-08-5) and codegen
/// (B-2026-08-05-35) failures already panic for the same reason: the toolchain
/// is present and the program is broken, which is never missing setup.
fn parse_failed(label: &str, errors: &[karac::parser::ParseError]) -> ! {
    panic!(
        "[{label}] PARSE FAILED — the program under test does not parse, so this \
         fixture asserts nothing.\n{errors:?}"
    );
}

fn run_under_asan_opts_inner(
    src: &str,
    label: &str,
    detect_leaks: bool,
    count_allocs: bool,
    auto_par: bool,
    seq_lane: bool,
) -> Option<(String, String, std::process::ExitStatus)> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);

    let mut parsed = karac::parse(src);
    if !parsed.errors.is_empty() {
        parse_failed(label, &parsed.errors);
    }
    // The `karac build` front end between parse and resolve — see
    // `lib.rs` `prepare_for_resolve`. Load-bearing here: it synthesizes
    // `#[derive(Default)]`'s inherent `Type.default` impl, without which a
    // derive-dependent program (std.mem `take[T: Default]`'s `T.default()`
    // dispatch) miscompiles to the const-0 fallback and double-frees — an
    // ASAN-harness-only artifact absent from shipped binaries — and it
    // splices gated stdlib imports, without which the ownership pass never
    // sees a gated fn's body and mis-scores a reused owned arg.
    karac::prepare_for_resolve(&mut parsed.program);
    let resolved = karac::resolve(&parsed.program);
    let typed = karac::typecheck(&parsed.program, &resolved);
    karac::lower(&mut parsed.program, &typed);
    // Ownership-loaded by default, mirroring `tests/codegen.rs`'s
    // `run_program`: `karac build` always passes ownership, and a
    // `None` here leaves the RC-fallback boxing surface untested —
    // exactly the divergence that hid the Option[shared] boxing
    // collision (b027fc15 bug 3) from the whole ASAN corpus.
    // B-2026-08-08-5 — TYPECHECK errors are a hard failure here, on exactly
    // the same grounds as the ownership gate below.
    //
    // This harness drives the phases by hand and, until now, only asserted
    // the OWNERSHIP result. A fixture whose program failed TYPECHECK was
    // still handed to codegen, which happily emitted something — so a test
    // written for a not-yet-supported shape could report `ok` for a program
    // `karac build` REFUSES. Found while stash-proving the `Vec[weak T]`
    // store: the fixture passed against a compiler that rejected its own
    // source, which means it was gating nothing.
    //
    // Same discrimination as the codegen-failure panic further down: a
    // signal that always means "the compiler rejected the program under
    // test" fails loudly instead of sliding through.
    if !typed.errors.is_empty() {
        panic!(
            "[{label}] TYPECHECK FAILED — this is a real failure, not missing setup.\n\
                 {}\n\
                 `karac build` would reject this program, so the fixture asserts nothing. \
                 Fix the test program, or mark the test `#[ignore = \"<gap>\"]` so it is \
                 visibly deferred rather than silently green.",
            typed
                .errors
                .iter()
                .map(|e| format!("   {} @ line {}", e.message, e.span.line))
                .collect::<Vec<_>>()
                .join("\n")
        );
    }
    let ownership = karac::ownershipcheck(&parsed.program, &typed);
    super::common::assert_check_clean(&resolved, &typed, src);
    // Effects are the THIRD phase of the same gate, and were simply
    // absent — a test could pin behaviour for a program `karac build`
    // refuses (B-2026-08-19-5). Runs after `lower`, threaded with the
    // typechecker's tables, exactly as `Pipeline::run_all_checks` does.
    super::common::assert_effects_clean_for(&parsed.program, &typed, src);
    super::common::assert_ownership_clean(&ownership, src);

    let id = COUNTER.fetch_add(1, Ordering::Relaxed);
    let obj_path = format!("/tmp/karac_asan_{}_{}.o", std::process::id(), id);
    let exe_path = format!("/tmp/karac_asan_{}_{}", std::process::id(), id);

    // B-2026-08-05-35 — a CODEGEN failure is a hard FAILURE, not a skip.
    //
    // The soft-skip below exists for MISSING INFRASTRUCTURE — no runtime
    // archive, no `cc` with `-fsanitize=address` — where the harness cannot
    // say anything about the program. A codegen failure is the opposite: the
    // toolchain is present and the compiler rejected the program under test.
    // Skipping there reported `ok` for a fixture that never ran, so a memory
    // test written for a not-yet-compiling shape could never be shown RED,
    // and a future regression that broke COMPILATION rather than ownership
    // would turn its test green.
    //
    // Same discrimination `common::link_or_skip` already applies to an
    // undefined-symbol link failure (CLAUDE.md): a signal that always means a
    // real defect panics with an actionable message instead of skipping.
    let concurrency = auto_par.then(|| {
        let effects = karac::effectcheck(&parsed.program);
        karac::concurrency_analyze_typed(&parsed.program, &effects, Some(&typed))
    });
    let compiled = if seq_lane {
        compile_to_object_sequential_lane(
            &parsed.program,
            &obj_path,
            Some(&ownership),
            concurrency.as_ref(),
        )
    } else {
        compile_to_object(
            &parsed.program,
            &obj_path,
            Some(&ownership),
            concurrency.as_ref(),
        )
    };
    if let Err(e) = compiled {
        panic!(
                "[{label}] CODEGEN FAILED — this is a real failure, not missing setup.\n                   {e}\n                   The program under test does not compile, so this fixture asserts nothing. \
                 Either fix the codegen gap, or mark the test `#[ignore = \"<gap>\"]` so it \
                 is visibly deferred rather than silently green."
            );
    }
    if !Path::new(&obj_path).exists() {
        panic!(
            "[{label}] object file missing after a SUCCESSFUL compile_to_object — \
                    the emit silently produced nothing"
        );
    }
    // Route through `link_or_skip` so this harness gets the same two
    // discriminations as the codegen E2E suite: a stale archive
    // (undefined symbol) panics with the rebuild recipe, and
    // KARAC_REQUIRE_RUNTIME_ARCHIVE=1 forbids the soft-skip outright —
    // this file's hand-rolled skip predated both and silently kept the
    // vacuous-pass holes open here.
    let link_res = link_executable_with_sanitizer(&obj_path, &exe_path, &["-fsanitize=address"]);
    if let Err(e) = &link_res {
        eprintln!("[{label}] link_executable_with_sanitizer failed: {e}");
    }
    if super::common::link_or_skip(link_res).is_none() {
        let _ = std::fs::remove_file(&obj_path);
        return None;
    }

    // LeakSanitizer (the leak-detection arm of ASAN) ships only with
    // upstream LLVM's ASAN runtime on Linux — Apple clang's macOS ASAN
    // does not include it. Setting `detect_leaks=1` on Darwin makes the
    // ASAN runtime print "detect_leaks is not supported on this platform"
    // and exit with the configured `exitcode=23`, which the harness
    // would interpret as a memory error. Drop the flag on macOS — keep
    // ASAN's double-free / invalid-free coverage there.
    // Leak-style bugs are caught separately on Linux + by the runtime
    // alloc/free counter assertion described in phase-7-codegen.md
    // (`scope_cleanup_actions` testing note).
    //
    // B-2026-09-07-40 — that list used to read "UAF / double-free /
    // heap-buffer-overflow", which overstated it on EVERY platform, not
    // just macOS. What the link flag alone buys is the ALLOCATOR
    // interposition; the memory-ACCESS checks are a compiler pass, and this
    // harness did not run it. `KARAC_SANITIZE_ADDRESS=1` does
    // (`scripts/asan-instrumented-leg.sh`), and only under that knob does
    // this suite see a use-after-free READ or WRITE or a heap-buffer
    // overflow that never reaches `free`.
    // LeakSanitizer ships only with upstream LLVM's ASAN runtime on Linux
    // (macOS Apple clang has no LSan — see the cfg below). `detect_leaks`
    // is the caller's steady-state-vs-panic-path choice; on macOS the flag
    // is moot (no LSan to disable).
    let asan_options = if cfg!(target_os = "macos") {
        "abort_on_error=0:exitcode=23"
    } else if detect_leaks {
        "detect_leaks=1:abort_on_error=0:exitcode=23"
    } else {
        "detect_leaks=0:abort_on_error=0:exitcode=23"
    };
    // `print_stats=1:atexit=1` makes the ASAN runtime dump its allocation
    // totals to stderr at exit. Verified not to mask anything: a leaky
    // program still exits 23 with `ERROR: LeakSanitizer`, and a double free
    // still exits 23 with `ERROR: AddressSanitizer: attempting double-free`.
    // Off unless asked for, so the ordinary cases keep their quiet stderr.
    let asan_options = if count_allocs {
        format!("{asan_options}:print_stats=1:atexit=1")
    } else {
        asan_options.to_string()
    };
    let output = Command::new(&exe_path)
        .env("ASAN_OPTIONS", asan_options)
        .output();

    let _ = std::fs::remove_file(&obj_path);
    let _ = std::fs::remove_file(&exe_path);

    match output {
        Ok(out) => {
            let stdout = String::from_utf8_lossy(&out.stdout).to_string();
            let stderr = String::from_utf8_lossy(&out.stderr).to_string();
            if !out.status.success() {
                eprintln!("[{label}] binary exited non-zero:\n{stderr}");
            }
            Some((stdout, stderr, out.status))
        }
        Err(e) => {
            eprintln!("[{label}] failed to run binary: {e}");
            None
        }
    }
}

/// Assert a program panics under ASAN (exit code 101) with `emit_panic`'s
/// `fprintf(stderr) + exit(101)` shape, and that the panic message appears
/// on STDERR. Skips on hosts lacking ASAN. Counterpart to
/// `assert_clean_asan_run` for runtime-guard tests (e.g. the
/// `extend_from_slice` source-alias guard) where the codegen
/// deliberately rejects a misuse rather than silently corrupting.
fn assert_asan_panics_with(src: &str, expected_substring: &str, label: &str) {
    if !asan_available() {
        eprintln!("[{label}] ASAN unavailable on this host — skipping");
        return;
    }
    // Leak detection OFF: the panic aborts mid-operation, so any in-flight
    // allocation LSan would report is an abort-time artifact, not a
    // steady-state leak — and would flip the exit code 101 -> 23, masking
    // the panic this assertion exists to verify. See
    // `run_under_asan_no_leak_check`.
    let Some((stdout, stderr, status)) = run_under_asan_no_leak_check(src, label) else {
        eprintln!("[{label}] setup failed — skipping");
        return;
    };
    // `emit_panic` exits with the spec's panic code 101 (B-2026-08-23-17).
    // `success()` is false; ASAN's own exit code (23) would indicate a
    // memory error rather than the expected panic, so check for exactly
    // 101 to disambiguate.
    assert_eq!(
        status.code(),
        Some(101),
        "[{label}] expected exit code 101 from emit_panic; got {:?}. \
             stdout was: {stdout:?}, stderr was: {stderr:?}",
        status.code(),
    );
    assert!(
        stderr.contains(expected_substring),
        "[{label}] panic message missing {expected_substring:?}; \
             stderr was: {stderr:?}, stdout was: {stdout:?}",
    );
}

/// Like [`run_under_asan`] but threads the FULL analysis pipeline —
/// ownership AND concurrency — into codegen, matching what `karac
/// build` ships. The default harness passes `None, None`, under
/// which the auto-par lowering (and every RC-fallback path) is dead
/// code; the slot-ownership UAF this variant exists to pin
/// (Map-handle published through a par return slot, then freed by
/// the producing branch) was invisible to it. See the bugs.md
/// harness-gap entry for the broader divergence.
fn run_under_asan_with_full_pipeline(
    src: &str,
    label: &str,
) -> Option<(String, std::process::ExitStatus)> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);

    let mut parsed = karac::parse(src);
    if !parsed.errors.is_empty() {
        parse_failed(label, &parsed.errors);
    }
    // The `karac build` front end between parse and resolve — see
    // `lib.rs` `prepare_for_resolve`. Load-bearing here: it synthesizes
    // `#[derive(Default)]`'s inherent `Type.default` impl, without which a
    // derive-dependent program (std.mem `take[T: Default]`'s `T.default()`
    // dispatch) miscompiles to the const-0 fallback and double-frees — an
    // ASAN-harness-only artifact absent from shipped binaries — and it
    // splices gated stdlib imports, without which the ownership pass never
    // sees a gated fn's body and mis-scores a reused owned arg.
    karac::prepare_for_resolve(&mut parsed.program);
    let resolved = karac::resolve(&parsed.program);
    let typed = karac::typecheck(&parsed.program, &resolved);
    karac::lower(&mut parsed.program, &typed);
    let effects = karac::effectcheck(&parsed.program);
    let ownership = karac::ownershipcheck(&parsed.program, &typed);
    super::common::assert_check_clean(&resolved, &typed, src);
    // Effects are the THIRD phase of the same gate, and were simply
    // absent — a test could pin behaviour for a program `karac build`
    // refuses (B-2026-08-19-5). Runs after `lower`, threaded with the
    // typechecker's tables, exactly as `Pipeline::run_all_checks` does.
    super::common::assert_effects_clean_for(&parsed.program, &typed, src);
    super::common::assert_ownership_clean(&ownership, src);
    let analysis = karac::concurrency_analyze_typed(&parsed.program, &effects, Some(&typed));

    let id = COUNTER.fetch_add(1, Ordering::Relaxed);
    let obj_path = format!("/tmp/karac_asan_cc_{}_{}.o", std::process::id(), id);
    let exe_path = format!("/tmp/karac_asan_cc_{}_{}", std::process::id(), id);

    if let Err(e) = compile_to_object(
        &parsed.program,
        &obj_path,
        Some(&ownership),
        Some(&analysis),
    ) {
        // B-2026-08-05-35 — see the primary helper: a codegen failure is the
        // defect under test, not absent setup.
        panic!(
            "[{label}] CODEGEN FAILED — this is a real failure, not missing setup.\n  \
                 {e}\n  \
                 Either fix the codegen gap, or mark the test `#[ignore = \"<gap>\"]`."
        );
    }
    // Same link_or_skip routing as the primary harness: stale archive
    // panics; KARAC_REQUIRE_RUNTIME_ARCHIVE=1 forbids the skip.
    let link_res = link_executable_with_sanitizer(&obj_path, &exe_path, &["-fsanitize=address"]);
    if let Err(e) = &link_res {
        eprintln!("[{label}] link_executable_with_sanitizer failed: {e}");
    }
    if super::common::link_or_skip(link_res).is_none() {
        let _ = std::fs::remove_file(&obj_path);
        return None;
    }
    let asan_options = if cfg!(target_os = "macos") {
        "abort_on_error=0:exitcode=23"
    } else {
        "detect_leaks=1:abort_on_error=0:exitcode=23"
    };
    let output = Command::new(&exe_path)
        .env("ASAN_OPTIONS", asan_options)
        .output();
    let _ = std::fs::remove_file(&obj_path);
    let _ = std::fs::remove_file(&exe_path);
    match output {
        Ok(out) => {
            let stdout = String::from_utf8_lossy(&out.stdout).to_string();
            if !out.status.success() {
                let stderr = String::from_utf8_lossy(&out.stderr);
                eprintln!("[{label}] binary exited non-zero:\n{stderr}");
            }
            Some((stdout, out.status))
        }
        Err(e) => {
            eprintln!("[{label}] failed to run binary: {e}");
            None
        }
    }
}

/// One `ALLOCAUDIT` row for the corpus sweep — see [`assert_clean_asan_run`].
///
/// `guard` names the threshold the fixture is held to (`min:60`, `max:12`,
/// or `-` when it has none), because a sweep that reports only a COUNT
/// cannot compute a fixture's MARGIN, and the margin is the number that
/// says whether the guard is sound. B-2026-09-09-4:
/// `rc_fb_twin_shape_both_boxed` sits 5 allocations above its own
/// `min_allocs` on x86_64 Linux and its raw count moves by 7 between runs
/// of ONE binary, so it fails under full-suite load and passes alone — and
/// the four-column row made that indistinguishable from a healthy fixture.
/// Emitting the threshold is what turns the sweep into a margin table.
fn alloc_audit_row(stderr: &str, label: &str, guard: &str, auto_par: bool) {
    if !std::env::var("KARAC_ASAN_ALLOC_AUDIT").is_ok_and(|v| v != "0") {
        return;
    }
    let raw = asan_malloc_calls(stderr).map_or(-1, |n| n as i64);
    let floor = asan_alloc_floor(auto_par) as i64;
    eprintln!(
        "ALLOCAUDIT\t{}\t{raw}\t{floor}\t{guard}\t{label}",
        (raw - floor).max(-1)
    );
}

fn assert_clean_asan_run(src: &str, expected_stdout: &[&str], label: &str) {
    if !asan_available() {
        eprintln!("[{label}] ASAN unavailable on this host — skipping");
        return;
    }
    // Corpus sweep for fixtures that allocate nothing at `-O2` and so assert
    // nothing (B-2026-08-04-17):
    //
    //   KARAC_ASAN_ALLOC_AUDIT=1 cargo test --features llvm \
    //     --test memory_sanitizer -- --nocapture
    //
    // prints one `ALLOCAUDIT\t<program>\t<raw>\t<floor>\t<guard>\t<label>` line
    // per
    // fixture, where `<program>` is `<raw> - <floor>` — the allocations the
    // program itself performed. A fixture at 0 there did no heap work of its
    // own and asserts nothing.
    //
    // The floor is MEASURED per host rather than assumed (B-2026-09-07-26).
    // This comment used to say a program that allocates nothing "reports 3",
    // which is a Linux number: macOS reports 199. Reading a raw column
    // against a remembered constant is how that row's disagreement survived
    // two sessions. Off by default: it costs an extra ASAN option and a very
    // noisy stderr. Fixtures that should never be allowed to drift back to
    // zero take a hard floor via [`assert_clean_asan_run_min_allocs`]
    // instead.
    let audit = std::env::var("KARAC_ASAN_ALLOC_AUDIT").is_ok_and(|v| v != "0");
    let ran = if audit {
        run_under_asan_counting(src, label).map(|(out, err, st)| {
            alloc_audit_row(&err, label, "-", true);
            (out, st)
        })
    } else {
        run_under_asan(src, label)
    };
    let Some((stdout, status)) = ran else {
        eprintln!("[{label}] setup failed — skipping");
        return;
    };
    assert!(
        status.success(),
        "[{label}] ASAN reported a memory error (exit code {:?}). \
             See stderr above — look for `ERROR: LeakSanitizer`, \
             `ERROR: AddressSanitizer: heap-use-after-free`, or `double-free`.",
        status.code()
    );
    let got: Vec<&str> = stdout.trim().lines().collect();
    assert_eq!(
        got, expected_stdout,
        "[{label}] unexpected stdout (ASAN passed, but output mismatched)"
    );
}

/// [`assert_clean_asan_run`] plus a floor on how many allocations the
/// program actually performed. B-2026-08-04-17.
///
/// A clean ASAN run only means nothing went wrong with the memory the
/// program touched — it says nothing about whether the program touched the
/// memory the fixture was written to exercise, and at `-O2` the answer is
/// often no. Two ways a heap fixture silently stops allocating: content the
/// optimizer can fold (a constant-seeded loop folds to its final total; an
/// f-string built from a literal becomes a static), and a buffer whose
/// bytes are never read (touched only through `.len()`), which is a
/// provably dead allocation LLVM deletes outright. Measured on one program
/// shape with ~1200 intended payload allocations: 1 alloc const-seeded, 6
/// with an opaque seed but `.len()`-only reads, 3206 with an opaque seed
/// and a byte-level read. The first two pass against a compiler that
/// aborts on the third.
///
/// So: seed from `env.args().len()` (a stable 1 here — the binary is
/// exec'd with no extra argv — while staying opaque to the optimizer),
/// read the payload's bytes rather than just its length, and set
/// `min_allocs` to a floor comfortably under the intended count but far
/// above what a folded-away version would reach.
///
/// Skips the check, rather than failing, when the ASAN runtime prints no
/// stats line — that is "cannot measure", not "measured zero".
/// [`assert_clean_asan_run_min_allocs`] with the AUTO-PARALLELIZER ON —
/// i.e. what `karac build` actually does to this program.
///
/// B-2026-08-08-15. See `run_under_asan_opts`'s `auto_par` doc for why this
/// is a separate entry point and not the default.
/// B-2026-08-29-63 — like [`assert_clean_asan_run_min_allocs_auto_par`] but
/// asserting a CEILING, because the defect this guards is a cost rather than
/// a crash.
///
/// Passing an own-heap struct BY VALUE used to deep-copy its heap fields at
/// every call even though the argument was MOVED, so the fixture below
/// allocated one extra element buffer per call. Correctness cannot catch
/// that — the pre-fix program was clean, balanced and printed the right
/// answer — so the regression test has to be a count. The ceiling is what
/// fails on the pre-fix compiler.
///
/// The clean-exit and stdout assertions come along for the opposite reason:
/// they are what fails if the transfer is ever widened past the shapes the
/// whole-program prepass admits. Both directions matter, so both are here.
/// Run a fixture with AUTO-PAR OFF and assert it is clean.
///
/// B-2026-09-07-30 — the auto-par-on configuration of these fixtures still
/// strands the 40-byte RC-fallback BOX, which is a SEPARATE defect with its
/// own trigger set (it reproduces with no projection at all, on an
/// owned-`self` method receiver, and disappears entirely when the program
/// is BUILT with `KARAC_AUTO_PAR=0`). Asserting leak-freedom under fan-out
/// here would fail on that row's defect rather than this one's, so the
/// double-free fix is asserted in the lane where these cells are fully
/// clean. The auto-par lane is still covered — for OUTPUT and EXIT STATUS,
/// which is what the double free actually broke — by the `codegen.rs` and
/// `par_codegen.rs` twins, neither of which inspects leaks.
/// Run a fixture in the SEQUENTIAL LANE — analysis on, fan-out emission
/// off — and assert it is clean. This is `KARAC_AUTO_PAR=0`
/// (B-2026-09-07-21).
///
/// NOT interchangeable with [`assert_clean_asan_run_no_auto_par`], which
/// withholds the concurrency analysis entirely. Every codegen predicate
/// keyed on `concurrency_decisions` takes its no-analysis path there, so
/// that helper reports CLEAN for the whole class of defects that only
/// appear when the table is populated and the backend is gated — which is
/// exactly the configuration a real `KARAC_AUTO_PAR=0` build produces.
fn assert_clean_asan_run_seq_lane(src: &str, expected_stdout: &[&str], label: &str) {
    if !asan_available() {
        eprintln!("[{label}] ASAN unavailable on this host — skipping");
        return;
    }
    let Some((stdout, _stderr, status)) = run_under_asan_lane(src, label, true, true, true, true)
    else {
        eprintln!("[{label}] setup failed — skipping");
        return;
    };
    assert!(
        status.success(),
        "[{label}] ASAN reported a memory error (exit code {:?}) in the SEQUENTIAL lane. \
             A `LeakSanitizer` report here with the auto-par lane clean means a registration \
             was declined for a fan-out worker that this lane never emits.",
        status.code()
    );
    let got: Vec<&str> = stdout.trim().lines().collect();
    assert_eq!(got, expected_stdout, "[{label}] stdout mismatch");
}

fn assert_clean_asan_run_no_auto_par(src: &str, expected_stdout: &[&str], label: &str) {
    if !asan_available() {
        eprintln!("[{label}] ASAN unavailable on this host — skipping");
        return;
    }
    let Some((stdout, _stderr, status)) = run_under_asan_opts(src, label, true, true, false) else {
        eprintln!("[{label}] setup failed — skipping");
        return;
    };
    assert!(
        status.success(),
        "[{label}] ASAN reported a memory error (exit code {:?}). A `double-free` here \
             means the destination and the RC-fallback box both own the projected buffer; a \
             `LeakSanitizer` report means neither does.",
        status.code()
    );
    let got: Vec<&str> = stdout.trim().lines().collect();
    assert_eq!(got, expected_stdout, "[{label}] stdout mismatch");
}

fn assert_clean_asan_run_max_allocs(
    src: &str,
    expected_stdout: &[&str],
    label: &str,
    max_allocs: u64,
) {
    if !asan_available() {
        eprintln!("[{label}] ASAN unavailable on this host — skipping");
        return;
    }
    let Some((stdout, stderr, status)) = run_under_asan_opts(src, label, true, true, false) else {
        eprintln!("[{label}] setup failed — skipping");
        return;
    };
    assert!(
        status.success(),
        "[{label}] ASAN reported a memory error (exit code {:?}). See stderr above — \
             a `double-free` here means the caller and the callee both own the transferred \
             buffer; a `LeakSanitizer` report means neither does.",
        status.code()
    );
    let got: Vec<&str> = stdout.trim().lines().collect();
    assert_eq!(got, expected_stdout, "[{label}] stdout mismatch");
    alloc_audit_row(&stderr, label, &format!("max:{max_allocs}"), false);
    if let Some(raw) = asan_malloc_calls(&stderr) {
        // Floor-relative, so the ceiling means the same thing on every host
        // — see [`asan_alloc_floor`]. Raw counts differ by 189 between
        // macOS and Linux for reasons that have nothing to do with the
        // program (B-2026-09-07-26).
        let floor = asan_alloc_floor(false);
        let allocs = raw.saturating_sub(floor);
        assert!(
            allocs <= max_allocs,
            "[{label}] {allocs} malloc calls by the program ({raw} raw, minus a {floor} \
                 host floor), over the {max_allocs} ceiling — the by-value struct param is \
                 still being ENTRY-COPIED at each call. Check that \
                 `param_transfer::compute_transferable_struct_params` still admits the callee \
                 and that `struct_param_transfer_eligible` still admits the type."
        );
    }
}

fn assert_clean_asan_run_min_allocs_auto_par(
    src: &str,
    expected_stdout: &[&str],
    label: &str,
    min_allocs: u64,
) {
    if !asan_available() {
        eprintln!("[{label}] ASAN unavailable on this host — skipping");
        return;
    }
    let Some((stdout, stderr, status)) = run_under_asan_opts(src, label, true, true, true) else {
        eprintln!("[{label}] setup failed — skipping");
        return;
    };
    assert!(
        status.success(),
        "[{label}] ASAN reported a memory error under AUTO-PAR (exit code {:?}). \
             See stderr above — look for `ERROR: LeakSanitizer`, \
             `ERROR: AddressSanitizer: heap-use-after-free`, or `double-free`.",
        status.code()
    );
    let got: Vec<&str> = stdout.trim().lines().collect();
    assert_eq!(got, expected_stdout, "[{label}] stdout mismatch");
    alloc_audit_row(&stderr, label, &format!("min:{min_allocs}"), true);
    if let Some(raw) = asan_malloc_calls(&stderr) {
        let floor = asan_alloc_floor(true);
        let allocs = raw.saturating_sub(floor);
        assert!(
            allocs >= min_allocs,
            "[{label}] only {allocs} malloc calls by the program ({raw} raw, minus a \
                 {floor} host floor) — under the {min_allocs} floor, so the program was \
                 optimized away and the fixture asserts nothing"
        );
    }
}

fn assert_clean_asan_run_min_allocs(
    src: &str,
    expected_stdout: &[&str],
    label: &str,
    min_allocs: u64,
) {
    if !asan_available() {
        eprintln!("[{label}] ASAN unavailable on this host — skipping");
        return;
    }
    let Some((stdout, stderr, status)) = run_under_asan_counting(src, label) else {
        eprintln!("[{label}] setup failed — skipping");
        return;
    };
    assert!(
        status.success(),
        "[{label}] ASAN reported a memory error (exit code {:?}). \
             See stderr above — look for `ERROR: LeakSanitizer`, \
             `ERROR: AddressSanitizer: heap-use-after-free`, or `double-free`.",
        status.code()
    );
    let got: Vec<&str> = stdout.trim().lines().collect();
    assert_eq!(
        got, expected_stdout,
        "[{label}] unexpected stdout (ASAN passed, but output mismatched)"
    );
    // Join the `KARAC_ASAN_ALLOC_AUDIT` sweep too, so a corpus scan sees
    // every fixture rather than only the unfloored ones.
    alloc_audit_row(&stderr, label, &format!("min:{min_allocs}"), true);
    match asan_malloc_calls(&stderr) {
        Some(raw) => {
            // Floor-relative — see [`asan_alloc_floor`]. Raw, this guard was
            // vacuous on macOS for every threshold at or below 199
            // (B-2026-09-07-26): start-up allocations alone cleared it, so a
            // fixture whose payload had been deleted still passed.
            let floor = asan_alloc_floor(true);
            let n = raw.saturating_sub(floor);
            assert!(
                n >= min_allocs,
                "[{label}] VACUOUS FIXTURE: the program performed {n} allocations \
                     ({raw} raw, minus a {floor} host floor), below the floor of \
                     {min_allocs}. A clean ASAN run over allocations that never happened \
                     proves nothing. The optimizer has most likely folded the payload away \
                     or deleted it as dead — check that the seed is runtime-opaque and that \
                     the buffer's BYTES are read, not just its length."
            );
        }
        None => eprintln!(
            "[{label}] ASAN printed no allocation stats — min_allocs={min_allocs} unchecked"
        ),
    }
}

/// Variant of `run_under_asan` that threads `OwnershipCheckResult` into
/// codegen. The plain `run_under_asan` passes `None`, which leaves the
/// `arc_fallback_fns` table empty — so atomic-RC inc/dec on
/// `arc_values`-promoted bindings would never fire from that harness.
/// The atomic-RC slice's race-detection check needs the full pipeline.
fn run_under_asan_with_ownership(
    src: &str,
    label: &str,
) -> Option<(String, std::process::ExitStatus)> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);

    let mut parsed = karac::parse(src);
    if !parsed.errors.is_empty() {
        parse_failed(label, &parsed.errors);
    }
    // The `karac build` front end between parse and resolve — see
    // `lib.rs` `prepare_for_resolve`. Load-bearing here: it synthesizes
    // `#[derive(Default)]`'s inherent `Type.default` impl, without which a
    // derive-dependent program (std.mem `take[T: Default]`'s `T.default()`
    // dispatch) miscompiles to the const-0 fallback and double-frees — an
    // ASAN-harness-only artifact absent from shipped binaries — and it
    // splices gated stdlib imports, without which the ownership pass never
    // sees a gated fn's body and mis-scores a reused owned arg.
    karac::prepare_for_resolve(&mut parsed.program);
    let resolved = karac::resolve(&parsed.program);
    let typed = karac::typecheck(&parsed.program, &resolved);
    karac::lower(&mut parsed.program, &typed);
    let ownership = karac::ownershipcheck(&parsed.program, &typed);
    super::common::assert_check_clean(&resolved, &typed, src);
    // Effects are the THIRD phase of the same gate, and were simply
    // absent — a test could pin behaviour for a program `karac build`
    // refuses (B-2026-08-19-5). Runs after `lower`, threaded with the
    // typechecker's tables, exactly as `Pipeline::run_all_checks` does.
    super::common::assert_effects_clean_for(&parsed.program, &typed, src);
    super::common::assert_ownership_clean(&ownership, src);

    let id = COUNTER.fetch_add(1, Ordering::Relaxed);
    let obj_path = format!("/tmp/karac_asan_ow_{}_{}.o", std::process::id(), id);
    let exe_path = format!("/tmp/karac_asan_ow_{}_{}", std::process::id(), id);

    // B-2026-08-05-35 — a CODEGEN failure is a hard FAILURE, not a skip.
    //
    // The soft-skip below exists for MISSING INFRASTRUCTURE — no runtime
    // archive, no `cc` with `-fsanitize=address` — where the harness cannot
    // say anything about the program. A codegen failure is the opposite: the
    // toolchain is present and the compiler rejected the program under test.
    // Skipping there reported `ok` for a fixture that never ran, so a memory
    // test written for a not-yet-compiling shape could never be shown RED,
    // and a future regression that broke COMPILATION rather than ownership
    // would turn its test green.
    //
    // Same discrimination `common::link_or_skip` already applies to an
    // undefined-symbol link failure (CLAUDE.md): a signal that always means a
    // real defect panics with an actionable message instead of skipping.
    if let Err(e) = compile_to_object(&parsed.program, &obj_path, Some(&ownership), None) {
        panic!(
                "[{label}] CODEGEN FAILED — this is a real failure, not missing setup.\n                   {e}\n                   The program under test does not compile, so this fixture asserts nothing. \
                 Either fix the codegen gap, or mark the test `#[ignore = \"<gap>\"]` so it \
                 is visibly deferred rather than silently green."
            );
    }
    if !Path::new(&obj_path).exists() {
        panic!(
            "[{label}] object file missing after a SUCCESSFUL compile_to_object — \
                    the emit silently produced nothing"
        );
    }
    // Same link_or_skip routing as the primary harness: stale archive
    // panics; KARAC_REQUIRE_RUNTIME_ARCHIVE=1 forbids the skip.
    let link_res = link_executable_with_sanitizer(&obj_path, &exe_path, &["-fsanitize=address"]);
    if let Err(e) = &link_res {
        eprintln!("[{label}] link_executable_with_sanitizer failed: {e}");
    }
    if super::common::link_or_skip(link_res).is_none() {
        let _ = std::fs::remove_file(&obj_path);
        return None;
    }

    let asan_options = if cfg!(target_os = "macos") {
        "abort_on_error=0:exitcode=23"
    } else {
        "detect_leaks=1:abort_on_error=0:exitcode=23"
    };
    let output = Command::new(&exe_path)
        .env("ASAN_OPTIONS", asan_options)
        .output();

    let _ = std::fs::remove_file(&obj_path);
    let _ = std::fs::remove_file(&exe_path);

    match output {
        Ok(out) => {
            let stdout = String::from_utf8_lossy(&out.stdout).to_string();
            if !out.status.success() {
                let stderr = String::from_utf8_lossy(&out.stderr);
                eprintln!("[{label}] binary exited non-zero:\n{stderr}");
            }
            Some((stdout, out.status))
        }
        Err(e) => {
            eprintln!("[{label}] failed to run binary: {e}");
            None
        }
    }
}

fn assert_clean_asan_run_with_ownership(src: &str, label: &str) {
    if !asan_available() {
        eprintln!("[{label}] ASAN unavailable on this host — skipping");
        return;
    }
    let Some((_stdout, status)) = run_under_asan_with_ownership(src, label) else {
        eprintln!("[{label}] setup failed — skipping");
        return;
    };
    assert!(
        status.success(),
        "[{label}] ASAN reported a memory error (exit code {:?}). \
             Look for `data race`, `heap-use-after-free`, `double-free`, \
             or `LeakSanitizer` in the stderr above.",
        status.code()
    );
}

/// Variant of `run_under_asan` that threads `ConcurrencyAnalysis`
/// into codegen. Slice A (Phase-7 — Par codegen: return values)
/// turns class-(ii) let-bindings inside an inferred parallel
/// group into parent-allocated return-slot reads after
/// `karac_par_run` joins. The plain `run_under_asan` passes
/// `None` for concurrency, which leaves auto-par dispatch dormant
/// and exercises only the existing sequential codepath.
fn run_under_asan_with_concurrency(
    src: &str,
    label: &str,
) -> Option<(String, std::process::ExitStatus)> {
    use karac::codegen::compile_to_object_with_options;
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);

    let mut parsed = karac::parse(src);
    if !parsed.errors.is_empty() {
        parse_failed(label, &parsed.errors);
    }
    // The `karac build` front end between parse and resolve — see
    // `lib.rs` `prepare_for_resolve`. Load-bearing here: it synthesizes
    // `#[derive(Default)]`'s inherent `Type.default` impl, without which a
    // derive-dependent program (std.mem `take[T: Default]`'s `T.default()`
    // dispatch) miscompiles to the const-0 fallback and double-frees — an
    // ASAN-harness-only artifact absent from shipped binaries — and it
    // splices gated stdlib imports, without which the ownership pass never
    // sees a gated fn's body and mis-scores a reused owned arg.
    karac::prepare_for_resolve(&mut parsed.program);
    let resolved = karac::resolve(&parsed.program);
    let typed = karac::typecheck(&parsed.program, &resolved);
    // B-2026-08-08-18 — the auto-par helper owed the same discrimination
    // its sequential sibling gained in B-2026-08-08-5 / -7 and never got: a
    // TYPECHECK or CODEGEN failure here used to `eprintln!` and return
    // `None`, which the caller's `let Some(..) else { return }` turns into
    // a PASS. Measured: with this fixture moved onto the auto-par lane and
    // the compiler fix reverted, the program failed LLVM verification and
    // the test still reported ok — the exact vacuous-pass shape
    // B-2026-08-08-16 is about, one helper further down.
    //
    // A missing ARCHIVE or a link failure stays a soft skip (that is real
    // missing setup); a program the compiler REFUSES is always a defect.
    if !typed.errors.is_empty() {
        panic!(
            "[{label}] TYPECHECK FAILED — this is a real failure, not missing setup.\n   {}\n\
                 `karac build` would reject this program, so the fixture asserts nothing. \
                 Fix the test program, or mark the test `#[ignore = \"<gap>\"]` so it is \
                 visibly deferred rather than silently green.",
            typed
                .errors
                .iter()
                .map(|e| e.to_string())
                .collect::<Vec<_>>()
                .join("\n   ")
        );
    }
    karac::lower(&mut parsed.program, &typed);
    let effects = karac::effectcheck(&parsed.program);
    let analysis = karac::concurrency_analyze_typed(&parsed.program, &effects, Some(&typed));

    let id = COUNTER.fetch_add(1, Ordering::Relaxed);
    let obj_path = format!("/tmp/karac_asan_par_{}_{}.o", std::process::id(), id);
    let exe_path = format!("/tmp/karac_asan_par_{}_{}", std::process::id(), id);

    if let Err(e) = compile_to_object_with_options(
        &parsed.program,
        &obj_path,
        None,
        Some(&analysis),
        None,
        None,
    ) {
        panic!(
            "[{label}] CODEGEN FAILED under auto-par — this is a real failure, not missing \
                 setup.\n   {e}\n   The program under test does not compile in the DEFAULT \
                 configuration, so this fixture asserts nothing. Either fix the codegen gap, or \
                 mark the test `#[ignore = \"<gap>\"]` so it is visibly deferred rather than \
                 silently green."
        );
    }
    if !Path::new(&obj_path).exists() {
        panic!("[{label}] object file missing after a SUCCESSFUL compile_to_object");
    }
    // Same link_or_skip routing as the primary harness: stale archive
    // panics; KARAC_REQUIRE_RUNTIME_ARCHIVE=1 forbids the skip.
    let link_res = link_executable_with_sanitizer(&obj_path, &exe_path, &["-fsanitize=address"]);
    if let Err(e) = &link_res {
        eprintln!("[{label}] link_executable_with_sanitizer failed: {e}");
    }
    if super::common::link_or_skip(link_res).is_none() {
        let _ = std::fs::remove_file(&obj_path);
        return None;
    }

    let asan_options = if cfg!(target_os = "macos") {
        "abort_on_error=0:exitcode=23"
    } else {
        "detect_leaks=1:abort_on_error=0:exitcode=23"
    };
    let output = Command::new(&exe_path)
        .env("ASAN_OPTIONS", asan_options)
        .output();

    let _ = std::fs::remove_file(&obj_path);
    let _ = std::fs::remove_file(&exe_path);

    match output {
        Ok(out) => {
            let stdout = String::from_utf8_lossy(&out.stdout).to_string();
            if !out.status.success() {
                let stderr = String::from_utf8_lossy(&out.stderr);
                eprintln!("[{label}] binary exited non-zero:\n{stderr}");
            }
            Some((stdout, out.status))
        }
        Err(e) => {
            eprintln!("[{label}] failed to run binary: {e}");
            None
        }
    }
}

fn assert_clean_asan_run_with_concurrency(src: &str, expected_stdout: &[&str], label: &str) {
    if !asan_available() {
        eprintln!("[{label}] ASAN unavailable on this host — skipping");
        return;
    }
    let Some((stdout, status)) = run_under_asan_with_concurrency(src, label) else {
        eprintln!("[{label}] setup failed — skipping");
        return;
    };
    assert!(
        status.success(),
        "[{label}] ASAN reported a memory error (exit code {:?}). \
             See stderr above — look for `LeakSanitizer`, `heap-use-after-free`, \
             or `double-free`.",
        status.code()
    );
    let got: Vec<&str> = stdout.trim().lines().collect();
    assert_eq!(
        got, expected_stdout,
        "[{label}] unexpected stdout (ASAN passed, but output mismatched)"
    );
}

// ── pattern-arm unbound heap-field drop (B) ──
//
// docs/spikes/pattern-arm-unbound-field-drop.md: a fresh-temp enum
// scrutinee (`if let Full(_, n) = make()`) had no source `EnumDrop`, so an
// arm leaving a heap payload field UNBOUND leaked it (IR-proven; invisible
// on macOS — no LeakSanitizer). The fix materializes the temp +
// `track_enum_var` so the enum drop walk frees unbound fields, and zeroes
// the cap of any field the pattern MOVED into a binding so it isn't
// double-freed. The bound-field case is the macOS-reliable gate here: an
// over-eager EnumDrop (suppression not firing) would double-free the moved
// buffer against the binding's own cleanup. Loops amplify any per-iteration
// imbalance into a deterministic fault.

const B_ASAN_PRELUDE: &str = r#"
enum Holder { Full(Vec[i64], i64), Empty }
fn make() -> Holder {
    let mut v: Vec[i64] = Vec.new();
    v.push(1_i64);
    v.push(2_i64);
    return Holder.Full(v, 42_i64);
}
"#;

// while-let surface of the B fix — the per-iteration outlier. The
// materialize + EnumDrop live in the loop body's per-iteration frame, so
// each iteration's scrutinee temp drops before the next eval. A
// many-iteration drain amplifies a per-iteration imbalance (stale alloca
// cap re-freed, or a moved field double-freed against its binding) into a
// deterministic macOS fault; the unbound case is the Linux leak oracle.
// `next(i)` returns `Full` while `i < 6`, then `Empty` (heap-free miss
// variant — the noted exit-edge leak does not apply).

const B_WHILELET_PRELUDE: &str = r#"
enum Holder { Full(Vec[i64], i64), Empty }
fn next(i: i64) -> Holder {
    if i < 6 {
        let mut v: Vec[i64] = Vec.new();
        v.push(1_i64);
        v.push(2_i64);
        return Holder.Full(v, i);
    }
    return Holder.Empty;
}
"#;

/// Write a small fixture file and hand back its escaped path, so a `File`
/// fixture opens something real rather than exercising only the `Err` arm.
/// Named per label so concurrently-running fixtures never share one path.
fn file_fixture_path(label: &str) -> String {
    let p = std::env::temp_dir().join(format!("karac_asan_{label}.txt"));
    std::fs::write(&p, b"ABCDEFGH").expect("fixture write");
    p.to_str().unwrap().replace('\\', "\\\\")
}
