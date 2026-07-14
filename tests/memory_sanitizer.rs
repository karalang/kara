//! Memory-behavior E2E tests under AddressSanitizer.
//!
//! Compiles representative Kāra programs, links them with `-fsanitize=address`,
//! runs the resulting binary, and asserts a clean ASAN exit. Catches leaks,
//! use-after-free, and double-free from codegen-emitted heap operations
//! (`emit_rc_dec`, `emit_scope_vec_cleanup`, `scope_cleanup_actions`).
//!
//! Necessary-but-not-sufficient: ASAN is blind to drop *ordering* and to
//! "freed late" bugs (frees that happen at process exit rather than scope
//! exit). See `Drop-order E2E tests` and the `scope_cleanup_actions` testing
//! note in `docs/implementation_checklist/` for those gaps.
//!
//! The tests skip gracefully if the host lacks ASAN runtime support (probed
//! once on first invocation) or if `KARAC_SKIP_ASAN_TESTS=1` is set in the
//! environment.

mod common;

#[cfg(feature = "llvm")]
mod memory_sanitizer_tests {
    use karac::codegen::{compile_to_object, link_executable_with_sanitizer};
    use std::path::Path;
    use std::process::Command;
    use std::sync::OnceLock;

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
    /// and the process exit status. `None` if the setup failed (parse error,
    /// runtime library missing, etc.) — tests should skip rather than fail in
    /// those cases to keep the harness robust on varied hosts.
    ///
    /// Leak detection is always on (Linux LSan) — the steady-state default for
    /// clean-run assertions. Panic-path assertions use
    /// [`run_under_asan_no_leak_check`] instead: a program that `emit_panic`s
    /// aborts mid-operation, so the in-flight allocations LSan would flag are
    /// abort-time, not steady-state leaks, and would spuriously flip the
    /// process exit code from `emit_panic`'s 1 to LSan's 23.
    fn run_under_asan(src: &str, label: &str) -> Option<(String, std::process::ExitStatus)> {
        run_under_asan_opts(src, label, true)
    }

    /// Variant of [`run_under_asan`] that disables LeakSanitizer for the run.
    /// Used by [`assert_asan_panics_with`]: an `emit_panic` exit aborts the
    /// program partway through an operation (e.g. the `extend_from_slice`
    /// source-alias guard fires after the destination Vec has grown but before
    /// the old buffer is reclaimed), leaving abort-time allocations that LSan
    /// reports as leaks — flipping the exit code from the expected 1 to 23 and
    /// masking the panic the test is actually asserting. Panic-path cleanup is
    /// the OS's job at process death; steady-state leaks are covered by every
    /// `assert_clean_asan_run` case.
    fn run_under_asan_no_leak_check(
        src: &str,
        label: &str,
    ) -> Option<(String, std::process::ExitStatus)> {
        run_under_asan_opts(src, label, false)
    }

    fn run_under_asan_opts(
        src: &str,
        label: &str,
        detect_leaks: bool,
    ) -> Option<(String, std::process::ExitStatus)> {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);

        let mut parsed = karac::parse(src);
        if !parsed.errors.is_empty() {
            eprintln!("[{label}] parse errors: {:?}", parsed.errors);
            return None;
        }
        // Mirror the real CLI pipeline (lib.rs / cli.rs) and the codegen
        // harness (`tests/codegen.rs::run_program_capturing_inner`): desugar
        // runs between parse and resolve. It synthesizes `#[derive(...)]`
        // bodies (e.g. `#[derive(Default)]` → the inherent `Type.default`
        // impl) and expands comptime — without it a derive-dependent program
        // (std.mem `take[T: Default]`'s `T.default()` dispatch) miscompiles to
        // the const-0 fallback and double-frees, an ASAN-harness-only artifact
        // absent from shipped binaries.
        karac::desugar_program(&mut parsed.program);
        let resolved = karac::resolve(&parsed.program);
        let typed = karac::typecheck(&parsed.program, &resolved);
        karac::lower(&mut parsed.program, &typed);
        // Ownership-loaded by default, mirroring `tests/codegen.rs`'s
        // `run_program`: `karac build` always passes ownership, and a
        // `None` here leaves the RC-fallback boxing surface untested —
        // exactly the divergence that hid the Option[shared] boxing
        // collision (b027fc15 bug 3) from the whole ASAN corpus.
        let ownership = karac::ownershipcheck(&parsed.program, &typed);
        super::common::assert_ownership_clean(&ownership, src);

        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let obj_path = format!("/tmp/karac_asan_{}_{}.o", std::process::id(), id);
        let exe_path = format!("/tmp/karac_asan_{}_{}", std::process::id(), id);

        if let Err(e) = compile_to_object(&parsed.program, &obj_path, Some(&ownership), None) {
            eprintln!("[{label}] compile_to_object failed: {e}");
            return None;
        }
        if !Path::new(&obj_path).exists() {
            eprintln!("[{label}] object file missing after compile_to_object");
            return None;
        }
        if let Err(e) =
            link_executable_with_sanitizer(&obj_path, &exe_path, &["-fsanitize=address"])
        {
            // Skip silently — runtime library absent or linker unavailable.
            eprintln!("[{label}] link_executable_with_sanitizer failed: {e}");
            let _ = std::fs::remove_file(&obj_path);
            return None;
        }

        // LeakSanitizer (the leak-detection arm of ASAN) ships only with
        // upstream LLVM's ASAN runtime on Linux — Apple clang's macOS ASAN
        // does not include it. Setting `detect_leaks=1` on Darwin makes the
        // ASAN runtime print "detect_leaks is not supported on this platform"
        // and exit with the configured `exitcode=23`, which the harness
        // would interpret as a memory error. Drop the flag on macOS — keep
        // ASAN's UAF / double-free / heap-buffer-overflow coverage there.
        // Leak-style bugs are caught separately on Linux + by the runtime
        // alloc/free counter assertion described in phase-7-codegen.md
        // (`scope_cleanup_actions` testing note).
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

    /// Assert a program panics under ASAN (exit code 1) with `emit_panic`'s
    /// `printf + exit(1)` shape, and that the panic message appears on
    /// stdout. Skips on hosts lacking ASAN. Counterpart to
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
        // steady-state leak — and would flip the exit code 1 -> 23, masking
        // the panic this assertion exists to verify. See
        // `run_under_asan_no_leak_check`.
        let Some((stdout, status)) = run_under_asan_no_leak_check(src, label) else {
            eprintln!("[{label}] setup failed — skipping");
            return;
        };
        // `emit_panic` exits with code 1. `success()` is false; ASAN's own
        // exit code (23) would indicate a memory error rather than the
        // expected panic, so check for exactly 1 to disambiguate.
        assert_eq!(
            status.code(),
            Some(1),
            "[{label}] expected exit code 1 from emit_panic; got {:?}. \
             stdout was: {stdout:?}",
            status.code(),
        );
        assert!(
            stdout.contains(expected_substring),
            "[{label}] panic message missing {expected_substring:?}; \
             stdout was: {stdout:?}",
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
            eprintln!("[{label}] parse errors: {:?}", parsed.errors);
            return None;
        }
        // Mirror the real CLI pipeline (lib.rs / cli.rs) and the codegen
        // harness (`tests/codegen.rs::run_program_capturing_inner`): desugar
        // runs between parse and resolve. It synthesizes `#[derive(...)]`
        // bodies (e.g. `#[derive(Default)]` → the inherent `Type.default`
        // impl) and expands comptime — without it a derive-dependent program
        // (std.mem `take[T: Default]`'s `T.default()` dispatch) miscompiles to
        // the const-0 fallback and double-frees, an ASAN-harness-only artifact
        // absent from shipped binaries.
        karac::desugar_program(&mut parsed.program);
        let resolved = karac::resolve(&parsed.program);
        let typed = karac::typecheck(&parsed.program, &resolved);
        karac::lower(&mut parsed.program, &typed);
        let effects = karac::effectcheck(&parsed.program);
        let ownership = karac::ownershipcheck(&parsed.program, &typed);
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
            eprintln!("[{label}] compile_to_object failed: {e}");
            return None;
        }
        if let Err(e) =
            link_executable_with_sanitizer(&obj_path, &exe_path, &["-fsanitize=address"])
        {
            eprintln!("[{label}] link_executable_with_sanitizer failed: {e}");
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

    /// Assert a program runs cleanly under ASAN and produces the expected
    /// stdout. Skips (prints a notice, passes the test) if the host can't
    /// support ASAN — see `asan_available` for the rationale.
    fn assert_clean_asan_run(src: &str, expected_stdout: &[&str], label: &str) {
        if !asan_available() {
            eprintln!("[{label}] ASAN unavailable on this host — skipping");
            return;
        }
        let Some((stdout, status)) = run_under_asan(src, label) else {
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

    // ── Heap-closure-env epic Slice 1 (B-2026-06-22-2) ───────────
    // A returned capturing closure gets a reference-counted HEAP environment
    // (`emit_rc_alloc { i64 refcount, env }`); the owning `let f = make(..)`
    // binding frees it via `FreeClosureEnv` at scope exit. This asserts the RC
    // env is freed exactly once — no leak (LSan) and no use-after-free /
    // double-free (ASAN) — for the supported call shape, including a binding
    // called multiple times.

    #[test]
    fn asan_oncelock_local_binding_freed_no_leak() {
        // A local `OnceLock[i64]` binding's scope-exit `FreeOnceHandle` must
        // reclaim the runtime cell (control block + sealed value buffer) — a
        // missed `karac_runtime_once_free` leaks one cell + buffer per
        // iteration (LSan on Linux CI catches it). 40 fresh cells, each
        // set+get, so any per-iteration leak accumulates well past noise. Also
        // pins the double-set `AlreadySetError` path (a second `set` allocates
        // nothing, so it cannot leak, but exercising it guards the Err arm).
        assert_clean_asan_run(
            r#"
fn main() {
    let mut i: i64 = 0i64;
    let mut sum: i64 = 0i64;
    while i < 40i64 {
        let cell: OnceLock[i64] = OnceLock.new();
        match cell.set(i) { Ok(_) => {}, Err(_) => {}, }
        match cell.set(i) { Ok(_) => {}, Err(_) => {}, }
        match cell.get() { Some(v) => { sum = sum + v; }, None => {}, }
        i = i + 1i64;
    }
    println(sum.to_string());
}
"#,
            // sum 0..39 = 780
            &["780"],
            "oncelock_local_binding_freed_no_leak",
        );
    }

    #[test]
    fn asan_oncelock_string_set_get_no_leak() {
        // B-2026-07-12-2 heap-`T` ungate (gap 1, success-path element leak): a
        // heap-owning `OnceLock[String]` `set(v)` moves `v`'s buffer into the
        // cell; the scope-exit `FreeOnceHandle` must run the ELEMENT drop on the
        // sealed value (the `String` char buffer) before `once_free`, else every
        // iteration leaks the buffer. 40 fresh cells, each a single `set` + a
        // `get` read-back.
        assert_clean_asan_run(
            r#"
fn main() {
    let mut i: i64 = 0i64;
    let mut total: i64 = 0i64;
    while i < 40i64 {
        let cell: OnceLock[String] = OnceLock.new();
        match cell.set("hello".to_string()) { Ok(_) => {}, Err(_) => {}, }
        match cell.get() { Some(v) => { total = total + v.len(); }, None => {}, }
        i = i + 1i64;
    }
    println(total.to_string());
}
"#,
            // 40 * len("hello") = 200
            &["200"],
            "oncelock_string_set_get_no_leak",
        );
    }

    #[test]
    fn asan_oncelock_string_double_set_discard_no_leak() {
        // B-2026-07-12-2 heap-`T` ungate (gap 2, rejected-value discard leak): a
        // second `set` on a filled cell returns `Err(AlreadySetError { rejected:
        // v2 })` carrying `v2`'s `String` buffer; a `match ... { Err(_) => {} }`
        // that discards it must free that buffer (the source `set`-result temp is
        // side-effecting, so it survives DCE and genuinely leaks). 40 cells, each
        // a winning `set` + a losing discarded `set` + a `get`.
        assert_clean_asan_run(
            r#"
fn main() {
    let mut i: i64 = 0i64;
    let mut total: i64 = 0i64;
    while i < 40i64 {
        let cell: OnceLock[String] = OnceLock.new();
        match cell.set("first".to_string()) { Ok(_) => {}, Err(_) => {}, }
        match cell.set("second".to_string()) { Ok(_) => {}, Err(_) => {}, }
        match cell.get() { Some(v) => { total = total + v.len(); }, None => {}, }
        i = i + 1i64;
    }
    println(total.to_string());
}
"#,
            // first wins → get is "first" (5); 40 * 5 = 200
            &["200"],
            "oncelock_string_double_set_discard_no_leak",
        );
    }

    #[test]
    fn asan_oncelock_string_reject_recover_no_leak_or_double_free() {
        // B-2026-07-12-2 heap-`T` ungate (recover path — the double-free guard):
        // a losing `set`'s `Err(e) => use(e.rejected)` MOVES the rejected `String`
        // out and consumes it (`println`). The consuming-arm suppressor must zero
        // the source so the rejected value is freed EXACTLY once — a missed
        // suppression double-frees, a missed free (when NOT recovered) leaks. 40
        // iterations recover-and-print.
        assert_clean_asan_run(
            r#"
fn main() {
    let mut i: i64 = 0i64;
    let mut n: i64 = 0i64;
    while i < 40i64 {
        let cell: OnceLock[String] = OnceLock.new();
        match cell.set("first".to_string()) { Ok(_) => {}, Err(_) => {}, }
        match cell.set("second".to_string()) {
            Ok(_) => {}
            Err(e) => { n = n + e.rejected.len(); }
        }
        i = i + 1i64;
    }
    println(n.to_string());
}
"#,
            // 40 * len("second") = 240
            &["240"],
            "oncelock_string_reject_recover_no_leak_or_double_free",
        );
    }

    #[test]
    fn asan_oncelock_string_reject_recover_consume_no_double_free() {
        // B-2026-07-12-2 heap-`T` recover-CONSUME: `Err(e) => { let s =
        // e.rejected; ... }` MOVES the rejected `String` out into `s`, which
        // owns + frees it. The materialized source's `FreeInlineResultPayload`
        // must be SUPPRESSED on this consuming arm (else double-free with `s`);
        // the borrow-only skip must NOT fire here (a field IS moved out). The
        // read variant (`e.rejected.len()`) is the sibling test above.
        assert_clean_asan_run(
            r#"
fn main() {
    let mut i: i64 = 0i64;
    let mut n: i64 = 0i64;
    while i < 40i64 {
        let cell: OnceLock[String] = OnceLock.new();
        match cell.set("first".to_string()) { Ok(_) => {}, Err(_) => {}, }
        match cell.set("second".to_string()) {
            Ok(_) => {}
            Err(e) => { let s = e.rejected; n = n + s.len(); }
        }
        i = i + 1i64;
    }
    println(n.to_string());
}
"#,
            // 40 * len("second") = 240
            &["240"],
            "oncelock_string_reject_recover_consume_no_double_free",
        );
    }

    #[test]
    fn asan_oncelock_single_field_struct_no_leak() {
        // B-2026-07-12-2 heap-`T` ungate — a single-heap-field WRAPPER struct
        // `T` (`Holder { val: String }`, exactly 3 words, fits): the only
        // heap-bearing struct shape that clears the `wide` (>3-word) gate, since
        // a `String`/`Vec` field is itself 3 words so any 2-field struct is >=4.
        // The cell's element drop (gap 1) + the discarded rejected value (gap 2)
        // both drive through `emit_struct_drop_synthesis_mono` / the transparent
        // single-field wrapper `inline_heap_payload_elem`.
        assert_clean_asan_run(
            r#"
struct Holder { val: String }
fn main() {
    let mut i: i64 = 0i64;
    let mut total: i64 = 0i64;
    while i < 40i64 {
        let cell: OnceLock[Holder] = OnceLock.new();
        match cell.set(Holder { val: "hi".to_string() }) { Ok(_) => {}, Err(_) => {}, }
        match cell.set(Holder { val: "second".to_string() }) { Ok(_) => {}, Err(_) => {}, }
        match cell.get() { Some(h) => { total = total + h.val.len(); }, None => {}, }
        i = i + 1i64;
    }
    println(total.to_string());
}
"#,
            // first wins → get "hi" (2); 40 * 2 = 80
            &["80"],
            "oncelock_single_field_struct_no_leak",
        );
    }

    #[test]
    fn asan_oncelock_vec_set_get_no_leak() {
        // B-2026-07-12-2 heap-`T` ungate — `OnceLock[Vec[i64]]` (Vec is also a
        // 3-word `{ptr,len,cap}` fitting `T`): the moved-in Vec buffer must be
        // freed by the cell's element drop.
        assert_clean_asan_run(
            r#"
fn main() {
    let mut i: i64 = 0i64;
    let mut total: i64 = 0i64;
    while i < 40i64 {
        let cell: OnceLock[Vec[i64]] = OnceLock.new();
        let mut v: Vec[i64] = Vec.new();
        v.push(1i64);
        v.push(2i64);
        match cell.set(v) { Ok(_) => {}, Err(_) => {}, }
        match cell.get() { Some(g) => { total = total + g.len(); }, None => {}, }
        i = i + 1i64;
    }
    println(total.to_string());
}
"#,
            // 40 * 2 = 80
            &["80"],
            "oncelock_vec_set_get_no_leak",
        );
    }

    #[test]
    fn asan_generic_forward_owned_collection_param_no_leak_or_double_free() {
        // B-2026-07-13-2: a bare generic param bound to a String/Vec forwarded
        // through a nested generic call (leg A) and a `Vec[i64]` bound to a
        // generic param (leg B) must deep-copy on return. 200 iterations: each
        // call frees the arg buffer + an independent returned copy; a missed
        // copy double-frees (ASAN) and a leaked copy accumulates (LSan). Was:
        // aborted with double-free before the fix.
        assert_clean_asan_run(
            r#"
fn id[T](x: T) -> T { x }
fn twice[T](x: T) -> T { id(x) }
fn main() {
    let mut i: i64 = 0i64;
    let mut total: i64 = 0i64;
    while i < 200i64 {
        let s = twice(f"str{i}");
        total = total + s.len();
        let mut v: Vec[i64] = Vec.new();
        v.push(i);
        v.push(i);
        let w = twice(v);
        total = total + w.len();
        i = i + 1i64;
    }
    println(total.to_string());
}
"#,
            // len("str{i}")=3+digits: i 0..9 -> 4 (*10=40), 10..99 -> 5 (*90=450),
            // 100..199 -> 6 (*100=600). String total = 40+450+600 = 1090.
            // Each Vec has len 2 -> 200*2 = 400. Grand total = 1090 + 400 = 1490.
            &["1490"],
            "generic_forward_owned_collection_param_no_leak_or_double_free",
        );
    }

    #[test]
    fn asan_generic_enum_heap_payload_bind_return_no_leak_or_double_free() {
        // B-2026-07-13-3: a GENERIC enum's bare-`T` variant payload (`enum
        // Opt[T] { Yes(T) }`) sizes its payload AREA for the erased `T` (1 word)
        // at declare time, so a heap monomorph (T=String/Vec, 3 words) is stored
        // BOXED. `match o { Opt.Yes(v) => v, Opt.No => d }` at T=String must
        // debox `v` (loading the full `{ptr,i64,i64}` from the heap box, freeing
        // the box) and return it deep-copied; the No arm returns the owned param
        // `d`. 200 iterations exercise both arms for String and Vec[i64]: a
        // missed debox / box-free leaks (LSan), a double-counted free aborts
        // (ASAN). Before the fix this failed codegen outright (`ret i64 0` vs
        // `{ptr,i64,i64}` module-verification failure), so it never linked.
        assert_clean_asan_run(
            r#"
enum Opt[T] { Yes(T), No }
fn get[T](o: Opt[T], d: T) -> T { match o { Opt.Yes(v) => v, Opt.No => d } }
fn main() {
    let mut i: i64 = 0i64;
    let mut total: i64 = 0i64;
    while i < 200i64 {
        let a: String = get(Opt.Yes(f"yes{i}"), f"fb");
        total = total + a.len();
        let b: String = get(Opt.No, f"no{i}");
        total = total + b.len();
        let mut vv: Vec[i64] = Vec.new();
        vv.push(i);
        vv.push(i);
        vv.push(i);
        let empty: Vec[i64] = Vec.new();
        let w: Vec[i64] = get(Opt.Yes(vv), empty);
        total = total + w.len();
        i = i + 1i64;
    }
    println(total.to_string());
}
"#,
            // String Yes ("yes{i}", len 3+digits): 10*4 + 90*5 + 100*6 = 1090.
            // String No ("no{i}", len 2+digits):   10*3 + 90*4 + 100*5 = 890.
            // Vec Yes (len 3 each): 200*3 = 600. Grand total = 1090+890+600 = 2580.
            &["2580"],
            "generic_enum_heap_payload_bind_return_no_leak_or_double_free",
        );
    }

    #[test]
    fn asan_generic_enum_struct_heap_payload_bind_no_leak_or_double_free() {
        // B-2026-07-13-3, user-struct payload sibling: a generic enum's bare-`T`
        // payload resolved to a USER STRUCT with a heap field (`enum Opt[T] {
        // Yes(T) }` at `T = struct Box { s: String }`) is also stored BOXED (the
        // 1-word erased area can't hold the 1-field `{ {ptr,i64,i64} }` struct).
        // The debox must rebuild at the struct's exact aggregate (not the 3-word
        // vec heuristic) and the moved-out struct's inner String must be freed
        // exactly once. 500 iters, both arms. Before the extension this rebuilt
        // as `{ptr,i64,i64}` and failed module verification (`ret i64 0`).
        assert_clean_asan_run(
            r#"
struct Box { s: String }
enum Opt[T] { Yes(T), No }
fn get[T](o: Opt[T], d: T) -> T { match o { Opt.Yes(v) => v, Opt.No => d } }
fn main() {
    let mut i: i64 = 0;
    let mut total: i64 = 0;
    while i < 500 {
        let b: Box = Box { s: f"inside-{i}" };
        let db: Box = Box { s: f"default" };
        let r: Box = get(Opt.Yes(b), db);
        total = total + r.s.len();
        let db2: Box = Box { s: f"fallback-{i}" };
        let r2: Box = get(Opt.No, db2);
        total = total + r2.s.len();
        i = i + 1;
    }
    println(total.to_string());
}
"#,
            &["10780"],
            "generic_enum_struct_heap_payload_bind_no_leak_or_double_free",
        );
    }

    #[test]
    fn asan_string_from_owned_source_copies_no_double_free() {
        // B-2026-07-13-8: `String.from(<String>)` returned the source aggregate
        // UNCHANGED (an alias of its `{ptr,len,cap}` buffer), so a fresh owned
        // source — an f-string temp `String.from(f"x")` or an owned String
        // binding `String.from(s)` — was freed BOTH by its own scope-exit
        // cleanup and by the result binding (`free(): double free detected in
        // tcache 2` under JIT/native; interpreter's value-copy was correct). The
        // fix builds a fresh owned copy (the `From` owning contract), so each
        // buffer frees exactly once. 300 iters over all three source shapes
        // (f-string temp, owned binding, string literal): a missed copy
        // double-frees (ASAN); a copy that failed to free the source leaks
        // (LSan).
        assert_clean_asan_run(
            r#"
fn main() {
    let mut i: i64 = 0;
    let mut total: i64 = 0;
    while i < 300 {
        let a: String = String.from(f"fstr{i}");
        total = total + a.len();
        let s: String = f"owned{i}";
        let b: String = String.from(s);
        total = total + b.len();
        let c: String = String.from("literal");
        total = total + c.len();
        i = i + 1;
    }
    println(total.to_string());
}
"#,
            &["6380"],
            "string_from_owned_source_copies_no_double_free",
        );
    }

    #[test]
    fn asan_owned_string_param_if_branch_return_no_leak_or_double_free() {
        // B-2026-07-13-1: an owned String param returned from an `if` branch
        // tail deep-copies (the caller retains the arg buffer). 200 iterations:
        // each call frees two arg temps + one deep-copied result, so a missed
        // copy (double-free, ASAN) or a leaked copy (LSan on Linux CI) both
        // accumulate well past noise. Was: aborted with double-free on the
        // first call before the fix.
        assert_clean_asan_run(
            r#"
fn pick(a: String, b: String) -> String {
    if a > b { a } else { b }
}

fn main() {
    let mut i: i64 = 0i64;
    let mut total: i64 = 0i64;
    while i < 200i64 {
        let r = pick(f"apple{i}", f"banana{i}");
        total = total + r.len();
        i = i + 1i64;
    }
    println(total.to_string());
}
"#,
            // "banana{i}" is chosen for i 0..199; len is 7 for i<10 (banana0..9),
            // 8 for 10..99, 9 for 100..199. 10*7 + 90*8 + 100*9 = 70+720+900 = 1690.
            &["1690"],
            "owned_string_param_if_branch_return_no_leak_or_double_free",
        );
    }

    #[test]
    fn asan_owned_vec_param_match_arm_return_no_leak_or_double_free() {
        // B-2026-07-13-1, nested-heap `match`-arm sibling: a `Vec[String]`
        // param returned from a `match` arm deep-copies the outer buffer AND
        // each String element. 100 iterations catch any element or outer leak.
        assert_clean_asan_run(
            r#"
fn choose(a: Vec[String], b: Vec[String], first: bool) -> Vec[String] {
    match first {
        true => a,
        false => b,
    }
}

fn main() {
    let mut i: i64 = 0i64;
    let mut total: i64 = 0i64;
    while i < 100i64 {
        let mut x: Vec[String] = Vec.new();
        x.push(f"one{i}");
        x.push(f"two{i}");
        let mut y: Vec[String] = Vec.new();
        y.push(f"three{i}");
        let r = choose(x, y, false);
        total = total + r.len();
        i = i + 1i64;
    }
    println(total.to_string());
}
"#,
            // `choose(.., false)` returns y (len 1) each of 100 iters = 100.
            &["100"],
            "owned_vec_param_match_arm_return_no_leak_or_double_free",
        );
    }

    #[test]
    fn asan_oncelock_wide_allscalar_no_leak() {
        // B-2026-07-12-2 gap 3 — a WIDE all-scalar element (`Wide { a,b,c,d }`,
        // 4 words > the 3-word `Option`/`Result` inline area). `get` heap-boxes
        // the borrow (box-only free) and `set`'s `Err` payload boxes past the
        // 5-word `Result` area; the double-set discard + the get borrow must
        // both stay leak-free.
        assert_clean_asan_run(
            r#"
struct Wide { a: i64, b: i64, c: i64, d: i64 }
fn main() {
    let mut i: i64 = 0i64;
    let mut total: i64 = 0i64;
    while i < 40i64 {
        let cell: OnceLock[Wide] = OnceLock.new();
        match cell.set(Wide { a: 1i64, b: 2i64, c: 3i64, d: 4i64 }) { Ok(_) => {}, Err(_) => {}, }
        match cell.set(Wide { a: 9i64, b: 9i64, c: 9i64, d: 9i64 }) { Ok(_) => {}, Err(_) => {}, }
        match cell.get() { Some(w) => { total = total + w.a + w.b + w.c + w.d; }, None => {}, }
        i = i + 1i64;
    }
    println(total.to_string());
}
"#,
            // first wins → 1+2+3+4 = 10; 40 * 10 = 400
            &["400"],
            "oncelock_wide_allscalar_no_leak",
        );
    }

    #[test]
    fn asan_oncelock_wide_heap_struct_no_leak() {
        // B-2026-07-12-2 gap 3 — a WIDE struct-with-heap element (`Rec { id:
        // i64, name: String }`, 4 words with a heap field). `get`'s boxed
        // borrow-copy aliases the cell's `String` (box-only free leaves the
        // cell's elem-drop sole owner); the DISCARDED second-`set` rejected
        // value's inner `String` is freed by the `FreeInlineResultPayload`
        // struct-drop arm (the multi-field struct the overlay can't handle).
        assert_clean_asan_run(
            r#"
struct Rec { id: i64, name: String }
fn main() {
    let mut i: i64 = 0i64;
    let mut total: i64 = 0i64;
    while i < 40i64 {
        let cell: OnceLock[Rec] = OnceLock.new();
        match cell.set(Rec { id: 7i64, name: "first".to_string() }) { Ok(_) => {}, Err(_) => {}, }
        match cell.set(Rec { id: 8i64, name: "second".to_string() }) { Ok(_) => {}, Err(_) => {}, }
        match cell.get() { Some(r) => { total = total + r.id + r.name.len(); }, None => {}, }
        i = i + 1i64;
    }
    println(total.to_string());
}
"#,
            // first wins → 7 + len("first")=5 → 12; 40 * 12 = 480
            &["480"],
            "oncelock_wide_heap_struct_no_leak",
        );
    }

    #[test]
    fn asan_oncelock_wide_heap_struct_reject_recover_no_leak() {
        // B-2026-07-12-2 gap 3 — recover the rejected WIDE struct value out of
        // the `Err` arm (`Err(e) => let r: Rec = e.rejected`). The move-out
        // binding `r` owns the recovered struct (its scope-exit drop frees the
        // inner `String`); the consuming arm zeros the whole payload area so the
        // discard struct-drop skips (no double-free). Chained field READ recovery
        // works too; a `let`-bind avoids the deferred chained-method-receiver.
        assert_clean_asan_run(
            r#"
struct Rec { id: i64, name: String }
fn main() {
    let mut i: i64 = 0i64;
    let mut total: i64 = 0i64;
    while i < 40i64 {
        let cell: OnceLock[Rec] = OnceLock.new();
        match cell.set(Rec { id: 1i64, name: "first".to_string() }) { Ok(_) => {}, Err(_) => {}, }
        match cell.set(Rec { id: 2i64, name: "second".to_string() }) {
            Ok(_) => {},
            Err(e) => { let r: Rec = e.rejected; total = total + r.id + r.name.len(); },
        }
        i = i + 1i64;
    }
    println(total.to_string());
}
"#,
            // rejected: 2 + len("second")=6 → 8; 40 * 8 = 320
            &["320"],
            "oncelock_wide_heap_struct_reject_recover_no_leak",
        );
    }

    #[test]
    fn asan_oncelock_wide_vec_field_struct_no_leak() {
        // B-2026-07-12-2 gap 3 — a WIDE struct whose heap field is a `Vec`
        // (`Bag { tag: i64, items: Vec[i64] }`, 4 words). Exercises the
        // struct-drop's recursive `Vec` buffer free through set/get.
        assert_clean_asan_run(
            r#"
struct Bag { tag: i64, items: Vec[i64] }
fn main() {
    let mut i: i64 = 0i64;
    let mut total: i64 = 0i64;
    while i < 40i64 {
        let cell: OnceLock[Bag] = OnceLock.new();
        let mut v: Vec[i64] = Vec.new();
        v.push(10i64);
        v.push(20i64);
        match cell.set(Bag { tag: 3i64, items: v }) { Ok(_) => {}, Err(_) => {}, }
        match cell.get() { Some(b) => { total = total + b.tag + b.items.len(); }, None => {}, }
        i = i + 1i64;
    }
    println(total.to_string());
}
"#,
            // (3 + 2) * 40 = 200
            &["200"],
            "oncelock_wide_vec_field_struct_no_leak",
        );
    }

    #[test]
    fn asan_oncelock_get_or_init_heapfree_aggregate_no_leak() {
        // B-2026-07-12-2 follow-on: `get_or_init` with a heap-FREE aggregate `T`
        // (`Point { x, y }`). No element heap to leak, but the per-iteration
        // once-handle + closure env must be reclaimed cleanly across the loop.
        assert_clean_asan_run(
            r#"
struct Point { x: i64, y: i64 }
fn main() {
    let mut i: i64 = 0i64;
    let mut total: i64 = 0i64;
    while i < 40i64 {
        let cell: OnceLock[Point] = OnceLock.new();
        let p = cell.get_or_init(|| Point { x: 3i64, y: 4i64 });
        total = total + p.x + p.y;
        i = i + 1i64;
    }
    println(total.to_string());
}
"#,
            // 7 * 40 = 280
            &["280"],
            "oncelock_get_or_init_heapfree_aggregate_no_leak",
        );
    }

    #[test]
    fn asan_result_discard_struct_with_heap_no_leak() {
        // B-2026-07-12-2 gap 3 (general, NOT once-specific) — a discarded
        // fresh-temp `Result[i64, Rec]` whose `Err` payload is a multi-field
        // struct-with-heap. The seeded `Result` layout carries no drop kind and
        // the `{ptr,len,cap}` overlay only frees a single buffer at offset 0, so
        // the struct's inner `String` leaked; the `FreeInlineResultPayload`
        // struct-drop arm now frees it. `boom` is side-effecting (mutates a
        // module-less counter via a Vec push) so the discarded `Err` survives DCE.
        assert_clean_asan_run(
            r#"
struct Rec { id: i64, name: String }
fn boom(v: mut ref Vec[i64]) -> Result[i64, Rec] {
    v.push(1i64);
    Err(Rec { id: 2i64, name: "leakme".to_string() })
}
fn main() {
    let mut i: i64 = 0i64;
    let mut sink: Vec[i64] = Vec.new();
    while i < 40i64 {
        match boom(mut sink) { Ok(_) => {}, Err(_) => {}, }
        i = i + 1i64;
    }
    println(sink.len().to_string());
}
"#,
            &["40"],
            "result_discard_struct_with_heap_no_leak",
        );
    }

    #[test]
    fn asan_nested_scope_heap_shadow_no_leak_or_double_free() {
        // B-2026-07-13-6 cleanup-safety guard: the lexical-scope revert
        // (`restore_var_env`) reverts NAME maps only; heap drops stay keyed by
        // alloca in the cleanup frame, so a heap-typed shadow's inner + outer
        // buffers must each free exactly once. 200 iterations: a nested block
        // and an inner loop each shadow the outer String `s` with a fresh heap
        // buffer; a missed drop leaks and a double-drop aborts — both accumulate
        // well past noise. The outer `s` must survive every inner scope intact.
        assert_clean_asan_run(
            r#"
fn main() {
    let mut i: i64 = 0i64;
    let mut total: i64 = 0i64;
    while i < 200i64 {
        let s = f"outer{i}";
        {
            let s = f"inner{i}";
            total = total + s.len();
        }
        let mut j: i64 = 0i64;
        while j < 2i64 {
            let s = f"loop{i}";
            total = total + s.len();
            j = j + 1i64;
        }
        total = total + s.len();
        i = i + 1i64;
    }
    println(total.to_string());
}
"#,
            // Per iter i: len("inner{i}")=5+digits, +2*len("loop{i}")=4+digits,
            // +len("outer{i}")=5+digits. i 0..9 (1 digit): inner=6,loop=5*2=10,
            // outer=6 -> 22; i 10..99 (2): inner=7,loop=6*2=12,outer=7 -> 26;
            // i 100..199 (3): inner=8,loop=7*2=14,outer=8 -> 30.
            // 10*22 + 90*26 + 100*30 = 220+2340+3000 = 5560.
            &["5560"],
            "nested_scope_heap_shadow_no_leak_or_double_free",
        );
    }

    // NOTE: the field-push residual UAF half (DEFECT 2) is covered by the
    // sibling's stronger `asan_field_read_option_shared_push_no_leak_or_uaf`
    // (200-iteration loop, drains + reads back) — no duplicate here. The two
    // tests below cover the pop-consume DRAIN leak (DEFECT 1), which the
    // sibling's for-loop-drain test does not exercise.
    #[test]
    fn asan_vec_option_shared_pop_consume_no_leak() {
        // B-2026-07-12-4 (leak half) — draining a `Vec[Option[shared]]` via
        // `match vec.pop() { Some(opt) => match opt { Some(n) => .. } }` used to
        // leak every popped node: the pop result is `Option[Option[shared]]`
        // whose boxed inner-Option payload was freed WITHOUT rc-deccing the node
        // (the box drop's inner drop fn was None). The boxed-scrutinee /
        // let-binding box drop now runs the inner `Option[T]` element drop.
        // Covers both a fresh `Some(Node)` push and a field-read push, drained to
        // empty (`total = 2 + 3 = 5`).
        assert_clean_asan_run(
            r#"
shared struct Node { val: i64, mut left: Option[Node], mut right: Option[Node] }
fn main() {
    let root = Some(Node { val: 5, left: Some(Node { val: 3, left: None, right: None }), right: None });
    let mut stack: Vec[Option[Node]] = Vec.new();
    stack.push(Some(Node { val: 2, left: None, right: None }));
    match root {
        None => {}
        Some(n) => { stack.push(n.left); }
    }
    let mut total = 0;
    while stack.len() > 0 {
        let x = stack.pop();
        match x {
            Some(opt) => { match opt { Some(node) => { total = total + node.val; } None => {} } }
            None => {}
        }
    }
    println(total.to_string());
}
"#,
            &["5"],
            "vec_option_shared_pop_consume_no_leak",
        );
    }

    #[test]
    fn asan_paired_stack_same_tree_no_leak_or_uaf() {
        // B-2026-07-12-4 source (kata #100 iterative paired-stack solver): two
        // `Vec[Option[shared]]` worklists, `stack.push(node.left/right)` field
        // pushes, and an early `false` return that leaves node-pairs resident on
        // the stacks — the exact residual + field-push shape that both UAF'd
        // (residual drop) and leaked (the drained pairs). Trees differ at the
        // right child, so it returns early with residuals still on the stacks.
        assert_clean_asan_run(
            r#"
shared struct Node { val: i64, mut left: Option[Node], mut right: Option[Node] }
fn same(p: Option[Node], q: Option[Node]) -> bool {
    let mut sp: Vec[Option[Node]] = Vec.new();
    let mut sq: Vec[Option[Node]] = Vec.new();
    sp.push(p);
    sq.push(q);
    let mut result = true;
    let mut go = true;
    while go {
        if sp.len() == 0 { go = false; }
        else {
            match sp.pop() {
                Some(a) => { match sq.pop() {
                    Some(b) => { match a {
                        Some(an) => { match b {
                            Some(bn) => {
                                if an.val != bn.val { result = false; go = false; }
                                else {
                                    sp.push(an.left); sq.push(bn.left);
                                    sp.push(an.right); sq.push(bn.right);
                                }
                            }
                            None => { result = false; go = false; }
                        } }
                        None => { match b { Some(bn) => { result = false; go = false; } None => {} } }
                    } }
                    None => { go = false; }
                } }
                None => { go = false; }
            }
        }
    }
    result
}
fn main() {
    let t1 = Some(Node { val: 1, left: Some(Node { val: 2, left: None, right: None }), right: Some(Node { val: 3, left: None, right: None }) });
    let t2 = Some(Node { val: 1, left: Some(Node { val: 2, left: None, right: None }), right: Some(Node { val: 9, left: None, right: None }) });
    if same(t1, t2) { println("equal"); } else { println("different"); }
}
"#,
            &["different"],
            "paired_stack_same_tree_no_leak_or_uaf",
        );
    }

    #[test]
    fn asan_freshtemp_call_match_option_shared_no_leak() {
        // B-2026-07-12-23 — a direct `match <call returning Option[shared]>`
        // fresh-temp scrutinee leaked the extracted node once per match (6,400 B
        // / 200 iters pre-fix). The callee (`take`) rebuilds its Option through a
        // returning arm (`Some(n) => Some(n)`), so it hands back an owned +1 the
        // caller's match never dropped — the fresh-temp scrutinee's drop was
        // resolved from the erased generic Option layout (all-None drop-kinds),
        // so has_droppable was false. The lowering pass now rewrites the direct
        // call form into a let-bound scrutinee whose concrete-type cleanup
        // releases the rc (sibling of the B-21 index-read rewrite). Looped 200x
        // so any per-iteration leak accumulates well past noise; prints 1400.
        assert_clean_asan_run(
            r#"
shared struct Node { val: i64, mut left: Option[Node], mut right: Option[Node] }
fn take() -> Option[Node] {
    let mut src: Vec[Option[Node]] = Vec.new();
    src.push(Some(Node { val: 7, left: None, right: None }));
    match src[0] {
        None => None,
        Some(n) => Some(n),
    }
}
fn main() {
    let mut i: i64 = 0;
    let mut t: i64 = 0;
    while i < 200 {
        match take() {
            None => {}
            Some(n) => { t = t + n.val; }
        }
        i = i + 1;
    }
    println(f"{t}");
}
"#,
            &["1400"],
            "freshtemp_call_match_option_shared_no_leak",
        );
    }

    #[test]
    fn asan_freshtemp_literal_call_match_option_shared_no_double_free() {
        // B-2026-07-12-23 discriminator (b) — a fresh-LITERAL `match make()`
        // (callee returns a bare `Some(Node{..})`, no returning-arm rebuild) was
        // already CLEAN pre-fix. The B-23 lowering rewrite widens to all `Call`
        // scrutinees, so this case is now rewritten too; it must STAY clean — a
        // spurious extra release here would double-free the freshly-built node.
        // Guards the widened gate against over-release.
        assert_clean_asan_run(
            r#"
shared struct Node { val: i64, mut left: Option[Node], mut right: Option[Node] }
fn make() -> Option[Node] {
    Some(Node { val: 7, left: None, right: None })
}
fn main() {
    let mut i: i64 = 0;
    let mut t: i64 = 0;
    while i < 200 {
        match make() {
            None => {}
            Some(n) => { t = t + n.val; }
        }
        i = i + 1;
    }
    println(f"{t}");
}
"#,
            &["1400"],
            "freshtemp_literal_call_match_option_shared_no_double_free",
        );
    }

    #[test]
    fn asan_freshtemp_call_match_result_shared_no_leak() {
        // B-2026-07-12-24 — the `Result[shared]` sibling of the Option B-23
        // leak. A direct `match take()` where `take` returns
        // `Result[shared Node, i64]` via a returning arm (`Some(n) => Ok(n)`)
        // leaked the node once per match (6,400 B / 200 iters pre-fix): the
        // B-21/B-23 lowering rewrite routes it through a synthetic let-bound
        // scrutinee, but `Result` had no rc cleanup for that scrutinee (the
        // Option-only `track_rc_option_var` had no Result sibling). The new
        // `track_rc_result_var` registers a tag-guarded RcDecOption for the
        // synthetic `__karac_msc_*` scrutinee. Looped 200x; prints 1400.
        assert_clean_asan_run(
            r#"
shared struct Node { val: i64, mut left: Option[Node], mut right: Option[Node] }
fn take() -> Result[Node, i64] {
    let mut src: Vec[Option[Node]] = Vec.new();
    src.push(Some(Node { val: 7, left: None, right: None }));
    match src[0] {
        None => Err(1),
        Some(n) => Ok(n),
    }
}
fn main() {
    let mut i: i64 = 0;
    let mut t: i64 = 0;
    while i < 200 {
        match take() {
            Err(e) => { t = t + e; }
            Ok(n) => { t = t + n.val; }
        }
        i = i + 1;
    }
    println(f"{t}");
}
"#,
            &["1400"],
            "freshtemp_call_match_result_shared_no_leak",
        );
    }

    #[test]
    fn asan_direct_index_match_result_shared_no_leak() {
        // B-2026-07-12-24 — the index-read (`match v[i]`) `Result[shared]` case
        // (sibling of the Option B-21 index fix). The index deep-clone rc-INCs
        // the node; the synthetic let-bound scrutinee the rewrite emits now
        // releases it via `track_rc_result_var`. Looped 200x; prints 2000.
        assert_clean_asan_run(
            r#"
shared struct Node { val: i64, mut left: Option[Node], mut right: Option[Node] }
fn xfer() -> i64 {
    let mut dst: Vec[Result[Node, i64]] = Vec.new();
    dst.push(Ok(Node { val: 10, left: None, right: None }));
    let mut r: i64 = 0;
    match dst[0] {
        Err(_) => {}
        Ok(nd) => { r = nd.val; }
    }
    r
}
fn main() {
    let mut i: i64 = 0;
    let mut t: i64 = 0;
    while i < 200 {
        t = t + xfer();
        i = i + 1;
    }
    println(f"{t}");
}
"#,
            &["2000"],
            "direct_index_match_result_shared_no_leak",
        );
    }

    #[test]
    fn asan_freshtemp_call_match_result_shared_err_arm_no_leak() {
        // B-2026-07-12-24 — the `Err`-shared arm: `Result[i64, shared Node]`
        // where the payload node lives in `Err`. `track_rc_result_var`
        // registers a tag-guarded RcDecOption for BOTH arms, so the live `Err`
        // node is released. Guards the two-arm registration. Prints 1800.
        assert_clean_asan_run(
            r#"
shared struct Node { val: i64, mut left: Option[Node], mut right: Option[Node] }
fn take() -> Result[i64, Node] {
    let mut src: Vec[Option[Node]] = Vec.new();
    src.push(Some(Node { val: 9, left: None, right: None }));
    match src[0] {
        None => Ok(0),
        Some(n) => Err(n),
    }
}
fn main() {
    let mut i: i64 = 0;
    let mut t: i64 = 0;
    while i < 200 {
        match take() {
            Ok(v) => { t = t + v; }
            Err(n) => { t = t + n.val; }
        }
        i = i + 1;
    }
    println(f"{t}");
}
"#,
            &["1800"],
            "freshtemp_call_match_result_shared_err_arm_no_leak",
        );
    }

    #[test]
    fn asan_freshtemp_call_match_result_shared_rebuild_arm_no_double_free() {
        // B-2026-07-12-24 guard — a returning-arm rebuild `Ok(n) => Ok(n)` that
        // hands the node out through a consumer `match relay()` (whose own
        // synthetic scrutinee releases it) must not double-free: the scrutinee
        // release and the rebuilt value's ownership are independent. Prints 1400.
        assert_clean_asan_run(
            r#"
shared struct Node { val: i64, mut left: Option[Node], mut right: Option[Node] }
fn take() -> Result[Node, i64] {
    let mut src: Vec[Option[Node]] = Vec.new();
    src.push(Some(Node { val: 7, left: None, right: None }));
    match src[0] {
        None => Err(1),
        Some(n) => Ok(n),
    }
}
fn relay() -> Result[Node, i64] {
    match take() {
        Err(e) => Err(e),
        Ok(n) => Ok(n),
    }
}
fn main() {
    let mut i: i64 = 0;
    let mut t: i64 = 0;
    while i < 200 {
        match relay() {
            Err(e) => { t = t + e; }
            Ok(n) => { t = t + n.val; }
        }
        i = i + 1;
    }
    println(f"{t}");
}
"#,
            &["1400"],
            "freshtemp_call_match_result_shared_rebuild_arm_no_double_free",
        );
    }

    #[test]
    fn asan_let_bound_match_result_shared_no_leak() {
        // B-2026-07-12-24 (residual, escape-gated): a USER `let d = take(); match
        // d { … }` — the idiomatic bind-then-match, `d` consumed in place — used
        // to leak (only the synthetic direct-`match` scrutinee was released). The
        // conservative escape analysis (`crate::result_escape`) recognizes `d` as
        // non-escaping (used solely as a match scrutinee) and `track_rc_result_var`
        // releases it. Looped 200x; prints 1400.
        assert_clean_asan_run(
            r#"
shared struct Node { val: i64, mut left: Option[Node], mut right: Option[Node] }
fn take() -> Result[Node, i64] {
    let mut src: Vec[Option[Node]] = Vec.new();
    src.push(Some(Node { val: 7, left: None, right: None }));
    match src[0] {
        None => Err(1),
        Some(n) => Ok(n),
    }
}
fn caller() -> i64 {
    let d = take();
    match d {
        Err(e) => e,
        Ok(n) => n.val,
    }
}
fn main() {
    let mut i: i64 = 0;
    let mut t: i64 = 0;
    while i < 200 {
        t = t + caller();
        i = i + 1;
    }
    println(f"{t}");
}
"#,
            &["1400"],
            "let_bound_match_result_shared_no_leak",
        );
    }

    #[test]
    fn asan_consuming_call_result_shared_no_leak() {
        // B-2026-07-12-24 (residual, consuming-call leg): an owned `Result[shared]`
        // passed BY VALUE to a consuming fn (`eat(d)`) used to leak — neither the
        // caller (d escapes as an arg → unregistered) nor the callee (params were
        // not RC-tracked) released it. Result parameter RC-tracking, gated by the
        // same escape analysis, now makes the callee's in-place-consumed param own
        // the caller's transferred +1 and release it. A forwarded param would stay
        // unregistered (terminal consumer decs); here `eat` matches in place.
        // Looped 200x; prints 1400.
        assert_clean_asan_run(
            r#"
shared struct Node { val: i64, mut left: Option[Node], mut right: Option[Node] }
fn take() -> Result[Node, i64] {
    let mut src: Vec[Option[Node]] = Vec.new();
    src.push(Some(Node { val: 7, left: None, right: None }));
    match src[0] {
        None => Err(1),
        Some(n) => Ok(n),
    }
}
fn eat(r: Result[Node, i64]) -> i64 {
    match r {
        Err(e) => e,
        Ok(n) => n.val,
    }
}
fn caller() -> i64 {
    let d = take();
    eat(d)
}
fn main() {
    let mut i: i64 = 0;
    let mut t: i64 = 0;
    while i < 200 {
        t = t + caller();
        i = i + 1;
    }
    println(f"{t}");
}
"#,
            &["1400"],
            "consuming_call_result_shared_no_leak",
        );
    }

    #[test]
    fn asan_forwarded_result_shared_param_no_double_free() {
        // B-2026-07-12-24 (residual, consuming-call leg) chain guard: a param
        // FORWARDED to another consuming call (`eat(r) { eat2(r) }`) must NOT be
        // released by the intermediate — it escapes → unregistered → only the
        // TERMINAL consumer (`eat2`, which matches in place) decs. Exactly one
        // release across the chain: no leak, no double-free. Prints 1400.
        assert_clean_asan_run(
            r#"
shared struct Node { val: i64, mut left: Option[Node], mut right: Option[Node] }
fn take() -> Result[Node, i64] {
    let mut src: Vec[Option[Node]] = Vec.new();
    src.push(Some(Node { val: 7, left: None, right: None }));
    match src[0] {
        None => Err(1),
        Some(n) => Ok(n),
    }
}
fn eat2(r: Result[Node, i64]) -> i64 {
    match r {
        Err(e) => e,
        Ok(n) => n.val,
    }
}
fn eat(r: Result[Node, i64]) -> i64 {
    eat2(r)
}
fn caller() -> i64 {
    let d = take();
    eat(d)
}
fn main() {
    let mut i: i64 = 0;
    let mut t: i64 = 0;
    while i < 200 {
        t = t + caller();
        i = i + 1;
    }
    println(f"{t}");
}
"#,
            &["1400"],
            "forwarded_result_shared_param_no_double_free",
        );
    }

    #[test]
    fn asan_if_let_result_shared_no_leak() {
        // B-2026-07-12-24 (residual): `if let Ok(n) = d { … }` is match-sugar —
        // a consume-in-place use of `d`, exactly like `match d`. The escape
        // analysis now recognizes if-let (and while-let / let-else) scrutinees
        // as consume points (not just `match`), so `d` is released. Looped 200x;
        // prints 1400.
        assert_clean_asan_run(
            r#"
shared struct Node { val: i64, mut left: Option[Node], mut right: Option[Node] }
fn take() -> Result[Node, i64] {
    let mut src: Vec[Option[Node]] = Vec.new();
    src.push(Some(Node { val: 7, left: None, right: None }));
    match src[0] {
        None => Err(1),
        Some(n) => Ok(n),
    }
}
fn caller() -> i64 {
    let d = take();
    let mut r = 0;
    if let Ok(n) = d {
        r = n.val;
    }
    r
}
fn main() {
    let mut i: i64 = 0;
    let mut t: i64 = 0;
    while i < 200 {
        t = t + caller();
        i = i + 1;
    }
    println(f"{t}");
}
"#,
            &["1400"],
            "if_let_result_shared_no_leak",
        );
    }

    #[test]
    fn asan_if_let_result_shared_move_out_body_no_double_free() {
        // B-2026-07-12-24 (residual) guard: an if-let whose body MOVES the
        // payload out (`if let Ok(n) = d { Ok(n) }`) must not double-free — the
        // scrutinee `d`'s scope-exit dec and the rebuilt value's ownership are
        // independent (same balance as the `match` returning-arm case). The
        // rebuilt `Ok(n)` flows to a consumer (`match relay()`) that releases
        // it. Prints 1400.
        assert_clean_asan_run(
            r#"
shared struct Node { val: i64, mut left: Option[Node], mut right: Option[Node] }
fn take() -> Result[Node, i64] {
    let mut src: Vec[Option[Node]] = Vec.new();
    src.push(Some(Node { val: 7, left: None, right: None }));
    match src[0] {
        None => Err(1),
        Some(n) => Ok(n),
    }
}
fn relay() -> Result[Node, i64] {
    let d = take();
    if let Ok(n) = d {
        Ok(n)
    } else {
        Err(0)
    }
}
fn caller() -> i64 {
    match relay() {
        Err(e) => e,
        Ok(n) => n.val,
    }
}
fn main() {
    let mut i: i64 = 0;
    let mut t: i64 = 0;
    while i < 200 {
        t = t + caller();
        i = i + 1;
    }
    println(f"{t}");
}
"#,
            &["1400"],
            "if_let_result_shared_move_out_body_no_double_free",
        );
    }

    #[test]
    fn asan_escaping_result_shared_binding_no_double_free() {
        // B-2026-07-12-24 (residual) safety guard: a USER `Result[shared]` binding
        // that ESCAPES must NOT be given a producer-side dec (that would
        // double-free). Here `d` is returned whole (`relay` → `d`) and reaches a
        // consumer (`match relay()`) that releases it — the escape analysis leaves
        // `d` unregistered, so exactly one release happens: no leak, no
        // double-free / use-after-free. A mis-classification would crash under
        // ASAN or corrupt the value; this pins clean + correct (1400).
        assert_clean_asan_run(
            r#"
shared struct Node { val: i64, mut left: Option[Node], mut right: Option[Node] }
fn take() -> Result[Node, i64] {
    let mut src: Vec[Option[Node]] = Vec.new();
    src.push(Some(Node { val: 7, left: None, right: None }));
    match src[0] {
        None => Err(1),
        Some(n) => Ok(n),
    }
}
fn relay() -> Result[Node, i64] {
    let d = take();
    d
}
fn caller() -> i64 {
    match relay() {
        Err(e) => e,
        Ok(n) => n.val,
    }
}
fn main() {
    let mut i: i64 = 0;
    let mut t: i64 = 0;
    while i < 200 {
        t = t + caller();
        i = i + 1;
    }
    println(f"{t}");
}
"#,
            &["1400"],
            "escaping_result_shared_binding_no_double_free",
        );
    }

    #[test]
    fn asan_return_owned_heap_struct_param_no_leak() {
        // B-2026-07-08-6 (non-generic leg, FIXED) — a fn that returns an owned
        // heap-owning STRUCT param used to leak the arg buffer: the caller's
        // return-passthrough guard suppressed the arg-temp drop assuming the
        // callee FORWARDS the buffer, but a copy-supported heap struct param is
        // ENTRY-COPIED at the callee, so the callee returns an INDEPENDENT copy
        // and the original moved-in buffer was orphaned. `id`/`pick`/`choose`
        // all leaked; a String param (no entry-copy) was already clean.
        // Exercises single-return, two-param-keep-one, and conditional-return.
        assert_clean_asan_run(
            r#"
struct Name { s: String }
fn id(a: Name) -> Name { a }
fn pick(a: Name, b: Name) -> Name { a }
fn choose(a: Name, b: Name, t: bool) -> Name { if t { a } else { b } }
fn main() {
    let mut i: i64 = 0i64;
    while i < 2i64 {
        let x = id(Name { s: f"id-{i}-padding-padding" });
        let y = pick(Name { s: f"pa-{i}-padding-padding" }, Name { s: f"pb-{i}-padding-padding" });
        let z = choose(Name { s: f"ca-{i}-padding-padding" }, Name { s: f"cb-{i}-padding-padding" }, true);
        println(x.s);
        println(y.s);
        println(z.s);
        i = i + 1i64;
    }
}
"#,
            &[
                "id-0-padding-padding",
                "pa-0-padding-padding",
                "ca-0-padding-padding",
                "id-1-padding-padding",
                "pa-1-padding-padding",
                "ca-1-padding-padding",
            ],
            "asan_return_owned_heap_struct_param_no_leak",
        );
    }

    #[test]
    fn asan_question_on_result_heap_enum_no_leak_or_double_free() {
        // B-2026-07-11-7 — `?` on `Result[<heap-bearing enum>, E]` reconstructs
        // the Ok payload from its words. The old 3-word
        // `rebuild_value_from_payload_words` truncated a 4-word enum
        // (`J { S(String) }` flattens to {tag, ptr, len, cap}), losing `cap`, so
        // the enum's drop freed the `String` with a garbage cap ("free(): invalid
        // pointer"). Flat-copying the enum's full word span across from the
        // Result's payload fixed it. Exercises BORROW-free MOVE-OUT of the String
        // payload (`S(s) => Ok(s)`), a tuple-variant with a heap tail
        // (`Pair(i64, String)`), and loop iteration — the double-free / invalid
        // free surfaced immediately without the fix.
        assert_clean_asan_run(
            r#"
enum J { N, S(String), Pair(i64, String) }
fn get(k: i64) -> Result[J, String] {
    if k == 0i64 { Result.Ok(J.S(f"str-{k}-padding-padding")) }
    else { Result.Ok(J.Pair(k, f"pair-{k}-padding-padding")) }
}
fn take(k: i64) -> Result[String, String] {
    let v = get(k)?;
    match v {
        N => { Result.Ok(f"n") }
        S(s) => { Result.Ok(s) }
        Pair(a, s) => { Result.Ok(f"{a}:{s}") }
    }
}
fn main() {
    let mut i: i64 = 0i64;
    while i < 2i64 {
        match take(0i64) { Ok(r) => println(r), Err(_) => println("e") }
        match take(1i64) { Ok(r) => println(r), Err(_) => println("e") }
        i = i + 1i64;
    }
}
"#,
            &[
                "str-0-padding-padding",
                "1:pair-1-padding-padding",
                "str-0-padding-padding",
                "1:pair-1-padding-padding",
            ],
            "asan_question_on_result_heap_enum_no_leak_or_double_free",
        );
    }

    #[test]
    fn asan_bytecode_vm_example_no_leak_or_double_free() {
        // examples/vm.kara under ASAN — the bytecode VM churns three Vecs
        // (Vec[Op] program, Vec[i64] data stack, Vec[i64] locals + call stack)
        // hard across the enum-dispatch loop, with per-program construction and
        // drop. `Op` is POD (i64 payloads only), so this is a buffer-lifecycle
        // check: every Vec allocated by the four `prog_*` builders and by `run`
        // is freed exactly once, no leak, across construct -> execute -> drop.
        assert_clean_asan_run(
            include_str!("../examples/vm.kara"),
            &["20", "15", "120", "42"],
            "asan_bytecode_vm_example_no_leak_or_double_free",
        );
    }

    #[test]
    fn asan_heap_example_no_leak() {
        // examples/heap.kara under ASAN — the generic `Heap[i64]` churns its
        // backing `Vec[i64]` hard (push sift-up, pop sift-down + `Vec.pop`,
        // per-swap index read/assign) across heapsort + a PQ drain, plus a fresh
        // `Heap.new()` per phase and `String`s built for the printed lines. `i64`
        // is POD, so this is a buffer-lifecycle check: every heap's backing Vec,
        // the heapsort output Vec, and the print Strings are freed exactly once —
        // no leak, no double-free — across construct -> churn -> drain -> drop.
        assert_clean_asan_run(
            include_str!("../examples/heap.kara"),
            &[
                "0 1 2 3 4 5 6 7 8 9",
                "size=6",
                "4 17 23 42 58 99",
                "empty-ok",
            ],
            "asan_heap_example_no_leak",
        );
    }

    #[test]
    fn asan_curry_closure_vec_store_no_leak() {
        // B-2026-07-12-12 — a curried closure (`let make = |n| |x| x + n`)
        // heap-allocates a reference-counted env box per outer call. When those
        // closures are stored in a `Vec[Fn]` that persists, the env boxes must
        // be freed when the Vec drops. This is un-elidable (the boxes escape
        // into a heap Vec, so LLVM's malloc-to-stack promotion can't remove
        // them): pre-fix it leaked one 16-byte box per iteration (1000 = 16 KB
        // definitely-lost under valgrind). The fix routes the curry call through
        // the SAME `is_heap_env_producing_call` predicate as a named heap-env
        // fn, so the Vec-owner slice frees each element's env on drop. 200 boxes
        // built + stored + dropped, so any per-iteration leak accumulates past
        // noise for LSan (Linux CI).
        assert_clean_asan_run(
            r#"
fn main() {
    let make = |n: i64| |x: i64| x + n;
    let mut fs: Vec[Fn(i64) -> i64] = Vec.new();
    let mut i: i64 = 0;
    while i < 200 {
        fs.push(make(i));
        i = i + 1;
    }
    println(f"{fs[100](0)}");
}
"#,
            &["100"],
            "asan_curry_closure_vec_store_no_leak",
        );
    }

    #[test]
    fn asan_generic_assoc_fn_vec_field_no_leak() {
        // B-2026-07-11-25 — a generic struct `S[T]` whose associated constructor
        // `S.new()` returns `S { items: Vec.new() }`, then pushes through
        // `mut ref self`. Before the fix the constructor returned a ZEROED struct
        // (its `items` a garbage Vec header), so pushes reallocated against
        // garbage (OOM / corruption). Now that `S.new()` monomorphizes correctly,
        // this asserts the Vec[T] field it builds is a real `{null,0,0}` that
        // grows and frees cleanly — no leak, no double-free — across a
        // String-element instantiation (heap payloads) built and dropped in a loop.
        assert_clean_asan_run(
            r#"
struct S[T] { items: Vec[T] }
impl[T] S[T] {
    fn new() -> S[T] { S { items: Vec.new() } }
    fn push(mut ref self, x: T) { self.items.push(x); }
    fn len(ref self) -> i64 { self.items.len() }
}
fn main() {
    let mut r: i64 = 0;
    while r < 3 {
        let mut s: S[String] = S.new();
        s.push(f"row-{r}-aaaa");
        s.push(f"row-{r}-bbbb");
        s.push(f"row-{r}-cccc");
        println(f"{s.len()}");
        r = r + 1;
    }
}
"#,
            &["3", "3", "3"],
            "asan_generic_assoc_fn_vec_field_no_leak",
        );
    }

    #[test]
    fn asan_pipeline_example_no_leak() {
        // examples/pipeline.kara under ASAN — the log-analytics pipeline runs
        // `iter()` chains of `map`/`filter`/`fold`/`collect` over `Req` records
        // whose `method`/`path` are heap `String`s. The `fold` terminal desugar
        // (B-2026-07-11-17) inlines the chain into a `for` loop over the base
        // source; this asserts that lowering leaks no source Vec, no per-element
        // String, and no collected `Vec[String]` (the slow-paths materialization)
        // across construct -> iterate -> aggregate -> drop.
        assert_clean_asan_run(
            include_str!("../examples/pipeline.kara"),
            &[
                "requests: 10",
                "ok: 7",
                "server_err: 2",
                "client_err: 1",
                "bytes_served: 18432",
                "max_latency_ms: 210",
                "avg_ok_latency_ms: 50",
                "slow_paths: 3",
                "  /api/orders",
                "  /api/users",
                "  /assets/app",
            ],
            "asan_pipeline_example_no_leak",
        );
    }

    #[test]
    fn asan_for_over_iter_chain_heap_elems_no_leak() {
        // B-2026-07-11-18 — `for <p> in <src>.iter().{map|filter}+ { .. }` over
        // HEAP elements. The desugar peels the adaptors into a `for` over the base
        // source and binds the user pattern (`let p = <adapted element>`) before
        // the body; over a `Vec[String]`, that must not leak the source Vec, a
        // per-element String, or a mapped String. Exercises a String-yielding map
        // (`|w| w.clone()`), a filter that drops elements, and a two-stage
        // filter+map, each consumed by a body that prints the bound String.
        assert_clean_asan_run(
            r#"
fn main() {
    let words: Vec[String] = ["alpha", "bb", "gamma", "dd", "epsilon"];
    for w in words.iter().filter(|w| w.len() > 2).map(|w| w.clone()) {
        println(w);
    }
    let nums: Vec[i64] = [1, 2, 3, 4, 5, 6];
    for s in nums.iter().filter(|n| n % 2 == 0).map(|n| f"n={n}") {
        println(s);
    }
}
"#,
            &["alpha", "gamma", "epsilon", "n=2", "n=4", "n=6"],
            "asan_for_over_iter_chain_heap_elems_no_leak",
        );
    }

    #[test]
    fn asan_iter_chain_any_all_short_circuit_no_leak() {
        // B-2026-07-11-19 — `any`/`all` short-circuit terminals over a chain whose
        // `map` produces HEAP Strings. When the predicate decides early the loop
        // `break`s mid-iteration; each per-element mapped String (the deciding one
        // included) must be dropped, and the source `Vec[String]` freed once. `any`
        // stops at the first match, `all` at the first failure — both mid-stream.
        assert_clean_asan_run(
            r#"
fn main() {
    let words: Vec[String] = ["alpha", "beta", "gamma", "delta", "epsilon"];
    let hit = words.iter().map(|w| f"[{w}]").any(|s| s.len() > 6);
    let allshort = words.iter().map(|w| f"[{w}]").all(|s| s.len() < 5);
    if hit { println("hit"); } else { println("miss"); }
    if allshort { println("all-short"); } else { println("not-all"); }
}
"#,
            &["hit", "not-all"],
            "asan_iter_chain_any_all_short_circuit_no_leak",
        );
    }

    #[test]
    fn asan_with_capacity_zero_no_leak() {
        // B-2026-07-11-15 — a `with_capacity(n)` whose `n` evaluates to 0 at
        // runtime leaked one byte per call. `karac_alloc_or_panic(0)` normalizes
        // `0 → 1` and returns a real non-null buffer, but the zero-cap collection
        // stores `cap = 0`, and the `cap > 0 ⇔ owned heap` drop convention skips
        // freeing a `cap == 0` buffer — orphaning that 1-byte allocation. The
        // `presize.rs` pass makes this common by rewriting
        // `let mut v = Vec.new(); while i < k { v.push(..) }` to
        // `Vec.with_capacity(k)`, so a `k == 0` counted-fill loop (here the VM's
        // `run(prog, 0)` with no locals) leaked once per call.
        //
        // Exercises all three affected constructors at a zero runtime capacity:
        // the presize-driven `Vec.with_capacity` (via the counted push loop), a
        // direct `Vec[i64].with_capacity(0)`, a `String.with_capacity(0)`, and
        // the fallible `Vec.try_with_capacity(0)` / `String.try_with_capacity(0)`
        // (whose zero case must be `Ok`, not a spurious OOM `Err`). Every one must
        // drop to `{null, 0, 0}` (bit-identical to `.new()`) — nothing to free.
        assert_clean_asan_run(
            r#"
fn fill(n: i64) -> i64 {
    // presize rewrites `Vec.new()` -> `Vec.with_capacity(n)`; n == 0 here.
    let mut v: Vec[i64] = Vec.new();
    let mut i = 0i64;
    while i < n {
        v.push(i);
        i = i + 1i64;
    }
    v.len()
}
fn main() {
    let mut total: i64 = 0i64;
    let mut r: i64 = 0i64;
    while r < 3i64 {
        total = total + fill(0i64);          // zero-cap presized Vec
        let a: Vec[i64] = Vec.with_capacity(0i64);
        total = total + a.len();             // direct zero-cap Vec
        let s: String = String.with_capacity(0i64);
        total = total + s.len();             // direct zero-cap String
        let tv: Vec[i64] = Vec.try_with_capacity(0i64).unwrap();
        total = total + tv.len();            // fallible zero-cap Vec -> Ok
        let ts: String = String.try_with_capacity(0i64).unwrap();
        total = total + ts.len();            // fallible zero-cap String -> Ok
        r = r + 1i64;
    }
    println(f"{total}");
    // A nonzero cap on the SAME path still grows and frees correctly.
    let mut w: Vec[i64] = Vec.with_capacity(0i64);
    w.push(7i64);
    w.push(8i64);
    println(f"{w.len()}");
}
"#,
            &["0", "2"],
            "asan_with_capacity_zero_no_leak",
        );
    }

    #[test]
    fn asan_cstr_to_string_slice_view_not_freed_and_copy_clean() {
        // `CStr.to_string_slice()` returns a BORROWED `{ptr, len, cap=0}` view
        // over the literal's rodata bytes. The `cap == 0` drop-skip must keep
        // the view from being freed — freeing a rodata pointer is an
        // invalid-free (ASan) — while the `.to_string()` copy allocates an
        // owning String that must be freed exactly once (LSan catches a leak).
        // Loop so any invalid-free / leak accumulates and trips the sanitizer.
        assert_clean_asan_run(
            r#"
fn main() {
    let mut i: i64 = 0i64;
    while i < 3i64 {
        match c"hello-view".to_string_slice() {
            Ok(s) => {
                println(s);
                println(s.to_string());
            }
            Err(_) => println("ERR"),
        }
        i = i + 1i64;
    }
}
"#,
            &[
                "hello-view",
                "hello-view",
                "hello-view",
                "hello-view",
                "hello-view",
                "hello-view",
            ],
            "asan_cstr_to_string_slice_view_not_freed_and_copy_clean",
        );
    }

    #[test]
    fn asan_string_to_cstring_owning_buffer_freed_once() {
        // `String.to_cstring()` returns an OWNING `CString` (`{ptr, len,
        // cap=len+1}`, heap buffer + trailing NUL). Unlike the borrowed
        // `to_string_slice` view, its `cap > 0` buffer MUST be freed exactly once
        // at the `Ok(cs)` binding's scope exit — LSan catches a leak, ASan a
        // double-free / use-after-free. Loop over a runtime-built (concatenated)
        // String so any per-iteration leak/double-free accumulates and trips the
        // sanitizer. The interior-NUL `Err` arm allocates nothing and must be
        // leak-clean too.
        assert_clean_asan_run(
            r#"
fn main() {
    let mut i: i64 = 0i64;
    while i < 3i64 {
        let s = "cs-" + "buf";
        match s.to_cstring() {
            Ok(cs) => {
                println(cs.len());
                let b = cs.as_bytes();
                println(b[0]);
            }
            Err(_) => println("ERR"),
        }
        let bad = "x\u{0}y";
        match bad.to_cstring() {
            Ok(_) => println("OK?"),
            Err(_) => println("interior-nul"),
        }
        i = i + 1i64;
    }
}
"#,
            &[
                "6",
                "99",
                "interior-nul",
                "6",
                "99",
                "interior-nul",
                "6",
                "99",
                "interior-nul",
            ],
            "asan_string_to_cstring_owning_buffer_freed_once",
        );
    }

    #[test]
    fn asan_std_cmp_min_max_clamp_heap_ord_no_leak() {
        // roadmap Phase 8 § std.cmp — `min`/`max`/`clamp` are generic stdlib
        // free fns monomorphized on demand from `ordering.kara`. Over a
        // HEAP-OWNING `Ord` type (a `String`-field struct) the un-returned
        // argument must drop exactly once. B-2026-07-08-6 (generic/mono leg,
        // FIXED): the monomorph now ENTRY-COPIES its owned struct params
        // (`compile_mono_function` → `make_aggregate_param_callee_owned`) and
        // the caller registers the original arg-temp's drop
        // (`compile_generic_call` → `track_inline_owned_aggregate_arg`),
        // bringing the mono path to ownership parity with the non-generic
        // `compile_function`/`compile_call` path. Before the fix `min`'s body
        // (`match a.cmp(b) { Greater => b, _ => a }`) returned one owned param
        // and the OTHER (plus every caller original) leaked. Ground-truthed
        // balanced via a malloc interposer (4 mallocs / 4 frees for
        // `min(Name, Name)`).
        assert_clean_asan_run(
            r#"
struct Name { s: String }
impl PartialEq for Name { fn eq(ref self, other: ref Name) -> bool { self.s == other.s } }
impl Eq for Name {}
impl PartialOrd for Name { fn partial_cmp(ref self, other: ref Name) -> Option[Ordering] { Some(self.s.cmp(other.s)) } }
impl Ord for Name { fn cmp(ref self, other: ref Name) -> Ordering { self.s.cmp(other.s) } }
fn main() {
    let mut i: i64 = 0i64;
    while i < 2i64 {
        let lo = min(Name { s: f"alpha-{i}-padding-padding-padding" }, Name { s: f"beta-{i}-padding-padding-padding" });
        let hi = max(Name { s: f"gamma-{i}-padding-padding-padding" }, Name { s: f"delta-{i}-padding-padding-padding" });
        let cl = clamp(Name { s: f"mid-{i}-padding-padding-padding" }, Name { s: f"aaa-{i}" }, Name { s: f"zzz-{i}" });
        println(lo.s);
        println(hi.s);
        println(cl.s);
        i = i + 1i64;
    }
}
"#,
            &[
                "alpha-0-padding-padding-padding",
                "gamma-0-padding-padding-padding",
                "mid-0-padding-padding-padding",
                "alpha-1-padding-padding-padding",
                "gamma-1-padding-padding-padding",
                "mid-1-padding-padding-padding",
            ],
            "asan_std_cmp_min_max_clamp_heap_ord_no_leak",
        );
    }

    #[test]
    fn asan_std_mem_swap_replace_heap_no_leak_no_double_free() {
        // roadmap Phase 8 § std.mem — `swap` / `replace` move String buffers
        // through `mut ref` places via raw load/store (no destructor on the
        // value that leaves the place). The memory contract: every buffer is
        // dropped EXACTLY once. `swap(s, t)` relocates two buffers (no alloc,
        // no free); `replace(s, v)` moves `v` in and returns the OLD `s`
        // (moved out, not freed — the caller's `old` binding owns and drops
        // it). LSan flags a leak if the old value's drop is dropped; ASan flags
        // a double-free if the store also freed the overwritten slot. Looped so
        // any per-iteration imbalance accumulates.
        assert_clean_asan_run(
            r#"
fn main() {
    let mut i: i64 = 0i64;
    while i < 2i64 {
        let mut s = f"aaa-{i}-padding-padding";
        let mut t = f"bbb-{i}-padding-padding";
        swap(mut s, mut t);
        let old = replace(mut s, f"ccc-{i}-padding-padding");
        println(s);
        println(t);
        println(old);
        i = i + 1i64;
    }
}
"#,
            &[
                "ccc-0-padding-padding",
                "aaa-0-padding-padding",
                "bbb-0-padding-padding",
                "ccc-1-padding-padding",
                "aaa-1-padding-padding",
                "bbb-1-padding-padding",
            ],
            "asan_std_mem_swap_replace_heap_no_leak_no_double_free",
        );
    }

    #[test]
    fn asan_map_try_insert_heap_value_overwrite_no_double_free() {
        // B-2026-07-09-15: `Map[i64, String].try_insert` on the fallible path
        // must have the SAME single-drop contract as the panicking `insert`.
        // On an overwrite the runtime copies the OLD value out into the
        // `Some(old)` payload (the match binding owns + drops it once) and the
        // bucket adopts the NEW value (the map's handle-drop frees it once).
        // ASan flags a double-free if either the old value is also freed by the
        // map, or the new value's adoption double-copies. Looped so any
        // per-iteration imbalance accumulates; f-strings force non-foldable heap.
        assert_clean_asan_run(
            r#"
fn main() {
    let mut m: Map[i64, String] = Map.new();
    let mut i: i64 = 0i64;
    while i < 4i64 {
        let _ = m.try_insert(i, f"val-{i}-padding-padding-padding");
        i = i + 1i64;
    }
    let mut j: i64 = 0i64;
    while j < 4i64 {
        match m.try_insert(j, f"new-{j}-padding-padding-padding") {
            Ok(o) => match o { Some(v) => println(v), None => println("none") },
            Err(_) => println("oom"),
        }
        j = j + 1i64;
    }
    match m.get(2i64) { Some(v) => println(v), None => println("none") }
}
"#,
            &[
                "val-0-padding-padding-padding",
                "val-1-padding-padding-padding",
                "val-2-padding-padding-padding",
                "val-3-padding-padding-padding",
                "new-2-padding-padding-padding",
            ],
            "asan_map_try_insert_heap_value_overwrite_no_double_free",
        );
    }

    #[test]
    fn asan_map_try_insert_heap_key_duplicate_no_leak() {
        // `Map[String, i64].try_insert` with a DUPLICATE heap key: the incoming
        // key is deep-copied (owned-param defensive copy), but on the update
        // path the map keeps its stored key and does NOT adopt the incoming
        // one — so the deep-copied buffer is orphaned and must be freed exactly
        // once (the no-adopt leak fix, B-2026-06-20-9 sibling). LSan flags the
        // leak if the free is missing; ASan flags a double-free if it aliases
        // the map's stored key. Every key uses the SAME literal so every insert
        // after the first is an update.
        assert_clean_asan_run(
            r#"
fn main() {
    let mut m: Map[String, i64] = Map.new();
    let mut i: i64 = 0i64;
    while i < 5i64 {
        let k: String = f"stable-key-padding-padding";
        match m.try_insert(k, i) {
            Ok(o) => match o { Some(v) => println(v), None => println("fresh") },
            Err(_) => println("oom"),
        }
        i = i + 1i64;
    }
    println(m.len());
}
"#,
            &["fresh", "0", "1", "2", "3", "1"],
            "asan_map_try_insert_heap_key_duplicate_no_leak",
        );
    }

    #[test]
    fn asan_set_try_insert_heap_element_duplicate_no_leak() {
        // `Set[String].try_insert` with a DUPLICATE heap element: same no-adopt
        // contract as Set.insert (B-2026-06-20-12) on the fallible path — the
        // deep-copied incoming element must be freed once on the duplicate
        // branch, never aliasing the stored element.
        assert_clean_asan_run(
            r#"
fn main() {
    let mut s: Set[String] = Set.new();
    let mut i: i64 = 0i64;
    while i < 5i64 {
        let e: String = f"stable-elem-padding-padding";
        match s.try_insert(e) {
            Ok(b) => println(b),
            Err(_) => println("oom"),
        }
        i = i + 1i64;
    }
    println(s.len());
}
"#,
            &["true", "false", "false", "false", "false", "1"],
            "asan_set_try_insert_heap_element_duplicate_no_leak",
        );
    }

    #[test]
    fn asan_sorted_set_string_iter_min_max_no_leak() {
        // B-2026-07-09-16: `SortedSet[String]` ordered observation. The
        // `karac_map_sorted_keys` buffer holds ALIASES into the set's owned key
        // data; the for-loop binds them borrow-like (no free) and frees only the
        // header buffer, while `min`/`max` CLONE the picked key so the returned
        // `Option[String]` owns an independent buffer. LSan flags a leak if a
        // clone is dropped or the header buffer is not freed; ASan flags a
        // double-free if a for-loop binding or a min/max result aliases (and
        // frees) the set's key. Looped so any imbalance accumulates.
        assert_clean_asan_run(
            r#"
fn main() {
    let mut n: i64 = 0i64;
    while n < 3i64 {
        let mut s: SortedSet[String] = SortedSet.new();
        let _ = s.insert(f"banana-{n}-pad-pad");
        let _ = s.insert(f"apple-{n}-pad-pad");
        let _ = s.insert(f"cherry-{n}-pad-pad");
        let mut out: String = "";
        for w in s { out.push_str(w); out.push_str("|"); }
        println(out);
        match s.min() { Some(v) => println(v), None => println("none") }
        match s.max() { Some(v) => println(v), None => println("none") }
        n = n + 1i64;
    }
}
"#,
            &[
                "apple-0-pad-pad|banana-0-pad-pad|cherry-0-pad-pad|",
                "apple-0-pad-pad",
                "cherry-0-pad-pad",
                "apple-1-pad-pad|banana-1-pad-pad|cherry-1-pad-pad|",
                "apple-1-pad-pad",
                "cherry-1-pad-pad",
                "apple-2-pad-pad|banana-2-pad-pad|cherry-2-pad-pad|",
                "apple-2-pad-pad",
                "cherry-2-pad-pad",
            ],
            "asan_sorted_set_string_iter_min_max_no_leak",
        );
    }

    #[test]
    fn asan_sorted_map_string_key_iter_no_leak() {
        // B-2026-07-09-17: `SortedMap[String, String]` ordered observation. The
        // `keys()`/`values()`/`entries()` producers DEEP-CLONE each half into
        // the owned result `Vec` (LSan flags a leak if a clone is dropped or the
        // sorted-key scratch buffer is not freed; ASan flags a double-free if a
        // clone aliases a map buffer), while the `for (k,v)` loop binds
        // borrow-like aliases and frees only the header buffer. Heap KEY *and*
        // heap VALUE, looped, so any imbalance accumulates.
        assert_clean_asan_run(
            r#"
fn main() {
    let mut n: i64 = 0i64;
    while n < 3i64 {
        let mut m: SortedMap[String, String] = SortedMap.new();
        let _ = m.insert(f"kb-{n}-pad-pad", f"vb-{n}-pad-pad");
        let _ = m.insert(f"ka-{n}-pad-pad", f"va-{n}-pad-pad");
        let _ = m.insert(f"kc-{n}-pad-pad", f"vc-{n}-pad-pad");
        let ks = m.keys();
        let mut ko: String = "";
        for k in ks { ko.push_str(k); ko.push_str("|"); }
        println(ko);
        let mut fo: String = "";
        for (k, v) in m { fo.push_str(k); fo.push_str("="); fo.push_str(v); fo.push_str(";"); }
        println(fo);
        n = n + 1i64;
    }
}
"#,
            &[
                "ka-0-pad-pad|kb-0-pad-pad|kc-0-pad-pad|",
                "ka-0-pad-pad=va-0-pad-pad;kb-0-pad-pad=vb-0-pad-pad;kc-0-pad-pad=vc-0-pad-pad;",
                "ka-1-pad-pad|kb-1-pad-pad|kc-1-pad-pad|",
                "ka-1-pad-pad=va-1-pad-pad;kb-1-pad-pad=vb-1-pad-pad;kc-1-pad-pad=vc-1-pad-pad;",
                "ka-2-pad-pad|kb-2-pad-pad|kc-2-pad-pad|",
                "ka-2-pad-pad=va-2-pad-pad;kb-2-pad-pad=vb-2-pad-pad;kc-2-pad-pad=vc-2-pad-pad;",
            ],
            "asan_sorted_map_string_key_iter_no_leak",
        );
    }

    #[test]
    fn asan_string_strip_prefix_suffix_heap_no_leak() {
        // Phase 8 § String — `strip_{prefix,suffix}(p) -> Option[String]`
        // ALLOCATES the owned remainder copy for the matched case
        // (`karac_string_strip_*` → `alloc_string_result`). The memory contract:
        // the `Some(rest)` String drops exactly once, the receiver drops exactly
        // once, and a FRESH-OWNED f-string argument (`strip_prefix(f"exact-{i}")`)
        // is freed by `free_fresh_owned_str_arg` (else it leaks). Covers matched
        // (heap remainder), no-match (None, no alloc), and matched-empty
        // (`Some("")` = `{null,0,0}`, no alloc). Looped so any per-iteration
        // imbalance accumulates for LSan.
        assert_clean_asan_run(
            r#"
fn main() {
    let mut i: i64 = 0i64;
    while i < 2i64 {
        let s: String = f"prefix-{i}-tail-padding";
        match s.strip_prefix("prefix-") {
            Some(rest) => println(rest),
            None => println("none"),
        }
        let t: String = f"head-{i}-suffix-padding";
        match t.strip_suffix("-suffix-padding") {
            Some(head) => println(head),
            None => println("none"),
        }
        let u: String = f"zzz-{i}-padding";
        match u.strip_prefix("nope") {
            Some(r) => println(r),
            None => println("none"),
        }
        let v: String = f"exact-{i}-padding";
        match v.strip_prefix(f"exact-{i}-padding") {
            Some(r) => println(f"empty:{r}"),
            None => println("none"),
        }
        i = i + 1i64;
    }
}
"#,
            &[
                "0-tail-padding",
                "head-0",
                "none",
                "empty:",
                "1-tail-padding",
                "head-1",
                "none",
                "empty:",
            ],
            "asan_string_strip_prefix_suffix_heap_no_leak",
        );
    }

    #[test]
    fn asan_std_mem_take_heap_no_leak_no_double_free() {
        // roadmap Phase 8 § std.mem — `take[T: Default](dest: mut ref T) -> T`
        // over a heap-owning type. `take` monomorphizes `replace(dest,
        // T.default())`: the old heap buffer is moved OUT (returned, the
        // caller's `old` binding owns and drops it exactly once) and a fresh
        // `T.default()` (empty String buffer, later dropped when `dest` goes out
        // of scope) is moved IN. The memory contract: no buffer leaks (LSan) and
        // none is freed twice (ASan) — in particular the `T.default()`
        // freshly-allocated empty String and the moved-out old value each drop
        // once. Both the named-struct field String and the bare-String cases are
        // looped so any per-iteration imbalance accumulates.
        assert_clean_asan_run(
            r#"
#[derive(Default)]
struct S { x: i64, name: String }
fn main() {
    let mut i: i64 = 0i64;
    while i < 2i64 {
        let mut a = S { x: i, name: f"held-{i}-padding-padding" };
        let old = take(mut a);
        println(old.name);
        println(f"[{a.name}]");
        let mut s = f"bare-{i}-padding-padding";
        let got = take(mut s);
        println(got);
        println(f"[{s}]");
        i = i + 1i64;
    }
}
"#,
            &[
                "held-0-padding-padding",
                "[]",
                "bare-0-padding-padding",
                "[]",
                "held-1-padding-padding",
                "[]",
                "bare-1-padding-padding",
                "[]",
            ],
            "asan_std_mem_take_heap_no_leak_no_double_free",
        );
    }

    #[test]
    fn asan_generic_fn_returns_owned_heap_struct_param_no_leak() {
        // B-2026-07-08-6 (generic/mono leg, FIXED) — the non-stdlib peer of
        // the std.cmp test: a USER generic fn that returns an owned heap-owning
        // struct param. Pins the mono-path ownership parity directly (entry-
        // copy in `compile_mono_function` + caller arg-temp drop in
        // `compile_generic_call`) independent of the baked stdlib. `gid`
        // (single param) and `gpick` (keep one of two) both leaked pre-fix: the
        // mono registered no owned-aggregate param drop and the generic call
        // path registered no caller arg-temp cleanup. (A `cmp`-based generic
        // body can't be user-written — the ownership checker treats a generic
        // trait-method value arg as a move — so this uses plain returns.)
        assert_clean_asan_run(
            r#"
struct Name { s: String }
fn gid[T](a: T) -> T { a }
fn gpick[T](a: T, b: T) -> T { a }
fn main() {
    let mut i: i64 = 0i64;
    while i < 2i64 {
        let x = gid(Name { s: f"gid-{i}-padding-padding" });
        let y = gpick(Name { s: f"gpa-{i}-padding-padding" }, Name { s: f"gpb-{i}-padding-padding" });
        println(x.s);
        println(y.s);
        i = i + 1i64;
    }
}
"#,
            &[
                "gid-0-padding-padding",
                "gpa-0-padding-padding",
                "gid-1-padding-padding",
                "gpa-1-padding-padding",
            ],
            "asan_generic_fn_returns_owned_heap_struct_param_no_leak",
        );
    }

    #[test]
    fn asan_generic_assoc_type_projection_heap_return_no_leak() {
        // A generic fn with an associated-type PROJECTION return
        // (`fn get[C: Container](c: C) -> C.Item`) whose concrete associated
        // type is a HEAP `Vec[i64]`. The projection now lowers to the concrete
        // `{ptr,i64,i64}` (previously it hit the i64 default and failed the LLVM
        // verifier), so the returned Vec's buffer must be owned by the caller
        // and freed exactly once — no leak (the mono must not drop it at its own
        // scope exit) and no double-free. Looped to accumulate any imbalance.
        assert_clean_asan_run(
            r#"
trait Container { type Item; fn make(ref self) -> Self.Item; }
struct VecMaker { base: i64 }
impl Container for VecMaker {
    type Item = Vec[i64];
    fn make(ref self) -> Vec[i64] { [self.base, self.base + 1i64, self.base + 2i64] }
}
fn build[C: Container](c: C) -> C.Item { c.make() }
fn main() {
    let mut i: i64 = 0i64;
    while i < 3i64 {
        let v = build(VecMaker { base: i });
        println(f"{v.len()}");
        i = i + 1i64;
    }
}
"#,
            &["3", "3", "3"],
            "generic_assoc_type_projection_heap_return_no_leak",
        );
    }

    #[test]
    fn asan_ref_self_field_return_no_double_free() {
        // Returning a heap FIELD through a BORROWED receiver (`fn name(ref self)
        // -> String { self.n }`). The borrow does not own the field, so the
        // returned value must be a deep CLONE — an alias would be freed twice
        // (the caller drops the receiver, freeing the field, AND drops the
        // returned value). The receiver is USED AFTER the call each iteration
        // (`x.n`/`x.tags` read), so a move would corrupt it; the clone leaves
        // the field intact. Covers a String field and a `Vec[i64]` field via
        // `ref self` / `mut ref self`; looped so any per-iteration imbalance
        // accumulates (leak on LSan, double-free / UAF on ASan).
        assert_clean_asan_run(
            r#"
struct Person { n: String, tags: Vec[i64] }
impl Person {
    fn name(ref self) -> String { self.n }
    fn steal_tags(mut ref self) -> Vec[i64] { self.tags }
}
fn main() {
    let mut i: i64 = 0i64;
    while i < 3i64 {
        let mut p = Person { n: f"name-{i}-padded-padded", tags: [i, i + 1i64, i + 2i64] };
        let nm = p.name();
        println(nm);
        println(p.n);
        let tg = p.steal_tags();
        println(f"{tg.len()}");
        println(f"{p.tags.len()}");
        i = i + 1i64;
    }
}
"#,
            &[
                "name-0-padded-padded",
                "name-0-padded-padded",
                "3",
                "3",
                "name-1-padded-padded",
                "name-1-padded-padded",
                "3",
                "3",
                "name-2-padded-padded",
                "name-2-padded-padded",
                "3",
                "3",
            ],
            "ref_self_field_return_no_double_free",
        );
    }

    #[test]
    fn asan_fs_read_lines_vec_string_elements_freed() {
        // B-2026-07-11-38: `fs.read_lines(path) -> Result[Vec[String], IoError]`
        // returns a `Vec[String]` whose per-element String buffers are heap
        // allocations the runtime hands over (`karac_runtime_fs_read_lines`).
        // The `?`-unwrapped binding must free each element String AND the Vec
        // buffer at scope exit — a missed element free leaks one buffer per
        // line per iteration (LSan on Linux CI catches it). The program is
        // self-contained: it writes the fixture with `fs.write`, then reads it
        // back 30× so any per-iteration leak accumulates well past noise.
        assert_clean_asan_run(
            r#"
fn count_bytes(path: String) -> Result[i64, IoError] with reads(FileSystem) {
    let lines = fs.read_lines(path)?;
    let mut total = 0;
    for line in lines {
        total = total + line.len();
    }
    Ok(total)
}
fn main() with reads(FileSystem) writes(FileSystem) {
    match fs.write("/tmp/karac_asan_b38_read_lines.txt", "alpha-line\nbeta-line\n\ndelta-line\n") {
        Ok(_) => {
            let mut i: i64 = 0i64;
            let mut last: i64 = 0i64;
            while i < 30i64 {
                match count_bytes("/tmp/karac_asan_b38_read_lines.txt") {
                    Ok(n) => { last = n; },
                    Err(_) => { last = -1i64; },
                }
                i = i + 1i64;
            }
            println(last.to_string());
        },
        Err(_) => println("write-err"),
    }
}
"#,
            // "alpha-line"(10)+"beta-line"(9)+""(0)+"delta-line"(10) = 29
            &["29"],
            "fs_read_lines_vec_string_elements_freed",
        );
    }

    #[test]
    fn asan_single_element_fstring_vec_return_no_double_free() {
        // B-2026-07-04-1: a fn returning a SINGLE-element `Vec[String]` whose
        // element is an f-string literal (`return Vec[f"…"]`) double-freed the
        // element String under `karac build` (SIGTRAP / exit 133), while a
        // two-element f-string Vec or a `.to_string()` element was clean. The
        // f-string temp's owned-temp free must be suppressed when it is moved
        // into the returned Vec literal.
        assert_clean_asan_run(
            r#"
fn build(i: i64) -> Vec[String] {
    return Vec[f"result-payload-element-number-{i}-aaaaaaaaaaaaaaaaaaaa"];
}
fn main() {
    let mut i: i64 = 0i64;
    while i < 50i64 {
        let r: Vec[String] = build(i);
        println(r[0]);
        i = i + 1i64;
    }
}
"#,
            &[
                "result-payload-element-number-0-aaaaaaaaaaaaaaaaaaaa",
                "result-payload-element-number-1-aaaaaaaaaaaaaaaaaaaa",
                "result-payload-element-number-2-aaaaaaaaaaaaaaaaaaaa",
                "result-payload-element-number-3-aaaaaaaaaaaaaaaaaaaa",
                "result-payload-element-number-4-aaaaaaaaaaaaaaaaaaaa",
                "result-payload-element-number-5-aaaaaaaaaaaaaaaaaaaa",
                "result-payload-element-number-6-aaaaaaaaaaaaaaaaaaaa",
                "result-payload-element-number-7-aaaaaaaaaaaaaaaaaaaa",
                "result-payload-element-number-8-aaaaaaaaaaaaaaaaaaaa",
                "result-payload-element-number-9-aaaaaaaaaaaaaaaaaaaa",
                "result-payload-element-number-10-aaaaaaaaaaaaaaaaaaaa",
                "result-payload-element-number-11-aaaaaaaaaaaaaaaaaaaa",
                "result-payload-element-number-12-aaaaaaaaaaaaaaaaaaaa",
                "result-payload-element-number-13-aaaaaaaaaaaaaaaaaaaa",
                "result-payload-element-number-14-aaaaaaaaaaaaaaaaaaaa",
                "result-payload-element-number-15-aaaaaaaaaaaaaaaaaaaa",
                "result-payload-element-number-16-aaaaaaaaaaaaaaaaaaaa",
                "result-payload-element-number-17-aaaaaaaaaaaaaaaaaaaa",
                "result-payload-element-number-18-aaaaaaaaaaaaaaaaaaaa",
                "result-payload-element-number-19-aaaaaaaaaaaaaaaaaaaa",
                "result-payload-element-number-20-aaaaaaaaaaaaaaaaaaaa",
                "result-payload-element-number-21-aaaaaaaaaaaaaaaaaaaa",
                "result-payload-element-number-22-aaaaaaaaaaaaaaaaaaaa",
                "result-payload-element-number-23-aaaaaaaaaaaaaaaaaaaa",
                "result-payload-element-number-24-aaaaaaaaaaaaaaaaaaaa",
                "result-payload-element-number-25-aaaaaaaaaaaaaaaaaaaa",
                "result-payload-element-number-26-aaaaaaaaaaaaaaaaaaaa",
                "result-payload-element-number-27-aaaaaaaaaaaaaaaaaaaa",
                "result-payload-element-number-28-aaaaaaaaaaaaaaaaaaaa",
                "result-payload-element-number-29-aaaaaaaaaaaaaaaaaaaa",
                "result-payload-element-number-30-aaaaaaaaaaaaaaaaaaaa",
                "result-payload-element-number-31-aaaaaaaaaaaaaaaaaaaa",
                "result-payload-element-number-32-aaaaaaaaaaaaaaaaaaaa",
                "result-payload-element-number-33-aaaaaaaaaaaaaaaaaaaa",
                "result-payload-element-number-34-aaaaaaaaaaaaaaaaaaaa",
                "result-payload-element-number-35-aaaaaaaaaaaaaaaaaaaa",
                "result-payload-element-number-36-aaaaaaaaaaaaaaaaaaaa",
                "result-payload-element-number-37-aaaaaaaaaaaaaaaaaaaa",
                "result-payload-element-number-38-aaaaaaaaaaaaaaaaaaaa",
                "result-payload-element-number-39-aaaaaaaaaaaaaaaaaaaa",
                "result-payload-element-number-40-aaaaaaaaaaaaaaaaaaaa",
                "result-payload-element-number-41-aaaaaaaaaaaaaaaaaaaa",
                "result-payload-element-number-42-aaaaaaaaaaaaaaaaaaaa",
                "result-payload-element-number-43-aaaaaaaaaaaaaaaaaaaa",
                "result-payload-element-number-44-aaaaaaaaaaaaaaaaaaaa",
                "result-payload-element-number-45-aaaaaaaaaaaaaaaaaaaa",
                "result-payload-element-number-46-aaaaaaaaaaaaaaaaaaaa",
                "result-payload-element-number-47-aaaaaaaaaaaaaaaaaaaa",
                "result-payload-element-number-48-aaaaaaaaaaaaaaaaaaaa",
                "result-payload-element-number-49-aaaaaaaaaaaaaaaaaaaa",
            ],
            "asan_single_element_fstring_vec_return_no_double_free",
        );
    }

    #[test]
    fn asan_shared_enum_map_payload_move_out_no_double_free() {
        // B-2026-07-08-22: a `shared enum` variant carrying an owning heap
        // payload (`Full(Map[K,V])` / `Full(Set[T])`), matched with a binding that
        // MOVES the payload out, double-freed it — once via the moved binding's
        // scope-exit cleanup, once via the enum box's rc-drop (which frees the Map
        // handle unconditionally). The match-arm move must zero the handle word in
        // the box so the rc-drop's free no-ops on the null handle. Loops so a leak
        // or double-free is observable; the `Empty`/`_`-arm drops exercise the
        // no-move path (must still free exactly once — no leak).
        assert_clean_asan_run(
            r#"
shared enum Store { Empty, Full(Map[i64, u64]) }
fn build(k: i64) -> Store {
    let mut m: Map[i64, u64] = Map.new();
    let _ = m.insert(k, 9u64);
    let _ = m.insert(k + 1i64, 10u64);
    Store.Full(m)
}
fn drop_no_move(k: i64) -> i64 {
    let x = build(k);
    match x { Store.Full(_) => 99i64, Store.Empty => 0i64 }
}
fn main() {
    let mut i: i64 = 0i64;
    while i < 40i64 {
        let s = build(i);
        match s {
            Store.Full(m) => { println(m.len()); }
            Store.Empty => { println(0); }
        }
        let _ = drop_no_move(i);
        i = i + 1i64;
    }
}
"#,
            &["2"; 40],
            "asan_shared_enum_map_payload_move_out_no_double_free",
        );
    }

    #[test]
    fn asan_option_heap_moved_from_recursive_shared_enum_into_mut_ref_self_method_no_double_free() {
        // B-2026-07-11-37: an `Option[String]` moved out of a RECURSIVE shared-enum
        // variant payload (`ContNode.label`, reached via `Expr.Cont` / `Expr.Wrap`)
        // and passed BY VALUE to a `mut ref self` method (`label_ref`) double-freed
        // the `Some` payload — once via the callee's arm drop, once via the caller's
        // scope-exit `FreeInlineOptionPayload` (the by-value arg transfer never nulled
        // the caller slot). The free-fn call path already zeroed the moved slot
        // (`suppress_inline_option_result_binding_move`); the method-call path did
        // not. AOT/JIT aborted (`free(): double free detected`); the interpreter was
        // correct — a run/build divergence with no diagnostic. Discriminators are all
        // in-shape: the RECURSIVE `Wrap` sibling (its `e3` path) is required to change
        // the enum's drop glue; a `None`-carrying `Cont` (`e2`) exercises the no-move
        // arm. Loops so the double-free (a `Some`-payload UAF/double-free) is
        // observable on every iteration.
        //
        // Full leak-checking applies: this shape once ALSO carried a separate leak
        // (dropping a recursive `shared enum` whose payload struct holds an
        // `Option[String]` field never freed that payload — reproduced with a bare
        // `let e = Expr.Cont(...)` and no method call at all), which forced this
        // test to run with LeakSanitizer off. That leak is now fixed
        // (B-2026-07-11-39), so this shape is fully leak-clean and the strong
        // `assert_clean_asan_run` gate covers both the double-free and no-leak.
        assert_clean_asan_run(
            r#"
shared enum Expr { Cont(ContNode), Wrap(WrapNode) }
struct ContNode { label: Option[String] }
struct WrapNode { inner: Expr }
struct R { labels: Vec[String], hits: i64 }
impl R {
  fn in_scope(ref self, name: ref String) -> bool {
    let mut i = 0;
    loop {
      if i >= self.labels.len() { return false; }
      if self.labels[i] == name { return true; }
      i = i + 1;
    }
  }
  fn label_ref(mut ref self, label: Option[String]) {
    match label {
      Some(l) => { if not self.in_scope(l) { self.hits = self.hits + 1; } }
      None => {}
    }
  }
  fn walk(mut ref self, e: Expr) {
    match e {
      Cont(n) => { let ContNode { label } = n; self.label_ref(label); }
      Wrap(n) => { let WrapNode { inner } = n; self.walk(inner); }
    }
  }
}
fn main() {
  let mut round: i64 = 0;
  while round < 50 {
    let mut r = R { labels: Vec.new(), hits: 0 };
    let e1 = Expr.Cont(ContNode { label: Some("continue-label-payload-xxxxxxxxxxxxxxxxxxxx".to_string()) });
    r.walk(e1);
    let e2 = Expr.Cont(ContNode { label: None });
    r.walk(e2);
    let e3 = Expr.Wrap(WrapNode { inner: Expr.Cont(ContNode { label: Some("nested-label-payload-yyyyyyyyyyyyyyyyyyyy".to_string()) }) });
    r.walk(e3);
    println(r.hits.to_string());
    round = round + 1;
  }
}
"#,
            &["2"; 50],
            "asan_option_heap_moved_from_recursive_shared_enum_into_mut_ref_self_method_no_double_free",
        );
    }

    #[test]
    fn asan_recursive_shared_enum_option_heap_payload_field_no_leak() {
        // B-2026-07-11-39: dropping a RECURSIVE `shared enum` whose variant payload
        // is a struct with an `Option[<inline-heap>]` field (`Option[String]` /
        // `Option[Vec[T]]`) leaked the whole boxed payload + its `Some` payload. The
        // recursive `Wrap` variant makes the enum need a generated rc-drop
        // destructor; inside it the `Cont(ContNode)` variant was judged non-walkable
        // because `type_expr_has_drop_heap` has a deliberate `Option => false` blind
        // spot, so the variant got no drop block and its box + String leaked. Native
        // run was clean (leaks do not crash); the Linux LSan gate is authoritative.
        // Fixed by teaching the walkability gates (`field_is_walkable` /
        // `emit_shared_enum_field_drop`) the Option-aware `te_owns_option_heap_payload`
        // predicate and adding an `Option[<inline-heap>]` arm to the payload-struct
        // walker (`emit_nested_struct_shared_rc_decs_ex`) that frees the Some payload
        // via `emit_option_drop_fn`. Exercises the bare create-and-drop trigger
        // (`a`), the `None`-carrying no-heap arm (`b`, must stay clean), nested
        // recursion through `Wrap` (`c`), and an `Option[Vec[String]]` inner (`d`).
        assert_clean_asan_run(
            r#"
shared enum Expr { Cont(ContNode), Wrap(WrapNode) }
struct ContNode { label: Option[String], tags: Option[Vec[String]] }
struct WrapNode { inner: Expr }
fn main() {
  let mut round = 0;
  while round < 50 {
    let a = Expr.Cont(ContNode { label: Some("label-payload-xxxxxxxxxxxxxxxxxxxx".to_string()), tags: None });
    let b = Expr.Cont(ContNode { label: None, tags: None });
    let c = Expr.Wrap(WrapNode { inner: Expr.Cont(ContNode { label: Some("nested-payload-yyyyyyyyyyyyyyyy".to_string()), tags: None }) });
    let d = Expr.Cont(ContNode { label: None, tags: Some(Vec["tag-aaaaaaaaaaaaaaaaaa".to_string(), "tag-bbbbbbbbbbbbbbbbbb".to_string()]) });
    println(round.to_string());
    round = round + 1;
  }
}
"#,
            &(0..50)
                .map(|i| i.to_string())
                .collect::<Vec<_>>()
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>(),
            "asan_recursive_shared_enum_option_heap_payload_field_no_leak",
        );
    }

    #[test]
    fn asan_for_loop_var_into_tuple_push_no_double_free() {
        // B-2026-07-04-3: an inline tuple `(i, x)` whose heap component `x` is a
        // `for`-loop element variable, pushed into a Vec, double-freed the heap
        // component (exit 133/134) — codegen iterates the Vec in place so `x`
        // ALIASES the source buffer; the tuple then aliased it too, and both the
        // source's scope-exit free and the pushed Vec's element drop released
        // it. `compile_tuple` now `maybe_defensive_copy_param_arg`s each element
        // (exactly as `v.push(x)` / struct-literal fields / call args do), so a
        // retaining source (for-loop borrow, owned param) is deep-copied into
        // the tuple. Exercises `.iter()` iteration, owned iteration, the heap
        // component in either tuple slot, and an owned-param-into-tuple — reading
        // an element each time to expose the UAF. `.clone()` and plain-local
        // elements (already clean) are the control.
        assert_clean_asan_run(
            r#"
fn wrap_param(s: String, k: i64) -> Vec[(i64, String)] {
    let mut v: Vec[(i64, String)] = Vec.new();
    v.push((k, s));
    return v;
}
fn main() {
    let mut round: i64 = 0i64;
    while round < 40i64 {
        let w: Vec[String] = Vec[
            "alpha-loop-element-payload-aaaaaaaaaaaaaaaaaaaa".to_string(),
            "bravo-loop-element-payload-bbbbbbbbbbbbbbbbbbbb".to_string(),
            "charlie-loop-element-payload-cccccccccccccccccc".to_string()
        ];
        let mut a: Vec[(i64, String)] = Vec.new();
        let mut i: i64 = 0i64;
        for x in w.iter() { a.push((i, x)); i = i + 1i64; }
        let pa = a[2];
        let mut b: Vec[(String, i64)] = Vec.new();
        for y in w.iter() { b.push((y, 9i64)); }
        let pb = b[0];
        let owned: Vec[String] = Vec[
            "delta-owned-element-payload-dddddddddddddddddddd".to_string(),
            "echo-owned-element-payload-eeeeeeeeeeeeeeeeeeeee".to_string()
        ];
        let mut c: Vec[(i64, String)] = Vec.new();
        for z in owned { c.push((0i64, z)); }
        let pc = c[1];
        let d: Vec[(i64, String)] = wrap_param("param-element-payload-ffffffffffffffffffff".to_string(), 5i64);
        let pd = d[0];
        println(f"{pa.0} {pa.1} {pb.0} {pb.1} {pc.0} {pc.1} {pd.0} {pd.1}");
        round = round + 1i64;
    }
}
"#,
            [
                "2 charlie-loop-element-payload-cccccccccccccccccc alpha-loop-element-payload-aaaaaaaaaaaaaaaaaaaa 9 0 echo-owned-element-payload-eeeeeeeeeeeeeeeeeeeee 5 param-element-payload-ffffffffffffffffffff",
            ]
            .repeat(40)
            .as_slice(),
            "asan_for_loop_var_into_tuple_push_no_double_free",
        );
    }

    #[test]
    fn asan_heap_enumerate_map_collect_no_double_free() {
        // B-2026-07-04-4: `<Vec[String]>.iter().enumerate().map(|p| …).collect()`
        // — a heap `enumerate` whose `(i64, String)` tuple flows into a terminal
        // `map`. The desugar binds the tuple DIRECTLY to the map's param (single
        // owning binding, no aliasing `let p = __ietup` copy), and the source
        // loop var gets a synthetic name so it can't collide with that param.
        // The map pushes a value transformed from the tuple. Exercises the
        // headline `map(|p| p.1)` (extract the heap component into `Vec[String]`),
        // a POD-producing `map(|p| p.0 + p.1.len())`, and `skip().enumerate().map`
        // — reading an element each round to expose any double-free/UAF.
        assert_clean_asan_run(
            r#"
fn main() {
    let mut round: i64 = 0i64;
    while round < 40i64 {
        let w: Vec[String] = Vec[
            "alpha-enum-map-payload-aaaaaaaaaaaaaaaaaaaa".to_string(),
            "bravo-enum-map-payload-bbbbbbbbbbbbbbbbbbbb".to_string(),
            "charlie-enum-map-payload-cccccccccccccccccc".to_string()
        ];
        let a: Vec[String] = w.iter().enumerate().map(|p| p.1).collect();
        let b: Vec[i64] = w.iter().enumerate().map(|p| p.0 + p.1.len()).collect();
        let c: Vec[String] = w.iter().skip(1i64).enumerate().map(|p| p.1).collect();
        let a2: String = a[2];
        let c0: String = c[0];
        println(f"{a.len()} {a2} {b[0]} {b[2]} {c.len()} {c0}");
        round = round + 1i64;
    }
}
"#,
            [
                "3 charlie-enum-map-payload-cccccccccccccccccc 43 45 2 bravo-enum-map-payload-bbbbbbbbbbbbbbbbbbbb",
            ]
            .repeat(40)
            .as_slice(),
            "asan_heap_enumerate_map_collect_no_double_free",
        );
    }

    #[test]
    fn asan_b04_4_heap_enumerate_conditional_tuple_collect_no_double_free() {
        // B-2026-07-04-4: a HEAP `enumerate` whose `(i64, String)` tuple flows
        // into a CONDITIONAL whole-tuple push — `filter`/`take_while`/
        // `skip_while`/`inspect` after enumerate, plus a `filter().take()` two-
        // stage chain. Each binds the tuple DIRECTLY to the (first) downstream
        // param, so the heap tuple keeps a SINGLE owning binding and its
        // conditional `if pred { push(p) }` is a clean move (no aliasing copy).
        // Previously gated to the loud dispatch-fail; now lowered. Reads an
        // element from every result each round to expose any double-free/UAF.
        assert_clean_asan_run(
            r#"
fn main() {
    let mut round: i64 = 0i64;
    while round < 40i64 {
        let w: Vec[String] = Vec[
            "alpha-b044-payload-aaaaaaaaaaaaaaaaaaaa".to_string(),
            "bravo-b044-payload-bbbbbbbbbbbbbbbbbbbb".to_string(),
            "charlie-b044-payload-cccccccccccccccc".to_string(),
            "delta-b044-payload-dddddddddddddddddddd".to_string()
        ];
        let f: Vec[(i64, String)] = w.iter().enumerate().filter(|p| p.0 > 0i64).collect();
        let tw: Vec[(i64, String)] = w.iter().enumerate().take_while(|p| p.0 < 2i64).collect();
        let sw: Vec[(i64, String)] = w.iter().enumerate().skip_while(|p| p.0 < 1i64).collect();
        let ft: Vec[(i64, String)] = w.iter().enumerate().filter(|p| p.0 > 0i64).take(1i64).collect();
        let ins: Vec[(i64, String)] = w.iter().enumerate().inspect(|p| p.0).collect();
        let f0: (i64, String) = f[0];
        let tw0: (i64, String) = tw[0];
        let sw2: (i64, String) = sw[2];
        let ft0: (i64, String) = ft[0];
        let ins3: (i64, String) = ins[3];
        println(f"{f.len()}:{f0.1} {tw.len()}:{tw0.1} {sw.len()}:{sw2.1} {ft.len()}:{ft0.1} {ins.len()}:{ins3.1}");
        round = round + 1i64;
    }
}
"#,
            [
                "3:bravo-b044-payload-bbbbbbbbbbbbbbbbbbbb 2:alpha-b044-payload-aaaaaaaaaaaaaaaaaaaa 3:delta-b044-payload-dddddddddddddddddddd 1:bravo-b044-payload-bbbbbbbbbbbbbbbbbbbb 4:delta-b044-payload-dddddddddddddddddddd",
            ]
            .repeat(40)
            .as_slice(),
            "asan_b04_4_heap_enumerate_conditional_tuple_collect_no_double_free",
        );
    }

    #[test]
    fn asan_b04_4_heap_enumerate_downstream_map_collect_no_double_free() {
        // B-2026-07-04-4 case D and siblings: a HEAP `enumerate` whose tuple
        // reaches a `map(|p| p.1)` AFTER a passthrough stage (`take`/`skip`) or a
        // `filter`. The `Enumerate` arm now searches PAST `take`/`skip`/`step_by`
        // to bind the tuple directly to the map's param, so the map extracts the
        // String field from the SINGLE owning binding — no `let p = __ietup`
        // whole-tuple bit-copy (the previous double-free). Each collects a
        // `Vec[String]`; reads an element each round.
        assert_clean_asan_run(
            r#"
fn main() {
    let mut round: i64 = 0i64;
    while round < 40i64 {
        let w: Vec[String] = Vec[
            "alpha-b044-payload-aaaaaaaaaaaaaaaaaaaa".to_string(),
            "bravo-b044-payload-bbbbbbbbbbbbbbbbbbbb".to_string(),
            "charlie-b044-payload-cccccccccccccccc".to_string(),
            "delta-b044-payload-dddddddddddddddddddd".to_string()
        ];
        let fm: Vec[String] = w.iter().enumerate().filter(|p| p.0 > 0i64).map(|p| p.1).collect();
        let tm: Vec[String] = w.iter().enumerate().take(2i64).map(|p| p.1).collect();
        let sm: Vec[String] = w.iter().enumerate().skip(1i64).map(|p| p.1).collect();
        let fm2: String = fm[2];
        let tm0: String = tm[0];
        let sm2: String = sm[2];
        println(f"{fm.len()}:{fm2} {tm.len()}:{tm0} {sm.len()}:{sm2}");
        round = round + 1i64;
    }
}
"#,
            [
                "3:delta-b044-payload-dddddddddddddddddddd 2:alpha-b044-payload-aaaaaaaaaaaaaaaaaaaa 3:delta-b044-payload-dddddddddddddddddddd",
            ]
            .repeat(40)
            .as_slice(),
            "asan_b04_4_heap_enumerate_downstream_map_collect_no_double_free",
        );
    }

    #[test]
    fn asan_b04_2_identity_collect_no_leak() {
        // B-2026-07-04-2 sub-part 4: a PLAIN `<src>.iter().collect()` identity
        // collect (no map/filter/... adaptor). The fix injects a synthetic
        // identity `map(|x| x)`, cloning each element into a fresh Vec. A
        // named-local source is BORROWED (survives, freed once at its own scope),
        // and a FRESH-TEMP source (`mk().iter().collect()`) must free its heap
        // after cloning. Loops 40× with >=36-byte payloads for LSan reachability;
        // reads an element each round to expose any double-free/UAF.
        assert_clean_asan_run(
            r#"
fn mk() -> Vec[String] {
    let mut v: Vec[String] = Vec.new();
    v.push("identity-collect-freshtemp-alpha-aaaaaaaaaaaa".to_string());
    v.push("identity-collect-freshtemp-bravo-bbbbbbbbbbbb".to_string());
    v
}
fn main() {
    let mut round: i64 = 0i64;
    while round < 40i64 {
        let w: Vec[String] = Vec[
            "identity-collect-local-alpha-aaaaaaaaaaaaaaaa".to_string(),
            "identity-collect-local-bravo-bbbbbbbbbbbbbbbb".to_string(),
            "identity-collect-local-charlie-cccccccccccccc".to_string()
        ];
        let a: Vec[String] = w.iter().collect();
        let ft: Vec[String] = mk().iter().collect();
        let a1: String = a[1i64];
        let ft0: String = ft[0i64];
        println(f"{a.len()} {w.len()} {a1} {ft.len()} {ft0}");
        round = round + 1i64;
    }
}
"#,
            [
                "3 3 identity-collect-local-bravo-bbbbbbbbbbbbbbbb 2 identity-collect-freshtemp-alpha-aaaaaaaaaaaa",
            ]
            .repeat(40)
            .as_slice(),
            "asan_b04_2_identity_collect_no_leak",
        );
    }

    #[test]
    fn asan_b04_2_into_iter_identity_collect_no_leak() {
        // B-2026-07-04-2 sub-part 4 (into_iter half): `<local>.into_iter()
        // .collect()` lowers identically to `.iter().collect()` — the ownership
        // checker treats it as NON-consuming (`w.len()` stays valid after), so
        // it clones each element into a fresh Vec and the source survives. Same
        // leak/double-free surface as the `.iter()` identity collect; asserts a
        // heap source over 40× ≥44-byte payloads (LSan reachability).
        assert_clean_asan_run(
            r#"
fn main() {
    let mut round: i64 = 0i64;
    while round < 40i64 {
        let w: Vec[String] = Vec[
            "into-iter-collect-alpha-aaaaaaaaaaaaaaaaaaaaaa".to_string(),
            "into-iter-collect-bravo-bbbbbbbbbbbbbbbbbbbbbb".to_string()
        ];
        let r: Vec[String] = w.into_iter().collect();
        let r1: String = r[1i64];
        println(f"{r.len()} {w.len()} {r1}");
        round = round + 1i64;
    }
}
"#,
            ["2 2 into-iter-collect-bravo-bbbbbbbbbbbbbbbbbbbbbb"]
                .repeat(40)
                .as_slice(),
            "asan_b04_2_into_iter_identity_collect_no_leak",
        );
    }

    #[test]
    fn asan_b06_5_blanket_vec_string_impl_loop_no_leak() {
        // B-2026-07-06-5 (blanket `impl Trait for Vec[String]`): the impl body
        // iterates the borrowed receiver (`for s in self { out = out + s; }`).
        // `self` is a `ref Vec[String]` (`SelfValue`) — the loop must NOT free
        // the Vec buffer OR its per-element heap Strings (the caller still owns
        // them). Before the SelfValue for-loop arm, `self` fell to the
        // materialize-iterate-DROP value path and double-freed the borrowed
        // heap. Drives the impl both directly (`v.concat()`) and through a
        // bound-generic mono (`callit(v)`) over 40× ≥40-byte payloads for LSan
        // reachability; the source `v` is re-read each round to expose UAF.
        assert_clean_asan_run(
            r#"
trait Joiner {
    fn concat(ref self) -> String;
}
impl Joiner for Vec[String] {
    fn concat(ref self) -> String {
        let mut out = String.new();
        for s in self { out = out + s; }
        out
    }
}
fn callit[C: Joiner](c: ref C) -> String { c.concat() }
fn main() {
    let mut round: i64 = 0i64;
    while round < 40i64 {
        let mut v: Vec[String] = Vec[
            "blanket-vec-string-loop-alpha-aaaaaaaaaaaaaaaa".to_string(),
            "blanket-vec-string-loop-bravo-bbbbbbbbbbbbbbbb".to_string()
        ];
        let direct: String = v.concat();
        let mono: String = callit(v);
        println(f"{direct} {mono} {v.len()}");
        round = round + 1i64;
    }
}
"#,
            [
                "blanket-vec-string-loop-alpha-aaaaaaaaaaaaaaaablanket-vec-string-loop-bravo-bbbbbbbbbbbbbbbb blanket-vec-string-loop-alpha-aaaaaaaaaaaaaaaablanket-vec-string-loop-bravo-bbbbbbbbbbbbbbbb 2",
            ]
            .repeat(40)
            .as_slice(),
            "asan_b06_5_blanket_vec_string_impl_loop_no_leak",
        );
    }

    #[test]
    fn asan_b04_2_zip_heap_collect_no_leak() {
        // B-2026-07-04-2 (heap-zip leg): `a.iter().zip(b.iter()).collect()` over
        // two `Vec[String]` sources. The pushed tuple `(a[i], b[i])` deep-clones
        // each named-Vec heap index-read, so the borrowed sources SURVIVE (freed
        // once at their own scope) and the collect result owns independent
        // buffers (freed once). Before the fix the index-read aliased the source
        // buffer — both the result's element-drop and the source's scope-exit
        // free released it (double-free). 40× ≥40-byte payloads for LSan
        // reachability; re-reads a source and a result element each round.
        assert_clean_asan_run(
            r#"
fn main() {
    let mut round: i64 = 0i64;
    while round < 40i64 {
        let mut a: Vec[String] = Vec[
            "zip-heap-collect-left-alpha-aaaaaaaaaaaaaaaa".to_string(),
            "zip-heap-collect-left-bravo-bbbbbbbbbbbbbbbb".to_string()
        ];
        let mut b: Vec[String] = Vec[
            "zip-heap-collect-right-charlie-cccccccccccc".to_string(),
            "zip-heap-collect-right-delta-dddddddddddddd".to_string()
        ];
        let z: Vec[(String, String)] = a.iter().zip(b.iter()).collect();
        let p: (String, String) = z[0i64];
        println(f"{z.len()} {a.len()} {b.len()} {p.0} {p.1}");
        round = round + 1i64;
    }
}
"#,
            [
                "2 2 2 zip-heap-collect-left-alpha-aaaaaaaaaaaaaaaa zip-heap-collect-right-charlie-cccccccccccc",
            ]
            .repeat(40)
            .as_slice(),
            "asan_b04_2_zip_heap_collect_no_leak",
        );
    }

    #[test]
    fn asan_heap_vec_index_read_into_sinks_no_double_free() {
        // General heap-index-read-into-owning-sink double-free (found fixing the
        // heap-zip leg). Reading `v[i]` (heap String element) into a tuple
        // literal, a `push`, and a struct field must deep-clone so the source
        // `v` stays the sole owner of its originals and each sink owns an
        // independent buffer. Before the fix each sink aliased the source buffer
        // → double-free (exit 133). 40× ≥40-byte payloads; `v` re-read each
        // round to expose UAF.
        assert_clean_asan_run(
            r#"
struct Pair { x: String, y: String }
fn main() {
    let mut round: i64 = 0i64;
    while round < 40i64 {
        let mut v: Vec[String] = Vec[
            "index-sink-element-alpha-aaaaaaaaaaaaaaaaaaaa".to_string(),
            "index-sink-element-bravo-bbbbbbbbbbbbbbbbbbbb".to_string(),
            "index-sink-element-charlie-cccccccccccccccccc".to_string()
        ];
        let t: (String, String) = (v[0i64], v[2i64]);
        let mut d: Vec[String] = Vec.new();
        d.push(v[1i64]);
        let p: Pair = Pair { x: v[0i64], y: v[1i64] };
        println(f"{t.0} {t.1} {d[0i64]} {p.x} {p.y} {v.len()}");
        round = round + 1i64;
    }
}
"#,
            [
                "index-sink-element-alpha-aaaaaaaaaaaaaaaaaaaa index-sink-element-charlie-cccccccccccccccccc index-sink-element-bravo-bbbbbbbbbbbbbbbbbbbb index-sink-element-alpha-aaaaaaaaaaaaaaaaaaaa index-sink-element-bravo-bbbbbbbbbbbbbbbbbbbb 3",
            ]
            .repeat(40)
            .as_slice(),
            "asan_heap_vec_index_read_into_sinks_no_double_free",
        );
    }

    #[test]
    fn asan_b04_2_chunks_heap_collect_no_leak() {
        // B-2026-07-04-2 sub-part 1 (chunks heap leg): `v.iter().chunks(2)
        // .collect()` over a `Vec[String]` -> `Vec[Vec[String]]`. Each chunk is
        // built as a FRESH temp via an inline block tail-return
        // (`acc.push({ let mut c = Vec.new(); ...; c })`), the `mk()`-fresh-temp
        // pattern inlined — not a consume-then-reuse loop binding (which needs
        // the ownership RC fallback the synthetic AST can't emit) nor an
        // in-place fill of a growing accumulator (which double-freed on
        // realloc). Each `base[j]` deep-clones (the heap-index-read fix), so
        // `base` survives and every clone is owned once by the result. 40x
        // >=40-byte payloads; re-reads `v` and reads nested chunk elements
        // INLINE via the f-string (a `let x = cs[i][j]` double-index bind hits
        // the separate documented `matrix[i][j]` clone gap, unrelated to
        // chunks).
        assert_clean_asan_run(
            r#"
fn main() {
    let mut round: i64 = 0i64;
    while round < 40i64 {
        let mut v: Vec[String] = Vec[
            "chunks-heap-collect-alpha-aaaaaaaaaaaaaaaaaa".to_string(),
            "chunks-heap-collect-bravo-bbbbbbbbbbbbbbbbbb".to_string(),
            "chunks-heap-collect-charlie-cccccccccccccccc".to_string(),
            "chunks-heap-collect-delta-dddddddddddddddddd".to_string(),
            "chunks-heap-collect-echo-eeeeeeeeeeeeeeeeeeee".to_string()
        ];
        let cs: Vec[Vec[String]] = v.iter().chunks(2i64).collect();
        println(f"{cs.len()} {cs[0i64].len()} {cs[2i64].len()} {cs[0i64][0i64]} {cs[2i64][0i64]} {v.len()}");
        round = round + 1i64;
    }
}
"#,
            [
                "3 2 1 chunks-heap-collect-alpha-aaaaaaaaaaaaaaaaaa chunks-heap-collect-echo-eeeeeeeeeeeeeeeeeeee 5",
            ]
            .repeat(40)
            .as_slice(),
            "asan_b04_2_chunks_heap_collect_no_leak",
        );
    }

    #[test]
    fn asan_b04_2_windows_heap_collect_no_leak() {
        // B-2026-07-04-2 sub-part 1 (windows heap leg): `v.iter().windows(2)
        // .collect()` over a `Vec[String]` -> overlapping length-2 slices. Each
        // element is cloned into MULTIPLE windows (`base[j]` deep-clones per
        // read), so each window owns independent buffers and the borrowed `v`
        // survives -- the overlap must not alias. Same fresh-temp block-return
        // lowering as chunks (step=1, full-window cutoff). 40x >=40-byte
        // payloads; inline nested reads.
        assert_clean_asan_run(
            r#"
fn main() {
    let mut round: i64 = 0i64;
    while round < 40i64 {
        let mut v: Vec[String] = Vec[
            "windows-heap-collect-alpha-aaaaaaaaaaaaaaaa".to_string(),
            "windows-heap-collect-bravo-bbbbbbbbbbbbbbbb".to_string(),
            "windows-heap-collect-charlie-cccccccccccccc".to_string(),
            "windows-heap-collect-delta-dddddddddddddddd".to_string()
        ];
        let ws: Vec[Vec[String]] = v.iter().windows(2i64).collect();
        println(f"{ws.len()} {ws[0i64].len()} {ws[0i64][0i64]} {ws[2i64][1i64]} {v.len()}");
        round = round + 1i64;
    }
}
"#,
            [
                "3 2 windows-heap-collect-alpha-aaaaaaaaaaaaaaaa windows-heap-collect-delta-dddddddddddddddd 4",
            ]
            .repeat(40)
            .as_slice(),
            "asan_b04_2_windows_heap_collect_no_leak",
        );
    }

    #[test]
    fn asan_nested_vec_index_bind_no_double_free() {
        // The `matrix[i][j]` clone gap: binding a nested heap element out of a
        // `Vec[Vec[String]]` (`let x = m[i][j]`) shallow-aliased the innermost
        // buffer, so both the binding's drop and the container's recursive drop
        // freed it (double-free, exit 133). `clone_owned_vec_index_element` now
        // peels one Vec layer per index level (`vec_index_elem_type_expr`) and
        // deep-clones, so the binding owns an independent buffer and the source
        // survives. Surfaced binding a `chunks()` result element. 40x >=40-byte
        // payloads; the source matrix is re-read each round.
        assert_clean_asan_run(
            r#"
fn main() {
    let mut round: i64 = 0i64;
    while round < 40i64 {
        let mut v: Vec[String] = Vec[
            "nested-idx-bind-alpha-aaaaaaaaaaaaaaaaaaaa".to_string(),
            "nested-idx-bind-bravo-bbbbbbbbbbbbbbbbbbbb".to_string(),
            "nested-idx-bind-charlie-cccccccccccccccccc".to_string(),
            "nested-idx-bind-delta-dddddddddddddddddddd".to_string()
        ];
        let m: Vec[Vec[String]] = v.iter().chunks(2i64).collect();
        let a: String = m[0i64][0i64];
        let b: String = m[1i64][1i64];
        println(f"{a} {b} {m.len()} {v.len()}");
        round = round + 1i64;
    }
}
"#,
            [
                "nested-idx-bind-alpha-aaaaaaaaaaaaaaaaaaaa nested-idx-bind-delta-dddddddddddddddddddd 2 4",
            ]
            .repeat(40)
            .as_slice(),
            "asan_nested_vec_index_bind_no_double_free",
        );
    }

    #[test]
    fn asan_b05_1_nonterminal_map_tuple_no_double_free() {
        assert_clean_asan_run(
            r#"
fn main() {
    let mut round: i64 = 0i64;
    while round < 30i64 {
        let mut v: Vec[String] = Vec[
            "b05-map-alpha-aaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(),
            "b05-map-bravo-bbbbbbbbbbbbbbbbbbbbbbbbbbbb".to_string()
        ];
        let r: Vec[(i64, String)] = v.iter().enumerate().map(|p| p).filter(|q| q.0 >= 0i64).collect();
        println(f"{r.len()} {r[0i64].0} {v.len()}");
        round = round + 1i64;
    }
}
"#,
            ["2 0 2"].repeat(30).as_slice(),
            "asan_b05_1_nonterminal_map_tuple_no_double_free",
        );
    }

    #[test]
    fn asan_b05_1_diff_param_name_no_double_free() {
        assert_clean_asan_run(
            r#"
fn main() {
    let mut round: i64 = 0i64;
    while round < 30i64 {
        let mut v: Vec[String] = Vec[
            "b05-diff-alpha-aaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(),
            "b05-diff-bravo-bbbbbbbbbbbbbbbbbbbbbbbbbbbb".to_string()
        ];
        let r: Vec[(i64, String)] = v.iter().enumerate().filter(|p| p.0 >= 0i64).inspect(|q| print(q.0)).collect();
        println(f"{r.len()} {r[1i64].0} {v.len()}");
        round = round + 1i64;
    }
}
"#,
            ["012 1 2"].repeat(30).as_slice(),
            "asan_b05_1_diff_param_name_no_double_free",
        );
    }

    #[test]
    fn asan_b05_1_retuple_map_no_double_free() {
        assert_clean_asan_run(
            r#"
fn main() {
    let mut round: i64 = 0i64;
    while round < 30i64 {
        let mut v: Vec[String] = Vec[
            "b05-retup-alpha-aaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(),
            "b05-retup-bravo-bbbbbbbbbbbbbbbbbbbbbbbbbb".to_string()
        ];
        let r: Vec[(i64, String)] = v.iter().enumerate().map(|p| (p.0, p.1)).filter(|q| q.0 >= 0i64).collect();
        println(f"{r.len()} {r[0i64].0} {r[1i64].0} {v.len()}");
        round = round + 1i64;
    }
}
"#,
            ["2 0 1 2"].repeat(30).as_slice(),
            "asan_b05_1_retuple_map_no_double_free",
        );
    }

    #[test]
    fn asan_b05_1_multistage_diff_params_no_double_free() {
        assert_clean_asan_run(
            r#"
fn main() {
    let mut round: i64 = 0i64;
    while round < 30i64 {
        let mut v: Vec[String] = Vec[
            "b05-multi-alpha-aaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(),
            "b05-multi-bravo-bbbbbbbbbbbbbbbbbbbbbbbbbb".to_string(),
            "b05-multi-charlie-cccccccccccccccccccccccc".to_string()
        ];
        let r: Vec[(i64, String)] = v.iter().enumerate().filter(|p| p.0 >= 0i64).filter(|q| q.0 < 5i64).collect();
        println(f"{r.len()} {r[2i64].0} {v.len()}");
        round = round + 1i64;
    }
}
"#,
            ["3 2 3"].repeat(30).as_slice(),
            "asan_b05_1_multistage_diff_params_no_double_free",
        );
    }

    #[test]
    fn asan_b04_2_chain_adaptor_side_heap_no_leak() {
        // B-2026-07-04-2 sub-part 1 (chain adaptor-carrying side): a `chain`
        // whose side carries its own adaptor (`a.iter().filter(g).chain(b.iter())
        // .collect()`) recursively collects each side and merges into a shared
        // accumulator. Both `Vec[String]` sources survive (freed once) and each
        // merged element is a clone owned once by the result. 30x >=40-byte
        // payloads; both sources re-read.
        assert_clean_asan_run(
            r#"
fn main() {
    let mut round: i64 = 0i64;
    while round < 30i64 {
        let mut a: Vec[String] = Vec[
            "chain-adp-left-alpha-aaaaaaaaaaaaaaaaaaaaaa".to_string(),
            "chain-adp-left-bravo-bbbbbbbbbbbbbbbbbbbbbb".to_string()
        ];
        let mut b: Vec[String] = Vec[
            "chain-adp-right-charlie-cccccccccccccccccccc".to_string(),
            "chain-adp-right-delta-dddddddddddddddddddddd".to_string()
        ];
        let r: Vec[String] = a.iter().filter(|s| s.len() > 0i64).chain(b.iter()).collect();
        println(f"{r.len()} {r[0i64]} {r[3i64]} {a.len()} {b.len()}");
        round = round + 1i64;
    }
}
"#,
            [
                "4 chain-adp-left-alpha-aaaaaaaaaaaaaaaaaaaaaa chain-adp-right-delta-dddddddddddddddddddddd 2 2",
            ]
            .repeat(30)
            .as_slice(),
            "asan_b04_2_chain_adaptor_side_heap_no_leak",
        );
    }

    #[test]
    fn asan_b04_2_zip_adaptor_side_heap_no_leak() {
        // B-2026-07-04-2 sub-part 1 (zip adaptor-carrying side): a `zip` whose
        // side carries its own adaptor (`a.iter().filter(g).zip(b.iter())
        // .collect()`) pre-collects each side to a typed temp and reuses the
        // identity zip. Both `Vec[String]` sources survive; each paired element
        // is a clone owned once by the result; the two side temps are dropped at
        // block exit. 30x >=40-byte payloads; both sources re-read.
        assert_clean_asan_run(
            r#"
fn main() {
    let mut round: i64 = 0i64;
    while round < 30i64 {
        let mut a: Vec[String] = Vec[
            "zip-adp-left-alpha-aaaaaaaaaaaaaaaaaaaaaaaa".to_string(),
            "zip-adp-left-bravo-bbbbbbbbbbbbbbbbbbbbbbbb".to_string(),
            "zip-adp-left-charlie-cccccccccccccccccccccc".to_string()
        ];
        let mut b: Vec[String] = Vec[
            "zip-adp-right-xray-xxxxxxxxxxxxxxxxxxxxxxxx".to_string(),
            "zip-adp-right-yankee-yyyyyyyyyyyyyyyyyyyyyy".to_string()
        ];
        let r: Vec[(String, String)] = a.iter().filter(|s| s.len() > 0i64).zip(b.iter()).collect();
        println(f"{r.len()} {r[0i64].0} {r[1i64].1} {a.len()} {b.len()}");
        round = round + 1i64;
    }
}
"#,
            [
                "2 zip-adp-left-alpha-aaaaaaaaaaaaaaaaaaaaaaaa zip-adp-right-yankee-yyyyyyyyyyyyyyyyyyyyyy 3 2",
            ]
            .repeat(30)
            .as_slice(),
            "asan_b04_2_zip_adaptor_side_heap_no_leak",
        );
    }

    #[test]
    fn asan_b04_2_nonterminal_fstring_map_no_leak() {
        // B-2026-07-04-2 sub-part 3 (non-terminal f-string map): `v.iter()
        // .map(|x| f"..").filter(g).collect()` splits at the f-string map —
        // collect the prefix (terminal f-string map -> Vec[String]) into a temp,
        // then continue the filter over the temp. The f-string temp Vec and its
        // Strings are owned once by the temp then cloned once into the result;
        // no staged-accumulator double-free. 40x heap payloads; result elements
        // read inline.
        assert_clean_asan_run(
            r#"
fn main() {
    let mut round: i64 = 0i64;
    while round < 40i64 {
        let v: Vec[i64] = Vec[1i64, 2i64, 3i64, 4i64];
        let r: Vec[String] = v.iter().map(|x| f"payload-element-number-{x}-aaaaaaaaaaaaaaaaaaaa").filter(|s| s.len() > 0i64).collect();
        println(f"{r.len()} {r[0i64]} {r[3i64]}");
        round = round + 1i64;
    }
}
"#,
            [
                "4 payload-element-number-1-aaaaaaaaaaaaaaaaaaaa payload-element-number-4-aaaaaaaaaaaaaaaaaaaa",
            ]
            .repeat(40)
            .as_slice(),
            "asan_b04_2_nonterminal_fstring_map_no_leak",
        );
    }

    #[test]
    fn asan_b04_2_flat_map_adaptor_outer_heap_no_leak() {
        // B-2026-07-04-2 sub-part 1 (flat_map adaptor-carrying outer): an outer
        // that carries its own adaptor (`a.iter().filter(g).flat_map(|p| p.iter())
        // .collect()`) pre-collects the outer to a Vec[Vec[String]] temp, then
        // reuses the identity flat_map. The temp is dropped at block exit; each
        // flattened element clones into the result. 30x heap payloads.
        assert_clean_asan_run(
            r#"
fn main() {
    let mut round: i64 = 0i64;
    while round < 30i64 {
        let a: Vec[Vec[String]] = Vec[
            Vec["flat-outer-alpha-aaaaaaaaaaaaaaaaaaaaaa".to_string(),
                "flat-outer-bravo-bbbbbbbbbbbbbbbbbbbbbb".to_string()],
            Vec["flat-outer-charlie-cccccccccccccccccccc".to_string()]
        ];
        let r: Vec[String] = a.iter().filter(|v| v.len() > 0i64).flat_map(|p| p.iter()).collect();
        println(f"{r.len()} {r[0i64]} {r[2i64]}");
        round = round + 1i64;
    }
}
"#,
            ["3 flat-outer-alpha-aaaaaaaaaaaaaaaaaaaaaa flat-outer-charlie-cccccccccccccccccccc"]
                .repeat(30)
                .as_slice(),
            "asan_b04_2_flat_map_adaptor_outer_heap_no_leak",
        );
    }

    #[test]
    fn asan_b04_2_cycle_take_heap_no_leak() {
        // B-2026-07-04-2 sub-part 1 (cycle+take): `v.iter().cycle().take(n)
        // .collect()` repeats the source until n elements. Each element is
        // cloned on push (the source may be read multiple times), so the
        // borrowed source survives and every clone is owned once. 40x heap
        // payloads; source re-read each round.
        assert_clean_asan_run(
            r#"
fn main() {
    let mut round: i64 = 0i64;
    while round < 40i64 {
        let v: Vec[String] = Vec[
            "cycle-take-alpha-aaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(),
            "cycle-take-bravo-bbbbbbbbbbbbbbbbbbbbbbbbbb".to_string()
        ];
        let r: Vec[String] = v.iter().cycle().take(5i64).collect();
        println(f"{r.len()} {r[0i64]} {r[4i64]} {v.len()}");
        round = round + 1i64;
    }
}
"#,
            [
                "5 cycle-take-alpha-aaaaaaaaaaaaaaaaaaaaaaaaaa cycle-take-alpha-aaaaaaaaaaaaaaaaaaaaaaaaaa 2",
            ]
            .repeat(40)
            .as_slice(),
            "asan_b04_2_cycle_take_heap_no_leak",
        );
    }

    #[test]
    fn asan_b04_2_scan_heap_no_leak() {
        // B-2026-07-04-2 sub-part 1 (scan): `v.iter().scan(init, |acc, x|
        // Some((new, out))).collect()` threads a running accumulator and
        // collects each output. Here the output is a heap f-string. The pushed
        // outputs are owned once by the result; the source survives. 40x heap
        // payloads.
        assert_clean_asan_run(
            r#"
fn main() {
    let mut round: i64 = 0i64;
    while round < 40i64 {
        let v: Vec[i64] = Vec[1i64, 2i64, 3i64];
        let r: Vec[String] = v.iter().scan(0i64, |acc, x| Some((acc + x, f"running-sum-payload-{acc}-plus-{x}"))).collect();
        println(f"{r.len()} {r[0i64]} {r[2i64]} {v.len()}");
        round = round + 1i64;
    }
}
"#,
            ["3 running-sum-payload-0-plus-1 running-sum-payload-3-plus-3 3"]
                .repeat(40)
                .as_slice(),
            "asan_b04_2_scan_heap_no_leak",
        );
    }

    #[test]
    fn asan_column_string_index_clone_out_no_leak() {
        // S6c-12 Slice 5: `Column[String]` indexing `c[i] -> Option[String]`
        // under `karac build` DEEP-CLONES the element so the returned Option
        // owns an independent heap and the column keeps its copy. Exercises
        // both a direct `c[i].unwrap()` and the `self[i]` form inside a user
        // `impl … for Column[String]` (the Slice 5 headline). Loops 40× with
        // >=36-byte payloads for LSan reachability; the per-round `a`/`b`
        // clones AND the column's 3 owned strings must all free with no
        // double-free / UAF (mac ASAN) and no leak (Linux LSan CI).
        assert_clean_asan_run(
            r#"
trait Pick { fn at(ref self, i: i64) -> String; }
impl Pick for Column[String] {
    fn at(ref self, i: i64) -> String { self[i].unwrap() }
}
fn main() {
    let mut round: i64 = 0i64;
    while round < 40i64 {
        let v: Vec[String] = Vec[
            "col-string-index-alpha-aaaaaaaaaaaaaaaaaaaa".to_string(),
            "col-string-index-bravo-bbbbbbbbbbbbbbbbbbbb".to_string(),
            "col-string-index-charlie-cccccccccccccccccc".to_string()
        ];
        let c: Column[String] = Column.from_vec(v);
        let a: String = c[0i64].unwrap();
        let b: String = c.at(2i64);
        println(f"{c.len()} {a} {b}");
        round = round + 1i64;
    }
}
"#,
            [
                "3 col-string-index-alpha-aaaaaaaaaaaaaaaaaaaa col-string-index-charlie-cccccccccccccccccc",
            ]
            .repeat(40)
            .as_slice(),
            "asan_column_string_index_clone_out_no_leak",
        );
    }

    #[test]
    fn asan_column_from_vec_temp_string_move_no_leak() {
        // B-2026-07-06-1: `Column.from_vec(<temporary Vec[String]>)` under
        // `karac build` MOVES the source's String structs into the column
        // (bitwise memcpy transfers each heap) and frees ONLY the source's
        // OUTER buffer — the elements are not drained, so the column becomes
        // their sole owner. No clone, no double-free, no leak — mirroring the
        // POD-temp path. Covers BOTH temp shapes: an inline array literal and a
        // function-call result. Loops 40x with >=36-byte payloads for LSan
        // reachability; each round the columns' 5 moved strings + the 2
        // index-clones (`a`/`e`) must all free exactly once (mac ASAN: no
        // double-free/UAF; Linux LSan CI: no leak). Sibling of
        // `asan_column_string_index_clone_out_no_leak` (a let-bound source).
        assert_clean_asan_run(
            r#"
fn mk() -> Vec[String] {
    let mut v: Vec[String] = Vec.new();
    v.push("from-vec-temp-call-delta-dddddddddddddddddddd".to_string());
    v.push("from-vec-temp-call-echo-eeeeeeeeeeeeeeeeeeeeee".to_string());
    v
}
fn main() {
    let mut round: i64 = 0i64;
    let mut total: i64 = 0i64;
    while round < 40i64 {
        let c: Column[String] = Column.from_vec([
            "from-vec-temp-lit-alpha-aaaaaaaaaaaaaaaaaaaa".to_string(),
            "from-vec-temp-lit-bravo-bbbbbbbbbbbbbbbbbbbb".to_string(),
            "from-vec-temp-lit-charlie-cccccccccccccccccc".to_string()
        ]);
        let d: Column[String] = Column.from_vec(mk());
        let a: String = c[0i64].unwrap();
        let e: String = d[1i64].unwrap();
        total = total + c.len() + d.len() + a.len() + e.len();
        round = round + 1i64;
    }
    println(f"{total}");
}
"#,
            &["3800"], // 40 * (3 + 2 + 44 + 46)
            "asan_column_from_vec_temp_string_move_no_leak",
        );
    }

    #[test]
    fn asan_b04_2_named_fn_map_collect_no_leak() {
        // B-2026-07-04-2 sub-part 2: a NAMED-FUNCTION `map` arg over a heap
        // source. The fix wraps `<fn>` in a synthetic body `<fn>(p)`, so each
        // element flows through `tag`/`nlen` and is pushed. `tag(s) -> s` returns
        // the String (entry-copied under caller-retains), `nlen(s) -> s.len()`
        // reads it; `.iter()` borrows, so the source Vec survives. Loops 40× with
        // >=45-byte payloads for LSan reachability; reads a mapped element each
        // round to expose any double-free/UAF.
        assert_clean_asan_run(
            r#"
fn tag(s: String) -> String { s }
fn nlen(s: String) -> i64 { s.len() }
fn main() {
    let mut round: i64 = 0i64;
    while round < 40i64 {
        let w: Vec[String] = Vec[
            "named-fn-map-payload-alpha-aaaaaaaaaaaaaaaaaa".to_string(),
            "named-fn-map-payload-bravo-bbbbbbbbbbbbbbbbbb".to_string(),
            "named-fn-map-payload-charlie-cccccccccccccccc".to_string()
        ];
        let mapped: Vec[String] = w.iter().map(tag).collect();
        let lens: Vec[i64] = w.iter().map(nlen).collect();
        let m1: String = mapped[1i64];
        println(f"{mapped.len()} {lens[0]} {lens[2]} {w.len()} {m1}");
        round = round + 1i64;
    }
}
"#,
            ["3 45 45 3 named-fn-map-payload-bravo-bbbbbbbbbbbbbbbbbb"]
                .repeat(40)
                .as_slice(),
            "asan_b04_2_named_fn_map_collect_no_leak",
        );
    }

    #[test]
    fn asan_b04_2_destructuring_closure_heap_collect_no_leak() {
        // B-2026-07-04-2 sub-part 2 (destructuring half): a tuple-destructuring
        // `map` param over a HEAP element — `enumerate().map(|(i, s)| s)` over
        // `Vec[String]`. The fix desugars it to `|__dp| { let i = __dp.0; let s =
        // __dp.1; s }`: the index i64 is bound and dropped, the String field is
        // MOVED out of the `(i64, String)` tuple and pushed. Neither a leak (the
        // tuple's non-returned field) nor a double-free (the moved String) may
        // result. `.iter()` borrows, so the source survives. 40× with ≥45-byte
        // payloads for LSan reachability; reads a collected element each round.
        assert_clean_asan_run(
            r#"
fn main() {
    let mut round: i64 = 0i64;
    while round < 40i64 {
        let w: Vec[String] = Vec[
            "destructure-map-payload-alpha-aaaaaaaaaaaaaaaa".to_string(),
            "destructure-map-payload-bravo-bbbbbbbbbbbbbbbb".to_string(),
            "destructure-map-payload-charlie-cccccccccccccc".to_string()
        ];
        let kept: Vec[String] = w.iter().enumerate().map(|(i, s)| s).collect();
        let k1: String = kept[1i64];
        println(f"{kept.len()} {w.len()} {k1}");
        round = round + 1i64;
    }
}
"#,
            ["3 3 destructure-map-payload-bravo-bbbbbbbbbbbbbbbb"]
                .repeat(40)
                .as_slice(),
            "asan_b04_2_destructuring_closure_heap_collect_no_leak",
        );
    }

    #[test]
    fn asan_b04_2_chain_identity_heap_collect_no_leak() {
        // B-2026-07-04-2 sub-part 1 (chain half): `A.chain(B).collect()` over two
        // heap `Vec[String]` sources. The fix emits `for x in A { acc.push x };
        // for y in B { acc.push y }` — each `push` over a borrowed source CLONES,
        // so both sources survive and the accumulator owns independent copies.
        // Neither a leak (a source's buffer / an un-cloned element) nor a
        // double-free (a shared heap element) may result. 40× with ≥45-byte
        // payloads for LSan reachability; reads a collected element each round.
        assert_clean_asan_run(
            r#"
fn main() {
    let mut round: i64 = 0i64;
    while round < 40i64 {
        let a: Vec[String] = Vec[
            "chain-collect-payload-alpha-aaaaaaaaaaaaaaaaaa".to_string(),
            "chain-collect-payload-bravo-bbbbbbbbbbbbbbbbbb".to_string()
        ];
        let b: Vec[String] = Vec[
            "chain-collect-payload-charlie-cccccccccccccccc".to_string()
        ];
        let r: Vec[String] = a.iter().chain(b.iter()).collect();
        let r2: String = r[2i64];
        println(f"{r.len()} {a.len()} {b.len()} {r2}");
        round = round + 1i64;
    }
}
"#,
            ["3 2 1 chain-collect-payload-charlie-cccccccccccccccc"]
                .repeat(40)
                .as_slice(),
            "asan_b04_2_chain_identity_heap_collect_no_leak",
        );
    }

    #[test]
    fn asan_b04_2_flat_map_heap_collect_no_leak() {
        // B-2026-07-04-2 sub-part 1 (flat_map): `<outer>.flat_map(|v|
        // v.iter()).collect()` over a HEAP `Vec[Vec[String]]`. The nested-loop
        // lowering `for v in outer { for x in v.iter() { acc.push(x) } }`
        // iterates and clones on `push`, so the nested source survives and the
        // flattened accumulator owns independent copies — no leak, no
        // double-free. 40× with ≥44-byte payloads for LSan reachability; reads a
        // flattened element each round.
        assert_clean_asan_run(
            r#"
fn main() {
    let mut round: i64 = 0i64;
    while round < 40i64 {
        let xs: Vec[Vec[String]] = Vec[
            Vec["flat-map-payload-alpha-aaaaaaaaaaaaaaaaaaaa".to_string(),
                "flat-map-payload-bravo-bbbbbbbbbbbbbbbbbbbb".to_string()],
            Vec["flat-map-payload-charlie-cccccccccccccccccc".to_string()]
        ];
        let r: Vec[String] = xs.iter().flat_map(|v| v.iter()).collect();
        let r2: String = r[2i64];
        println(f"{r.len()} {xs.len()} {r2}");
        round = round + 1i64;
    }
}
"#,
            ["3 2 flat-map-payload-charlie-cccccccccccccccccc"]
                .repeat(40)
                .as_slice(),
            "asan_b04_2_flat_map_heap_collect_no_leak",
        );
    }

    #[test]
    fn asan_fresh_temp_source_enumerate_collect_no_double_free() {
        // B-2026-07-04-5: a collect-adaptor chain whose SOURCE is a fresh-temp
        // call result (`mk().iter()…`) rather than a named local. The for-loop
        // over the materialized `mk()` temp resolved its element type from the
        // span-colliding `owned_temp_drops` — which held the OUTERMOST result
        // `Vec[(i64, String)]` instead of the source `Vec[String]` — and so read
        // and dropped the `Vec[String]` buffer at the wider `(i64, String)`
        // element stride, freeing garbage (`pointer being freed was not
        // allocated`). The terminal heap enumerate (whole `(i64, String)` tuple
        // pushed and later dropped alongside the freed `mk()` temp) is the
        // double-free shape; `enumerate().map(|p| p.1)` and a plain `.map` over a
        // fresh-temp source ride the same corrected element-type resolution.
        // Reads `.0`/`.1` and an element each round to expose the free.
        assert_clean_asan_run(
            r#"
fn mk() -> Vec[String] {
    return Vec[
        "fresh-temp-enum-payload-alpha-aaaaaaaaaaaaaaaa".to_string(),
        "fresh-temp-enum-payload-bravo-bbbbbbbbbbbbbbbb".to_string()
    ];
}
fn main() {
    let mut round: i64 = 0i64;
    while round < 40i64 {
        let he: Vec[(i64, String)] = mk().iter().enumerate().collect();
        let e0: (i64, String) = he[0];
        let e1: (i64, String) = he[1];
        let hm: Vec[String] = mk().iter().enumerate().map(|p| p.1).collect();
        let m1: String = hm[1];
        let hl: Vec[i64] = mk().iter().map(|s| s.len()).collect();
        println(f"{he.len()} {e0.0} {e0.1} {e1.0} {e1.1} {m1} {hl[0]}");
        round = round + 1i64;
    }
}
"#,
            [
                "2 0 fresh-temp-enum-payload-alpha-aaaaaaaaaaaaaaaa 1 fresh-temp-enum-payload-bravo-bbbbbbbbbbbbbbbb fresh-temp-enum-payload-bravo-bbbbbbbbbbbbbbbb 46",
            ]
            .repeat(40)
            .as_slice(),
            "asan_fresh_temp_source_enumerate_collect_no_double_free",
        );
    }

    #[test]
    fn asan_heap_env_closure_freed_no_leak() {
        assert_clean_asan_run(
            r#"
fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }
fn main() {
    let f = make(21i64);
    println(f"{f(21i64)}");
}
"#,
            &["42"],
            "asan_heap_env_closure_freed_no_leak",
        );
    }

    #[test]
    fn asan_heap_env_closure_multi_call_freed_no_leak() {
        assert_clean_asan_run(
            r#"
fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }
fn main() {
    let f = make(20i64);
    let a = f(1i64);
    let b = f(2i64);
    println(f"{a + b}");
}
"#,
            &["43"],
            "asan_heap_env_closure_multi_call_freed_no_leak",
        );
    }

    #[test]
    fn asan_closure_mut_ref_capture_no_leak() {
        // B-2026-07-11-23 — a non-escaping stored closure that MUTATES a captured
        // local captures it BY REFERENCE (a `ptr` to the outer slot in the env).
        // The body writes through the pointer to the real binding, so the write
        // lands on the outer `c` (yielding 7 over `f(3); f(4)`) and the by-ref
        // env carries no separate heap allocation to leak — a wrong (value-copy)
        // capture would silently drop the write. Also mixes in a read-only (by-
        // value) capture `k` to exercise the mixed per-name env layout.
        assert_clean_asan_run(
            r#"
fn main() {
    let mut c: i64 = 0;
    let k: i64 = 100;
    let f = |x: i64| { c = c + x + k; };
    f(3i64);
    f(4i64);
    println(f"{c}");
}
"#,
            &["207"],
            "asan_closure_mut_ref_capture_no_leak",
        );
    }

    /// Shared-ownership inc-on-copy (B-2026-06-22-2): copying a heap-env closure
    /// binding (`let g = f`, plus a copy-of-a-copy `let h = g`) shares ONE RC env
    /// box across all owners — the copy increments the refcount and each owner's
    /// `FreeClosureEnv` decrements, so the box is freed EXACTLY once. Asserts no
    /// leak (LSan) and no use-after-free / double-free (ASAN). Without the
    /// inc-on-copy the box would be under-counted and freed early (UAF) by the
    /// first owner's scope exit while later owners still alias it.
    #[test]
    fn asan_heap_env_closure_copy_freed_no_leak() {
        assert_clean_asan_run(
            r#"
fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }
fn main() {
    let f = make(10i64);
    let g = f;
    let h = g;
    println(f"{f(1i64) + g(2i64) + h(3i64)}");
}
"#,
            &["36"],
            "asan_heap_env_closure_copy_freed_no_leak",
        );
    }

    /// Slice 2 (B-2026-06-22-2): an escaping closure that captures a heap
    /// String/Vec value. The env OWNS the buffer (freed by the per-closure
    /// env-drop fn at RC-zero). A captured owned PARAM shallow-aliases the
    /// caller's buffer, so it is DEEP-COPIED into the env (caller keeps its own,
    /// env owns an independent copy); a captured LOCAL is moved (the source
    /// binding's cap is zeroed so it does not double-free). All strings exceed
    /// the SSO inline limit so the heap path is exercised. Asserts no leak (LSan) + no
    /// double-free / UAF (ASAN) — without the env-drop the buffer leaks; without
    /// the param deep-copy the caller and env both free it (double-free, which
    /// glibc detects for Vec and silently corrupts for String).
    #[test]
    fn asan_heap_env_string_param_capture_no_leak() {
        assert_clean_asan_run(
            r#"
fn make(p: String) -> Fn(i64) -> i64 { |n| p.len() + n }
fn main() {
    let name = String.from("a heap-backed string well beyond the sso inline limit");
    let f = make(name);
    println(f"{f(0i64)}");
}
"#,
            &["53"],
            "asan_heap_env_string_param_capture_no_leak",
        );
    }

    #[test]
    fn asan_heap_env_vec_param_capture_no_leak() {
        assert_clean_asan_run(
            r#"
fn make(v: Vec[i64]) -> Fn(i64) -> i64 { |n| v.len() + n }
fn main() {
    let mut xs = Vec.new();
    xs.push(1i64);
    xs.push(2i64);
    xs.push(3i64);
    let f = make(xs);
    println(f"{f(10i64)}");
}
"#,
            &["13"],
            "asan_heap_env_vec_param_capture_no_leak",
        );
    }

    #[test]
    fn asan_heap_env_local_string_capture_no_leak() {
        assert_clean_asan_run(
            r#"
fn make() -> Fn(i64) -> i64 {
    let s = String.from("a heap-backed string well beyond the sso inline limit");
    |n| s.len() + n
}
fn main() { let f = make(); println(f"{f(0i64)}"); }
"#,
            &["53"],
            "asan_heap_env_local_string_capture_no_leak",
        );
    }

    #[test]
    fn asan_heap_env_local_vec_capture_no_leak() {
        assert_clean_asan_run(
            r#"
fn make() -> Fn(i64) -> i64 {
    let mut v = Vec.new();
    v.push(5i64);
    v.push(6i64);
    |n| v.len() + n
}
fn main() { let f = make(); println(f"{f(1i64)}"); }
"#,
            &["3"],
            "asan_heap_env_local_vec_capture_no_leak",
        );
    }

    /// The heap-capture env box is RC-shared across a copy (`let g = f`); the
    /// captured buffer is freed EXACTLY once when the last owner drops.
    #[test]
    fn asan_heap_env_string_capture_copied_no_double_free() {
        assert_clean_asan_run(
            r#"
fn make(p: String) -> Fn(i64) -> i64 { |n| p.len() + n }
fn main() {
    let f = make(String.from("a heap-backed string well beyond the sso inline limit"));
    let g = f;
    println(f"{f(0i64) + g(0i64)}");
}
"#,
            &["106"],
            "asan_heap_env_string_capture_copied_no_double_free",
        );
    }

    /// A mixed env (POD `base` + heap `String`): the env-drop frees ONLY the
    /// String field's buffer, leaving the POD word untouched.
    #[test]
    fn asan_heap_env_mixed_pod_string_capture_no_leak() {
        assert_clean_asan_run(
            r#"
fn make(p: String, base: i64) -> Fn(i64) -> i64 { |n| p.len() + base + n }
fn main() {
    let f = make(String.from("a heap-backed string well beyond the sso inline limit"), 100i64);
    println(f"{f(5i64)}");
}
"#,
            &["158"],
            "asan_heap_env_mixed_pod_string_capture_no_leak",
        );
    }

    /// Return-again move-out (B-2026-06-22-2): a relay RE-RETURNS a bound
    /// heap-env closure (explicit `return f`, bare-identifier tail, relay-of-a-
    /// relay, and copy-then-return). The RC env box MOVES OUT of each relay to
    /// its caller — the source binding's `FreeClosureEnv` is neutralized on the
    /// returning path, so the box is freed EXACTLY once at the final owner's
    /// scope exit. Asserts no leak (LSan) and no use-after-free / double-free
    /// (ASAN). Without the move-out, the relay's scope exit would free the box
    /// the caller still holds (UAF), or double-free across copy-then-return.
    #[test]
    fn asan_heap_env_closure_returned_again_freed_no_leak() {
        assert_clean_asan_run(
            r#"
fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }
fn relay(k: i64) -> Fn(i64) -> i64 { let f = make(k); return f; }
fn relay_tail(k: i64) -> Fn(i64) -> i64 { let f = make(k); f }
fn relay2(k: i64) -> Fn(i64) -> i64 { let g = relay_tail(k); g }
fn relay_copy(k: i64) -> Fn(i64) -> i64 { let f = make(k); let g = f; g }
fn main() {
    let a = relay(10i64);
    let b = relay_tail(20i64);
    let c = relay2(30i64);
    let d = relay_copy(40i64);
    println(f"{a(1i64) + b(2i64) + c(3i64) + d(4i64)}");
}
"#,
            &["110"],
            "asan_heap_env_closure_returned_again_freed_no_leak",
        );
    }

    /// Store-in-struct slice (B-2026-06-22-2): a fresh heap-env closure stored in
    /// a struct literal field (`let h = H { f: make(k) }`) is RC-dropped
    /// per-instance via a `FreeClosureEnv` on that field at the struct local's
    /// scope exit — freed EXACTLY once. Covers a closure-only struct and a struct
    /// with a sibling data field. Asserts no leak (LSan) and no use-after-free /
    /// double-free (ASAN). Without the instance field drop the env would leak;
    /// with a (wrong) type-driven drop a sibling stack-env closure would crash —
    /// neither happens here.
    #[test]
    fn asan_heap_env_stored_in_struct_field_freed_no_leak() {
        assert_clean_asan_run(
            r#"
struct H { f: Fn(i64) -> i64 }
struct G { f: Fn(i64) -> i64, n: i64 }
fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }
fn main() {
    let h = H { f: make(21i64) };
    let g = G { f: make(20i64), n: 2i64 };
    println(f"{(h.f)(21i64) + (g.f)(20i64) + g.n}");
}
"#,
            &["84"],
            "asan_heap_env_stored_in_struct_field_freed_no_leak",
        );
    }

    /// Binding-source slice (B-2026-06-22-2): a heap-env BINDING stored into a
    /// struct field (`let h = H { f: f }`) is co-owned — the store bumps the shared
    /// RC env's refcount, so both the source binding's scope-exit drop AND the
    /// field's instance drop fire and the box is freed EXACTLY once. Covers
    /// co-ownership with the source still used after the store, a store through a
    /// COPY of the binding (`let g = f; H { f: g }`), and the composite where the
    /// source is MOVED OUT (tail return) after being stored — the field drop decs,
    /// the caller frees the last ref. Without the store inc the box would be
    /// double-freed (ASAN); without the field drop it would leak (LSan).
    #[test]
    fn asan_heap_env_binding_stored_in_struct_field_freed_no_leak() {
        assert_clean_asan_run(
            r#"
struct H { f: Fn(i64) -> i64 }
struct G { f: Fn(i64) -> i64, n: i64 }
fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }
fn relay(k: i64) -> Fn(i64) -> i64 { let f = make(k); let h = H { f: f }; f }
fn main() {
    let f = make(10i64);
    let h = H { f: f };
    let a = f(1i64);
    let b = (h.f)(2i64);
    let g = make(5i64);
    let g2 = g;
    let gg = G { f: g2, n: 3i64 };
    let c = g(4i64);
    let d = (gg.f)(6i64);
    let e = gg.n;
    let r = relay(30i64);
    let k = r(7i64);
    println(f"{a + b + c + d + e + k}");
}
"#,
            &["83"],
            "asan_heap_env_binding_stored_in_struct_field_freed_no_leak",
        );
    }

    /// Aggregate-escape slice (B-2026-06-22-2): a function may RETURN a struct that
    /// OWNS a heap-env closure field. The env box MOVES OUT inside the struct — the
    /// callee neutralizes the owner's field env slot on the returning path (so its
    /// `FreeClosureEnv` no-ops), and the caller's `let r = build(..)` binding
    /// registers an instance `FreeClosureEnv` on each owned field and frees it once.
    /// Covers an explicit return, a bare-tail return, a sibling data field, a
    /// binding-source field (store inc → rc 2, then move-out decs to 1 at callee
    /// scope exit), and a relay-of-aggregate (fixpoint). Each env freed EXACTLY
    /// once across the move boundary — without the move-out the callee would free
    /// the box the caller holds (UAF); without the caller field drop it would leak.
    #[test]
    fn asan_heap_env_aggregate_returned_freed_no_leak() {
        assert_clean_asan_run(
            r#"
struct H { f: Fn(i64) -> i64 }
struct G { f: Fn(i64) -> i64, n: i64 }
fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }
fn build(k: i64) -> H { let h = H { f: make(k) }; return h; }
fn build_tail(k: i64) -> H { let h = H { f: make(k) }; h }
fn build_data(k: i64) -> G { let g = G { f: make(k), n: 3i64 }; g }
fn build_binding(k: i64) -> H { let f = make(k); let h = H { f: f }; return h; }
fn relay(k: i64) -> H { let r = build(k); return r; }
fn main() {
    let a = build(10i64);
    let b = build_tail(20i64);
    let c = build_data(30i64);
    let d = build_binding(40i64);
    let e = relay(5i64);
    println(f"{(a.f)(1i64) + (b.f)(2i64) + (c.f)(3i64) + c.n + (d.f)(4i64) + (e.f)(6i64)}");
}
"#,
            &["124"],
            "asan_heap_env_aggregate_returned_freed_no_leak",
        );
    }

    /// Tuple-store slice (B-2026-06-22-2): a heap-env closure stored in a tuple
    /// element is RC-dropped per-instance via a `FreeClosureEnv` on that element.
    /// Covers a FRESH-call element with a sibling data element, a BINDING source
    /// (store inc → rc 2, source still used, both drop → one free), and two
    /// closures in one tuple. Each env freed EXACTLY once at scope exit — without
    /// the element drop they leak (LSan); with the binding store missing the inc it
    /// double-frees (ASAN).
    #[test]
    fn asan_heap_env_stored_in_tuple_freed_no_leak() {
        assert_clean_asan_run(
            r#"
fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }
fn main() {
    let t = (make(10i64), 2i64);
    let f = make(20i64);
    let u = (f, 3i64);
    let v = (make(5i64), make(7i64));
    println(f"{(t.0)(1i64) + t.1 + f(0i64) + (u.0)(1i64) + u.1 + (v.0)(1i64) + (v.1)(2i64)}");
}
"#,
            &["72"],
            "asan_heap_env_stored_in_tuple_freed_no_leak",
        );
    }

    /// Array-store slice (B-2026-06-22-2): a heap-env closure stored in a
    /// fixed-size array element is RC-dropped per-instance via a `FreeClosureEnv`
    /// on that element GEP. Covers a FRESH single-element array, a multi-element
    /// array (two closures, each called through), and a BINDING source (store inc →
    /// rc 2, source still used, both drop → one free). Each env freed EXACTLY once
    /// at scope exit — without the element drop they leak (LSan); with the binding
    /// store missing the inc it double-frees (ASAN).
    #[test]
    fn asan_heap_env_stored_in_array_freed_no_leak() {
        assert_clean_asan_run(
            r#"
fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }
fn main() {
    let a: Array[Fn(i64) -> i64, 1] = [make(10i64)];
    let b: Array[Fn(i64) -> i64, 2] = [make(5i64), make(7i64)];
    let f = make(20i64);
    let c: Array[Fn(i64) -> i64, 1] = [f];
    println(f"{(a[0])(1i64) + (b[0])(1i64) + (b[1])(2i64) + f(0i64) + (c[0])(1i64)}");
}
"#,
            &["67"],
            "asan_heap_env_stored_in_array_freed_no_leak",
        );
    }

    /// Vec-store slice (B-2026-06-22-2): heap-env closures pushed into a `Vec[Fn]`
    /// are RC-dropped by a DYNAMIC `0..len` drop loop at the Vec's scope exit.
    /// Covers a LOOP of fresh pushes (the dynamic-length case — three element envs
    /// freed by the loop) and a BINDING push (push inc → rc 2, source `f` still
    /// used, both the source's `FreeClosureEnv` and the Vec drop loop decrement →
    /// one free). Without the drop loop the element envs leak (LSan); with the
    /// binding push missing the inc it double-frees (ASAN). Also exercises the
    /// auto-par bail (`let f = make(..)` no longer parallelized with `Vec.new()`).
    #[test]
    fn asan_heap_env_stored_in_vec_freed_no_leak() {
        assert_clean_asan_run(
            r#"
fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }
fn main() {
    let mut v: Vec[Fn(i64) -> i64] = Vec.new();
    let mut i = 0i64;
    while i < 3i64 { v.push(make(i)); i = i + 1i64; }
    let f = make(20i64);
    let mut w: Vec[Fn(i64) -> i64] = Vec.new();
    w.push(f);
    let mut acc = 0i64;
    let mut j = 0i64;
    while j < v.len() { acc = acc + (v[j])(10i64); j = j + 1i64; }
    acc = acc + f(0i64) + (w[0])(1i64);
    println(f"{acc}");
}
"#,
            &["74"],
            "asan_heap_env_stored_in_vec_freed_no_leak",
        );
    }

    /// Container-escape slice (B-2026-06-22-2): a function returns a TUPLE / ARRAY
    /// owning heap-env closure elements; the callee moves the env boxes out (its
    /// return neutralizes the owner's element env slots) and the caller's binding
    /// adopts a per-element `FreeClosureEnv`. Covers a fresh-element tuple escape, a
    /// fresh-element array escape, and a BINDING-element tuple escape (store inc →
    /// callee source drop + caller adopted drop = one free). Each env freed EXACTLY
    /// once — without the caller-adopt it leaks (LSan); without the callee neutralize
    /// it double-frees (ASAN).
    #[test]
    fn asan_heap_env_container_escape_freed_no_leak() {
        assert_clean_asan_run(
            r#"
fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }
fn build_t(k: i64) -> (Fn(i64) -> i64, i64) { let t = (make(k), 1i64); t }
fn build_a(k: i64) -> Array[Fn(i64) -> i64, 1] { let a: Array[Fn(i64) -> i64, 1] = [make(k)]; a }
fn build_bf(k: i64) -> (Fn(i64) -> i64, i64) { let f = make(k); let t = (f, 2i64); t }
fn main() {
    let r = build_t(10i64);
    let s = build_a(20i64);
    let u = build_bf(30i64);
    println(f"{(r.0)(1i64) + r.1 + (s[0])(2i64) + (u.0)(0i64) + u.1}");
}
"#,
            &["66"],
            "asan_heap_env_container_escape_freed_no_leak",
        );
    }

    /// Vec-escape slice (B-2026-06-22-2): a function returns a closure-owning
    /// `Vec[Fn]`; the callee moves the BUFFER out (its tail-return cap-zero
    /// suppresses its own dynamic drop loop) and the caller's binding adopts that
    /// `0..len` drop loop. Covers a loop-built escape (3 element envs adopted), a
    /// BINDING-push escape (store inc → callee source drop + caller loop drop = one
    /// free), and a RELAY (the buffer flows callee→relay→caller, freed once at the
    /// outermost binding). Without the caller-adopt the envs leak (LSan); if the
    /// callee's loop weren't cap-zero suppressed it double-frees (ASAN).
    #[test]
    fn asan_heap_env_vec_escape_freed_no_leak() {
        assert_clean_asan_run(
            r#"
fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }
fn build(n: i64) -> Vec[Fn(i64) -> i64] {
    let mut v: Vec[Fn(i64) -> i64] = Vec.new();
    let mut i = 0i64;
    while i < n { v.push(make(i)); i = i + 1i64; }
    v
}
fn build_bf(k: i64) -> Vec[Fn(i64) -> i64] {
    let f = make(k);
    let mut v: Vec[Fn(i64) -> i64] = Vec.new();
    v.push(f);
    v
}
fn relay(n: i64) -> Vec[Fn(i64) -> i64] { let q = build(n); q }
fn main() {
    let r = build(3i64);
    let w = build_bf(20i64);
    let z = relay(2i64);
    let mut acc = 0i64;
    let mut j = 0i64;
    while j < r.len() { acc = acc + (r[j])(10i64); j = j + 1i64; }
    acc = acc + (w[0])(2i64);
    acc = acc + (z[0])(5i64) + (z[1])(6i64);
    println(f"{acc}");
}
"#,
            &["67"],
            "asan_heap_env_vec_escape_freed_no_leak",
        );
    }

    /// By-value arg-pass slice (B-2026-06-22-2): a heap-env closure BINDING passed
    /// BY VALUE to a borrows-only callee (one that only CALLS it) is a pure
    /// BORROW — the callee never frees the shared RC env, and the CALLER retains
    /// sole ownership and RC-drops it EXACTLY once at scope exit (no inc, no
    /// move-out). Covers the bare binding (`apply(f, ..)`), a borrow inside a
    /// loop (`sumcalls`), a copy passed by value (`apply(g, ..)`), a borrows-only
    /// aggregate builder (`build2` — its own returned env is a SECOND, distinct
    /// box freed once), and continued use of `f` after the borrow. Asserts no
    /// leak (LSan) and no use-after-free / double-free (ASAN). Without the
    /// borrow-only treatment the callee would either free the caller's box early
    /// (UAF) or the caller would free it twice; an erroneous inc would leak it.
    #[test]
    fn asan_heap_env_arg_pass_borrow_freed_no_leak() {
        assert_clean_asan_run(
            r#"
struct H { f: Fn(i64) -> i64 }
fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }
fn apply(g: Fn(i64) -> i64, x: i64) -> i64 { g(x) }
fn sumcalls(g: Fn(i64) -> i64, n: i64) -> i64 {
    let mut s = 0i64;
    let mut i = 0i64;
    while i < n { s = s + g(i); i = i + 1i64; }
    s
}
fn build2(g: Fn(i64) -> i64) -> H { let local = make(7i64); let h = H { f: local }; let _u = g(0i64); h }
fn main() {
    let f = make(10i64);
    let g = f;
    let a = apply(f, 5i64);
    let b = sumcalls(g, 4i64);
    let r = build2(f);
    let c = (r.f)(3i64);
    let d = f(100i64);
    println(f"{a + b + c + d}");
}
"#,
            &["181"],
            "asan_heap_env_arg_pass_borrow_freed_no_leak",
        );
    }

    /// Owner by-value arg-pass slice (B-2026-06-22-2): a heap-env STRUCT OWNER
    /// (`let a = H { f: make(k), g: make(k) }`) passed BY VALUE to a borrows-only
    /// callee (one that only CALLS the owner's closure fields via `(h.f)(x)`) is a
    /// pure BORROW — the callee never frees the shared RC envs (a param gets no
    /// Fn-field `FreeClosureEnv`), and the CALLER retains sole ownership and
    /// RC-drops each env EXACTLY once at scope exit (no inc, no move-out — a call
    /// arg is not a return move-out, so the owner's env slots are not neutralized).
    /// The owner stays usable after the call (`(a.f)(1) + (a.g)(1)`). Without the
    /// borrow-only treatment the callee would free the caller's boxes early (UAF)
    /// or the owner would free them twice; an erroneous inc would leak them.
    #[test]
    fn asan_heap_env_struct_owner_arg_pass_borrow_freed_no_leak() {
        assert_clean_asan_run(
            r#"
struct H { f: Fn(i64) -> i64, g: Fn(i64) -> i64 }
fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }
fn use_it(h: H) -> i64 { (h.f)(1i64) + (h.g)(2i64) }
fn main() {
    let a = H { f: make(10i64), g: make(20i64) };
    let r = use_it(a);
    println(f"{r + (a.f)(1i64) + (a.g)(1i64)}");
}
"#,
            &["65"],
            "asan_heap_env_struct_owner_arg_pass_borrow_freed_no_leak",
        );
    }

    /// Owner by-value arg-pass with a sibling HEAP String field: the struct owner is
    /// passed by value to a borrows-only callee. The `Fn` env is borrowed (caller
    /// frees once), and the sibling `String` is handled by the normal owned-struct
    /// arg-pass copy semantics — the caller's owner stays valid and readable after
    /// the call (`a.name`), each heap allocation freed exactly once. Guards against
    /// a String double-free (if the param drop and the caller's owner drop both
    /// freed a shared buffer) or a leak.
    #[test]
    fn asan_heap_env_struct_owner_arg_pass_string_sibling_freed_no_leak() {
        assert_clean_asan_run(
            r#"
struct H { f: Fn(i64) -> i64, name: String }
fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }
fn use_it(h: H) -> i64 { (h.f)(1i64) }
fn main() {
    let a = H { f: make(10i64), name: "an independently long heap string payload here" };
    let r = use_it(a);
    println(f"{r + (a.f)(2i64)}");
    println(a.name);
}
"#,
            &["23", "an independently long heap string payload here"],
            "asan_heap_env_struct_owner_arg_pass_string_sibling_freed_no_leak",
        );
    }

    /// Container owner arg-pass slice (B-2026-06-22-2): a heap-env TUPLE owner with
    /// TWO closure elements passed BY VALUE to a borrows-only callee. The callee
    /// receives a shallow copy of the tuple aliasing the SAME RC env boxes, calls
    /// both elements, and frees neither (a param gets no per-element
    /// `FreeClosureEnv`); the caller retains sole ownership and RC-drops each env
    /// EXACTLY once at scope exit (no inc, no move-out). The owner stays usable
    /// after the call. `(t.0)(1)+(t.1)(2)=33` in the callee, then `(a.0)(1)+(a.1)(1)
    /// =32` after → `65`. Without the borrow treatment the callee would free the
    /// caller's boxes early (UAF) or both would free them (double-free); a stray inc
    /// would leak them.
    #[test]
    fn asan_heap_env_tuple_owner_arg_pass_borrow_freed_no_leak() {
        assert_clean_asan_run(
            r#"
fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }
fn use_t(t: (Fn(i64) -> i64, Fn(i64) -> i64)) -> i64 { (t.0)(1i64) + (t.1)(2i64) }
fn main() {
    let a = (make(10i64), make(20i64));
    let r = use_t(a);
    println(f"{r + (a.0)(1i64) + (a.1)(1i64)}");
}
"#,
            &["65"],
            "asan_heap_env_tuple_owner_arg_pass_borrow_freed_no_leak",
        );
    }

    /// The ARRAY twin: a two-element `Array[Fn,2]` owner borrowed (both elements
    /// called via the `[0, idx]` GEP), reused after the call. Each shared env freed
    /// exactly once. `33` in the callee, `32` after → `65`.
    #[test]
    fn asan_heap_env_array_owner_arg_pass_borrow_freed_no_leak() {
        assert_clean_asan_run(
            r#"
fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }
fn use_a(a: Array[Fn(i64) -> i64, 2]) -> i64 { (a[0])(1i64) + (a[1])(2i64) }
fn main() {
    let a: Array[Fn(i64) -> i64, 2] = [make(10i64), make(20i64)];
    let r = use_a(a);
    println(f"{r + (a[0])(1i64) + (a[1])(1i64)}");
}
"#,
            &["65"],
            "asan_heap_env_array_owner_arg_pass_borrow_freed_no_leak",
        );
    }

    /// The `Vec[Fn]` owner arg-pass — the CRITICAL case. By-value arg-pass passes
    /// the Vec header by value WITHOUT zeroing the caller's cap (unlike `let w = v`,
    /// a move), so it is a BORROW: the callee reads through the shared buffer and
    /// frees nothing; the caller's dynamic per-element drop loop frees every element
    /// env (and the buffer) EXACTLY once. Multi-element to exercise that loop, and
    /// the owner is reused after the call (`(v[0])/(v[1])` still valid — the cap was
    /// not zeroed). `33` in the callee, `32` after → `65`. Were arg-pass a move, the
    /// callee would adopt and free the buffer while the caller's loop freed it too
    /// (double-free), or the element envs would leak.
    #[test]
    fn asan_heap_env_vec_owner_arg_pass_borrow_freed_no_leak() {
        assert_clean_asan_run(
            r#"
fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }
fn use_v(v: Vec[Fn(i64) -> i64]) -> i64 { (v[0])(1i64) + (v[1])(2i64) }
fn main() {
    let mut v: Vec[Fn(i64) -> i64] = Vec.new();
    v.push(make(10i64));
    v.push(make(20i64));
    let r = use_v(v);
    println(f"{r + (v[0])(1i64) + (v[1])(1i64)}");
}
"#,
            &["65"],
            "asan_heap_env_vec_owner_arg_pass_borrow_freed_no_leak",
        );
    }

    /// A TUPLE owner with a sibling HEAP String element passed by value to a
    /// borrows-only callee. The `Fn` env is borrowed (caller frees once) and the
    /// String element rides along by the normal owned arg-pass copy — the caller's
    /// owner stays valid and readable after the call (`a.1`), each heap allocation
    /// freed exactly once. Guards against a String double-free or a leak alongside
    /// the borrowed closure env. `r=(t.0)(1)=11`, `r+(a.0)(2)=23`, then `a.1`.
    #[test]
    fn asan_heap_env_tuple_owner_arg_pass_string_sibling_freed_no_leak() {
        assert_clean_asan_run(
            r#"
fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }
fn use_t(t: (Fn(i64) -> i64, String)) -> i64 { (t.0)(1i64) }
fn main() {
    let a = (make(10i64), "an independently long heap string payload here");
    let r = use_t(a);
    println(f"{r + (a.0)(2i64)}");
    println(a.1);
}
"#,
            &["23", "an independently long heap string payload here"],
            "asan_heap_env_tuple_owner_arg_pass_string_sibling_freed_no_leak",
        );
    }

    /// Reassignment slice (B-2026-06-22-2): `g = f` where both are heap-env closure
    /// bindings is a COPY — the reassignment drops `g`'s OLD env, incs the SHARED
    /// env `f` holds, and stores it, so `g` and `f` co-own one box freed EXACTLY
    /// once while `g`'s original env is freed once at the reassignment. Both `g`
    /// and `f` are used after (`g(1) + f(1)`), confirming the source stays a live
    /// co-owner. Without the drop-old the original `g` env leaks; without the inc
    /// the shared box is freed twice (the source's and `g`'s scope-exit drops).
    #[test]
    fn asan_heap_env_binding_reassign_copy_no_leak() {
        assert_clean_asan_run(
            r#"
fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }
fn main() {
    let f = make(10i64);
    let mut g = make(20i64);
    g = f;
    println(f"{g(1i64) + f(1i64)}");
}
"#,
            &["22"],
            "asan_heap_env_binding_reassign_copy_no_leak",
        );
    }

    /// `g = make(j)` is a MOVE to a fresh env: the reassignment drops `g`'s old env
    /// (freed once) and `g` becomes the sole owner of the fresh one (freed once at
    /// scope exit). Without the drop-old the original env leaks. `g(5) = 35`.
    #[test]
    fn asan_heap_env_binding_reassign_to_fresh_no_leak() {
        assert_clean_asan_run(
            r#"
fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }
fn main() {
    let mut g = make(20i64);
    g = make(30i64);
    println(f"{g(5i64)}");
}
"#,
            &["35"],
            "asan_heap_env_binding_reassign_to_fresh_no_leak",
        );
    }

    /// The strongest leak case: reassigning in a LOOP. Each of the 50 iterations
    /// drops the prior env before storing the next, so every intermediate env box
    /// is freed once — without the per-assignment drop-old, 50 unreachable env
    /// boxes would leak (LSan-caught). The final `make(50*10)`; `g(5) = 505`.
    #[test]
    fn asan_heap_env_binding_reassign_in_loop_no_leak() {
        assert_clean_asan_run(
            r#"
fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }
fn main() {
    let mut g = make(0i64);
    let mut i = 1i64;
    while i <= 50i64 {
        g = make(i * 10i64);
        i = i + 1i64;
    }
    println(f"{g(5i64)}");
}
"#,
            &["505"],
            "asan_heap_env_binding_reassign_in_loop_no_leak",
        );
    }

    /// FIELD reassignment slice (B-2026-06-22-2): `r.f = g` where `r` is a heap-env
    /// struct owner and `g` a heap-env binding is a COPY — the reassignment drops
    /// `r.f`'s OLD env, incs the SHARED env `g` holds, and stores it into the field
    /// slot, so `r.f` and `g` co-own one box freed EXACTLY once while `r.f`'s
    /// original env is freed once at the reassignment. Both `r.f` and `g` are used
    /// after, confirming the source stays a live co-owner. Without the drop-old the
    /// original field env leaks; without the inc the shared box is freed twice (the
    /// field's and `g`'s scope-exit drops). A POD sibling field rides along.
    #[test]
    fn asan_heap_env_struct_field_reassign_copy_no_leak() {
        assert_clean_asan_run(
            r#"
struct H { f: Fn(i64) -> i64, n: i64 }
fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }
fn main() {
    let g = make(100i64);
    let mut h = H { f: make(10i64), n: 7i64 };
    h.f = g;
    println(f"{(h.f)(1i64) + g(1i64) + h.n}");
}
"#,
            &["209"],
            "asan_heap_env_struct_field_reassign_copy_no_leak",
        );
    }

    /// `r.f = make(j)` is a MOVE to a fresh field env: the reassignment drops
    /// `r.f`'s old env (freed once) and the field becomes the sole owner of the
    /// fresh one (freed once at scope exit). Without the drop-old the original
    /// field env leaks. `(h.f)(5) = 25`.
    #[test]
    fn asan_heap_env_struct_field_reassign_to_fresh_no_leak() {
        assert_clean_asan_run(
            r#"
struct H { f: Fn(i64) -> i64 }
fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }
fn main() {
    let mut h = H { f: make(10i64) };
    h.f = make(20i64);
    println(f"{(h.f)(5i64)}");
}
"#,
            &["25"],
            "asan_heap_env_struct_field_reassign_to_fresh_no_leak",
        );
    }

    /// The strongest field-reassign leak case: reassigning a field in a LOOP. Each
    /// of the 50 iterations drops the prior field env before storing the next, so
    /// every intermediate env box is freed once — without the per-assignment
    /// drop-old, 50 unreachable env boxes would leak (LSan-caught). A two-closure-
    /// field owner: only `f` is reassigned, `g` stays the original (freed once).
    /// Last `f` is `make(500)`; `(h.f)(5) + (h.g)(0) = 505 + 20 = 525`.
    #[test]
    fn asan_heap_env_struct_field_reassign_in_loop_no_leak() {
        assert_clean_asan_run(
            r#"
struct H { f: Fn(i64) -> i64, g: Fn(i64) -> i64 }
fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }
fn main() {
    let mut h = H { f: make(0i64), g: make(20i64) };
    let mut i = 1i64;
    while i <= 50i64 {
        h.f = make(i * 10i64);
        i = i + 1i64;
    }
    println(f"{(h.f)(5i64) + (h.g)(0i64)}");
}
"#,
            &["525"],
            "asan_heap_env_struct_field_reassign_in_loop_no_leak",
        );
    }

    /// VEC ELEMENT reassignment slice (B-2026-06-22-2), the final form: `v[i] = g`
    /// where `v` is a heap-env `Vec[Fn]` owner and `g` a heap-env binding is a COPY
    /// — the reassignment drops `v[i]`'s OLD env, incs the SHARED env `g` holds, and
    /// stores it into the element slot, so `v[i]` and `g` co-own one box freed
    /// EXACTLY once (the Vec's refcount-aware drop loop decs it, `g`'s scope-exit
    /// drop decs it) while `v[i]`'s original env is freed once at the reassignment.
    /// Both `v[i]` and `g` are used after. A second element rides along untouched.
    /// Without the drop-old the original element env leaks; without the inc the
    /// shared box is freed twice.
    #[test]
    fn asan_heap_env_vec_element_reassign_copy_no_leak() {
        assert_clean_asan_run(
            r#"
fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }
fn main() {
    let g = make(100i64);
    let mut v: Vec[Fn(i64) -> i64] = Vec.new();
    v.push(make(10i64));
    v.push(make(20i64));
    v[0i64] = g;
    println(f"{(v[0i64])(1i64) + g(1i64) + (v[1i64])(2i64)}");
}
"#,
            &["224"],
            "asan_heap_env_vec_element_reassign_copy_no_leak",
        );
    }

    /// `v[i] = make(j)` is a MOVE to a fresh element env: the reassignment drops
    /// `v[i]`'s old env (freed once) and the element becomes the sole owner of the
    /// fresh one (freed once by the drop loop). Without the drop-old the original
    /// element env leaks. `(v[0])(5) = 25`.
    #[test]
    fn asan_heap_env_vec_element_reassign_to_fresh_no_leak() {
        assert_clean_asan_run(
            r#"
fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }
fn main() {
    let mut v: Vec[Fn(i64) -> i64] = Vec.new();
    v.push(make(10i64));
    v[0i64] = make(20i64);
    println(f"{(v[0i64])(5i64)}");
}
"#,
            &["25"],
            "asan_heap_env_vec_element_reassign_to_fresh_no_leak",
        );
    }

    /// The strongest Vec-element leak case: reassigning over a DYNAMIC index in a
    /// LOOP. Each of the 50 iterations drops the prior element env before storing
    /// the next, across all elements, so every intermediate env box is freed once —
    /// without the per-assignment drop-old, the overwritten env boxes would leak
    /// (LSan-caught). Sum over the final pass: `(v[k])(0) = k*10` for k in 0..5 plus
    /// the loop's last writes — the program prints the final-state sum `100`.
    #[test]
    fn asan_heap_env_vec_element_reassign_in_loop_no_leak() {
        assert_clean_asan_run(
            r#"
fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }
fn main() {
    let mut v: Vec[Fn(i64) -> i64] = Vec.new();
    v.push(make(0i64));
    v.push(make(0i64));
    v.push(make(0i64));
    v.push(make(0i64));
    v.push(make(0i64));
    let mut i = 0i64;
    while i < 50i64 {
        let mut k = 0i64;
        while k < 5i64 {
            v[k] = make(k * 10i64);
            k = k + 1i64;
        }
        i = i + 1i64;
    }
    println(f"{(v[0i64])(0i64) + (v[1i64])(0i64) + (v[2i64])(0i64) + (v[3i64])(0i64) + (v[4i64])(0i64)}");
}
"#,
            &["100"],
            "asan_heap_env_vec_element_reassign_in_loop_no_leak",
        );
    }

    /// Owner-copy slice (B-2026-06-22-2): `let s = a` where `a` is a heap-env
    /// STRUCT owner. The struct copy shallow-copies the `Fn` field so `s` aliases
    /// `a`'s SAME RC env box; the copy INCs the shared env and registers `s`'s own
    /// `FreeClosureEnv`, so each owner RC-drops once and the box is freed EXACTLY
    /// once (COPY semantics — `a` stays live). Covers a 3-owner copy chain
    /// (`a`→`s`→`t`, rc reaches 3, three balanced drops), a sibling HEAP String
    /// field (DEEP-copied to independent buffers, composing with the env inc), and
    /// owner-copy-then-ESCAPE (`build` returns the copy `s` — move-out + caller
    /// adopt). Asserts no leak (LSan) and no use-after-free / double-free (ASAN).
    /// Without the inc the first owner's drop would free the box the others still
    /// alias (UAF / double-free); a stray extra inc would leak it.
    #[test]
    fn asan_heap_env_owner_copy_freed_no_leak() {
        assert_clean_asan_run(
            r#"
struct H { f: Fn(i64) -> i64, name: String }
fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }
fn build(k: i64) -> H { let a = H { f: make(k), name: "an independently heap-copied payload" }; let s = a; s }
fn main() {
    let a = H { f: make(10i64), name: "another sufficiently long heap string here" };
    let s = a;
    let t = s;
    let r = build(20i64);
    println(f"{(a.f)(1i64) + (s.f)(1i64) + (t.f)(1i64) + (r.f)(2i64)}");
}
"#,
            &["55"],
            "asan_heap_env_owner_copy_freed_no_leak",
        );
    }

    /// Owner-copy slice (B-2026-06-22-2), TUPLE: `let s = t` where `t` is a heap-env
    /// tuple owner. The tuple copy shallow-copies the inline `Fn` fat pointer so
    /// `s`'s element aliases `t`'s SAME RC env box; the copy INCs the shared env and
    /// registers `s`'s own per-element `FreeClosureEnv`, so each owner RC-drops once
    /// and the box is freed EXACTLY once (COPY semantics — `t` stays live). Covers a
    /// 3-owner copy chain (`a`→`s`→`t`, rc reaches 3, three balanced drops) and
    /// owner-copy-then-ESCAPE (`build` returns the copy `s` — move-out neutralizes
    /// `s`, the caller adopts). Without the inc the first owner's drop would free the
    /// box the others still alias (UAF / double-free); a stray extra inc would leak.
    #[test]
    fn asan_heap_env_tuple_owner_copy_freed_no_leak() {
        assert_clean_asan_run(
            r#"
fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }
fn build(k: i64) -> (Fn(i64) -> i64, i64) { let a = (make(k), 0i64); let s = a; s }
fn main() {
    let a = (make(10i64), 0i64);
    let s = a;
    let t = s;
    let r = build(20i64);
    println(f"{(a.0)(1i64) + (s.0)(1i64) + (t.0)(1i64) + (r.0)(2i64)}");
}
"#,
            &["55"],
            "asan_heap_env_tuple_owner_copy_freed_no_leak",
        );
    }

    /// The ARRAY twin: a fixed-size `Array[Fn,2]` owner copied (chain + escape).
    /// Exercises the array element-GEP path (`[0, idx]`) across BOTH elements — each
    /// env is inc'd and freed exactly once. `build` returns a 2-element array copy
    /// `s` (multi-element move-out + caller adopt).
    #[test]
    fn asan_heap_env_array_owner_copy_freed_no_leak() {
        assert_clean_asan_run(
            r#"
fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }
fn build(k: i64) -> Array[Fn(i64) -> i64, 2] { let a: Array[Fn(i64) -> i64, 2] = [make(k), make(k + 5i64)]; let s = a; s }
fn main() {
    let a: Array[Fn(i64) -> i64, 2] = [make(10i64), make(20i64)];
    let s = a;
    let t = s;
    let r = build(30i64);
    println(f"{(a[0])(1i64) + (s[1])(1i64) + (t[0])(1i64) + (r[0])(2i64) + (r[1])(2i64)}");
}
"#,
            &["112"],
            "asan_heap_env_array_owner_copy_freed_no_leak",
        );
    }

    /// Owner-copy slice (B-2026-06-22-2), tuple with a HEAP String SIBLING: the
    /// String's buffer is SHARED (the source's drop is suppressed via cap-zero, the
    /// copy frees it exactly once) while the `Fn` element env is RC-inc'd. LSan/ASAN
    /// confirm the string is freed exactly once and the env exactly once — no leak,
    /// no double-free, no use-after-free (both owners read the shared buffer before
    /// scope exit). Composes the closure-env inc with the pre-existing tuple-copy
    /// heap-field move.
    #[test]
    fn asan_heap_env_tuple_owner_copy_string_sibling_freed_no_leak() {
        assert_clean_asan_run(
            r#"
fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }
fn main() {
    let a = (make(10i64), "an independently long heap string payload here");
    let s = a;
    println(f"{(a.0)(1i64) + (s.0)(2i64)}");
    println(s.1);
    println(a.1);
}
"#,
            &[
                "23",
                "an independently long heap string payload here",
                "an independently long heap string payload here",
            ],
            "asan_heap_env_tuple_owner_copy_string_sibling_freed_no_leak",
        );
    }

    /// Owner-copy slice (B-2026-06-22-2), VEC: `let w = v` where `v` is a heap-env
    /// `Vec[Fn]` owner is a MOVE (not a copy) — codegen zeroes `v`'s cap, which the
    /// `cap > 0` guard in the `FreeVecBuffer` cleanup uses to skip v's WHOLE cleanup
    /// (the dynamic per-element env-drop loop AND the buffer free), while `w`
    /// registers its own loop. Each element env (and the buffer) is freed EXACTLY
    /// once. Covers a multi-element buffer, a move chain (`v`→`w`→`x`, the cap
    /// zeroed at each hop so only the final owner frees), and move-then-ESCAPE
    /// (`build` returns the moved owner `w` — drop-loop relocation + caller adopt).
    /// Without the cap-zero, both `v` and `w` would run the drop loop (double-free);
    /// without `w`'s registration, the moved buffer would leak.
    #[test]
    fn asan_heap_env_vec_owner_move_freed_no_leak() {
        assert_clean_asan_run(
            r#"
fn make(k: i64) -> Fn(i64) -> i64 { |x| x + k }
fn build(k: i64) -> Vec[Fn(i64) -> i64] { let mut v: Vec[Fn(i64) -> i64] = Vec.new(); v.push(make(k)); v.push(make(k + 5i64)); let w = v; w }
fn main() {
    let mut v: Vec[Fn(i64) -> i64] = Vec.new();
    v.push(make(10i64));
    v.push(make(20i64));
    let w = v;
    let x = w;
    let r = build(30i64);
    println(f"{(x[0])(1i64) + (x[1])(1i64) + (r[0])(2i64) + (r[1])(2i64)}");
}
"#,
            &["101"],
            "asan_heap_env_vec_owner_move_freed_no_leak",
        );
    }

    // ── Baseline: no heap allocations ─────────────────────────────
    // Sanity-checks the harness itself — should trivially pass on any host
    // with a working `cc + ASAN`. If this fails, the infrastructure is
    // broken, not the codegen.

    #[test]
    fn asan_baseline_no_allocations() {
        assert_clean_asan_run(
            r#"
fn main() {
    println(42);
}
"#,
            &["42"],
            "baseline_no_allocations",
        );
    }

    /// Borrow-elision (B-2026-06-19-6): a read-only `let r = out[j]` over a
    /// `Vec[Vec[i64]]` binds `r` as a borrow of the element and SKIPS both the
    /// deep clone and the binding's scope-exit free. ASAN must confirm this is
    /// clean — no leak (the container still owns and frees each buffer), no
    /// double-free (the borrow doesn't free), no use-after-free. Inner vectors
    /// carry 8 i64s (64 bytes) so a wrongly-skipped owned-buffer free would be a
    /// reachable-at-exit leak LSan can see (≥36-byte payload rule).
    #[test]
    fn asan_borrow_elision_read_only_vecvec_index_is_clean() {
        assert_clean_asan_run(
            r#"
fn main() {
    let mut out: Vec[Vec[i64]] = Vec.new();
    let mut k = 0i64;
    while k < 32i64 {
        let mut b: Vec[i64] = Vec.new();
        let mut p = 0i64;
        while p < 8i64 { b.push(k * 8i64 + p); p = p + 1i64; }
        out.push(b);
        k = k + 1i64;
    }
    let mut acc = 0i64;
    let m = out.len();
    let mut j = 0i64;
    while j < m {
        let r = out[j];
        let mut i = 0i64;
        let rl = r.len();
        while i < rl { acc = acc + r[i]; i = i + 1i64; }
        j = j + 1i64;
    }
    println(acc);
}
"#,
            &["32640"],
            "borrow_elision_read_only_vecvec_index",
        );
    }

    /// Type-changing shadow (phase-5-diagnostics "codegen
    /// type-changing-shadow"): `let v = v.len()` rebinds a heap `Vec` to a
    /// scalar. The shadow dance purges `v`'s `vec_elem_types` tag so the new
    /// i64 binding dispatches correctly — but the OLD Vec's scope-exit free
    /// MUST still fire. Scope-exit drops are queued by alloca at bind time
    /// (`scope_cleanup_actions`), not re-derived from the purged name-maps, so
    /// forgetting the metadata cannot drop the cleanup. If it could, the
    /// 128-byte buffer (16 i64s — well past the ≥36-byte LSan reachability
    /// floor) would leak. ASAN must report clean: no leak, no double-free.
    #[test]
    fn asan_type_changing_shadow_vec_to_scalar_frees_old_buffer() {
        assert_clean_asan_run(
            r#"
fn main() {
    let mut v: Vec[i64] = Vec.new();
    let mut i = 0i64;
    while i < 16i64 { v.push(i * 7i64); i = i + 1i64; }
    let v = v.len();
    println(v);
}
"#,
            &["16"],
            "type_changing_shadow_vec_to_scalar",
        );
    }

    /// String sibling of the Vec shadow above: a ≥36-byte heap `String`
    /// (40 chars, past the SSO/short-string window LSan can't see) rebound to
    /// the scalar `s.len()`. The old String's buffer must still drop after the
    /// `string_vars` tag is purged. ASAN must report clean.
    #[test]
    fn asan_type_changing_shadow_string_to_scalar_frees_old_buffer() {
        assert_clean_asan_run(
            r#"
fn main() {
    let s = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    let s = s.len();
    println(s);
}
"#,
            &["40"],
            "type_changing_shadow_string_to_scalar",
        );
    }

    /// Slice 3d-i (self-hosting parser tail): dropping an item node that carries
    /// `Vec[AttrNode]` — the attribute list — where each `AttrNode` owns a
    /// `Vec[String]` path, a `Vec[AttrArgNode]` (each arg an `Option[String]`
    /// name + an `Option[Expr]` value), and an `Option[String]` string value.
    /// The `value` field mirrors the port's real `Option[Expr]` where `Expr` is
    /// a `shared enum` (RC) — modeled here as `Option[shared enum Val]`. This
    /// nested `Vec[struct{ Vec[String], Vec[struct{Option[String],
    /// Option[shared enum]}], Option[String] }]` is the Cluster-1
    /// heap-in-Vec-in-struct shape; exercises BOTH the consume path (each node
    /// moved into a render-like fn and dropped there) and the plain-drop path (a
    /// built list dropped at scope exit without consuming). All heap payloads are
    /// ≥36 bytes so LSan sees any leaked buffer; a missed drop leaks, a
    /// double-drop aborts. (An `Option[PLAIN enum]` payload — which the port does
    /// NOT use — leaks under LSan; that separate gap is pinned in
    /// `asan_option_plain_enum_heap_payload_undestructured_drop_leaks_pinned`.)
    ///
    /// B-2026-07-03-28 (FIXED — Phase 2 of the caller-retains model): was 240 B /
    /// 6 allocs (down from 1434 B / 36 in earlier steps). The residual was the
    /// `AttrArgNode` (`ArgN`) options nested in `Vec[ArgN]`: `name: Option[String]`
    /// leaked because `ArgN` was NOT copy-supported (its `value: Option[shared]`
    /// field failed `field_copy_supported`), so the value drop's `OptionInline`
    /// gate — keyed on `aggregate_param_copy_supported_struct` — stayed OFF and
    /// `name`'s buffer was never freed. Closed by making `Option[shared]`
    /// copy-supported (the shared leg): (1) `field_copy_supported` admits an
    /// `Option[shared]` field; (2) `deep_copy_option_inline_payload_in_place`
    /// rc-INCs the inline box (word 1) on entry-copy — symmetric with the
    /// `emit_nested_struct_shared_rc_decs_ex` / `RcDecOption` rc-DEC on drop; and
    /// (3) `track_struct_var` registers the COMBINED drop
    /// (`emit_vec_elem_struct_with_shared_drop_fn` = value-drop PLUS the
    /// shared-field rc-dec walker) for any struct owning shared fields, so a
    /// scope-exit drop of an owning struct local / callee-owned by-value param
    /// rc-decs its `shared` / `Option[shared]` children (the value drop alone
    /// skips them). With `ArgN` copy-supported, `OptionInline` frees `name`, and
    /// the shared box balances (inc == dec). The consume path (each node moved
    /// into `render_*` and destructured) self-balances via the entry-copy +
    /// destructure-leaf rc-dec; the plain-drop path (`more`, dropped at scope) is
    /// fixed by the combined drop. NOTE: element-DEEP entry-copy of a `Vec[struct]`
    /// FIELD (the "piece (b)" the original scope named) is NOT needed here — this
    /// test consumes `args` via a for-loop; it is only needed for an
    /// entry-copy-THEN-whole-drop of such a field, a separate PRE-EXISTING
    /// double-free tracked as B-2026-07-04-9. Run: `scripts/lsan-local.sh
    /// "asan_attr_node_list_drop_consume_and_plain"`.
    #[test]
    fn asan_attr_node_list_drop_consume_and_plain() {
        assert_clean_asan_run(
            r#"
shared enum Val { Nothing, Ident(String), Num(i64) }
struct ArgN { name: Option[String], value: Option[Val] }
struct AttrN { path: Vec[String], args: Vec[ArgN], string_value: Option[String] }

fn render_arg(a: ArgN) -> i64 {
    let ArgN { name, value } = a;
    let mut touched = 0;
    match name { Some(s) => { if s.len() >= 0 { touched = touched + 1; } } None => {} }
    // Flat `Some(_)` — the `Option[Val]` value field still drops wholesale
    // (recursing into the `Val::Ident` String), which is the drop path under
    // test; the exact variant is irrelevant to the count.
    match value { Some(_) => { touched = touched + 1; } None => {} }
    touched
}

fn render_attr(a: AttrN) -> i64 {
    let AttrN { path, args, string_value } = a;
    let mut touched = 0;
    for seg in path { if seg.len() >= 0 { touched = touched + 1; } }
    for arg in args { touched = touched + render_arg(arg); }
    match string_value { Some(s) => { if s.len() >= 0 { touched = touched + 1; } } None => {} }
    touched
}

fn build() -> Vec[AttrN] {
    let mut v: Vec[AttrN] = Vec.new();
    let mut i = 0;
    while i < 6 {
        let mut path: Vec[String] = Vec.new();
        path.push("diagnostic_namespace_segment_alpha_aaaaa".to_string());
        path.push("on_unimplemented_attribute_segment_betaa".to_string());
        let mut args: Vec[ArgN] = Vec.new();
        args.push(ArgN {
            name: Some("note_argument_name_key_gamma_ccccccccccc".to_string()),
            value: Some(Val.Ident("clone_derive_identifier_value_ddddddddd".to_string())),
        });
        args.push(ArgN { name: None, value: Some(Val.Num(42)) });
        v.push(AttrN {
            path: path,
            args: args,
            string_value: Some("string_value_payload_epsilon_eeeeeeeeee".to_string()),
        });
        i = i + 1;
    }
    v
}

fn main() {
    let attrs = build();
    let mut total = 0;
    for a in attrs { total = total + render_attr(a); }
    let more = build();
    total = total + more.len();
    println(total);
}
"#,
            &["42"],
            "attr_node_list_drop_consume_and_plain",
        );
    }

    /// B-2026-07-03-28 shared-leg focused coverage — a NON-shared struct whose
    /// only heap is a `shared` / `Option[shared]` field must rc-dec that field on
    /// EVERY owned-drop path, symmetric with the caller-retains entry-copy's
    /// rc-inc. Before the fix, a plain struct LOCAL (`let h = Holder{..}`) and a
    /// callee-owned by-value PARAM both dropped via `__karac_drop_struct_<S>`
    /// alone, which SKIPS shared fields — so the box leaked (the direct-`shared`
    /// case never rc-dec'd at all; the `Option[shared]` case leaked once
    /// `field_copy_supported` admitted it and the entry-copy rc-INC'd with no
    /// matching dec). Fixed by `track_struct_var` registering the COMBINED drop
    /// (value-drop + `emit_nested_struct_shared_rc_decs`) for any shared-owning
    /// struct, plus the fresh-temp-arg gate (`call_dispatch`) recognizing a
    /// shared-owning struct (invisible to `type_expr_has_drop_heap`). Exercises
    /// the owned-drop shapes for both a direct `shared` field and an
    /// `Option[shared]` field: (1) plain local scope-drop; (2) by-value param
    /// BORROWED then dropped (a fresh-temp arg for the copy-supported
    /// `Option[shared]` struct, a local for the caller-retains direct-`shared`
    /// one); (3) by-value param DESTRUCTURED (the destructure leaf rc-decs,
    /// source neutralized — no double-free); (4) `Vec[Holder]` element
    /// scope-drop. Payloads ≥36 bytes so LSan sees a leak; a double rc-dec would
    /// abort under ASAN. (The direct-`shared` FRESH-TEMP arg and the
    /// entry-copy-then-whole-drop of a `Vec[struct]` FIELD are separate
    /// PRE-EXISTING residuals — B-2026-07-04-9 — deliberately not exercised.)
    #[test]
    fn asan_b28_option_shared_and_direct_shared_struct_drop_no_leak() {
        assert_clean_asan_run(
            r#"
shared enum Val { Nothing, Ident(String), Num(i64) }
struct OptH { value: Option[Val] }
struct DirH { value: Val }

fn borrow_opt(h: OptH) -> i64 {
    let mut r = 0;
    match h.value { Some(_) => { r = 1; } None => {} }
    r
}
fn destr_opt(h: OptH) -> i64 {
    let OptH { value } = h;
    let mut r = 0;
    match value { Some(_) => { r = 1; } None => {} }
    r
}
fn borrow_dir(h: DirH) -> i64 {
    let mut r = 0;
    match h.value { Val.Ident(_) => { r = 1; } _ => {} }
    r
}

fn main() {
    let mut total = 0;
    let mut i = 0;
    while i < 4 {
        // (1) plain local scope-drop, both field shapes.
        let a = OptH { value: Some(Val.Ident("option_shared_local_payload_alpha_aaaaaaaa".to_string())) };
        match a.value { Some(_) => { total = total + 1; } None => {} }
        let b = DirH { value: Val.Ident("direct_shared_local_payload_beta_bbbbbbbbbb".to_string()) };
        match b.value { Val.Ident(_) => { total = total + 1; } _ => {} }
        // (2) by-value param borrowed — fresh-temp for the copy-supported
        // Option[shared] struct; a LOCAL for the caller-retains direct-shared one
        // (a direct-shared fresh-temp arg is a separate pre-existing residual).
        total = total + borrow_opt(OptH { value: Some(Val.Ident("byvalue_borrow_opt_payload_gamma_cccccccc".to_string())) });
        let d = DirH { value: Val.Ident("byvalue_borrow_dir_payload_delta_dddddddd".to_string()) };
        total = total + borrow_dir(d);
        // (3) by-value param destructured.
        total = total + destr_opt(OptH { value: Some(Val.Ident("byvalue_destr_opt_payload_epsilon_eeeeeee".to_string())) });
        i = i + 1;
    }
    // (4) Vec[OptH] element scope-drop (built, len-read, dropped unconsumed).
    let mut v: Vec[OptH] = Vec.new();
    let mut j = 0;
    while j < 4 {
        v.push(OptH { value: Some(Val.Ident("vec_element_option_shared_payload_zeta_fff".to_string())) });
        j = j + 1;
    }
    total = total + v.len();
    println(total);
}
"#,
            &["24"],
            "b28_option_shared_and_direct_shared_struct_drop",
        );
    }

    /// B-2026-07-04-9(b) (FIXED): a struct with a DIRECT `shared` field
    /// (`DirH { value: Val }`, `Val` a shared enum) passed as an INLINE
    /// fresh-temp arg (`borrow_dir(DirH { value: Val.Ident(..) })`) leaked its
    /// RC box. `DirH` is NOT copy-supported (`field_copy_supported` bails on a
    /// direct shared field), so the fresh-temp struct-arg cleanup gate — which
    /// required `aggregate_param_copy_supported_struct` — registered no
    /// caller-temp drop, and the caller-retains param doesn't drop it either. A
    /// LOCAL arg (`let d = DirH { .. }; f(d)`) was already covered by
    /// `track_struct_var` at the binding site. Fixed by registering the combined
    /// drop (`track_struct_var`, a pure rc-dec of the shared field — no buffer
    /// copy) for any shared-owning fresh-temp struct, copy-supported or not;
    /// such a struct is caller-retains, so the caller temp is its sole owner.
    /// Payload ≥36 bytes so LSan sees the leaked box; a double rc-dec would abort
    /// under ASAN. Exercises the fresh-temp direct-shared arg across a loop.
    /// Run: `scripts/lsan-local.sh "b04_9b_direct_shared_freshtemp"`.
    #[test]
    fn asan_b04_9b_direct_shared_freshtemp_struct_arg_no_leak() {
        assert_clean_asan_run(
            r#"
shared enum Val { Nothing, Ident(String), Num(i64) }
struct DirH { value: Val }

fn borrow_dir(h: DirH) -> i64 {
    let mut r = 0;
    match h.value { Val.Ident(_) => { r = 1; } _ => {} }
    r
}

fn main() {
    let mut total = 0;
    let mut i = 0;
    while i < 5 {
        // INLINE fresh-temp arg — the leaking shape (no `let` binding).
        total = total + borrow_dir(DirH {
            value: Val.Ident("b049b_direct_shared_freshtemp_payload_omega_ffff".to_string()),
        });
        i = i + 1;
    }
    println(total);
}
"#,
            &["5"],
            "b04_9b_direct_shared_freshtemp",
        );
    }

    /// B-2026-07-04-9(a): entry-copy-THEN-whole-drop of a `Vec[struct-with-heap]`
    /// FIELD double-frees (exit 133 / ASAN). A struct `AttrN` with a
    /// `Vec[ArgN]` field (`ArgN` owning `Option[String]`/`Option[shared]`) is
    /// passed BY VALUE to `count(a)` — which is copy-supported, so `a` is
    /// entry-copied — reads `a.args.len()`, and the copy is WHOLE-dropped at
    /// return (no element-by-element consume). The prior entry-copy was a
    /// SHALLOW bit-copy of the `Vec[ArgN]` field, so the callee's whole-drop and
    /// the caller's for-loop element drop free the SAME element buffers. Distinct
    /// from `asan_attr_node_list_drop_consume_and_plain`, which consumes the args
    /// Vec element-by-element (self-balancing) — the two must BOTH stay clean:
    /// the whole-drop path needs an element-DEEP entry-copy, and that copy must
    /// NOT strand the consume path's source drain (the regression that reverted
    /// two prior attempts). Payloads ≥36 B for LSan reachability. Run:
    /// `scripts/lsan-local.sh "b04_9a_vec_struct_field_entrycopy_wholedrop"`.
    #[test]
    fn asan_b04_9a_vec_struct_field_entrycopy_wholedrop_no_double_free() {
        assert_clean_asan_run(
            r#"
shared enum Val { Nothing, Ident(String), Num(i64) }
struct ArgN { name: Option[String], value: Option[Val] }
struct AttrN { path: Vec[String], args: Vec[ArgN], string_value: Option[String] }

// Entry-copy-THEN-whole-drop: `a` is entry-copied, `a.args.len()` read, then
// the copy is WHOLE-dropped at return while the caller's for-loop element `a`
// also drops. A shallow field bit-copy => the same element buffers freed twice.
fn count(a: AttrN) -> i64 { a.args.len() }

fn build() -> Vec[AttrN] {
    let mut v: Vec[AttrN] = Vec.new();
    let mut i = 0;
    while i < 6 {
        let mut args: Vec[ArgN] = Vec.new();
        args.push(ArgN {
            name: Some("note_argument_name_key_gamma_ccccccccccc".to_string()),
            value: Some(Val.Ident("clone_derive_identifier_value_ddddddddd".to_string())),
        });
        args.push(ArgN { name: None, value: Some(Val.Num(42)) });
        let mut path: Vec[String] = Vec.new();
        path.push("diagnostic_namespace_segment_alpha_aaaaa".to_string());
        v.push(AttrN {
            path: path,
            args: args,
            string_value: Some("string_value_payload_epsilon_eeeeeeeeee".to_string()),
        });
        i = i + 1;
    }
    v
}

fn main() {
    let items = build();
    let mut total = 0;
    for a in items { total = total + count(a); }
    println(total);
}
"#,
            &["12"],
            "b04_9a_vec_struct_field_entrycopy_wholedrop",
        );
    }

    /// B-2026-07-03-27 (FIXED 009fd479-follow-on): an `Option[E]` field where `E`
    /// is a PLAIN (non-`shared`) user enum carrying a heap payload, destructured
    /// into a local and dropped, leaked the enum payload's heap buffer — the
    /// inline-Option drop path (cf. B-2026-06-10-6, which covered
    /// `Option[String]`/`[Vec]`/`[Map]`) did not recurse into a user-enum (or
    /// struct) payload's drop, and the destructure leaf got no cleanup at all
    /// (`destructure_field_needs_cleanup` excludes `Option`; struct drop skips
    /// `Option` fields — B-2026-07-03-28). The fix registers a tag-guarded
    /// `karac_drop_Option_<payload>` (`emit_option_drop_fn` — the same fn the
    /// `Vec[Option[..]]` element path uses, handling the heap-BOXED wide payload)
    /// on the leaf when the destructure OWNS the source, paired with a Some-arm
    /// tag-zeroing suppressor so a `Some(v)` move-out doesn't double-free.
    /// Exercises: (1) `Some(_)` wildcard match then drop; (2) `Some(v)` move-out
    /// (the bound payload frees it once, source drop suppressed); (3) a
    /// fresh-temp source destructure. Payloads are 40 bytes (LSan reachability).
    /// B-2026-07-03-27 (FIXED): an `Option[E]` field where `E` is a PLAIN
    /// (non-`shared`) user enum/struct carrying a heap payload, destructured into
    /// a local and dropped UNDESTRUCTURED (a `Some(_)` wildcard match, or plain
    /// scope-drop), leaked the payload's heap buffer. The inline-Option drop
    /// (B-2026-06-10-6) covered only `Option[String]`/`[Vec]`/`[Map]`, and the
    /// destructure leaf got no cleanup (`destructure_field_needs_cleanup` excludes
    /// `Option`; struct drop skips `Option` fields — B-2026-07-03-28). The fix
    /// registers a tag-guarded `karac_drop_Option_<payload>` (`emit_option_drop_fn`
    /// — the same fn the `Vec[Option[..]]` element path uses, handling the
    /// heap-BOXED wide enum payload) on the leaf when the destructure OWNS the
    /// source. LSan-confirmed: without the fix this leaks 360 B / 10 allocs.
    /// Covers a Vec-sourced (moved-in owned param) source and a fresh-temp source.
    #[test]
    fn asan_b27_option_enum_undestructured_drop_no_leak() {
        assert_clean_asan_run(
            r#"
enum Val { Nothing, Ident(String) }
struct A { value: Option[Val] }
fn use_wild(a: A) -> i64 { let A { value } = a; match value { Some(_) => 1, None => 0 } }
fn mk() -> A { A { value: Some(Val.Ident("kkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkk".to_string())) } }
fn build() -> Vec[A] {
    let mut v: Vec[A] = Vec.new();
    let mut i = 0;
    while i < 6 { v.push(A { value: Some(Val.Ident("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string())) }); i = i + 1; }
    v
}
fn main() {
    let mut t = 0;
    let xs = build();                            // moved-in owned param source
    for a in xs { t = t + use_wild(a); }
    let mut i = 0;                               // fresh-temp source
    while i < 6 { t = t + use_wild(mk()); i = i + 1; }
    println(t);
}
"#,
            &["12"],
            "b27_option_enum_undestructured_drop",
        );
    }

    /// Sibling of the enum case — an `Option[<user struct>]` field
    /// (`Option[Inner]`, `Inner { s: String }`) destructured and dropped
    /// undestructured. Same fix (`emit_option_drop_fn` recurses into the
    /// struct's `__karac_drop_struct_Inner`). B-2026-07-03-27.
    #[test]
    fn asan_b27_option_struct_undestructured_drop_no_leak() {
        assert_clean_asan_run(
            r#"
struct Inner { s: String }
struct A { value: Option[Inner] }
fn use_a(a: A) -> i64 { let A { value } = a; match value { Some(_) => 1, None => 0 } }
fn build() -> Vec[A] {
    let mut v: Vec[A] = Vec.new();
    let mut i = 0;
    while i < 6 { v.push(A { value: Some(Inner { s: "ssssssssssssssssssssssssssssssssssssssss".to_string() }) }); i = i + 1; }
    v
}
fn main() { let xs = build(); let mut t = 0; for a in xs { t = t + use_a(a); } println(t); }
"#,
            &["6"],
            "b27_option_struct_undestructured_drop",
        );
    }

    /// B-2026-07-03-31 (FIXED, Phase 1 of the caller-retains model): binding the
    /// `Some` payload out of an `Option[<agg>]` destructure leaf and using it
    /// ONLY as a borrow — `Some(v) => ident_len(v)`, where `ident_len`
    /// entry-copies its owned param — must NOT disarm the source payload drop,
    /// or the payload's inner heap leaks. The consumption classifier
    /// (`arm_only_borrows_option_agg_payload` /
    /// `block_only_borrows_option_agg_payload`) now keeps the drop armed for
    /// borrow-only arms across `match`, `if let`, and `while let`. Covers a
    /// boxed payload (`Val::Ident(String)`, tag + String > 3 words) and an
    /// inline payload (`Inner { s: String }`, 3 words). Consuming arms still
    /// suppress (verified double-free-clean under ASAN by the many existing
    /// Option/enum match tests + the consuming arm below). ≥36-byte payloads for
    /// LSan reachability under the Linux gate.
    #[test]
    fn asan_b31_option_agg_payload_borrow_only_no_leak() {
        assert_clean_asan_run(
            r#"
enum Val { Nothing, Ident(String) }
struct A { value: Option[Val] }
struct B { value: Option[Val] }
fn ident_len(v: Val) -> i64 { match v { Val.Ident(s) => s.len(), Val.Nothing => 0 } }
fn via_match(a: A) -> i64 { let A { value } = a; match value { Some(v) => ident_len(v), None => 0 } }
fn via_iflet(a: A) -> i64 { let A { value } = a; if let Some(v) = value { ident_len(v) } else { 0 } }
fn via_whilelet(a: A) -> i64 {
    let A { value } = a;
    let mut vv = value;
    let mut acc = 0;
    while let Some(v) = vv { acc = acc + ident_len(v); vv = val_none(); }
    acc
}
fn val_none() -> Option[Val] { Option.None }
fn build() -> Vec[A] {
    let mut v: Vec[A] = Vec.new();
    let mut i = 0;
    while i < 6 { v.push(A { value: Some(Val.Ident("payload-borrow-only-aaaaaaaaaaaaaaaaaaaaaaaa".to_string())) }); i = i + 1; }
    v
}
fn main() {
    let mut t = 0;
    for a in build() { t = t + via_match(a); }
    for a in build() { t = t + via_iflet(a); }
    for a in build() { t = t + via_whilelet(a); }
    println(t);
}
"#,
            &["792"],
            "b31_option_agg_payload_borrow_only",
        );
    }

    /// B-2026-07-04-7 (FIXED): a `struct A { value: Option[<non-shared enum/struct>] }`
    /// field is now DROP-SUPPORTED. Before, `emit_struct_drop_synthesis(A)` emitted no
    /// drop for the `Option[<heap enum>]` field (the `OptionInline` pass was gated to
    /// String/Vec payloads), so `A` read as heapless and a `Vec[A]` teardown skipped the
    /// element walk — the `Some` payload (String + boxed enum) leaked. The fix makes
    /// `Option[<struct/enum>]` copy-supported (`field_copy_supported`'s Option arm +
    /// `deep_copy_option_struct_enum_payload_in_place`, the box-aware copy peer of
    /// `emit_option_drop_fn`) and broadens the `OptionInline` drop pass to that payload
    /// class — copy == drop, so an entry-copied param and the caller's retained original
    /// own independent heap. The destructure move-out (`let A { value } = a`) zeros the
    /// callee-owned source's Option tag (`zero_struct_field_move_cap`) so the source
    /// struct-drop skips the moved-out payload (else double-free vs the B-27 leaf drop).
    /// Exercises: zero-escape build+drop, pass-by-value borrow, pass-by-value payload
    /// move-out, destructure+wildcard (B-27 shape), destructure+move-out (B-31 shape),
    /// and an index-alias move-out — all in `Vec[A]`-consuming loops. ≥36-byte payloads
    /// for LSan reachability. (Payload len = 40, so `6 + 6 + 6*40 + 6 + 6*40 + 40 = 538`.)
    #[test]
    fn asan_b04_7_option_heap_enum_struct_field_drop_no_leak() {
        assert_clean_asan_run(
            r#"
enum Val { Nothing, Ident(String), Num(i64) }
struct A { value: Option[Val] }
fn ident_len(v: Val) -> i64 { match v { Val.Ident(s) => s.len(), Val.Num(n) => n, Val.Nothing => 0 } }
fn count(a: A) -> i64 { match a.value { Some(_) => 1, None => 0 } }
fn get_len(a: A) -> i64 { match a.value { Some(v) => ident_len(v), None => 0 } }
fn use_wild(a: A) -> i64 { let A { value } = a; match value { Some(_) => 1, None => 0 } }
fn via_match(a: A) -> i64 { let A { value } = a; match value { Some(v) => ident_len(v), None => 0 } }
fn build() -> Vec[A] {
    let mut v: Vec[A] = Vec.new();
    let mut i = 0;
    while i < 6 { v.push(A { value: Some(Val.Ident("payload_b047_aaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string())) }); i = i + 1; }
    v
}
fn main() {
    let z: Vec[A] = build();
    let mut t = z.len();
    for a in build() { t = t + count(a); }
    for a in build() { t = t + get_len(a); }
    for a in build() { t = t + use_wild(a); }
    for a in build() { t = t + via_match(a); }
    let xs: Vec[A] = build();
    t = t + get_len(xs[0]);
    println(t);
}
"#,
            &["538"],
            "b04_7_option_heap_enum_struct_field_drop",
        );
    }

    /// Borrow-elision negative: each `r` is moved into `keep`, so the gate must
    /// KEEP the deep clone — `r` owns an independent buffer that outlives `out`.
    /// ASAN confirms no use-after-free (a mis-borrowed `r` would dangle once
    /// `out` drops) / double-free / leak. Inner vectors are 8 i64s (64 bytes) for
    /// LSan reachability.
    #[test]
    fn asan_borrow_elision_escape_negative_clones_and_is_clean() {
        assert_clean_asan_run(
            r#"
fn main() {
    let mut out: Vec[Vec[i64]] = Vec.new();
    let mut k = 0i64;
    while k < 32i64 {
        let mut b: Vec[i64] = Vec.new();
        let mut p = 0i64;
        while p < 8i64 { b.push(7i64); p = p + 1i64; }
        out.push(b);
        k = k + 1i64;
    }
    let mut keep: Vec[Vec[i64]] = Vec.new();
    let mut j = 0i64;
    while j < out.len() {
        let r = out[j];
        keep.push(r);
        j = j + 1i64;
    }
    let mut acc = 0i64;
    let mut i = 0i64;
    while i < keep.len() {
        let z = keep[i];
        acc = acc + z[0i64];
        i = i + 1i64;
    }
    println(acc);
}
"#,
            &["224"],
            "borrow_elision_escape_negative",
        );
    }

    /// Index-store of a heap-owning Vec element (B-2026-06-19-7): `out[j] = nb`
    /// over a `Vec[Vec[i64]]` in a loop. The store must (a) drop the old element
    /// buffer (no leak) and (b) suppress the moved source binding's cleanup (no
    /// double-free); pre-fix the AOT binary SIGTRAPped. ASAN confirms a clean run
    /// (no use-after-free / double-free; Linux CI LSan covers the leak arm).
    /// Inner vectors carry 8 i64s (64 bytes) for LSan reachability. Sum of heads
    /// j=0..31 = 496.
    #[test]
    fn asan_index_store_heap_vec_element_no_double_free() {
        assert_clean_asan_run(
            r#"
fn main() {
    let mut out: Vec[Vec[i64]] = Vec.new();
    let mut k = 0i64;
    while k < 32i64 {
        let mut b: Vec[i64] = Vec.new();
        let mut p = 0i64;
        while p < 8i64 { b.push(7i64); p = p + 1i64; }
        out.push(b);
        k = k + 1i64;
    }
    let mut acc = 0i64;
    let mut j = 0i64;
    while j < 32i64 {
        let mut nb: Vec[i64] = Vec.new();
        let mut q = 0i64;
        while q < 8i64 { nb.push(j); q = q + 1i64; }
        out[j] = nb;
        acc = acc + out[j][0i64];
        j = j + 1i64;
    }
    println(acc);
}
"#,
            &["496"],
            "index_store_heap_vec_element",
        );
    }

    #[test]
    fn asan_nested_receiver_push_string_field_no_leak() {
        // B-2026-07-11-11: `.push()` on a nested place-expression receiver
        // (`g.rows[i].cells.push(s)` — index-then-field) now resolves the
        // receiver pointer through the place chain instead of failing codegen.
        // The pushed String buffers are owned by the innermost Vec, itself
        // owned by the row, itself owned by the grid — dropped exactly once at
        // scope exit. ASAN/LSan guards no leak (every buffer reclaimed) and no
        // double-free (the aliasing field pointer does not mint a second owner).
        assert_clean_asan_run(
            r#"
struct Row { cells: Vec[String] }
struct Grid { rows: Vec[Row] }
fn main() {
    let mut g = Grid { rows: Vec.new() };
    let mut r = 0i64;
    while r < 8i64 {
        g.rows.push(Row { cells: Vec.new() });
        let mut c = 0i64;
        while c < 4i64 {
            g.rows[r].cells.push("cell".to_string());
            c = c + 1i64;
        }
        r = r + 1i64;
    }
    let mut total = 0i64;
    let mut i = 0i64;
    while i < 8i64 {
        total = total + g.rows[i].cells.len();
        i = i + 1i64;
    }
    println(total);
}
"#,
            &["32"],
            "nested_receiver_push_string_field",
        );
    }

    // ── Direct recursive shared enum (RC tree) ────────────────────
    //
    // `shared enum Expr { Num(i64), Add(Expr, Expr) }` builds an RC tree whose
    // children are RC handles. `eval` recursively consumes each child (passing
    // it by value moves the handle). ASAN guards that the tree is freed exactly
    // once — no leak (every node reclaimed) and no double-free (a moved child is
    // not freed again at the parent's scope exit). This is the allocation
    // correctness check for the direct-recursion feature; the by-value layout
    // bug that preceded it would have ICE'd before reaching a binary at all.

    #[test]
    fn asan_direct_recursive_shared_enum_tree_freed_once() {
        assert_clean_asan_run(
            r#"
shared enum Expr {
    Num(i64),
    Add(Expr, Expr),
}
fn eval(e: Expr) -> i64 {
    match e {
        Num(n) => n,
        Add(a, b) => eval(a) + eval(b),
    }
}
fn main() {
    let e = Add(Num(3), Add(Num(4), Num(5)));
    println(eval(e));
}
"#,
            &["12"],
            "direct_recursive_shared_enum_tree",
        );
    }

    #[test]
    fn asan_direct_recursive_shared_enum_single_field_no_leak() {
        assert_clean_asan_run(
            r#"
shared enum Wrap {
    Leaf(i64),
    Box(Wrap),
}
fn depth(w: Wrap) -> i64 {
    match w {
        Leaf(n) => 0,
        Box(inner) => 1 + depth(inner),
    }
}
fn main() {
    let w = Box(Box(Box(Leaf(7))));
    println(depth(w));
}
"#,
            &["3"],
            "direct_recursive_shared_enum_single_field",
        );
    }

    #[test]
    fn asan_recursive_shared_enum_children_freed_no_leak() {
        // B-2026-06-13-11: a recursive `shared enum` (AST/tree shape) must
        // recursively rc-dec its child boxes when the parent box's refcount
        // hits zero. Pre-fix `emit_rc_dec` plain-`free`d a shared enum box with
        // NO payload walk (shared enums cached `None` in `rc_drop_fns`), so
        // every child `Bin`/`Num` box leaked (~96 B / iter over the loop). The
        // new `emit_shared_enum_rc_drop_fn` tag-switches and walks each
        // variant's shared children. Looping makes the per-iteration leak
        // visible to the Linux-CI LSan gate; mac checks no double-free / UAF on
        // the recursive free. (Base-case-first variant order — recursive-first
        // is the separate B-2026-06-13-10 layout overflow.)
        assert_clean_asan_run(
            r#"
shared enum Expr { Num(i64), Bin(Expr, Expr) }
fn eval(e: Expr) -> i64 {
    match e {
        Num(n) => n,
        Bin(l, r) => eval(l) + eval(r),
    }
}
fn main() {
    let mut i: i64 = 0;
    let mut total: i64 = 0;
    while i < 40 {
        let t: Expr = Bin(Num(i), Bin(Num(i), Num(2)));
        total = total + eval(t);
        i = i + 1;
    }
    println(total);
}
"#,
            &["1640"],
            "recursive_shared_enum_children_freed",
        );
    }

    #[test]
    fn asan_shared_enum_fnret_temp_arg_freed_no_leak() {
        // B-2026-06-19-3 — the self-hosted parser's `render_expr(parse_expr(src))`
        // leak: a bare `shared enum` AST node, produced as a function-return (or inline
        // variant-ctor) TEMPORARY and passed BY VALUE to a consumer, was never
        // freed. A bare-shared by-value param is NET-ZERO (callee `emit_refcount_inc`
        // at entry + `track_rc_var` dec at exit — the caller-keeps-reference
        // convention), so the caller still owns the temp's +1; but a directly
        // passed temp has no binding to carry that dec, so the box leaked once per
        // call (input `"1"`: a single 80-byte node; a deep parse: the whole tree).
        // A let-bound producer (`let e = parse(...); render(e)`) was always freed
        // via the binding's scope-exit dec — only the *temporary* arg leaked. The
        // fix queues the caller-side dec for a fresh bare-shared box arg
        // (`fresh_arg_bare_shared_heap_type` + `track_rc_var`, call_dispatch.rs),
        // mirroring the Vec/String fresh-temp-arg arm next to it.
        //
        // Shape mirrors `render_expr(parse_expr(src))`: `build` returns a fresh RC
        // tree, `render` consumes it by value and recurses on the destructured
        // children. Looped so the per-iteration box leak accumulates LSan-visibly;
        // each `Expr` box is >=48 bytes, above the short-allocation reachability
        // floor LSan silently tolerates. Payloads are all-scalar (`Span`) on
        // purpose — this pins the BOX free, isolated from the orthogonal
        // whole-struct-payload-binding String-field drop (a separate gap).
        assert_clean_asan_run(
            r#"
struct Span { line: i64, column: i64, offset: i64, length: i64 }
struct BinData { left: Expr, right: Expr, span: Span }
shared enum Expr { Int(Span), Bin(BinData) }
fn build(depth: i64, v: i64) -> Expr {
    if depth <= 0 {
        return Expr.Int(Span { line: v, column: 0, offset: v, length: 1 });
    }
    let l = build(depth - 1, v);
    let r = build(depth - 1, v + 1);
    return Expr.Bin(BinData { left: l, right: r, span: Span { line: 0, column: 0, offset: 0, length: 2 } });
}
fn render(e: Expr) -> String {
    let mut out = "".to_string();
    match e {
        Int(n) => { out.push_str("(int "); out.push_str(n.line.to_string()); out.push_str(")"); }
        Bin(b) => {
            let BinData { left, right, span } = b;
            out.push_str("(bin ");
            out.push_str(span.length.to_string());
            out.push_str(" ");
            out.push_str(render(left));
            out.push_str(" ");
            out.push_str(render(right));
            out.push_str(")");
        }
    }
    out
}
fn main() {
    let mut i: i64 = 0;
    let mut total: i64 = 0;
    while i < 50 {
        // render(build(..)) — the tree is a function-return temporary passed by
        // value; render(Expr.Int(..)) — an inline variant-ctor temporary.
        total = total + render(build(3, i)).len();
        total = total + render(Expr.Int(Span { line: i, column: 0, offset: i, length: 1 })).len();
        i = i + 1;
    }
    if total > 0 { println("ok"); } else { println("bad"); }
}
"#,
            &["ok"],
            "shared_enum_fnret_temp_arg_freed",
        );
    }

    #[test]
    fn asan_vec_shared_elem_into_some_returned_no_leak_no_uaf() {
        // B-2026-06-15-1 (#226 invert-binary-tree): a bare `shared` struct read
        // out of a `Vec` element into an enum-ctor payload (`Some(nodes[i])`)
        // shallow-aliases without an rc-inc. `rhs_yields_fresh_ref` treats the
        // ctor as fresh, so the return/field consumers skip their inc; the
        // payload was then under-counted and freed when the source `Vec`
        // dropped (its correct per-element dec landed in 0890627c). Building a
        // chain through `nodes[i]`, returning `Some(nodes[0])`, then walking it
        // AFTER the Vec drops read freed memory — non-deterministic garbage /
        // crash (mac ASAN: UAF). The fix (`share_bare_shared_ctor_payload`,
        // scoped to the `v[i]` index) rc-inc's the aliased element so the
        // returned chain outlives the Vec. The loop makes any over-inc visible
        // to the Linux-CI LSan gate (the broad first cut leaked on fresh-local
        // `Some(node)` payloads — this pins the index-only scope).
        assert_clean_asan_run(
            r#"
shared struct N { v: i64, mut next: Option[N] }
fn build(k: i64) -> Option[N] {
    let mut nodes: Vec[N] = Vec.new();
    let mut i: i64 = 0;
    while i < k {
        nodes.push(N { v: i, next: None });
        i = i + 1;
    }
    let mut j: i64 = 1;
    while j < k {
        let mut cur = nodes[j - 1];
        cur.next = Some(nodes[j]);
        j = j + 1;
    }
    return Some(nodes[0]);
}
fn sum_chain(root: Option[N]) -> i64 {
    let mut s: i64 = 0;
    let mut cur = root;
    loop {
        match cur {
            None => { break; },
            Some(n) => { s = s + n.v; cur = n.next; },
        }
    }
    return s;
}
fn main() {
    let mut iter: i64 = 0;
    let mut total: i64 = 0;
    while iter < 50 {
        let r = build(20);
        total = total + sum_chain(r);
        iter = iter + 1;
    }
    println(total);
}
"#,
            &["9500"],
            "vec_shared_elem_into_some_returned",
        );
    }

    #[test]
    fn asan_for_over_collection_body_local_no_leak() {
        // B-2026-06-14-21: a body-local owned heap `let` inside a
        // for-over-COLLECTION loop (Vec/Slice/Map/Set/String/array — NOT
        // for-over-range, which already had per-iteration cleanup) leaked
        // every iteration but the last. The binding's `FreeVecBuffer` was
        // registered in the enclosing FUNCTION frame (the collection
        // for-variants called `compile_block(body)` with no per-iteration
        // cleanup frame), so only the final iteration's value was freed at
        // the function tail — N-1 iterations leaked (surfaced as a browser
        // OOM in the Fathom dogfood: `for handle in handles { let chunk =
        // handle.join(); … }` leaked the joined Vec every frame). The fix
        // wraps each collection for-variant's body in
        // `compile_loop_body_with_cleanup`. Looping over a Vec-of-keys with
        // a per-iteration `let row = build(…)` makes the leak visible to the
        // Linux-CI LSan gate (mac checks no double-free / UAF).
        assert_clean_asan_run(
            r#"
fn build(n: i64) -> Vec[i64] {
    let mut v: Vec[i64] = Vec.new();
    let mut i = 0;
    while i < n { v.push(i); i = i + 1; }
    v
}
fn main() {
    let mut keys: Vec[i64] = Vec.new();
    let mut k = 0;
    while k < 200 { keys.push(k); k = k + 1; }
    let mut total: i64 = 0;
    for key in keys {
        let row: Vec[i64] = build(64);
        total = total + (row.len() as i64) + key;
    }
    println(total);
}
"#,
            &["32700"],
            "for_over_collection_body_local_no_leak",
        );
    }

    #[test]
    fn asan_shared_enum_struct_variant_no_leak_no_double_free() {
        // B-2026-06-13-8: a shared enum struct-variant with a heap (String)
        // payload field — construct the RC box, match-bind the field, drop. The
        // box and its String buffer must be freed exactly once (the Linux-CI
        // LSan job is the leak gate; mac catches double-free/UAF). Looped to
        // make a per-iteration leak or double-free trip the sanitizer. (Uses
        // the base-case-first variant order — recursive-variant-first is a
        // separate pre-existing layout overflow, B-2026-06-13-9.)
        assert_clean_asan_run(
            r#"
shared enum Msg { Empty, Text { body: String, code: i64 } }
fn render(m: Msg) -> i64 {
    match m {
        Text { body, code } => body.len() + code,
        Empty => 0,
    }
}
fn main() {
    let mut i: i64 = 0;
    let mut total: i64 = 0;
    while i < 50 {
        let m: Msg = Msg.Text { body: f"line-{i}", code: i };
        total = total + render(m);
        i = i + 1;
    }
    println(total);
}
"#,
            &["1565"],
            "shared_enum_struct_variant",
        );
    }

    #[test]
    fn asan_plain_enum_struct_variant_string_payload_no_double_free() {
        // The Weave `ParseError` class: a NON-shared enum struct-variant with a
        // heap (String) payload. A real (cap>0) String local moved into the
        // payload and then CONSUMED — matched both externally and through a
        // `Display(ref self)` impl — must free its buffer exactly once.
        // Pre-fix, three sites each double-freed: construction (no source
        // move-suppression), external match destructuring (struct-variant arm
        // skipped by the cap-suppression), and the `ref self` match (borrowed
        // scrutinee bindings tracked as owned). Looped so any per-iteration
        // double-free trips ASAN. The `cap==0` literal payload masked it before.
        assert_clean_asan_run(
            r#"
enum E { Empty, NoAt { value: String } }
impl Display for E {
    fn to_string(ref self) -> String {
        match self { Empty => "empty", NoAt { value } => f"no-at '{value}'" }
    }
}
fn make(raw: String) -> E {
    let v = raw.clone();
    if not v.contains("@") { return E.NoAt { value: v }; }
    E.Empty
}
fn main() {
    let mut i: i64 = 0;
    let mut count: i64 = 0;
    while i < 50 {
        let raw = f"bad-no-at-{i}";
        let e = make(raw);
        // Render through Display(ref self) — borrowed-scrutinee match ...
        let s = e.to_string();
        if s.len() > 0 { count = count + 1; }
        // ... then destructure externally — owned-scrutinee match.
        match e {
            NoAt { value } => { if value.len() > 0 { count = count + 1; } },
            Empty          => count = count + 0,
        }
        i = i + 1;
    }
    println(count);
}
"#,
            &["100"],
            "plain_enum_struct_variant_string_payload",
        );
    }

    #[test]
    fn asan_refinement_try_from_vec_no_double_free() {
        // `Refined.try_from(v)` over a collection base (`type NonEmptyV =
        // Vec[String] where ...`) consumes `v`: on the Ok path the buffer lives
        // in the `Ok` payload, so the source must not free it again. Looped so
        // a per-iteration double-free trips ASAN. The Weave
        // `NonEmpty.try_from(enriched)` class.
        assert_clean_asan_run(
            r#"
type NonEmptyV = Vec[String] where self.len() > 0;
fn main() {
    let mut i: i64 = 0;
    let mut total: i64 = 0;
    while i < 50 {
        let mut v: Vec[String] = Vec.new();
        v.push(f"a-{i}");
        v.push(f"b-{i}");
        match NonEmptyV.try_from(v) {
            Ok(rows) => total = total + rows.len(),
            Err(_)   => total = total + 0,
        }
        i = i + 1;
    }
    println(total);
}
"#,
            &["100"],
            "refinement_try_from_vec",
        );
    }

    // ── L5: NUL-safe print over heap + literal storage ────────────
    //
    // The print path uses `fwrite(data, 1, len, stdout)` so interior NUL
    // bytes are emitted, not truncated. ASAN guards the no-over-read
    // property at the byte boundary: a heap String from concat (`a + b`) is
    // `len`-prefixed and NOT NUL-terminated, so `fwrite` reading exactly
    // `len` bytes must not touch the byte past the buffer (the prior `%s`
    // path did, an ASAN heap-buffer-overflow). The literal `"AB\0"` global
    // is sized `len + 1` and its interior NUL must survive the concat memcpy.

    #[test]
    fn asan_println_interior_nul_no_overflow() {
        assert_clean_asan_run(
            r#"
fn main() {
    let a = "AB\0";
    let b = "CD";
    let s = a + b;
    println(s);
    println('\0');
}
"#,
            &["AB\u{0}CD", "\u{0}"],
            "println_interior_nul_no_overflow",
        );
    }

    #[test]
    fn asan_string_slice_no_double_free() {
        // StringSlice v1: a borrowed view (`{ptr,len,cap=0}`) aliases the
        // source String's buffer. Its `cap == 0` must keep the scope-exit drop's
        // `cap > 0` guard a no-op, so the view never frees the source's buffer —
        // only the owned source frees it (once), and each `.to_string()` owned
        // copy frees its own. A view returned from `first_word` into the
        // caller's String is the escaping case. A spurious free of the cap=0
        // view (double-free vs the source) would trip ASAN here.
        assert_clean_asan_run(
            r#"
fn first_word(s: ref String) -> StringSlice {
    let sp = s.find(' ');
    let end = sp.unwrap_or(s.len());
    s.slice(0, end)
}
fn main() {
    let s = "hello world".to_string();
    let w = s.slice(0, 5);
    println(w.to_string());
    let fw = first_word(s);
    println(fw.to_string());
}
"#,
            &["hello", "hello"],
            "string_slice_no_double_free",
        );
    }

    // ── `collect_all_vec` gather (phase-6 slice 1b) ───────────────
    //
    // Lowers a runtime Vec of closures into parallel `karac_par_run`
    // branches, each writing a `Result` into a malloc'd slot array; the
    // slots become the output Vec's buffer while the temp branch/ctx
    // arrays are freed. The `Err` payloads are heap `String`s
    // (`f"neg:{n}"`) that flow closure → slot → output Vec → print →
    // drop; a double-free across the par boundary or in the slot/Vec
    // hand-off (or a use-after-free of a freed temp array) would trip
    // ASAN here. (LeakSanitizer is unsupported on Darwin, so a pure
    // leak is caught only in Linux CI; this guards the UAF/double-free
    // class, which is the codegen-ownership risk.)

    #[test]
    fn asan_collect_all_vec_gather_no_double_free() {
        assert_clean_asan_run(
            r#"
fn work(n: i64) -> Result[i64, String] {
    if n > 0 { Result.Ok(n * 10) } else { Result.Err(f"neg:{n}") }
}
fn main() {
    let fs: Vec[Fn() -> Result[i64, String]] = Vec[|| work(1), || work(-2), || work(3)];
    let results: Vec[Result[i64, String]] = collect_all_vec(fs);
    for r in results {
        match r {
            Result.Ok(v) => { println(f"ok {v}"); }
            Result.Err(e) => { println(f"err {e}"); }
        }
    }
}
"#,
            &["ok 10", "err neg:-2", "ok 30"],
            "collect_all_vec_gather",
        );
    }

    // ── `String.split` — Vec[String] buffer + per-element String frees ──
    //
    // Each `split` returns a `Vec[String]` whose buffer and every element
    // String are libc::malloc'd by `karac_runtime_string_split`; the binding's
    // scope-exit drop must free each element's buffer AND the Vec buffer
    // exactly once. Looped 1000× so the Linux-CI LSan gate catches a per-iter
    // leak; local mac ASAN catches a double-free / UAF. GAP-W2.
    #[test]
    fn asan_string_split_no_leak_no_double_free() {
        assert_clean_asan_run(
            r#"
fn main() {
    let mut i: i64 = 0;
    let mut total: i64 = 0;
    while i < 1000 {
        let line = "a,bb,ccc,dddd";
        let fields = line.split(',');
        total = total + fields.len();
        for f in fields { total = total + f.len(); }
        i = i + 1;
    }
    println(f"{total}");
}
"#,
            &["14000"],
            "string_split_loop",
        );
    }

    // ── `String.lines()` → Vec[String] — per-line heap ownership ──
    //
    // `lines()` allocates a fresh `Vec[String]`, one malloc'd buffer per
    // non-empty line (empty lines are non-owning `{null,0,0}`). Each buffer is
    // owned by the result Vec and freed exactly once at scope exit (same
    // ownership path as `split`); a missed element drop leaks (LSan), a stray
    // alias double-frees (ASAN). Looped 1000× with ≥36-byte lines past any
    // short-String fast path; the CRLF + empty-middle-line input exercises the
    // `\r`-strip / empty-`{null,0,0}` branches, and the `for l in lines`
    // consumes the materialized Vec.
    #[test]
    fn asan_string_lines_no_leak_no_double_free() {
        assert_clean_asan_run(
            r#"
fn main() {
    let mut i: i64 = 0;
    let mut total: i64 = 0;
    while i < 1000 {
        let text = "first-line-payload-aaaaaaaaaaaaaaaa\r\n\r\nthird-line-payload-bbbbbbbbbbbbbbbb\n";
        let ls = text.lines();
        total = total + ls.len();
        for l in ls { total = total + l.len(); }
        i = i + 1;
    }
    println(f"{total}");
}
"#,
            // lines: ["first…"(35), ""(0), "third…"(35)] → 3 lines/iter,
            // len sum 70; (3 + 70) × 1000 = 73000.
            &["73000"],
            "string_lines_loop",
        );
    }

    // `String.split_whitespace()` → Vec[String], same per-piece heap ownership
    // as `lines` (every piece is a fresh malloc'd buffer — split_whitespace
    // never yields an empty `{null,0,0}`). Each freed exactly once at scope
    // exit; a missed drop leaks (LSan), a stray alias double-frees (ASAN).
    // Looped 1000× with ≥36-byte tokens past any short-String fast path; the
    // leading / trailing / repeated whitespace exercises the run-collapsing.
    #[test]
    fn asan_string_split_whitespace_no_leak_no_double_free() {
        assert_clean_asan_run(
            r#"
fn main() {
    let mut i: i64 = 0;
    let mut total: i64 = 0;
    while i < 1000 {
        let text = "   alpha-token-payload-aaaaaaaaaaaaaa   beta-token-payload-bbbbbbbbbbbbbb   ";
        let ws = text.split_whitespace();
        total = total + ws.len();
        for w in ws { total = total + w.len(); }
        i = i + 1;
    }
    println(f"{total}");
}
"#,
            // Tokens 34 + 33 bytes/iter → (2 + 67) × 1000 = 69000.
            &["69000"],
            "string_split_whitespace_loop",
        );
    }

    // ── `Vec[String].binary_search(fresh_needle)` — needle-temp ownership ──
    //
    // `binary_search` itself allocates nothing (the `Option[i64]` result is
    // scalar) and only READS the receiver's String elements (no free). The one
    // ownership obligation is a FRESH-owned String needle passed directly
    // (`v.binary_search(key.to_string())`): the search must free that temp
    // exactly once (`free_fresh_owned_str_arg`), and must NOT free the borrowed
    // receiver elements. Looped 1000× — LSan catches a per-iter needle leak,
    // local ASAN a double-free of the needle or a receiver element. ≥36-byte
    // payloads keep every String heap-allocated.
    #[test]
    fn asan_vec_binary_search_string_needle_no_leak_no_double_free() {
        assert_clean_asan_run(
            r#"
fn main() {
    let mut i: i64 = 0;
    let mut total: i64 = 0;
    while i < 1000 {
        let v: Vec[String] = vec![
            "alpha_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "bravo_bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            "delta_dddddddddddddddddddddddddddddddd",
        ];
        let key: String = "bravo_bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        match v.binary_search(key.to_string()) {
            Some(idx) => total = total + idx,
            None => total = total + 100,
        }
        i = i + 1;
    }
    println(f"{total}");
}
"#,
            // "bravo…" is element 1 every iteration → total = 1000.
            &["1000"],
            "vec_binary_search_string_needle_loop",
        );
    }

    // ── allocating String→String transforms — fresh-buffer ownership ──
    //
    // trim / to_lowercase / to_uppercase / replace each return a FRESH heap
    // buffer (`karac_string_{trim,to_lowercase,to_uppercase,replace}`), which the
    // scope-cleanup machinery must free exactly once. The receiver is untouched
    // (a literal's rodata buffer must never be freed; a derived String must not
    // be aliased by its transform's result). Looped 1000× — LSan catches a
    // per-iter leak (a result never freed), local ASAN catches a double-free (a
    // result aliasing the receiver's buffer). The 36-byte trimmed payload stays
    // heap-allocated past any short-buffer fast path (≥36 bytes — see the
    // LSan-reachability note in lsan-reachability-short-string-leaks.md).
    #[test]
    fn asan_string_trim_replace_case_no_leak_no_double_free() {
        assert_clean_asan_run(
            r#"
fn main() {
    let mut i: i64 = 0;
    let mut total: i64 = 0;
    while i < 1000 {
        let s: String = "  abcdefghijklmnopqrstuvwxyz0123456789  ";
        let t = s.trim();
        let u = t.to_uppercase();
        let l = u.to_lowercase();
        let r = t.replace("abc", "XY");
        total = total + t.len() + u.len() + l.len() + r.len();
        i = i + 1;
    }
    println(f"{total}");
}
"#,
            &["143000"],
            "string_trim_replace_case_loop",
        );
    }

    // `String.trim_start()` / `.trim_end()` return FRESH heap buffers
    // (`karac_string_trim_{start,end}`), the same allocate-and-hand-back shape
    // as `trim` — scope cleanup must free each exactly once and never alias the
    // receiver (the literal's rodata must not be freed). Looped 1000× so LSan
    // catches a per-iter leak and local ASAN a double-free; the ≥36-byte inner
    // payload keeps every result heap-allocated past any short-buffer fast path.
    #[test]
    fn asan_string_trim_start_end_no_leak_no_double_free() {
        assert_clean_asan_run(
            r#"
fn main() {
    let mut i: i64 = 0;
    let mut total: i64 = 0;
    while i < 1000 {
        let s: String = "   abcdefghijklmnopqrstuvwxyz0123456789   ";
        let a = s.trim_start();
        let b = s.trim_end();
        total = total + a.len() + b.len();
        i = i + 1;
    }
    println(f"{total}");
}
"#,
            // trim_start → 39 (drops 3 leading), trim_end → 39 (drops 3 trailing);
            // 78/iter × 1000 = 78000.
            &["78000"],
            "string_trim_start_end_loop",
        );
    }

    // `String.sorted()` returns a FRESH heap buffer (`karac_string_sorted`), the
    // same allocate-and-hand-back shape as trim / to_uppercase — the scope-cleanup
    // machinery must free it exactly once, and it must never alias the receiver's
    // buffer (the literal's rodata must not be freed). Looped 1000× so LSan catches
    // a per-iter leak (a result never freed) and local ASAN catches a double-free
    // (a result aliasing the receiver). The 36-byte payload stays heap-allocated
    // past any short-buffer fast path (lsan-reachability-short-string-leaks.md).
    #[test]
    fn asan_string_sorted_no_leak_no_double_free() {
        assert_clean_asan_run(
            r#"
fn main() {
    let mut i: i64 = 0;
    let mut total: i64 = 0;
    while i < 1000 {
        let s: String = "zyxwvutsrqponmlkjihgfedcba9876543210";
        let k = s.sorted();
        total = total + k.len();
        i = i + 1;
    }
    println(f"{total}");
}
"#,
            &["36000"],
            "string_sorted_loop",
        );
    }

    // ── bound `s.chars()` iterator materialized as Vec[char] — clone ownership ──
    //
    // B-2026-06-18-5: `let it = s.chars()` materializes an eager `Vec[char]`
    // snapshot, and `it.collect()` returns a CLONE of it. Collecting the same
    // bound iterator twice yields two independent buffers, and the snapshot `it`
    // is itself freed at scope exit — three buffers per iteration that must each
    // be freed exactly once. A `for c in it` also drains a materialized snapshot.
    // Looped 1000× so LSan catches a per-iter leak (e.g. the snapshot never
    // freed) and local ASAN catches a double-free (e.g. a clone aliasing the
    // snapshot's buffer). A ≥36-byte payload keeps the snapshot heap-allocated
    // past any short-buffer fast path.
    #[test]
    fn asan_chars_bound_iterator_collect_clone_no_leak_no_double_free() {
        assert_clean_asan_run(
            r#"
fn main() {
    let mut i: i64 = 0;
    let mut total: i64 = 0;
    while i < 1000 {
        let s = "the quick brown fox jumps over the lazy dog";
        let it = s.chars();
        let a: Vec[char] = it.collect();
        let b: Vec[char] = it.collect();
        total = total + a.len() + b.len();
        let it2 = s.chars();
        for c in it2 { if c == 'o' { total = total + 1; } }
        i = i + 1;
    }
    println(f"{total}");
}
"#,
            // len 43 each (×2 = 86) + 4 'o's, ×1000 = 90000.
            &["90000"],
            "chars_bound_iterator_collect_clone_loop",
        );
    }

    // ── `<iter>.map/filter(...).collect()` adaptor chain → Vec (B-2026-07-03-25) ──
    //
    // The desugar builds a fresh Vec and pushes each transformed/surviving
    // element. Two heap-ownership hazards it must get right, looped 1000× so
    // Linux LSan catches a per-iter leak and local ASAN a double-free/UAF:
    //   1. Heap OUTPUT: `.map(|n| n.to_string())` collects a `Vec[String]`; each
    //      produced String is owned by the Vec and freed exactly once at scope
    //      exit (payloads ≥36 bytes so LSan's short-String reachability blind
    //      spot doesn't mask a leak — see the user's LSan memory).
    //   2. Heap SOURCE, borrowed: `words.iter().map(|w| w.len())` reads each
    //      `String` element of the source Vec without consuming it — the source
    //      must remain fully owned and be freed once (a stray move would
    //      double-free or leak). The source is re-read after the collect to
    //      prove it survived. A `filter().map()` chain over the same heap source
    //      exercises the multi-stage path.
    #[test]
    fn asan_iter_adaptor_collect_to_vec_heap_no_leak_no_double_free() {
        assert_clean_asan_run(
            r#"
fn main() {
    let mut i: i64 = 0;
    let mut total: i64 = 0;
    while i < 1000 {
        let src: Vec[i64] = Vec[1i64, 2i64, 3i64, 4i64];
        // Heap OUTPUT — Vec[String], long payloads.
        let strs: Vec[String] = src.iter().map(|n| f"iteration-payload-number-{n}-xyzzy").collect();
        for s in strs { total = total + s.len(); }
        // Heap SOURCE, borrowed — must survive the collect.
        let words: Vec[String] = Vec[
            "alpha-alpha-alpha-alpha-alpha-alpha".to_string(),
            "beta-beta-beta-beta-beta-beta-beta-beta".to_string(),
            "gamma-gamma-gamma-gamma-gamma-gamma-gamma".to_string()
        ];
        let lens: Vec[i64] = words.iter().map(|w| w.len()).collect();
        total = total + lens[0] + lens[1] + lens[2];
        // Multi-stage over the heap source.
        let longs: Vec[i64] = words.iter().filter(|w| w.len() > 36i64).map(|w| w.len()).collect();
        total = total + longs.len();
        // Source still owned/usable after the collects.
        total = total + words.len();
        i = i + 1;
    }
    println(f"{total}");
}
"#,
            // strs: 4 payloads/iter, each "iteration-payload-number-N-xyzzy" (32).
            // words lens: 35 + 39 + 41 = 115. longs: 2 (39,41 > 36). words.len()=3.
            // per iter = 128 + 115 + 2 + 3 = 248; ×1000 = 248000 (matches `karac run`).
            &["248000"],
            "iter_adaptor_collect_to_vec_heap",
        );
    }

    // ── `Vec[String].clear()` + `.extend(...)` — heap-element drop ownership ──
    //
    // `clear()` must DROP every element before resetting the length — for a
    // `Vec[String]` those are heap buffers, so a "just set len=0" implementation
    // would leak them (LSan). `extend(other)` appends CLONES of `other`'s
    // elements, so both vectors own their strings and each buffer is freed
    // exactly once (a stray alias would double-free under ASAN; a missed clone
    // would leak the source at scope exit). Looped 1000× with 40-byte payloads
    // so LSan catches a per-iter leak past any short-String fast path; the
    // reuse-after-clear (push + extend rebuild the buffer) exercises the
    // reset-to-`{null,0,0}` header path.
    #[test]
    fn asan_vec_clear_extend_heap_no_leak_no_double_free() {
        assert_clean_asan_run(
            r#"
fn main() {
    let mut i: i64 = 0;
    let mut total: i64 = 0;
    while i < 1000 {
        let mut v: Vec[String] = Vec.new();
        v.push("payload-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        v.push("payload-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
        v.push("payload-cccccccccccccccccccccccccccccccc");
        v.clear();
        v.push("payload-dddddddddddddddddddddddddddddddd");
        let mut w: Vec[String] = Vec.new();
        w.push("payload-eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee");
        w.push("payload-ffffffffffffffffffffffffffffffff");
        v.extend(w);
        total = total + v[0].len() + v[1].len() + v[2].len();
        i = i + 1;
    }
    println(f"{total}");
}
"#,
            // Each payload is 40 bytes; after clear+push+extend `v` is [d, e, f]
            // → 120/iter, ×1000 = 120000.
            &["120000"],
            "vec_clear_extend_heap",
        );
    }

    // ── `String.split` on a NON-identifier receiver — temp drop ownership ──
    //
    // `make_csv().split(',')` — a String method on a CALL-RESULT receiver. The
    // `try_compile_nonident_collection_method` shim materializes the receiver
    // into a synth local and routes through `compile_vec_method`. The receiver's
    // heap buffer (`make_csv()`'s String) is freed by the statement-level
    // owned-temp machinery; the shim must NOT separately drop-track its slot, or
    // the buffer double-frees (a tracked variant SIGABRT'd at scope exit). Plus
    // a string-LITERAL receiver (`"...".split`) whose rodata buffer must not be
    // freed at all. Looped 1000× — LSan catches a per-iter leak of the receiver
    // temp, local ASAN catches the double-free. (phase-7 non-identifier receiver)
    #[test]
    fn asan_string_method_nonident_receiver_no_leak_no_double_free() {
        assert_clean_asan_run(
            r#"
fn make_csv() -> String { return "a,bb,ccc,dddd"; }
fn main() {
    let mut i: i64 = 0;
    let mut total: i64 = 0;
    while i < 1000 {
        let call_fields = make_csv().split(',');
        total = total + call_fields.len();
        for f in call_fields { total = total + f.len(); }
        let lit_fields = "p,qq,rrr".split(',');
        total = total + lit_fields.len();
        i = i + 1;
    }
    println(f"{total}");
}
"#,
            &["17000"],
            "string_split_nonident_recv_loop",
        );
    }

    // ── Inline index of a fn-returned `Vec` — temp drop + element clone ──
    //
    // `names()[i]` indexes a fresh owned `Vec[String]` temporary inline
    // (no intermediate binding). The element read shallow-aliases the
    // temp's buffer, so codegen deep-clones the indexed `String` before
    // dropping the temp Vec (buffer + every element's char heap). A
    // missing clone → use-after-free on the printed value; a missing drop
    // → leak of the buffer + the un-indexed elements; double-freeing the
    // clone's source → double-free. Each `names()` allocates three
    // Strings; only the indexed one's clone escapes, the rest and the
    // buffer must free exactly once. (phase-11-stdlib-longtail.md)

    #[test]
    fn asan_inline_index_fn_returned_vec_string_no_leak() {
        assert_clean_asan_run(
            r#"
fn names() -> Vec[String] {
    let mut v: Vec[String] = Vec.new();
    v.push("alice"); v.push("bob"); v.push("carol");
    v
}
fn main() {
    println(names()[0]);
    println(names()[2]);
}
"#,
            &["alice", "carol"],
            "inline_index_fn_returned_vec_string",
        );
    }

    // Sibling of the above (B-2026-06-14-32, the by-value-argument consumer):
    // the inline-temp-Vec heap-element clone passed DIRECTLY to a user fn
    // (`sink(names()[0])`) — not just to `println` — must also free exactly
    // once. The callee takes the `String` owned by value (which the callee does
    // NOT free: owned String/Vec params land in `owned_vecstr_params`), so the
    // caller-side `materialize_owned_temp` is the only thing reclaiming the
    // clone. A missing materialization leaks the clone (Linux LSan); a stray
    // second free (callee + caller both freeing) double-frees (ASAN, every
    // host). Looping makes either fault unmistakable.
    #[test]
    fn asan_inline_index_fn_returned_vec_string_fn_arg_no_leak() {
        assert_clean_asan_run(
            r#"
fn names() -> Vec[String] {
    let mut v: Vec[String] = Vec.new();
    v.push("alice"); v.push("bob"); v.push("carol");
    v
}
fn sink(s: String) { println(s); }
fn main() {
    let mut i = 0i64;
    while i < 3 {
        sink(names()[0]);
        sink(names()[2]);
        i = i + 1;
    }
}
"#,
            &["alice", "carol", "alice", "carol", "alice", "carol"],
            "inline_index_fn_returned_vec_string_fn_arg",
        );
    }

    // ── tuple-destructure leaf cleanup (B-2026-06-13-5) ───────────
    //
    // `let (a, b) = pair()` extracts each element into a fresh leaf alloca.
    // Pre-fix the leaves got NO scope-exit free, so a String/Vec element's
    // heap buffer leaked once per destructure (2000 leaks / 46 KB over a
    // 1000-iter loop). `finish_owned_tuple_destructure` now frees each
    // heap-owning leaf. The Linux-CI LSan job is the leak gate; this run also
    // guards against the move-out double-free risk the fix introduces — a leaf
    // RETURNED from a fn (`first`, moved out of the destructure) and a leaf
    // produced by a NON-fresh destructure (`let (c, d) = t`, a move of an
    // existing tuple binding the source frees) must each be freed exactly once.
    // Looping a few hundred times makes any double-free / UAF trip ASAN.
    #[test]
    fn asan_tuple_destructure_leaf_cleanup_no_leak_no_double_free() {
        assert_clean_asan_run(
            r#"
fn pair(n: i64) -> (String, String) { (f"L{n}", f"R{n}") }
fn first(n: i64) -> String { let (a, _) = pair(n); a }
fn main() {
    let (a, b) = pair(1);     // both leaves owned + freed (the reported leak)
    println(a);
    println(b);
    println(first(2));        // returned leaf — moved out, must not double-free
    let t = pair(3);
    let (c, d) = t;           // non-fresh destructure — source frees, not c/d
    println(c);
    println(d);
    let (e, _) = pair(4);     // wildcard-discarded element also freed
    println(e);
}
"#,
            &["L1", "R1", "L2", "L3", "R3", "L4"],
            "tuple_destructure_leaf_cleanup",
        );
    }

    // ── push_str of a fresh-owned String temp (lexer token-text shape) ──
    //
    // `buffer.push_str(s.substring(a, b))` passes a freshly-malloc'd String
    // to push_str, which copies its bytes then frees the temp immediately.
    // Pre-fix the temp leaked ~48 bytes/call, unbounded (kata-katas #722
    // bench: 93.6 MiB → 1.7 MiB at 2M iters). The new free() must not
    // double-free the source buffer nor UAF a later read — looping the
    // append a few times makes either trip ASAN.

    #[test]
    fn asan_push_str_substring_temp_no_double_free() {
        assert_clean_asan_run(
            r#"
fn main() {
    let s: String = "alpha beta gamma delta";
    let mut acc: String = "";
    let mut k = 0i64;
    while k < 4i64 {
        acc.push_str(s.substring(0i64, 5i64));
        acc.push_str("-");
        k = k + 1i64;
    }
    println(acc);
}
"#,
            &["alpha-alpha-alpha-alpha-"],
            "push_str_substring_temp",
        );
    }

    // ── contains / starts_with of a fresh-owned String temp ───────
    //
    // `keyword.contains(s.substring(a, b))` / `name.starts_with(tok)` — the
    // lexer's keyword-membership and prefix-check surface — pass a freshly-
    // malloc'd String the method reads then discards. Codegen frees the temp
    // at the post-scan merge block via the shared `free_fresh_owned_str_arg`
    // helper (same as push_str). Pre-fix each leaked unbounded (~32 MiB at 2M
    // iters); this run guards the frees against double-free / UAF.

    #[test]
    fn asan_contains_starts_with_substring_temp_no_double_free() {
        assert_clean_asan_run(
            r#"
fn main() {
    let hay: String = "fn let mut while return match";
    let src: String = "returns_here_padded_xxxxxxxxxx";
    let mut hits = 0i64;
    let mut k = 0i64;
    while k < 4i64 {
        if hay.contains(src.substring(0i64, 6i64)) { hits = hits + 1i64; }
        if src.starts_with(src.substring(0i64, 3i64)) { hits = hits + 1i64; }
        k = k + 1i64;
    }
    println(f"{hits}");
}
"#,
            &["8"],
            "contains_starts_with_substring_temp",
        );
    }

    // ── B-2026-06-12-5: push_str of a fresh-owned String RANGE-SLICE temp ──
    //
    // `buffer.push_str(src[a..b])` — the lexer's idiomatic zero-copy token-text
    // shape — passes a `String[a..b]` range-index slice, which `compile_index`
    // → `compile_string_slice` lowers to a *freshly* `karac_string_slice`-
    // allocated owned `{ptr,len,cap}` (cap > 0), exactly like `.substring(a,b)`.
    // But a range slice is an `Index`, not a `Call`/`MethodCall`, so the pre-fix
    // `expr_yields_fresh_owned_temp` gate missed it and `free_fresh_owned_str_arg`
    // never fired — the slice buffer leaked once per call (measured 34 MiB at 2M
    // iters vs 2.5 MiB clean). The fix broadens the gate with
    // `expr_is_fresh_owned_string_slice`; this run guards that free against
    // double-free / UAF (the `cap > 0` guard + place-safe gate keep a borrowed
    // view or a `ref String` identifier untouched).

    #[test]
    fn asan_push_str_range_slice_temp_no_double_free() {
        assert_clean_asan_run(
            r#"
fn make(s: ref String) -> String {
    let mut o: String = "";
    o.push_str(s[0..5]);
    o
}

fn main() {
    let src: String = "alpha beta gamma delta";
    let mut acc: String = "";
    let mut k = 0i64;
    while k < 4i64 {
        let d = make(src);
        acc.push_str(d[0..3]);
        k = k + 1i64;
    }
    println(acc);
}
"#,
            &["alpalpalpalp"],
            "push_str_range_slice_temp",
        );
    }

    // ── #20: call / method result as an inline argument ──────────
    //
    // A heap String produced by a `Call` (`sink(mk(i))`) or `MethodCall`
    // (`println(i.to_string())`) and passed DIRECTLY as a by-value argument
    // is a fresh owned temp with no consuming binding. Owned String params
    // are caller-freed (the callee never drops them), so the temp orphaned
    // and leaked one buffer per call — unbounded in a loop, and a real
    // accumulating leak in the lexer/parser's inline string building.
    // Fixed by materializing the user-fn call-result arg into the caller
    // scope (`materialize_owned_temp`) and freeing the `println` arg buffer
    // via `free_fresh_owned_str_arg`. Both are Call/MethodCall-only and
    // place-/literal-safe, so a `let`-bound arg (owned by its binding) is
    // untouched — no double-free.

    #[test]
    fn asan_call_result_arg_temp_no_leak() {
        assert_clean_asan_run(
            r#"
fn mk(i: i64) -> String {
    let mut s = String.new();
    s.push_str("v");
    s.push_str(i.to_string());
    s
}
fn sink(s: String) { if s.len() > 99999 { println(s); } }
fn main() {
    let mut i = 0i64;
    while i < 5i64 {
        sink(mk(i));
        i = i + 1i64;
    }
    let t = mk(7i64);
    sink(t);
    println("ok");
}
"#,
            &["ok"],
            "call_result_arg_temp",
        );
    }

    #[test]
    fn asan_println_method_result_temp_no_leak() {
        assert_clean_asan_run(
            r#"
fn main() {
    let mut i = 0i64;
    while i < 5i64 {
        println(i.to_string());
        i = i + 1i64;
    }
}
"#,
            &["0", "1", "2", "3", "4"],
            "println_method_result_temp",
        );
    }

    // ── `collect_all_vec` with capturing closures ─────────────────
    //
    // The canonical fan-out shape: each closure captures an outer
    // binding and wraps a named call (`|| fetch(a)`). The captured
    // values live in stack env allocas in `main`'s frame and are read
    // by the worker threads across the synchronous `karac_par_run`
    // join — a use-after-free of the env (or of the freed input Vec
    // buffer) would trip ASAN here.

    #[test]
    fn asan_collect_all_vec_capturing_closures_no_uaf() {
        assert_clean_asan_run(
            r#"
fn fetch(id: i64) -> Result[i64, String] {
    if id > 0 { Result.Ok(id * 10) } else { Result.Err(f"bad:{id}") }
}
fn main() {
    let a: i64 = 1;
    let b: i64 = -2;
    let c: i64 = 3;
    let fs: Vec[Fn() -> Result[i64, String]] = Vec[|| fetch(a), || fetch(b), || fetch(c)];
    let results: Vec[Result[i64, String]] = collect_all_vec(fs);
    for r in results {
        match r {
            Result.Ok(v) => { println(f"ok {v}"); }
            Result.Err(e) => { println(f"err {e}"); }
        }
    }
}
"#,
            &["ok 10", "err bad:-2", "ok 30"],
            "collect_all_vec_capturing",
        );
    }

    // ── First-class closure-value codegen (closure-value-codegen-fixes) ──
    //
    // Three pre-existing gaps that `collect_all_vec` surfaced, fixed in
    // `src/codegen/closures.rs`: (1) a closure body that inline-constructs
    // an enum variant (`|| Result.Ok(x)`) — return-type inference returned
    // the payload type, not the enum, so the closure fn `ret`'d a mismatched
    // type; (2) an f-string inside a closure body (`|| Result.Err(f"…")`) —
    // the accumulator's cleanup leaked into the outer fn's frame
    // (dominance verifier error); (3) direct closure-value call + match,
    // a downstream symptom of (1). The cleanup-frame isolation that fixes
    // (2) is the ASAN-sensitive change: an f-string moved into the
    // returned `Result` must be freed exactly once (by the consumer's
    // drop, NOT the closure), so this run guards against a double-free.

    #[test]
    fn asan_closure_inline_result_and_fstring_no_double_free() {
        assert_clean_asan_run(
            r#"
fn main() {
    let base: i64 = 100;
    let ok: Fn() -> Result[i64, String] = || Result.Ok(base + 1);
    let err: Fn() -> Result[i64, String] = || Result.Err(f"bad{base}");
    match ok() {
        Result.Ok(v) => { println(f"ok {v}"); }
        Result.Err(e) => { println(f"err {e}"); }
    }
    match err() {
        Result.Ok(v) => { println(f"ok {v}"); }
        Result.Err(e) => { println(f"err {e}"); }
    }
}
"#,
            &["ok 101", "err bad100"],
            "closure_inline_result_and_fstring",
        );
    }

    // ── Match arm whose VALUE is an f-string (phase-12 blocker #3) ──
    //
    // A direct-f-string match arm (`Some(name) => f"[{name}]"`) builds the
    // f-string accumulator, which is `track_vec_var`-registered for the
    // per-arm scope cleanup. Before the fix, the per-arm drain freed the
    // acc's buffer between the value load and the merge phi, so the match
    // result was an empty/dangling String. The fix zeroes the acc's `cap`
    // when the arm tail is an f-string (ownership moves to the match result).
    // This run exercises the consumed (let-bound / returned) AND discarded
    // forms over non-foldable (concat-built) payloads — a stale free of the
    // moved buffer trips ASAN's double-free, and the discarded form must
    // still single-free via the expression-statement cleanup.

    #[test]
    fn asan_fstring_match_arm_value_no_double_free() {
        assert_clean_asan_run(
            r#"
enum E { A(String), B(i64) }
fn describe(e: E) -> String {
    match e {
        E.A(name) => f"A[{name}]",
        E.B(k) => f"B[{k}]",
    }
}
fn main() {
    let mut i: i64 = 0;
    while i < 3 {
        let e = E.A("dyn" + "amic");
        let s = describe(e);
        println(s);
        // discarded arm-f-string result — must single-free, no double-free
        let d = E.A("tmp" + "val");
        match d {
            E.A(n) => f"[{n}]",
            E.B(_) => "x",
        };
        i = i + 1;
    }
}
"#,
            &["A[dynamic]", "A[dynamic]", "A[dynamic]"],
            "fstring_match_arm_value",
        );
    }

    // ── Block expression used AS A VALUE returns a live (not freed) buffer ──
    //
    // B-2026-06-11-2: a block in value position (`let s = { …; tail }`, an
    // `if`/`match` arm, a function-return block) whose tail is a
    // scope-registered heap value — an f-string accumulator or a block-local
    // `let`-bound String — was freed by the block frame's `drain_top_frame_
    // with_emit` between the tail-value load and the value escaping. That left
    // the consumer holding a dangling buffer (use-after-free) and, against the
    // consumer's own owner cleanup, a double-free. Fix: suppress the tail
    // value's cleanup before the block-frame drain so the consumer's binding is
    // the sole owner. The loop builds a FRESH heap String each iteration in
    // every consumer position with non-foldable (concat / f-string) sources, so
    // a stale free of any trips ASAN's heap-use-after-free / double-free.

    #[test]
    fn asan_block_expr_value_heap_return_no_stale_free() {
        assert_clean_asan_run(
            r#"
enum E { A(String), B }
fn mk(n: i64) -> String { { f"r{n}" } }
fn main() {
    let mut i: i64 = 0;
    while i < 3 {
        let a = { f"a{i}" };
        println(a);
        let b = { let p = "x" + "y"; p };
        println(b);
        let c = if i < 5 { f"c{i}" } else { f"d{i}" };
        println(c);
        let e1 = E.A("m" + "m");
        let d = match e1 { E.A(n) => { f"<{n}>" }, E.B => "z" };
        println(d);
        let e2 = E.A("k" + "k");
        let g = match e2 { E.A(n) => { let p = f"[{n}]"; p }, E.B => "z" };
        println(g);
        println(mk(i));
        i = i + 1;
    }
}
"#,
            &[
                "a0", "xy", "c0", "<mm>", "[kk]", "r0", "a1", "xy", "c1", "<mm>", "[kk]", "r1",
                "a2", "xy", "c2", "<mm>", "[kk]", "r2",
            ],
            "block_expr_value_heap_return",
        );
    }

    // ── Block-construct call argument owns its temp (no leak) ──
    //
    // B-2026-06-11-5 (residual of B-2026-06-11-2): a block passed DIRECTLY as
    // a call argument (`take({ f"…" })`) had its tail acc suppressed by
    // `suppress_block_tail_cleanup` so a binding/return consumer could own it —
    // but a bare call argument has no owning consumer, so the temp orphaned and
    // leaked (a DIRECT `take(f"…")` is caller-owned and clean). Fix:
    // `materialize_owned_temp` the block-arg value into the caller scope, the
    // same caller ownership a direct f-string arg gets. The loop builds a fresh
    // heap String/Vec each iteration in argument position; on Linux LSan a
    // leaked temp trips, and a double-free / UAF (if the temp were both
    // materialized AND owned elsewhere) trips macOS ASAN too.
    #[test]
    fn asan_block_arg_temp_owned_no_leak() {
        assert_clean_asan_run(
            r#"
fn take_s(s: String) { if s.len() > 99999 { println(s); } }
fn take_v(v: Vec[i64]) { if v.len() > 99999 { println(v.len()); } }
fn two(a: String, b: String) { if a.len() > 99999 { println(a); println(b); } }
fn main() {
    let mut i: i64 = 0;
    while i < 5 {
        take_s({ f"arg{i}-{i}" });
        take_s({ let p = "x" + "y"; p });
        take_v({ Vec[i, i, i] });
        two({ f"p{i}" }, { f"q{i}" });
        take_s(f"direct{i}");
        i = i + 1;
    }
    println("done");
}
"#,
            &["done"],
            "block_arg_temp_owned",
        );
    }

    // ── By-value aggregate (tuple / literal / nested) heap-field drops ──
    //
    // B-2026-06-11-4: by-value aggregates leaked their String/Vec fields across
    // shapes the named-struct drop path didn't reach — a let-bound tuple (no
    // type name → no `track_struct_var`), a tuple/struct LITERAL arg (no binding
    // → no owner), and a nested-struct field (the synthesized struct drop didn't
    // recurse). Fix: `track_tuple_var` (anonymous-aggregate drop at the let
    // site), aggregate-literal materialization at the call site, and
    // nested-aggregate recursion in `emit_struct_drop_synthesis`; tuple moves
    // (`let u = t` / `return t`) suppress the source via `zero_aggregate_field_
    // caps`. The loop builds a fresh heap aggregate each iteration in every
    // shape; a leaked field trips Linux LSan, and a double-free (if a moved
    // tuple or a materialized literal were owned twice) trips macOS ASAN.
    #[test]
    fn asan_by_value_aggregate_drops_single_free() {
        assert_clean_asan_run(
            r#"
struct S { k: i64, name: String }
struct Inner { name: String }
struct Outer { id: i64, inner: Inner }
fn show_tup(p: (i64, String)) { if p.0 > 99999 { println(p.1); } }
fn fwd(p: (i64, String)) { show_tup(p); }
fn show_s(s: S) { if s.k > 99999 { println(s.name); } }
fn show_o(o: Outer) { if o.id > 99999 { println(o.inner.name); } }
fn mk(n: i64) -> (i64, String) { (n, f"r-{n}") }
fn main() {
    let mut i: i64 = 0;
    while i < 5 {
        let t = (i, f"let-{i}");
        show_tup(t);
        let u = (i, f"mv-{i}");
        let w = u;
        if w.0 > 99999 { println(w.1); }
        let r = mk(i);
        if r.0 > 99999 { println(r.1); }
        fwd((i, f"fwd-{i}"));
        show_tup((i, f"lit-{i}"));
        show_s(S { k: i, name: f"slit-{i}" });
        let o = Outer { id: i, inner: Inner { name: f"nest-{i}" } };
        show_o(o);
        i = i + 1;
    }
    println("done");
}
"#,
            &["done"],
            "by_value_aggregate_drops",
        );
    }

    // ── Closures that RETURN a heap value (closure-heap-return-cleanup) ──
    //
    // A closure whose body is a block returning a heap binding
    // (`|| { let s = mk(); s }`) used to free that binding via the
    // block's *nested* scope cleanup BEFORE the tail-return suppression
    // could fire — handing back a dangling pointer (use-after-free, and a
    // double-free / SIGABRT for the String-from-call case). The fix
    // compiles the closure's block body like a function body (raw
    // `compile_block`, no nested scope) so the suppression zeroes the
    // returned binding's `cap` before the closure's own scope cleanup
    // runs. This run exercises String (via f-string and via call),
    // direct-f-string, and Vec returns — a stale free of any would trip
    // ASAN's double-free / heap-use-after-free.

    #[test]
    fn asan_closure_returns_heap_value_no_double_free() {
        assert_clean_asan_run(
            r#"
fn mk() -> String { f"made" }
fn compute() -> Vec[i64] { Vec[9, 8] }
fn main() {
    let f2: Fn() -> String = || { let s = f"hi"; s };
    let f4: Fn() -> String = || { let s: String = mk(); s };
    let f5: Fn() -> String = || f"direct";
    let f1: Fn() -> Vec[i64] = || { let v: Vec[i64] = Vec[1, 2, 3]; v };
    let f3: Fn() -> Vec[i64] = || { let w = compute(); w };
    let r2: String = f2();
    let r4: String = f4();
    let r5: String = f5();
    let r1: Vec[i64] = f1();
    let r3: Vec[i64] = f3();
    println(r2);
    println(r4);
    println(r5);
    println(r1.len());
    println(r3.len());
}
"#,
            &["hi", "made", "direct", "3", "2"],
            "closure_returns_heap_value",
        );
    }

    // ── `collect_all` heterogeneous tuple gather (phase-6) ────────
    //
    // Static-N sibling of collect_all_vec: each inline closure runs via
    // karac_par_run into a stack Result slot, then the slots are assembled
    // into a tuple. Captured args (`base`) live in stack env allocas read
    // by worker threads across the synchronous join, and the f-string
    // `Err` payloads (`f"a{n}"`) flow closure → slot → tuple → match →
    // print → drop. A use-after-free of an env / slot, or a double-free of
    // an Err String, would trip ASAN here.

    #[test]
    fn asan_collect_all_heterogeneous_tuple_no_uaf() {
        assert_clean_asan_run(
            r#"
fn fa(n: i64) -> Result[i64, String] {
    if n > 0 { Result.Ok(n * 10) } else { Result.Err(f"a{n}") }
}
fn fb(s: String) -> Result[String, i64] { Result.Err(7) }
fn main() {
    let base: i64 = 3;
    let t: (Result[i64, String], Result[String, i64], Result[i64, String]) =
        collect_all(|| fa(-5), || fb("x"), || fa(base));
    match t.0 { Result.Ok(v) => { println(f"0 ok {v}"); } Result.Err(e) => { println(f"0 err {e}"); } }
    match t.1 { Result.Ok(v) => { println(f"1 ok {v}"); } Result.Err(e) => { println(f"1 err {e}"); } }
    match t.2 { Result.Ok(v) => { println(f"2 ok {v}"); } Result.Err(e) => { println(f"2 err {e}"); } }
}
"#,
            &["0 err a-5", "1 err 7", "2 ok 30"],
            "collect_all_heterogeneous_tuple",
        );
    }

    // ── `println(String)` — `%.*s` length-bounded format ──────────
    //
    // Pre-fix `compile_print`'s struct-value arm passed the String's
    // data pointer to `printf("%s\n", str)` directly. LLVM rewrote
    // the call to `puts(str)` as a libc-call optimization, and ASAN
    // flagged the 1-byte overread when puts walked past the
    // non-NUL-terminated heap buffer. String-literal cases worked
    // by luck — clang's `c"...\0"` global form puts a NUL right
    // after — but any heap-allocated String (concat result, function
    // return) overran. The fix routes through `%.*s` with the
    // explicit length, so printf reads exactly `len` bytes and the
    // libc-call optimizer doesn't substitute puts. Covers four
    // shapes that all hit the same struct-value arm: literal,
    // heap concat, function-return-via-let-binding, ref-String
    // parameter to a heap source.

    #[test]
    fn asan_println_string_literal_no_overread() {
        // Literal: `cap = 0`, buffer in .rodata. Pre-fix this case
        // worked by luck because the compiler's static-string emitter
        // (clang's `c"...\0"` form) writes a trailing NUL into
        // .rodata even though `cap` doesn't account for it. The fix
        // routes the same buffer through `%.*s` with the explicit
        // length, never depending on the trailing-byte coincidence.
        assert_clean_asan_run(
            r#"
fn main() {
    println("hello literal");
}
"#,
            &["hello literal"],
            "println_string_literal_no_overread",
        );
    }

    #[test]
    fn asan_println_heap_string_concat() {
        // Heap-owning rvalue from concatenation. Pre-fix this case
        // failed with a heap-buffer-overflow at puts because the
        // concat helper allocates exactly `len` bytes (no trailing
        // NUL) and `printf("%s\n", str)` → `puts(str)` walked one
        // past the buffer.
        assert_clean_asan_run(
            r#"
fn main() {
    let a = "left ";
    let b = "right";
    println(a + b);
}
"#,
            &["left right"],
            "println_heap_string_concat",
        );
    }

    #[test]
    fn asan_operand_temp_string_concat_freed() {
        // General owned-temp tracking, slice 3c: a fresh-temp String OPERAND of
        // a string binop. `make_s() + " [suffix]"` reads the fresh `make_s()`
        // buffer into a new concat result but never frees the operand, so it
        // leaks once per iteration (LeakSanitizer on Linux CI). The concat
        // RESULT is bound to `r` and freed by its binding (the operand is the
        // only new leak); a regression that frees the operand twice — or that
        // frees the still-read operand before the concat copies it — double-frees
        // / UAFs (macOS ASAN). ≥36-byte operand defeats LSan short-string
        // reachability; the loop forces per-iteration accumulation.
        assert_clean_asan_run(
            r#"
fn make_s() -> String {
    let s: String = "a freshly allocated heap operand string over thirty-six bytes";
    return s;
}

fn main() {
    let mut i = 0;
    while i < 3 {
        let r = make_s() + " [suffix]";
        println(r);
        i = i + 1;
    };
}
"#,
            &[
                "a freshly allocated heap operand string over thirty-six bytes [suffix]",
                "a freshly allocated heap operand string over thirty-six bytes [suffix]",
                "a freshly allocated heap operand string over thirty-six bytes [suffix]",
            ],
            "operand_temp_string_concat_freed",
        );
    }

    #[test]
    fn asan_operand_temp_chained_concat_freed() {
        // Slice 3c chained case: `make_s() + " mid " + <tail>` parses as
        // `(make_s() + " mid ") + <tail>`. The fresh `make_s()` operand of the
        // INNER `+` is freed there; the inner `+` RESULT is itself a fresh temp
        // consumed as the outer `+`'s left operand and must also be freed (it is
        // a `Binary{Add}` operand, recognized as a fresh String concat). Three
        // distinct buffers — `make_s()`, the inner concat, the outer concat (the
        // last bound to `r`) — each freed exactly once: no leak, no double-free.
        assert_clean_asan_run(
            r#"
fn make_s() -> String {
    let s: String = "a freshly allocated heap operand string over thirty-six bytes";
    return s;
}

fn main() {
    let mut i = 0;
    while i < 3 {
        let r = make_s() + " mid " + "tail padded out beyond thirty-six bytes here";
        println(r);
        i = i + 1;
    };
}
"#,
            &[
                "a freshly allocated heap operand string over thirty-six bytes mid tail padded out beyond thirty-six bytes here",
                "a freshly allocated heap operand string over thirty-six bytes mid tail padded out beyond thirty-six bytes here",
                "a freshly allocated heap operand string over thirty-six bytes mid tail padded out beyond thirty-six bytes here",
            ],
            "operand_temp_chained_concat_freed",
        );
    }

    #[test]
    fn asan_operand_temp_named_binding_not_double_freed() {
        // Slice 3c negative / double-free guard: a NAMED String binding used as
        // a binop operand (`s + " [suffix]"`) must NOT be freed by the
        // operand-temp path — `s` is an `Identifier` (not a fresh-temp shape),
        // so it owns its buffer and frees it at iteration-scope exit. If the
        // operand-free wrongly fired on `s`, the binding's own free would
        // double-free it each iteration (macOS ASAN). The concat result `r` is
        // freed by its own binding.
        assert_clean_asan_run(
            r#"
fn make_s() -> String {
    let s: String = "a freshly allocated heap operand string over thirty-six bytes";
    return s;
}

fn main() {
    let mut i = 0;
    while i < 3 {
        let s = make_s();
        let r = s + " [suffix]";
        println(r);
        i = i + 1;
    };
}
"#,
            &[
                "a freshly allocated heap operand string over thirty-six bytes [suffix]",
                "a freshly allocated heap operand string over thirty-six bytes [suffix]",
                "a freshly allocated heap operand string over thirty-six bytes [suffix]",
            ],
            "operand_temp_named_binding_not_double_freed",
        );
    }

    #[test]
    fn asan_primitive_to_string_owning() {
        // `x.to_string()` mallocs an owning String (same shape as the f-string
        // builder). Exercise the let-bound, printed-temp, and concatenated
        // forms so ASAN catches any over-read / double-free / leak in the new
        // primitive `to_string` lowering.
        assert_clean_asan_run(
            r#"
fn main() {
    let n = -42i64;
    let s = n.to_string();
    println(s);
    println(n.to_string());
    println(n.to_string() + "!");
}
"#,
            &["-42", "-42", "-42!"],
            "primitive_to_string_owning",
        );
    }

    #[test]
    fn asan_struct_display_to_string() {
        // User-struct Display renders via the synthetic-f-string path, which
        // mallocs an owning String (and, for nested structs, intermediate
        // Strings registered for scope cleanup). Exercise println + a bound
        // to_string of a nested struct so ASAN catches over-read / leak /
        // double-free in the struct Display lowering.
        assert_clean_asan_run(
            r#"
#[derive(Display)]
struct Point { x: i64, y: i64 }
#[derive(Display)]
struct Wrap { p: Point, name: String, ok: bool }
fn main() {
    let w = Wrap { p: Point { x: 1, y: 2 }, name: "hi", ok: true };
    println(w);
    let s = w.to_string();
    println(s);
}
"#,
            &[
                "Wrap { p: Point { x: 1, y: 2 }, name: hi, ok: true }",
                "Wrap { p: Point { x: 1, y: 2 }, name: hi, ok: true }",
            ],
            "struct_display_to_string",
        );
    }

    #[test]
    fn asan_enum_display_to_string() {
        // All-unit enum `to_string()` mallocs an owning String of the variant
        // name; the enum-in-struct field path renders via the same lowering.
        // ASAN guards the variant-name copy + nested render.
        assert_clean_asan_run(
            r#"
#[derive(Display)]
enum Color { Red, Green, Blue }
#[derive(Display)]
struct Tagged { c: Color, n: i64 }
fn main() {
    let a = Color.Green;
    let s = a.to_string();
    println(s);
    let t = Tagged { c: Color.Blue, n: 9 };
    println(t.to_string());
}
"#,
            &["Green", "Tagged { c: Blue, n: 9 }"],
            "enum_display_to_string",
        );
    }

    #[test]
    fn asan_collection_display_buffer() {
        // The unified buffer-render path mallocs/grows an accumulator for
        // collection println (freed inline), f-string interpolation (scope-
        // tracked), and `.to_string()` (binding-owned). Exercise all three so
        // ASAN catches over-read / leak / double-free across the paths.
        assert_clean_asan_run(
            r#"
fn main() {
    let mut v: Vec[i64] = Vec.new();
    v.push(1);
    v.push(2);
    println(v);
    println(f"v={v}");
    let s = v.to_string();
    println(s);
    let mut m: Map[String, i64] = Map.new();
    m.insert("k", 9);
    println(m.to_string());
}
"#,
            &["[1, 2]", "v=[1, 2]", "[1, 2]", "{k: 9}"],
            "collection_display_buffer",
        );
    }

    #[test]
    fn asan_println_function_return_string_via_let_binding() {
        // Function returns owned heap String; bound to a local;
        // printed. This is the let-binding form the kata workaround
        // used pre-C — even with the materialization fix landed,
        // `println(m)` still hit the overread until this slice.
        assert_clean_asan_run(
            r#"
fn make() -> String {
    let a = "made ";
    let b = "string";
    return a + b;
}

fn main() {
    let m = make();
    println(m);
}
"#,
            &["made string"],
            "println_function_return_string_via_let_binding",
        );
    }

    #[test]
    fn asan_fstring_into_returned_plain_struct_field_no_double_free() {
        // An f-string used DIRECTLY as a struct-literal field value
        // (`Resp { body: f"..." }`) moves the accumulator buffer into the
        // field; the struct is returned, so the caller owns the buffer.
        // Before the fix, `compile_struct_init` left `last_fstr_acc`
        // staged, so the accumulator's scope-exit `FreeVecBuffer` freed
        // the same buffer the returned struct carried — a double-free that
        // aborted under macOS malloc (exit 133). The fix takes + cap-zeros
        // the staged acc at the struct-field site, mirroring the Let /
        // Assign take points. Reading the field three times in the caller
        // surfaces a UAF under ASAN if the buffer were freed early. Covers
        // the non-shared (stack-aggregate) branch of `compile_struct_init`.
        assert_clean_asan_run(
            r#"
struct Resp { status: i64, body: String }
fn make(id: i64, name: String) -> Resp {
    Resp { status: 200, body: f"id={id} name={name}" }
}
fn main() {
    let r = make(7, "Alice");
    println(r.status);
    println(r.body);
    println(r.body);
    println(r.body);
}
"#,
            &[
                "200",
                "id=7 name=Alice",
                "id=7 name=Alice",
                "id=7 name=Alice",
            ],
            "fstring_into_returned_plain_struct_field_no_double_free",
        );
    }

    #[test]
    fn asan_fstring_into_returned_shared_struct_field_no_double_free() {
        // Same double-free, the shared-struct branch of
        // `compile_struct_init` (Arc heap-RC layout — fields stored inline
        // after the refcount header). An f-string field value transfers the
        // buffer into the heap slot, so the staged acc must be suppressed
        // identically. Reading the field twice through the returned handle
        // surfaces a UAF under ASAN if the accumulator freed it early.
        assert_clean_asan_run(
            r#"
shared struct Holder { label: String }
fn make(n: i64) -> Holder {
    Holder { label: f"n={n}" }
}
fn main() {
    let h = make(42);
    println(h.label);
    println(h.label);
}
"#,
            &["n=42", "n=42"],
            "fstring_into_returned_shared_struct_field_no_double_free",
        );
    }

    #[test]
    fn asan_fstring_explicit_return_no_double_free() {
        // Sibling double-free site: a DIRECT `return f"..."` mid-function
        // moves the accumulator buffer to the caller. The `Return` arm's
        // pre-compile suppression is Identifier-only; the accumulator is
        // staged only during `compile_expr`, so without post-compile
        // suppression the scope-cleanup walk freed the returned buffer — a
        // double-free aborting under macOS malloc. Exercises both the
        // early-return arm and the tail-expr arm of the same fn; the caller
        // reads each returned String, which surfaces a UAF under ASAN if the
        // buffer were freed early.
        assert_clean_asan_run(
            r#"
fn pick(id: i64) -> String {
    if id > 0 { return f"pos={id}"; }
    f"nonpos={id}"
}
fn main() {
    let a = pick(5);
    let b = pick(-3);
    println(a);
    println(b);
}
"#,
            &["pos=5", "nonpos=-3"],
            "fstring_explicit_return_no_double_free",
        );
    }

    #[test]
    fn asan_generic_tail_fstring_no_double_free() {
        // A generic (mono) fn whose IMPLICIT TAIL is a bare `f"…"`. The mono
        // path lacked the InterpolatedStringLit-tail cap suppression the
        // non-generic `compile_function` has, so the accumulator buffer was
        // freed between the return-value load and `ret` and the caller's
        // binding then freed the dangling pointer again — a double-free (and a
        // use-after-free when the caller read it). Surfaced via `describe[T:
        // Display](x) { f"..{x}.." }`; the explicit-`return`/`let`-bound forms
        // already had suppression. Covers a `Display` struct arg, a primitive,
        // a String arg, and a no-interp tail — each returned String is read
        // (println) so a UAF trips under ASAN; looped to accumulate any leak.
        assert_clean_asan_run(
            r#"
struct P { x: i64, y: i64 }
impl Display for P { fn to_string(ref self) -> String { f"({self.x}, {self.y})" } }
fn describe[T: Display](item: T) -> String { f"item is {item} padded padded padded" }
fn tag[T](item: T) -> String { f"constant tail padded padded padded" }
fn main() {
    let mut i: i64 = 0i64;
    while i < 2i64 {
        println(describe(P { x: i, y: 2i64 }));
        println(describe(42i64));
        println(describe("hi".to_string()));
        println(tag(7i64));
        i = i + 1i64;
    }
}
"#,
            &[
                "item is (0, 2) padded padded padded",
                "item is 42 padded padded padded",
                "item is hi padded padded padded",
                "constant tail padded padded padded",
                "item is (1, 2) padded padded padded",
                "item is 42 padded padded padded",
                "item is hi padded padded padded",
                "constant tail padded padded padded",
            ],
            "generic_tail_fstring_no_double_free",
        );
    }

    #[test]
    fn asan_println_ref_string_param_over_heap_source() {
        // `s: ref String` parameter, heap-source caller. The
        // identifier `s` inside `show` loads through the ref param
        // and arrives at compile_print as a struct value (per
        // `load_variable`'s ref-deref). Pre-fix the struct-value
        // arm overread; with `%.*s` it reads exactly `len` bytes.
        // This was the canonical failure shape during the
        // C-followup ASAN test development.
        assert_clean_asan_run(
            r#"
fn show(s: ref String) {
    println(s);
}

fn main() {
    let a = "left ";
    let b = "right";
    show(a + b);
}
"#,
            &["left right"],
            "println_ref_string_param_over_heap_source",
        );
    }

    #[test]
    fn asan_vec_get_ref_t_over_heap_source_no_double_free() {
        // `Vec[String].get(i)` types as `Option[ref T]` (B-2026-06-07-5
        // Option[ref T] slice). The `Some(n)` binding is a by-value alias of
        // the element's heap buffer — cleanup is suppressed via
        // `scrutinee_is_borrow_call` so the binding does NOT register a second
        // free against a buffer the Vec still owns. Each element is forced
        // onto the heap (`"al" + "ice"` concat allocates), read through a
        // `ref String` param + `.len()`, in a loop (alloca reuse). The Vec
        // frees each String exactly once at scope exit; a regression where the
        // borrow binding re-frees the aliased buffer surfaces here as ASAN
        // double-free / heap-use-after-free.
        assert_clean_asan_run(
            r#"
fn shout(s: ref String) {
    println(s);
    println(s.len());
}

fn main() {
    let mut names: Vec[String] = Vec.new();
    names.push("al" + "ice");
    names.push("b" + "ob");
    names.push("ca" + "rol");
    let mut i = 0;
    while i < 3 {
        match names.get(i) {
            Some(n) => shout(n),
            None => println("none"),
        };
        i = i + 1;
    };
}
"#,
            &["alice", "5", "bob", "3", "carol", "5"],
            "vec_get_ref_t_over_heap_source_no_double_free",
        );
    }

    #[test]
    fn asan_freshtemp_vec_get_no_double_free() {
        // General owned-temp tracking, slice 3b: `make_vec().get(i)` on a
        // FRESH-TEMP receiver in a loop. Each iteration builds a fresh Vec
        // temp whose heap buffer the `get` borrows read-only; codegen
        // materializes it into a `__vrecv_tmp` slot and frees the buffer once
        // per iteration. A regression that skipped the free leaks (caught by
        // LeakSanitizer on Linux CI); one that freed it twice (or freed a
        // buffer the scalar `Option[ref i64]` result still read) double-frees /
        // UAFs (caught here on macOS too). The loop forces alloca reuse so a
        // per-iteration leak accumulates.
        assert_clean_asan_run(
            r#"
fn ids() -> Vec[i64] {
    let mut v: Vec[i64] = Vec.new();
    v.push(100_i64);
    v.push(200_i64);
    v.push(300_i64);
    return v;
}

fn main() {
    let mut i = 0;
    while i < 3 {
        match ids().get(i) {
            Some(x) => println(x),
            None => println(0_i64),
        };
        i = i + 1;
    };
}
"#,
            &["100", "200", "300"],
            "freshtemp_vec_get_no_double_free",
        );
    }

    #[test]
    fn asan_freshtemp_vec_first_last_contains_no_double_free() {
        // Slice 3b: the remaining element-type-aware read methods on fresh-temp
        // receivers — `first`/`last` (return `Option[ref i64]`) and `contains`
        // (returns `bool`). Each receiver Vec temp's buffer must be freed
        // exactly once after the borrow is read. The `make_vec` body forces a
        // real heap buffer (three `push`es past the inline cap).
        assert_clean_asan_run(
            r#"
fn nums() -> Vec[i64] {
    let mut v: Vec[i64] = Vec.new();
    v.push(7_i64);
    v.push(8_i64);
    v.push(9_i64);
    return v;
}

fn main() {
    match nums().first() {
        Some(x) => println(x),
        None => println(0_i64),
    };
    match nums().last() {
        Some(x) => println(x),
        None => println(0_i64),
    };
    println(nums().contains(8_i64));
    println(nums().contains(42_i64));
}
"#,
            &["7", "9", "true", "false"],
            "freshtemp_vec_first_last_contains_no_double_free",
        );
    }

    #[test]
    fn asan_freshtemp_vec_string_get_no_double_free() {
        // Slice 3b-heap: `make_strvec().get(i)` on a FRESH-TEMP `Vec[String]`
        // receiver in a loop. `get` returns `Option[ref String]` — the payload
        // borrows an element *inside* the temp's buffer, which the
        // `__vrecv_tmp` `FreeVecBuffer` frees at the enclosing frame's exit.
        // Two distinct hazards this gates:
        //   (1) DOUBLE-FREE — the `Some(s)` arm binds `s: ref String`; if it
        //       were dropped independently it would free the same buffer the
        //       per-element `FreeVecBuffer` recursion frees. The match path's
        //       `scrutinee_is_borrow_call` suppression must hold for a temp
        //       receiver (it keys off the method, not the object). Caught here
        //       on macOS.
        //   (2) LEAK — the receiver's three per-element String buffers must be
        //       freed by the vec-struct recursion before the outer buffer; a
        //       regression that frees only the outer buffer leaks all three.
        //       Caught by LeakSanitizer on Linux CI. The strings are ≥36 bytes
        //       so LSan cannot dismiss them as reachable short-strings, and the
        //       loop forces per-iteration accumulation.
        assert_clean_asan_run(
            r#"
fn names() -> Vec[String] {
    let mut v: Vec[String] = Vec.new();
    v.push("alpha element string padded well past thirty-six bytes");
    v.push("beta element string also padded past thirty-six bytes!!");
    v.push("gamma element string likewise padded beyond thirty-six b");
    return v;
}

fn main() {
    let mut i = 0;
    while i < 3 {
        match names().get(i) {
            Some(s) => println(s),
            None => println("none"),
        };
        i = i + 1;
    };
}
"#,
            &[
                "alpha element string padded well past thirty-six bytes",
                "beta element string also padded past thirty-six bytes!!",
                "gamma element string likewise padded beyond thirty-six b",
            ],
            "freshtemp_vec_string_get_no_double_free",
        );
    }

    #[test]
    fn asan_freshtemp_vec_nested_get_no_double_free() {
        // Slice 3e: `make_grid().get(i)` on a fresh-temp `Vec[Vec[i64]]` in a
        // loop. `get` returns `Option[ref Vec[i64]]` — a borrow into an inner row
        // *inside* the temp's outer buffer, which `__vrecv_tmp`'s `FreeVecBuffer`
        // frees at frame exit. Hazards mirror the `Vec[String]` case: (1) the
        // `Some(r)` arm binds a `ref Vec[i64]` that must NOT be independently
        // dropped (`scrutinee_is_borrow_call`), else it double-frees the inner
        // row the vec-struct recursion also frees (macOS ASAN); (2) the inner row
        // data buffers must be per-element freed before the outer buffer, else
        // they leak (Linux LSan). Each row has several elements so its buffer is
        // a real heap allocation; the loop accumulates.
        assert_clean_asan_run(
            r#"
fn make_grid() -> Vec[Vec[i64]] {
    let mut g: Vec[Vec[i64]] = Vec.new();
    let mut a: Vec[i64] = Vec.new();
    a.push(10_i64);
    a.push(11_i64);
    a.push(12_i64);
    a.push(13_i64);
    g.push(a);
    let mut b: Vec[i64] = Vec.new();
    b.push(20_i64);
    b.push(21_i64);
    b.push(22_i64);
    b.push(23_i64);
    g.push(b);
    return g;
}

fn main() {
    let mut i = 0;
    while i < 2 {
        match make_grid().get(i) {
            Some(r) => println(r[1]),
            None => println(0_i64),
        };
        i = i + 1;
    };
}
"#,
            &["11", "21"],
            "freshtemp_vec_nested_get_no_double_free",
        );
    }

    #[test]
    fn asan_freshtemp_vec_struct_get_no_double_free() {
        // Slice 3f: `make_recs().get(i)` on a fresh-temp `Vec[Rec]` (Rec has a
        // String field) in a loop. `get` returns `Option[ref Rec]` borrowing an
        // element inside the temp's buffer, which `__vrecv_tmp`'s `FreeVecBuffer`
        // — now carrying the per-element `__karac_drop_struct_Rec` agg drop —
        // frees at frame exit. Hazards: (1) the `Some(r)` arm binds a `ref Rec`
        // that must NOT be independently dropped (`scrutinee_is_borrow_call`),
        // else it double-frees the element's String field the agg drop also frees
        // (macOS ASAN); (2) each element's String field must be freed by the agg
        // drop before the outer buffer, else they leak (Linux LSan). ≥36-byte
        // String fields defeat LSan short-string reachability; loop accumulates.
        assert_clean_asan_run(
            r#"
struct Rec { name: String, n: i64 }

fn make_recs() -> Vec[Rec] {
    let mut v: Vec[Rec] = Vec.new();
    v.push(Rec { name: "first record name field padded beyond thirty-six bytes", n: 10_i64 });
    v.push(Rec { name: "second record name field padded beyond thirty-six byte", n: 20_i64 });
    v.push(Rec { name: "third record name field padded beyond thirty-six bytess", n: 30_i64 });
    return v;
}

fn main() {
    let mut i = 0;
    while i < 3 {
        match make_recs().get(i) {
            Some(r) => println(r.n),
            None => println(0_i64),
        };
        i = i + 1;
    };
}
"#,
            &["10", "20", "30"],
            "freshtemp_vec_struct_get_no_double_free",
        );
    }

    #[test]
    fn asan_freshtemp_vec_struct_string_field_read_no_double_free() {
        // Slice 3f companion: read the borrowed struct's STRING field through the
        // `Option[ref Rec]` borrow (`r.name`), not just the scalar. This exercises
        // the borrow aliasing the very String buffer the agg drop frees — a
        // tighter check that the borrow is read before the frame-exit free and
        // that the field String is freed exactly once.
        assert_clean_asan_run(
            r#"
struct Rec { name: String, n: i64 }

fn make_recs() -> Vec[Rec] {
    let mut v: Vec[Rec] = Vec.new();
    v.push(Rec { name: "alpha name field padded out beyond thirty-six bytes ok", n: 1_i64 });
    v.push(Rec { name: "beta name field padded out beyond thirty-six bytes okk", n: 2_i64 });
    return v;
}

fn main() {
    let mut i = 0;
    while i < 2 {
        match make_recs().get(i) {
            Some(r) => println(r.name),
            None => println("none"),
        };
        i = i + 1;
    };
}
"#,
            &[
                "alpha name field padded out beyond thirty-six bytes ok",
                "beta name field padded out beyond thirty-six bytes okk",
            ],
            "freshtemp_vec_struct_string_field_read_no_double_free",
        );
    }

    #[test]
    fn asan_for_self_field_vec_iter_no_double_free() {
        // `for s in self.items.iter()` inside an impl method (`ref self`) —
        // the silent-0-iteration miscompile fix. The loop binds each field
        // String element as a BORROW: the enclosing `Counter` owns the field
        // Vec and frees each element String exactly once at the struct's own
        // scope-exit drop, so the loop body must NOT independently free `s`
        // (else double-free on macOS ASAN), and every element String must be
        // freed once by the struct drop (else leak on Linux LSan). Two
        // counters are built and totalled so the field buffers are freed on a
        // real path. ≥36-byte payloads defeat LSan short-string reachability.
        assert_clean_asan_run(
            r#"
struct Counter { items: Vec[String], base: i64 }
impl Counter {
    fn total(ref self) -> i64 {
        let mut t = self.base;
        for s in self.items.iter() { t = t + s.len(); };
        return t;
    }
}
fn make_counter(tag: i64) -> Counter {
    let mut c = Counter { items: Vec.new(), base: tag };
    c.items.push("first field string padded well beyond thirty-six bytes ok");
    c.items.push("second field string padded well beyond thirty-six byte");
    return c;
}
fn main() {
    let a = make_counter(100_i64);
    println(a.total());
    let b = make_counter(200_i64);
    println(b.total());
}
"#,
            &["211", "311"],
            "for_self_field_vec_iter_no_double_free",
        );
    }

    #[test]
    fn asan_for_shared_self_field_vec_iter_no_double_free() {
        // Shared-struct sibling of `asan_for_self_field_vec_iter_no_double_free`.
        // `for s in self.items.iter()` on a `shared struct` `ref self` receiver:
        // the field Vec[String] is owned by the RC struct and freed once when
        // the last handle drops, so the loop's `s` bindings must be borrows (no
        // per-iteration free → no double-free on macOS ASAN; every element freed
        // once by the struct drop → no leak on Linux LSan). ≥36-byte payloads
        // defeat LSan short-string reachability.
        assert_clean_asan_run(
            r#"
shared struct SBag { mut items: Vec[String], base: i64 }
impl SBag {
    fn total(ref self) -> i64 {
        let mut t = self.base;
        for s in self.items.iter() { t = t + s.len(); };
        return t;
    }
}
fn make_bag(tag: i64) -> SBag {
    let b = SBag { items: Vec.new(), base: tag };
    b.items.push("shared field string padded well beyond thirty-six bytes ok");
    b.items.push("another shared field string well beyond thirty-six byte");
    return b;
}
fn main() {
    let a = make_bag(10_i64);
    println(a.total());
    let b = make_bag(20_i64);
    println(b.total());
}
"#,
            &["123", "133"],
            "for_shared_self_field_vec_iter_no_double_free",
        );
    }

    #[test]
    fn asan_freshtemp_vec_enum_get_no_double_free() {
        // Slice 3g: `make_toks().get(i)` on a fresh-temp `Vec[Tok]` where `Tok`
        // is a user enum with a heap-bearing variant (`Word { s: String }`), in
        // a loop. `get` returns `Option[ref Tok]` borrowing an element inside
        // the temp's buffer, which `__vrecv_tmp`'s `FreeVecBuffer` — now carrying
        // the per-element `__karac_drop_Tok` agg drop (slice 3f machinery, enum
        // routed via `emit_enum_drop_switch`) — frees at frame exit. Hazards:
        // (1) the `Some(t)` arm binds a `ref Tok` that must NOT be independently
        // dropped (`scrutinee_is_borrow_call`), else it double-frees the `Word`
        // variant's String the agg drop also frees (macOS ASAN); (2) each
        // element's payload String must be freed by the agg drop before the
        // outer buffer, else it leaks (Linux LSan). The borrow is further matched
        // on its variant and the payload String is read through it (`s.len()`),
        // aliasing the very buffer the agg drop frees. ≥36-byte payloads defeat
        // LSan short-string reachability; loop accumulates.
        assert_clean_asan_run(
            r#"
enum Tok { Word { s: String }, Num { n: i64 } }

fn make_toks() -> Vec[Tok] {
    let mut v: Vec[Tok] = Vec.new();
    v.push(Tok.Word { s: "first token payload string padded beyond thirty-six b" });
    v.push(Tok.Num { n: 20_i64 });
    v.push(Tok.Word { s: "third token payload string padded beyond thirty-six b" });
    return v;
}

fn main() {
    let mut i = 0;
    while i < 3 {
        match make_toks().get(i) {
            Some(t) => match t {
                Word { s } => println(s.len()),
                Num { n } => println(n),
            },
            None => println(0_i64),
        };
        i = i + 1;
    };
}
"#,
            &["53", "20", "53"],
            "freshtemp_vec_enum_get_no_double_free",
        );
    }

    #[test]
    fn asan_freshtemp_vec_iter_string_no_double_free() {
        // Slice 3h: `for s in make_v().iter()` on a fresh-temp `Vec[String]`,
        // looped. The for-loop peels `.iter()` and recurses on the temp receiver;
        // codegen materializes it into a `__for_vec_` synth local whose
        // `FreeVecBuffer` (per-element vec-struct recursion) frees each element
        // String + the outer buffer at scope exit. Pre-fix the body was silently
        // skipped (output 0). Hazards: (1) each iteration binds `s` borrowing an
        // element inside the temp's buffer that must NOT be independently dropped,
        // else it double-frees the element String the buffer drop also frees
        // (macOS ASAN); (2) every element String must be freed by the per-element
        // drop before the buffer, else they leak (Linux LSan). ≥36-byte strings
        // defeat LSan short-string reachability; outer loop re-materializes the
        // temp each pass to accumulate any per-pass leak.
        assert_clean_asan_run(
            r#"
fn make_v() -> Vec[String] {
    let mut v: Vec[String] = Vec.new();
    v.push("first iter string padded out beyond thirty-six bytes");
    v.push("second iter string padded out beyond thirty-six byte");
    return v;
}

fn main() {
    let mut pass = 0;
    while pass < 3 {
        let mut total = 0_i64;
        for s in make_v().iter() {
            total = total + s.len();
        };
        println(total);
        pass = pass + 1;
    };
}
"#,
            &["104", "104", "104"],
            "freshtemp_vec_iter_string_no_double_free",
        );
    }

    #[test]
    fn asan_freshtemp_vec_into_iter_struct_no_double_free() {
        // Slice 3h companion: `for r in make_recs().into_iter()` on a fresh-temp
        // `Vec[Rec]` (Rec has a String field), reading the heap field through the
        // bound element (`r.name.len()`). Exercises the agg-drop threading
        // (`track_vec_of_aggs_var` → `__karac_drop_struct_Rec`) on the
        // materialized iter temp: each element's String field must be freed once
        // by the per-element drop before the buffer. `.into_iter()` rides the same
        // materialize path as `.iter()` here. Loops to accumulate any leak.
        assert_clean_asan_run(
            r#"
struct Rec { name: String, n: i64 }

fn make_recs() -> Vec[Rec] {
    let mut v: Vec[Rec] = Vec.new();
    v.push(Rec { name: "alpha rec name padded out beyond thirty-six bytes ok", n: 1_i64 });
    v.push(Rec { name: "beta rec name padded out beyond thirty-six bytes okk", n: 2_i64 });
    return v;
}

fn main() {
    let mut pass = 0;
    while pass < 3 {
        let mut total = 0_i64;
        for r in make_recs().into_iter() {
            total = total + r.n + r.name.len();
        };
        println(total);
        pass = pass + 1;
    };
}
"#,
            &["107", "107", "107"],
            "freshtemp_vec_into_iter_struct_no_double_free",
        );
    }

    #[test]
    fn asan_freshtemp_map_iter_string_key_no_double_free() {
        // Slice 3i: `for (k, v) in make_map().iter()` on a fresh-temp
        // `Map[String, i64]`, looped. The for-loop peels `.iter()` and recurses on
        // the temp receiver; codegen materializes the handle into a
        // `__for_mapset_` synth local whose `FreeMapHandle`
        // (`karac_map_free_with_drop_vec`) frees the handle + each stored String
        // key at scope exit. Pre-fix the body was silently skipped (output 0).
        // Hazards: (1) each iteration's `k` String struct points into the map's
        // storage (a borrow) and must NOT be independently dropped, else it
        // double-frees the key the handle drop also frees (macOS ASAN); (2) every
        // stored String key must be freed by the per-entry drop, else they leak
        // (Linux LSan). ≥36-byte keys defeat LSan short-string reachability; outer
        // loop re-materializes the temp each pass.
        assert_clean_asan_run(
            r#"
fn make_map() -> Map[String, i64] {
    let mut m: Map[String, i64] = Map.new();
    m.insert("first map key padded out beyond thirty-six bytes ok", 10_i64);
    m.insert("second map key padded out beyond thirty-six bytes o", 20_i64);
    return m;
}

fn main() {
    let mut pass = 0;
    while pass < 3 {
        let mut total = 0_i64;
        for (k, v) in make_map().iter() {
            total = total + v + k.len();
        };
        println(total);
        pass = pass + 1;
    };
}
"#,
            &["132", "132", "132"],
            "freshtemp_map_iter_string_key_no_double_free",
        );
    }

    #[test]
    fn asan_freshtemp_set_bare_string_no_double_free() {
        // Slice 3i companion: `for x in make_set()` (bare, no `.iter()`) on a
        // fresh-temp `Set[String]`. The bare form reaches the materialize path via
        // `owned_temp_drops` (Set is droppable) rather than
        // `temp_recv_mapset_types`; same `__for_mapset_` synth local + handle drop.
        // Each iteration's `x` String borrows the set's storage (must not be
        // independently dropped → macOS ASAN), and every stored element must be
        // freed by the per-entry drop (→ Linux LSan). Loops to accumulate leaks.
        assert_clean_asan_run(
            r#"
fn make_set() -> Set[String] {
    let mut s: Set[String] = Set.new();
    s.insert("first set element padded out beyond thirty-six byte");
    s.insert("second set element padded out beyond thirty-six byt");
    return s;
}

fn main() {
    let mut pass = 0;
    while pass < 3 {
        let mut total = 0_i64;
        for x in make_set() {
            total = total + x.len();
        };
        println(total);
        pass = pass + 1;
    };
}
"#,
            &["102", "102", "102"],
            "freshtemp_set_bare_string_no_double_free",
        );
    }

    #[test]
    fn asan_freshtemp_user_method_ref_self_no_double_free() {
        // Slice 3j: a `ref self` user method on a fresh-temp struct receiver
        // (`make_counter().m_get()`), looped. The struct owns a `Vec[String]`
        // field; the temp materializes into `__urecv_tmp` and — because `self` is
        // borrowed — is drop-tracked so its field Vec + Strings free once via
        // `__karac_drop_struct_Counter` at scope exit. The method reads a field
        // String through `self.items.get(0)` (an `Option[ref String]` borrow, not
        // consumed). Hazards: (1) the borrowed temp must be freed exactly once —
        // the method borrows, so the caller owns it (macOS ASAN would catch a
        // double-free against any spurious second drop); (2) the field Strings
        // must be freed by the struct drop, else they leak (Linux LSan). ≥36-byte
        // field strings defeat LSan short-string reachability; the outer loop
        // re-materializes the temp each pass.
        assert_clean_asan_run(
            r#"
struct Counter { items: Vec[String], base: i64 }
impl Counter {
    fn m_get(ref self) -> i64 {
        match self.items.get(0) {
            Some(s) => return self.base + s.len(),
            None => return self.base,
        };
    }
}
fn make_counter() -> Counter {
    let mut c = Counter { items: Vec.new(), base: 100_i64 };
    c.items.push("first field string padded beyond thirty-six bytes ok");
    c.items.push("second field string padded beyond thirty-six byte");
    return c;
}
fn main() {
    let mut p = 0;
    while p < 3 {
        println(make_counter().m_get());
        p = p + 1;
    };
}
"#,
            &["152", "152", "152"],
            "freshtemp_user_method_ref_self_no_double_free",
        );
    }

    #[test]
    fn asan_freshtemp_user_method_owned_self_no_double_free() {
        // Slice 3j companion: an OWNED-`self` user method on a fresh-temp struct
        // receiver (`make_counter().consume()`), looped. Owned `self` moves the
        // receiver into the method, which drops its `Vec[String]` field at method
        // scope exit — so the fresh-temp path must NOT drop-track the caller's
        // shallow copy, else the field Vec + Strings are freed twice (macOS ASAN).
        // Conversely the method's own drop must fire, else they leak (Linux LSan).
        // The looped re-materialization accumulates either fault.
        assert_clean_asan_run(
            r#"
struct Counter { items: Vec[String], base: i64 }
impl Counter {
    fn consume(self) -> i64 { return self.base + self.items.len(); }
}
fn make_counter() -> Counter {
    let mut c = Counter { items: Vec.new(), base: 100_i64 };
    c.items.push("first field string padded beyond thirty-six bytes ok");
    c.items.push("second field string padded beyond thirty-six byte");
    return c;
}
fn main() {
    let mut p = 0;
    while p < 3 {
        println(make_counter().consume());
        p = p + 1;
    };
}
"#,
            &["102", "102", "102"],
            "freshtemp_user_method_owned_self_no_double_free",
        );
    }

    #[test]
    fn asan_freshtemp_enum_method_no_double_free() {
        // Slice 3k: a user method on a fresh-temp VALUE-ENUM receiver
        // (`make().size()`), looped. The `Text` variant owns a heap `String`; the
        // temp materializes into `__urecv_tmp` and is drop-tracked via
        // `track_enum_var`, whose scope-exit `EnumDrop` runs `__karac_drop_Msg` to
        // free the payload String once. Hazards: the temp must free exactly once
        // (macOS ASAN catches a double-free), and the payload String must free at
        // all (Linux LSan catches a leak). ≥36-byte payload defeats LSan
        // short-string reachability; the loop re-materializes each pass.
        assert_clean_asan_run(
            r#"
enum Msg { Text(String), Empty }
impl Msg {
    fn size(self) -> i64 {
        match self {
            Msg.Text(s) => return s.len(),
            Msg.Empty => return 0_i64,
        };
    }
}
fn make() -> Msg { Msg.Text("a message payload string padded beyond thirty-six bytes") }
fn main() {
    let mut p = 0;
    while p < 3 {
        println(make().size());
        p = p + 1;
    };
}
"#,
            &["55", "55", "55"],
            "freshtemp_enum_method_no_double_free",
        );
    }

    #[test]
    fn asan_freshtemp_shared_struct_method_no_double_free() {
        // Slice 3k: a user method on a fresh-temp SHARED-STRUCT receiver
        // (`make().count()`), looped. `make()` returns an RC box at rc==1 owning a
        // `Vec[String]` field; the temp materializes into `__urecv_tmp` and is
        // drop-tracked as ONE scope-exit `RcDec` (`track_rc_var`) — the method
        // borrows / shallow-copies `self`, net-zero on the count, so this single
        // dec drives rc→0 and `__karac_rc_drop_Bag` frees the box + both field
        // Strings. A spurious second dec would free-at-rc==0 twice (macOS ASAN);
        // no dec leaks the whole box (Linux LSan). ≥36-byte field strings + the
        // loop expose either fault.
        assert_clean_asan_run(
            r#"
shared struct Bag { items: Vec[String] }
impl Bag { fn count(self) -> i64 { self.items.len() } }
fn make() -> Bag {
    let mut v: Vec[String] = Vec.new();
    v.push("first field string padded beyond thirty-six bytes ok");
    v.push("second field string padded beyond thirty-six byte");
    Bag { items: v }
}
fn main() {
    let mut p = 0;
    while p < 3 {
        println(make().count());
        p = p + 1;
    };
}
"#,
            &["2", "2", "2"],
            "freshtemp_shared_struct_method_no_double_free",
        );
    }

    #[test]
    fn asan_freshtemp_shared_enum_method_no_double_free() {
        // Slice 3k: the shared-ENUM sibling. A `shared enum` receiver is `Shared`
        // and RC-managed, so it rides the same `track_rc_var` path as the shared
        // struct (`track_enum_var` no-ops for shared enums — DP3). The temp
        // materializes into `__urecv_tmp` and one scope-exit `RcDec` →
        // `__karac_rc_drop_Expr` frees the box and the live `Name` payload String.
        // Same double-free (ASAN) / leak (LSan) hazards as the shared-struct case;
        // guards that the shared-enum branch isn't mis-routed to the value-enum
        // drop (which would double-count).
        assert_clean_asan_run(
            r#"
shared enum Expr { Lit(i64), Name(String) }
impl Expr {
    fn weight(self) -> i64 {
        match self {
            Expr.Lit(n) => return n,
            Expr.Name(s) => return s.len(),
        };
    }
}
fn make() -> Expr { Expr.Name("an expr name payload padded beyond thirty-six bytes") }
fn main() {
    let mut p = 0;
    while p < 3 {
        println(make().weight());
        p = p + 1;
    };
}
"#,
            &["51", "51", "51"],
            "freshtemp_shared_enum_method_no_double_free",
        );
    }

    #[test]
    fn asan_freshtemp_vec_string_first_last_no_double_free() {
        // Slice 3b-heap companion: `first`/`last` on a fresh-temp `Vec[String]`
        // — the other two borrow-returning (`Option[ref String]`) read methods
        // routed through `scrutinee_is_borrow_call`. Same single-free / borrow-
        // not-dropped obligation as `get`; verifies the method set, not just
        // `get`. (`contains` on a String temp is covered separately below;
        // `get_unchecked` — bare `ref String` via a builtin-method let-binding
        // suppression path that doesn't fire, plus an `unsafe` block — stays
        // scalar-only as a follow-on.)
        assert_clean_asan_run(
            r#"
fn names() -> Vec[String] {
    let mut v: Vec[String] = Vec.new();
    v.push("first padded element string beyond thirty-six bytes ok");
    v.push("middle padded element string beyond thirty-six bytes k");
    v.push("last padded element string beyond thirty-six bytes okk");
    return v;
}

fn main() {
    match names().first() {
        Some(s) => println(s),
        None => println("none"),
    };
    match names().last() {
        Some(s) => println(s),
        None => println("none"),
    };
}
"#,
            &[
                "first padded element string beyond thirty-six bytes ok",
                "last padded element string beyond thirty-six bytes okk",
            ],
            "freshtemp_vec_string_first_last_no_double_free",
        );
    }

    #[test]
    fn asan_freshtemp_vec_string_contains_no_double_free() {
        // Slice 3b-heap follow-on: `contains` on a fresh-temp `Vec[String]` in
        // a loop. Unlike `get`/`first`/`last`, `contains` returns `bool` — no
        // borrow escapes, so there is no arm-binding to suppress; the only
        // obligation is that the receiver temp's per-element String buffers AND
        // outer buffer are freed once per iteration (the `FreeVecBuffer`
        // vec-struct recursion). A regression that frees only the outer buffer
        // leaks the three element Strings (LeakSanitizer on Linux CI); the
        // compared arg is a static literal (`cap = 0`), so it is not part of the
        // free accounting. ≥36-byte elements defeat LSan short-string
        // reachability; the loop forces per-iteration accumulation.
        assert_clean_asan_run(
            r#"
fn names() -> Vec[String] {
    let mut v: Vec[String] = Vec.new();
    v.push("alpha element string padded well past thirty-six bytes");
    v.push("beta element string also padded past thirty-six bytes!!");
    v.push("gamma element string likewise padded beyond thirty-six b");
    return v;
}

fn main() {
    let mut i = 0;
    while i < 3 {
        println(names().contains("beta element string also padded past thirty-six bytes!!"));
        i = i + 1;
    };
}
"#,
            &["true", "true", "true"],
            "freshtemp_vec_string_contains_no_double_free",
        );
    }

    #[test]
    fn asan_freshtemp_map_get_no_double_free() {
        // Slice 3d: `make_map().get(k)` on a fresh-temp `Map[i64,i64]` receiver
        // in a loop. `get` returns `Option[ref V]` borrowing a value slot inside
        // the map handle, which the `__mrecv_tmp` `FreeMapHandle` frees at frame
        // exit. Two hazards: (1) DOUBLE-FREE — the `Some(v)` arm binds a borrow
        // (`scrutinee_is_borrow_call` must suppress its independent drop for a
        // temp receiver); (2) LEAK — the whole map handle must be freed once per
        // iteration. macOS ASAN catches (1); Linux LSan catches (2). The loop
        // forces per-iteration accumulation.
        assert_clean_asan_run(
            r#"
fn make_map() -> Map[i64, i64] {
    let mut m: Map[i64, i64] = Map.new();
    m.insert(1_i64, 100_i64);
    m.insert(2_i64, 200_i64);
    m.insert(3_i64, 300_i64);
    return m;
}

fn main() {
    let mut i = 1;
    while i < 4 {
        match make_map().get(i) {
            Some(v) => println(v),
            None => println(0_i64),
        };
        i = i + 1;
    };
}
"#,
            &["100", "200", "300"],
            "freshtemp_map_get_no_double_free",
        );
    }

    #[test]
    fn asan_letbound_map_get_moveout_no_double_free() {
        // B-2026-07-09-13: `let g = m.get(k); match g { Some(v) => <move v> }`.
        // `Map.get` returns an `Option[V]` whose String payload ALIASES the
        // bucket's stored value; moving `v` out and dropping it (here via
        // `println` consuming the returned String) freed that buffer a second
        // time against the Map's own value drop (`karac_map_free_with_drop_vec`)
        // — a double-free the DIRECT `match m.get(k)` form was already protected
        // from, but the intermediate `let g` binding hid the alias property.
        // `borrow_accessor_let_payload` re-admits the binding into the
        // clone-on-escape protection so the escaping payload is an independent
        // buffer. Covers the bare-String value, a `#[derive(Hash)]`-struct KEY
        // (the shape that first surfaced the glibc tcache abort), and the
        // `if let` sibling; looped so any per-iteration imbalance accumulates.
        assert_clean_asan_run(
            r#"
#[derive(Hash, Eq, PartialEq)]
struct P { x: i64, y: i64 }
fn main() {
    let mut i: i64 = 0i64;
    while i < 2i64 {
        let mut m: Map[i64, String] = Map.new();
        m.insert(1i64, f"val-{i}-padding-padding-padding");
        m.insert(2i64, f"other-{i}-padding-padding-padding");
        let g = m.get(1i64);
        match g {
            Some(v) => println(v),
            None => println("miss"),
        };
        let mut sm: Map[P, String] = Map.new();
        sm.insert(P { x: 1i64, y: 2i64 }, f"struct-{i}-padding-padding-padding");
        sm.insert(P { x: 3i64, y: 4i64 }, f"skey-{i}-padding-padding-padding");
        let sg = sm.get(P { x: 1i64, y: 2i64 });
        if let Some(v) = sg {
            println(v);
        };
        i = i + 1i64;
    }
}
"#,
            &[
                "val-0-padding-padding-padding",
                "struct-0-padding-padding-padding",
                "val-1-padding-padding-padding",
                "struct-1-padding-padding-padding",
            ],
            "letbound_map_get_moveout_no_double_free",
        );
    }

    #[test]
    fn asan_freshtemp_map_contains_key_set_contains_no_double_free() {
        // Slice 3d: the `bool`-returning reads — `Map.contains_key` and
        // `Set.contains` — on fresh-temp receivers. No borrow escapes, so the
        // sole obligation is freeing the handle once per call. A `FreeMapHandle`
        // that double-freed would crash here (macOS ASAN); a missing one leaks
        // the handle (Linux LSan).
        assert_clean_asan_run(
            r#"
fn make_map() -> Map[i64, i64] {
    let mut m: Map[i64, i64] = Map.new();
    m.insert(5_i64, 50_i64);
    return m;
}

fn make_set() -> Set[i64] {
    let mut s: Set[i64] = Set.new();
    s.insert(7_i64);
    s.insert(9_i64);
    return s;
}

fn main() {
    let mut i = 0;
    while i < 2 {
        println(make_map().contains_key(5_i64));
        println(make_set().contains(7_i64));
        println(make_set().contains(42_i64));
        i = i + 1;
    };
}
"#,
            &["true", "true", "false", "true", "true", "false"],
            "freshtemp_map_contains_key_set_contains_no_double_free",
        );
    }

    #[test]
    fn asan_freshtemp_map_string_key_no_double_free() {
        // Slice 3d-heap: `make_map().get(k)` / `.contains_key(k)` on a fresh-temp
        // `Map[String, i64]` (heap KEY). The handle drop must per-entry free each
        // key String (`karac_map_free_with_drop_vec`) before the handle. The
        // value is scalar (`Option[ref i64]` — no value borrow concern); the
        // looked-up key arg is a static literal. Leak (entry keys) caught by
        // Linux LSan; a double-free of the handle/keys by macOS ASAN. ≥36-byte
        // keys defeat LSan short-string reachability; loop accumulates.
        assert_clean_asan_run(
            r#"
fn kmap() -> Map[String, i64] {
    let mut m: Map[String, i64] = Map.new();
    m.insert("alpha key padded out well beyond thirty-six bytes ok", 11_i64);
    m.insert("beta key padded out well beyond thirty-six bytes okk", 22_i64);
    return m;
}

fn main() {
    let mut i = 0;
    while i < 3 {
        match kmap().get("beta key padded out well beyond thirty-six bytes okk") {
            Some(v) => println(v),
            None => println(0_i64),
        };
        println(kmap().contains_key("alpha key padded out well beyond thirty-six bytes ok"));
        i = i + 1;
    };
}
"#,
            &["22", "true", "22", "true", "22", "true"],
            "freshtemp_map_string_key_no_double_free",
        );
    }

    #[test]
    fn asan_freshtemp_map_string_value_no_double_free() {
        // Slice 3d-heap, the riskiest case: `make_map().get(k)` on a fresh-temp
        // `Map[i64, String]` (heap VALUE). `get` returns `Option[ref String]`
        // borrowing a value String *inside* the handle, which the
        // `karac_map_free_with_drop_vec` per-entry drop frees at frame exit. The
        // `Some(s)` arm binds a `ref String` that must NOT be dropped
        // independently (`scrutinee_is_borrow_call`) — otherwise it double-frees
        // the entry String the handle drop also frees (macOS ASAN). A handle drop
        // that skipped the per-entry free leaks every value String (Linux LSan).
        assert_clean_asan_run(
            r#"
fn vmap() -> Map[i64, String] {
    let mut m: Map[i64, String] = Map.new();
    m.insert(1_i64, "first value string padded out beyond thirty-six bytes");
    m.insert(2_i64, "second value string padded out beyond thirty-six byte");
    m.insert(3_i64, "third value string padded out beyond thirty-six bytess");
    return m;
}

fn main() {
    let mut i = 1;
    while i < 4 {
        match vmap().get(i) {
            Some(s) => println(s),
            None => println("none"),
        };
        i = i + 1;
    };
}
"#,
            &[
                "first value string padded out beyond thirty-six bytes",
                "second value string padded out beyond thirty-six byte",
                "third value string padded out beyond thirty-six bytess",
            ],
            "freshtemp_map_string_value_no_double_free",
        );
    }

    #[test]
    fn asan_freshtemp_map_keys_values_scalar_no_double_free() {
        // Slice 3l: `make_map().keys()` / `.values()` on a fresh-temp
        // `Map[i64,i64]`, looped. `.keys()`/`.values()` materialize a fresh
        // `Vec[i64]`, but the MAP receiver is a fresh owned temp — the fresh-temp
        // Map path materializes it into `__mrecv_tmp` and frees the handle once
        // (`karac_map_free`) at frame exit. The returned Vec is owned by the
        // binding / for-loop. Scalar K/V → no per-entry heap. A leaked handle
        // (Linux LSan) or a double-freed handle (macOS ASAN) is the hazard; the
        // loop re-materializes each pass.
        assert_clean_asan_run(
            r#"
fn make_map() -> Map[i64, i64] {
    let mut m: Map[i64, i64] = Map.new();
    m.insert(1_i64, 100_i64);
    m.insert(2_i64, 200_i64);
    return m;
}
fn main() {
    let mut i = 0;
    while i < 3 {
        let ks: Vec[i64] = make_map().keys();
        println(ks.len());
        let mut s = 0;
        for v in make_map().values() { s = s + v; }
        println(s);
        i = i + 1;
    };
}
"#,
            &["2", "300", "2", "300", "2", "300"],
            "freshtemp_map_keys_values_scalar_no_double_free",
        );
    }

    #[test]
    fn asan_freshtemp_map_keys_string_key_no_double_free() {
        // Slice 3l-heap: `make_map().keys()` on a fresh-temp `Map[String, i64]`
        // (heap KEY), iterated, looped. Two independent heap owners: (1) the map
        // handle, whose per-entry drop (`karac_map_free_with_drop_vec`) frees each
        // stored key String; (2) the returned `Vec[String]`, into which `.keys()`
        // CLONED each key — freed by the for-loop's Vec drop. Both must free
        // exactly once: a double-free (aliased clone-vs-stored key) is caught by
        // macOS ASAN, a leak of either by Linux LSan. ≥36-byte keys defeat LSan
        // short-string reachability; the loop accumulates.
        assert_clean_asan_run(
            r#"
fn kmap() -> Map[String, i64] {
    let mut m: Map[String, i64] = Map.new();
    m.insert("alpha key padded out well beyond thirty-six bytes ok", 11_i64);
    m.insert("beta key padded out well beyond thirty-six bytes okk", 22_i64);
    return m;
}
fn main() {
    let mut i = 0;
    while i < 3 {
        let mut s = 0;
        for k in kmap().keys() { s = s + k.len(); }
        println(s);
        i = i + 1;
    };
}
"#,
            &["104", "104", "104"],
            "freshtemp_map_keys_string_key_no_double_free",
        );
    }

    #[test]
    fn asan_freshtemp_map_values_string_value_no_double_free() {
        // Slice 3l-heap sibling: `make_map().values()` on a fresh-temp
        // `Map[i64, String]` (heap VALUE), looped. Same two-owner shape — the
        // handle's per-entry drop frees the stored value Strings, and the returned
        // `Vec[String]` (cloned values) frees its own. Guards the same
        // double-free / leak hazards for the value side.
        assert_clean_asan_run(
            r#"
fn vmap() -> Map[i64, String] {
    let mut m: Map[i64, String] = Map.new();
    m.insert(1_i64, "alpha value padded out beyond thirty-six bytes okay");
    m.insert(2_i64, "beta value padded out beyond thirty-six bytes okayy");
    return m;
}
fn main() {
    let mut i = 0;
    while i < 3 {
        let vs: Vec[String] = vmap().values();
        println(vs.len());
        i = i + 1;
    };
}
"#,
            &["2", "2", "2"],
            "freshtemp_map_values_string_value_no_double_free",
        );
    }

    #[test]
    fn asan_freshtemp_map_entries_scalar_no_double_free() {
        // Slice 3m: `make_map().entries()` on a fresh-temp `Map[i64,i64]`, looped.
        // `.entries()` materializes a fresh `Vec[(i64,i64)]`; the MAP receiver is a
        // fresh owned temp freed once (`karac_map_free`) at frame exit. Scalar K/V
        // → no per-entry heap. A leaked handle (Linux LSan) or a double-freed
        // handle (macOS ASAN) is the hazard; the loop re-materializes each pass.
        assert_clean_asan_run(
            r#"
fn make_map() -> Map[i64, i64] {
    let mut m: Map[i64, i64] = Map.new();
    m.insert(1_i64, 100_i64);
    m.insert(2_i64, 200_i64);
    return m;
}
fn main() {
    let mut i = 0;
    while i < 3 {
        let es: Vec[(i64, i64)] = make_map().entries();
        println(es.len());
        i = i + 1;
    };
}
"#,
            &["2", "2", "2"],
            "freshtemp_map_entries_scalar_no_double_free",
        );
    }

    #[test]
    fn asan_freshtemp_map_entries_string_key_no_double_free() {
        // Slice 3m-heap: `make_map().entries()` on a fresh-temp `Map[String, i64]`
        // (heap KEY), iterated, looped. Two independent heap owners: (1) the map
        // handle, whose per-entry drop (`karac_map_free_with_drop_vec`) frees each
        // stored key String; (2) the returned `Vec[(String,i64)]`, into which
        // `.entries()` CLONED each pair — freed by the for-loop's tuple-Vec drop.
        // Both free exactly once: a double-free (aliased clone-vs-stored key) is
        // caught by macOS ASAN, a leak of either by Linux LSan. ≥36-byte keys
        // defeat LSan short-string reachability; the loop accumulates.
        assert_clean_asan_run(
            r#"
fn kmap() -> Map[String, i64] {
    let mut m: Map[String, i64] = Map.new();
    m.insert("alpha key padded out well beyond thirty-six bytes ok", 11_i64);
    m.insert("beta key padded out well beyond thirty-six bytes okk", 22_i64);
    return m;
}
fn main() {
    let mut i = 0;
    while i < 3 {
        let mut s = 0;
        for pair in kmap().entries() { s = s + pair.0.len() + pair.1; }
        println(s);
        i = i + 1;
    };
}
"#,
            &["137", "137", "137"],
            "freshtemp_map_entries_string_key_no_double_free",
        );
    }

    #[test]
    fn asan_freshtemp_map_entries_string_value_no_double_free() {
        // Slice 3m-heap sibling: `make_map().entries()` on a fresh-temp
        // `Map[i64, String]` (heap VALUE), iterated, looped. Same two-owner shape
        // — the handle's per-entry drop frees the stored value Strings, and the
        // returned `Vec[(i64,String)]` (cloned pairs) frees its own tuple elements
        // via the SAME machinery the named-map entries path uses. Guards the same
        // double-free / leak hazards on the value side.
        assert_clean_asan_run(
            r#"
fn vmap() -> Map[i64, String] {
    let mut m: Map[i64, String] = Map.new();
    m.insert(1_i64, "alpha value padded out beyond thirty-six bytes okay");
    m.insert(2_i64, "beta value padded out beyond thirty-six bytes okayy");
    return m;
}
fn main() {
    let mut i = 0;
    while i < 3 {
        let mut s = 0;
        for pair in vmap().entries() { s = s + pair.0 + pair.1.len(); }
        println(s);
        i = i + 1;
    };
}
"#,
            &["105", "105", "105"],
            "freshtemp_map_entries_string_value_no_double_free",
        );
    }

    // ── B-2026-06-10-6: inline-heap `Option[T]` payload drop ──────
    //
    // An `Option[String]` / `Option[Vec[_]]` dropped WITHOUT being
    // destructured leaks its inline heap payload — the type-erased `Option`
    // layout's drop switch can't free a payload that's a buffer for
    // `Option[String]` but a scalar for `Option[i64]`, so a concrete-typed
    // `FreeInlineOptionPayload` is registered at the binding / discard site.
    // On Linux these run under LeakSanitizer (the leak itself is caught); on
    // macOS LSan is off, so these primarily guard the DOUBLE-FREE risk — a
    // `match`/`if let` arm binds the payload (its own cleanup frees it) AND
    // the source `Option`'s scope-exit free must be suppressed (source `cap`
    // zeroed), else the buffer is freed twice. Runtime/non-foldable payloads
    // (`f"..{n}.."`) so the heap allocation actually happens (a constant
    // concat folds to a static string and hides the path).

    #[test]
    fn asan_option_string_let_unused_freed() {
        // `let x = mk(42)` never destructured → the scope-exit
        // FreeInlineOptionPayload must free the `Some` String. (Linux LSan
        // catches the leak; macOS confirms no spurious double-free.)
        assert_clean_asan_run(
            r#"
fn mk(n: i64) -> Option[String] { Some(f"value-{n}-runtime-heap") }
fn main() {
    let x = mk(42);
    println("done");
}
"#,
            &["done"],
            "option_string_let_unused_freed",
        );
    }

    #[test]
    fn asan_option_string_let_then_match_no_double_free() {
        // `let x = mk(); match x { Some(s) => ... }`: the arm binding `s`
        // frees the payload; the source `Option`'s scope-exit free must be
        // suppressed (cap zeroed) or this double-frees the same buffer.
        assert_clean_asan_run(
            r#"
fn mk(n: i64) -> Option[String] { Some(f"value-{n}-runtime-heap") }
fn main() {
    let x = mk(42);
    match x {
        Some(s) => { println(s); }
        None => { println("none"); }
    };
}
"#,
            &["value-42-runtime-heap"],
            "option_string_let_then_match_no_double_free",
        );
    }

    #[test]
    fn asan_option_string_let_then_if_let_no_double_free() {
        // `if let Some(s) = x` companion to the match double-free guard.
        assert_clean_asan_run(
            r#"
fn mk(n: i64) -> Option[String] { Some(f"v-{n}-runtime-heap-payload") }
fn main() {
    let x = mk(7);
    if let Some(s) = x {
        println(s);
    } else {
        println("none");
    }
}
"#,
            &["v-7-runtime-heap-payload"],
            "option_string_let_then_if_let_no_double_free",
        );
    }

    #[test]
    fn asan_option_string_discarded_freed() {
        // Discarded `mk();` statement temp — no binding, unconditional free.
        assert_clean_asan_run(
            r#"
fn mk(n: i64) -> Option[String] { Some(f"discarded-{n}-runtime-heap") }
fn main() {
    mk(3);
    println("done");
}
"#,
            &["done"],
            "option_string_discarded_freed",
        );
    }

    #[test]
    fn asan_option_pop_discarded_freed() {
        // `v.pop();` discards an `Option[String]` temp (the popped element).
        // Borrow accessors (`get`) are excluded; `pop` owns its result.
        assert_clean_asan_run(
            r#"
fn main() {
    let mut v: Vec[String] = Vec.new();
    v.push("he" + "llo");
    v.push("wor" + "ld");
    v.pop();
    println(v[0]);
}
"#,
            &["hello"],
            "option_pop_discarded_freed",
        );
    }

    #[test]
    fn asan_option_vec_let_unused_freed() {
        // `Option[Vec[i64]]` let-unused: the payload Vec's element buffer
        // must be freed by the scope-exit FreeInlineOptionPayload.
        assert_clean_asan_run(
            r#"
fn mk() -> Option[Vec[i64]] {
    let mut v: Vec[i64] = Vec.new();
    v.push(1); v.push(2); v.push(3);
    Some(v)
}
fn main() {
    let x = mk();
    println("done");
}
"#,
            &["done"],
            "option_vec_let_unused_freed",
        );
    }

    #[test]
    fn asan_option_string_some_wildcard_arm_freed() {
        // `Some(_)` binds nothing, so the source free must STILL fire (the
        // payload isn't moved out) — and exactly once (no double-free).
        assert_clean_asan_run(
            r#"
fn mk(n: i64) -> Option[String] { Some(f"wild-{n}-runtime-heap") }
fn main() {
    let x = mk(9);
    match x {
        Some(_) => { println("some"); }
        None => { println("none"); }
    };
}
"#,
            &["some"],
            "option_string_some_wildcard_arm_freed",
        );
    }

    // ── B-2026-06-10-6 follow-ons: Result / Option[Map] / non-Call RHS ──
    // The Option-core fix's three open follow-ons (each a leak on Linux LSan,
    // a no-double-free guard on macOS): `Result[T,E]` inline Ok/Err payloads,
    // `Option[Map]`/`Option[Set]` inline handle payloads, and non-`Call`
    // let-RHS (`if`/`match`/block yielding a fresh inline Option/Result).

    #[test]
    fn asan_result_ok_string_undestructured_freed() {
        // `Result[String, i64]` dropped without destructuring → the
        // scope-exit `FreeInlineResultPayload` frees the `Ok` String.
        assert_clean_asan_run(
            r#"
fn mk(n: i64) -> Result[String, i64] { Ok(f"ok-value-{n}-runtime-heap") }
fn main() {
    let x = mk(42);
    println("done");
}
"#,
            &["done"],
            "result_ok_string_undestructured_freed",
        );
    }

    #[test]
    fn asan_result_err_string_undestructured_freed() {
        // `Result[i64, String]` — the heap is on the `Err` side; the cleanup
        // reads the tag and frees the `Err` overlay.
        assert_clean_asan_run(
            r#"
fn mk(bad: bool) -> Result[i64, String] {
    if bad { Err(f"err-value-runtime-heap") } else { Ok(7i64) }
}
fn main() {
    let x = mk(true);
    println("done");
}
"#,
            &["done"],
            "result_err_string_undestructured_freed",
        );
    }

    #[test]
    fn asan_result_consumed_match_no_double_free() {
        // `match r { Ok(v) => ... }` binds the payload out; the source
        // `Result`'s scope-exit free must be suppressed (cap zeroed on the
        // taken arm) or this double-frees the same buffer on macOS.
        assert_clean_asan_run(
            r#"
fn mk() -> Result[String, i64] { Ok(f"consumed-ok-runtime-heap") }
fn main() {
    let r = mk();
    match r {
        Ok(v) => { println(v); }
        Err(_e) => { println("err"); }
    };
}
"#,
            &["consumed-ok-runtime-heap"],
            "result_consumed_match_no_double_free",
        );
    }

    #[test]
    fn asan_option_map_undestructured_freed() {
        // `Option[Map[i64,i64]]` dropped without destructuring → the
        // scope-exit `FreeInlineOptionMapPayload` frees the `Some` handle
        // (and its bucket storage) via `emit_free_one_map_handle`.
        assert_clean_asan_run(
            r#"
fn mk() -> Option[Map[i64, i64]] {
    let mut m: Map[i64, i64] = Map.new();
    m.insert(1i64, 10i64);
    Some(m)
}
fn main() {
    let om = mk();
    println("done");
}
"#,
            &["done"],
            "option_map_undestructured_freed",
        );
    }

    #[test]
    fn asan_option_if_else_fresh_payload_freed() {
        // Non-`Call` let-RHS: `let x = if c { Some(a) } else { None };`
        // yields a FRESH inline Option — the let-path registration is
        // broadened past `Call` to provably-fresh if/match/block tails
        // (`rhs_is_fresh_inline_enum`). The `Some` String must be freed.
        assert_clean_asan_run(
            r#"
fn main() {
    let c = true;
    let mut a = String.new();
    a.push_str("noncall-if-runtime-heap-payload");
    let x = if c { Some(a) } else { None };
    println("done");
}
"#,
            &["done"],
            "option_if_else_fresh_payload_freed",
        );
    }

    #[test]
    fn asan_owned_struct_option_shared_field_captured_from_builder_no_uaf() {
        // #48 (phase-12 self-hosting): an owned (non-`shared`) struct with an
        // `Option[shared T]` field, built in a helper and returned, then read.
        // The non-shared struct-literal path didn't capture-inc the field's
        // inner RC handle (only the shared-struct path did), so the source
        // local's scope-exit `FreeInlineOptionPayload` dec freed the inner
        // `Expr` to refcount 0 before the caller read its tail — a
        // heap-use-after-free (and an under-count → eventual double-free). The
        // inner payload carries a ≥36-byte String so the freed-then-read access
        // lands on a real heap block ASAN flags (and LSan would flag the leak
        // if the count went the other way). Mirrors the codegen E2E
        // `test_e2e_owned_struct_option_shared_field_captured_from_builder`.
        assert_clean_asan_run(
            r#"
struct Span { line: i64, column: i64, offset: i64, length: i64 }
enum Stmt { Empty }
shared enum Expr { Str(String), Blk(Block), Error }
struct Block { stmts: Vec[Stmt], tail: Option[Expr], span: Span }
fn mk() -> Block {
    let s: Vec[Stmt] = [];
    let mut payload = String.new();
    payload.push_str("owned-struct-option-shared-field-uaf-payload");
    let e = Expr.Str(payload);
    let tail: Option[Expr] = Some(e);
    Block { stmts: s, tail: tail, span: Span { line: 0, column: 0, offset: 0, length: 5 } }
}
fn render_block(b: Block) -> String {
    let Block { stmts, tail, span } = b;
    match tail { Some(e) => render_expr(e), None => "no-tail".to_string() }
}
fn render_expr(e: Expr) -> String {
    match e {
        Str(s) => s,
        Blk(b) => render_block(b),
        Error => "error".to_string(),
    }
}
fn main() {
    let blk = mk();
    println(render_expr(Expr.Blk(blk)));
}
"#,
            &["owned-struct-option-shared-field-uaf-payload"],
            "owned_struct_option_shared_field_captured_from_builder_no_uaf",
        );
    }

    #[test]
    fn asan_single_field_struct_option_payload_sizing_no_bad_access() {
        // #49 (phase-12 self-hosting, found while minimizing #48): a struct
        // whose ONLY field is an `Option[T]`, used as a shared-enum payload
        // (`struct Block { tail: Option[Expr] }` in `Expr.Blk(Block)`). The
        // variant's payload AREA is undersized to 1 word (the `Option` field
        // hits the enum-in-enum carve-out in `payload_word_count_for_type_expr`),
        // and `coerce_to_payload_words`'s scalar fast path (`num_words <= 1`)
        // then collapsed the real 4-word `Block` value to `0` via `coerce_to_i64`
        // — dropping the payload. Unpack/drop independently treat it as BOXED
        // (`llvm_type_word_count(T) > area`) and `inttoptr` the `0`. With a heap
        // String inner this manifests as a wild-pointer read/free ASAN flags
        // (≥36-byte payload so it lands on instrumented heap); the value-correct
        // form would SIGSEGV. The fix guards the fast path on the value's real
        // width so the payload boxes (the proven-correct multi-field path) and
        // pack/unpack/drop stay coherent. Mirrors the codegen E2E
        // `test_e2e_single_field_struct_option_payload_sizing`.
        assert_clean_asan_run(
            r#"
shared enum Expr { Str(String), Blk(Block), Error }
struct Block { tail: Option[Expr] }
fn render_block(b: Block) -> String {
    let Block { tail } = b;
    match tail { Some(e) => render_expr(e), None => "no-tail".to_string() }
}
fn render_expr(e: Expr) -> String {
    match e {
        Str(s) => s,
        Blk(b) => render_block(b),
        Error => "error".to_string(),
    }
}
fn main() {
    let mut payload = String.new();
    payload.push_str("single-field-struct-option-payload-sizing-payload");
    let blk = Block { tail: Some(Expr.Str(payload)) };
    println(render_expr(Expr.Blk(blk)));
}
"#,
            &["single-field-struct-option-payload-sizing-payload"],
            "single_field_struct_option_payload_sizing_no_bad_access",
        );
    }

    // ── Vec: owned heap buffer, scope-exit free ───────────────────
    // Exercises `emit_scope_vec_cleanup` — the Vec's data pointer must be
    // freed when `v` goes out of scope at the end of `main`.

    #[test]
    fn asan_vec_push_scope_exit_free() {
        assert_clean_asan_run(
            r#"
fn main() {
    let mut v: Vec[i64] = Vec.new();
    v.push(1);
    v.push(2);
    v.push(3);
    println(v.len());
}
"#,
            &["3"],
            "vec_push_scope_exit_free",
        );
    }

    // ── Vec growth: multiple reallocations ────────────────────────
    // Forces Vec growth (2x doubling, floor 4) so the scope-exit free has
    // to release a larger buffer than the initial allocation. Catches
    // bugs where growth replaces the data pointer without freeing the old
    // buffer (leak) or where the grown pointer is freed twice.

    #[test]
    fn asan_vec_growth_multiple_reallocs() {
        assert_clean_asan_run(
            r#"
fn main() {
    let mut v: Vec[i64] = Vec.new();
    let mut i = 0;
    while i < 32 {
        v.push(i);
        i = i + 1;
    }
    println(v.len());
}
"#,
            &["32"],
            "vec_growth_multiple_reallocs",
        );
    }

    // ── Vec.with_capacity: scope-exit free of pre-allocated buffer ─
    // `with_capacity(N)` malloc's a buffer up front; the scope-exit
    // cleanup must free it once, even though `len == 0` and no push
    // ever fired. Catches a regression where the free path keys off
    // `len > 0` instead of `cap > 0` and leaks the entire buffer.

    #[test]
    fn asan_vec_with_capacity_unused_buffer_freed() {
        assert_clean_asan_run(
            r#"
fn main() {
    let v: Vec[i64] = Vec.with_capacity(16);
    println(v.len());
}
"#,
            &["0"],
            "vec_with_capacity_unused_buffer_freed",
        );
    }

    // B-2026-07-08-7: `Vec.filled(n, 0)` now allocates its buffer via the
    // `calloc`-backed zeroed wrapper instead of malloc + fill loop. The buffer
    // is still an ordinary heap allocation freed by the standard Vec drop —
    // this pins that the calloc path leaks nothing and double-frees nothing
    // (indexed writes into the zeroed buffer, then scope-exit free).
    #[test]
    fn asan_vec_filled_zero_calloc_buffer_freed() {
        assert_clean_asan_run(
            r#"
fn main() {
    let mut v: Vec[i64] = Vec.filled(8, 0);
    v[3] = 42;
    println(v.len());
    println(v[3]);
    println(v[0]);
}
"#,
            &["8", "42", "0"],
            "vec_filled_zero_calloc_buffer_freed",
        );
    }

    // `with_capacity(N)` + push exactly N times — every slot fits in
    // the pre-allocated buffer, no realloc fires, scope-exit frees
    // the single original allocation. Counterpart to
    // `asan_vec_growth_multiple_reallocs` which verifies the grow
    // path; this one verifies the no-grow path.

    #[test]
    fn asan_vec_with_capacity_push_exact_n_no_grow() {
        assert_clean_asan_run(
            r#"
fn main() {
    let mut v: Vec[i64] = Vec.with_capacity(16);
    let mut i = 0;
    while i < 16 {
        v.push(i);
        i = i + 1;
    }
    println(v.len());
}
"#,
            &["16"],
            "vec_with_capacity_push_exact_n_no_grow",
        );
    }

    // `with_capacity(N)` + push more than N times — forces a grow
    // mid-flight, so both the original `with_capacity` malloc'd
    // buffer AND the grown buffer need to be tracked correctly
    // (old freed on grow, new freed on scope-exit). Catches a
    // double-free if the grow path doesn't free the original
    // before swapping the data pointer.

    #[test]
    fn asan_vec_with_capacity_push_past_n_grows_once() {
        assert_clean_asan_run(
            r#"
fn main() {
    let mut v: Vec[i64] = Vec.with_capacity(4);
    let mut i = 0;
    while i < 16 {
        v.push(i);
        i = i + 1;
    }
    println(v.len());
}
"#,
            &["16"],
            "vec_with_capacity_push_past_n_grows_once",
        );
    }

    // ── extend_from_slice ─────────────────────────────────────────
    // Memcpy + grow path; both source and destination get a
    // scope-exit free, neither is freed twice, no leak in the
    // grown-buffer hand-off.

    #[test]
    fn asan_vec_extend_from_slice_no_grow_clean() {
        assert_clean_asan_run(
            r#"
fn main() {
    let src: Vec[i64] = Vec.filled(4, 7);
    let mut dst: Vec[i64] = Vec.with_capacity(8);
    dst.push(1);
    dst.push(2);
    dst.extend_from_slice(src);
    println(dst.len());
}
"#,
            &["6"],
            "vec_extend_from_slice_no_grow_clean",
        );
    }

    #[test]
    fn asan_vec_extend_from_slice_triggers_grow_clean() {
        // Forces a grow mid-extend (dst cap=2, src len=4). The
        // grow path replaces dst's data pointer; the old buffer
        // must be freed on grow (not on scope exit), and the new
        // buffer must be freed on scope exit (not on grow).
        assert_clean_asan_run(
            r#"
fn main() {
    let src: Vec[i64] = Vec.filled(4, 5);
    let mut dst: Vec[i64] = Vec.with_capacity(2);
    dst.push(1);
    dst.extend_from_slice(src);
    println(dst.len());
}
"#,
            &["5"],
            "vec_extend_from_slice_triggers_grow_clean",
        );
    }

    // ── extend_from_slice + from_slice: RC-bearing element types ─
    // The bit-copy code path bit-copies String / Vec / shared-T
    // aggregates between source and dest. Both observers then alias
    // the same inner heap pointers, so the first scope-exit free
    // wins and the second hits double-free / UAF. Fix routes through
    // per-element synth_clone for non-trivially-copyable elements.
    // These tests verify the fix; they fail under the bit-copy v1
    // implementation.

    #[test]
    fn asan_vec_extend_from_slice_string_smallest_repro_with_cap() {
        // Smallest repro for debugging: 2 heap strings, no grow on
        // src (uses with_capacity(4)), no grow on dst (uses
        // with_capacity(4)). If this passes, the bug is in the
        // grow-path interaction. If it fails, the bug is in the
        // per-element String clone path itself.
        assert_clean_asan_run(
            r#"
fn main() {
    let mut a: String = String.new();
    a.push_str("hi");
    let mut b: String = String.new();
    b.push_str("ho");
    let mut src: Vec[String] = Vec.with_capacity(4);
    src.push(a);
    src.push(b);
    let mut dst: Vec[String] = Vec.with_capacity(4);
    dst.extend_from_slice(src);
    println(dst[0]);
}
"#,
            &["hi"],
            "vec_extend_from_slice_string_smallest_repro_with_cap",
        );
    }

    #[test]
    fn asan_vec_extend_from_slice_string_elements_independent() {
        // Vec[String] source — each String must be deep-cloned into
        // dest, not bit-copied. Strings here are heap-allocated (via
        // push_str on a fresh String) so cap > 0 and the scope-exit
        // free does fire. Without the fix, dst[0]'s String
        // {ptr, len, cap} aliases src[0]'s; scope-exit frees both,
        // ASAN reports double-free of the char buffer.
        //
        // The string-literal version of this test (push("hello"))
        // doesn't catch the bug because literals are rodata-backed
        // with cap=0 and the free path skips them — that's the same
        // shape that hid the bug pre-fix.
        assert_clean_asan_run(
            r#"
fn main() {
    let mut a: String = String.new();
    a.push_str("hello");
    let mut b: String = String.new();
    b.push_str("world");
    let mut src: Vec[String] = Vec.new();
    src.push(a);
    src.push(b);
    let mut dst: Vec[String] = Vec.new();
    dst.extend_from_slice(src);
    println(dst[0]);
    println(dst[1]);
}
"#,
            &["hello", "world"],
            "vec_extend_from_slice_string_elements_independent",
        );
    }

    #[test]
    fn asan_vec_extend_from_slice_nested_vec_elements_independent() {
        // Vec[Vec[i64]] source — the inner Vec storage must be
        // deep-cloned into dest. Without the fix, dst[0]'s inner Vec
        // aliases src[0]'s buffer; both scope-exit frees the same
        // pointer.
        assert_clean_asan_run(
            r#"
fn main() {
    let mut src: Vec[Vec[i64]] = Vec.new();
    let mut a: Vec[i64] = Vec.new();
    a.push(1);
    a.push(2);
    src.push(a);
    let mut b: Vec[i64] = Vec.new();
    b.push(3);
    src.push(b);
    let mut dst: Vec[Vec[i64]] = Vec.new();
    dst.extend_from_slice(src);
    println(dst[0].len());
    println(dst[1].len());
}
"#,
            &["2", "1"],
            "vec_extend_from_slice_nested_vec_elements_independent",
        );
    }

    #[test]
    fn asan_vec_try_extend_from_slice_triggers_grow_clean() {
        // Fallible sibling of `asan_vec_extend_from_slice_triggers_grow_clean`
        // (phase-8-stdlib-floor item 8). `try_extend_from_slice` shares the
        // grow CFG with the panicking base but allocates through
        // `karac_alloc_fallible`; the success path must still free the old
        // buffer on grow (not on scope exit) and free the new buffer on scope
        // exit (not on grow). dst cap=2, src len=4 forces the grow.
        assert_clean_asan_run(
            r#"
fn main() {
    let src: Vec[i64] = Vec.filled(4, 5);
    let mut dst: Vec[i64] = Vec.with_capacity(2);
    dst.push(1);
    let _ = dst.try_extend_from_slice(src);
    println(dst.len());
}
"#,
            &["5"],
            "vec_try_extend_from_slice_triggers_grow_clean",
        );
    }

    #[test]
    fn asan_vec_try_extend_from_slice_string_elements_independent() {
        // `try_extend_from_slice` must take the same per-element clone path as
        // the panicking base for heap-bearing elements — bit-copying String
        // aggregates would alias src/dst inner buffers and double-free at
        // scope exit. Heap-allocated Strings (cap > 0) so the free fires.
        assert_clean_asan_run(
            r#"
fn main() {
    let mut a: String = String.new();
    a.push_str("hello");
    let mut b: String = String.new();
    b.push_str("world");
    let mut src: Vec[String] = Vec.new();
    src.push(a);
    src.push(b);
    let mut dst: Vec[String] = Vec.new();
    let _ = dst.try_extend_from_slice(src);
    println(dst[0]);
    println(dst[1]);
}
"#,
            &["hello", "world"],
            "vec_try_extend_from_slice_string_elements_independent",
        );
    }

    #[test]
    fn asan_vec_from_slice_string_elements_independent() {
        // Same hazard for `Vec.from_slice` — pre-dates
        // `extend_from_slice` but inherits the same v1 limitation.
        // Heap-allocated Strings to ensure cap > 0 and the
        // scope-exit free actually fires.
        assert_clean_asan_run(
            r#"
fn main() {
    let mut a: String = String.new();
    a.push_str("alpha");
    let mut b: String = String.new();
    b.push_str("beta");
    let mut src: Vec[String] = Vec.new();
    src.push(a);
    src.push(b);
    let dst: Vec[String] = Vec.from_slice(src);
    println(dst[0]);
    println(dst[1]);
}
"#,
            &["alpha", "beta"],
            "vec_from_slice_string_elements_independent",
        );
    }

    #[test]
    fn asan_vec_try_from_slice_string_elements_independent() {
        // Fallible sibling of the above (phase-8-stdlib-floor item 8).
        // `try_from_slice` wraps the new Vec in `Result.Ok(_)`; the
        // Vec[String]-in-Result payload must drop exactly once at scope
        // exit (no double-free against the per-element-cloned source).
        // Heap-allocated Strings (cap > 0) so the free fires.
        assert_clean_asan_run(
            r#"
fn main() {
    let mut a: String = String.new();
    a.push_str("alpha");
    let mut b: String = String.new();
    b.push_str("beta");
    let mut src: Vec[String] = Vec.new();
    src.push(a);
    src.push(b);
    match Vec.try_from_slice(src) {
        Ok(dst) => { println(dst[0]); println(dst[1]); }
        Err(_) => { println("err"); }
    }
}
"#,
            &["alpha", "beta"],
            "vec_try_from_slice_string_elements_independent",
        );
    }

    #[test]
    fn asan_question_multiword_ok_payload_owned_once() {
        // The `?` multi-word Ok-payload reconstruction (phase-8-stdlib-floor
        // item 8) rebuilds a 3-word String/Vec from all its payload words. The
        // unwrapped heap value must be owned by the binding and freed exactly
        // once at scope exit — a reconstruction that aliased or dropped a word
        // would double-free or leak. `?`-unwrap a heap `String` and a heap
        // `Vec[i64]` (cap > 0 so the free fires), use both, return.
        assert_clean_asan_run(
            r#"
fn take() -> Result[i64, AllocError] {
    let mut a: String = String.new();
    a.push_str("hello");
    let r: Result[String, AllocError] = Ok(a);
    let s: String = r?;
    let src: Vec[i64] = Vec.filled(3, 9);
    let v: Vec[i64] = Vec.try_from_slice(src)?;
    Ok(s.len() + v.len())
}
fn main() {
    match take() { Ok(n) => println(n), Err(_) => println("err") }
}
"#,
            &["8"],
            "question_multiword_ok_payload_owned_once",
        );
    }

    #[test]
    fn asan_vecdeque_payload_in_match_freed_once() {
        // B-2026-06-10-3: a VecDeque bound out of an Option via `match` is
        // reconstructed as a 3-word `{ptr,len,cap}` value (the gates now handle
        // `VecDeque`, not just `Vec`/`String`) and freed exactly once at the
        // arm's scope exit. A 1-word-default reconstruction freed a garbage
        // pointer (SIGTRAP); a mis-registered cleanup could double-free. Heap
        // buffer (cap > 0 via pushes) so the free actually fires.
        assert_clean_asan_run(
            r#"
fn mk() -> VecDeque[i64] {
    let mut q: VecDeque[i64] = VecDeque.new();
    q.push_back(5);
    q.push_back(6);
    q
}
fn main() {
    let o: Option[VecDeque[i64]] = Some(mk());
    match o {
        Some(v) => { println(v.len()); println(v[0]); }
        None => { println("n"); }
    }
}
"#,
            &["2", "5"],
            "vecdeque_payload_in_match_freed_once",
        );
    }

    #[test]
    fn asan_vec_from_slice_nested_index_source_clean() {
        // `Vec.from_slice(rows[r])` on Vec[Vec[T]] — symmetric to the
        // extend_from_slice nested-index test. The new codegen branch
        // compiles `rows[r]` directly, extracts {data, len}, and
        // routes through the standard alloc + memcpy/clone path.
        // Catches RC-aliasing bugs that would surface if the per-
        // element clone path missed the new entry shape.
        assert_clean_asan_run(
            r#"
fn main() {
    let mut rows: Vec[Vec[i64]] = Vec.new();
    let mut r0: Vec[i64] = Vec.new();
    r0.push(11);
    r0.push(22);
    rows.push(r0);
    let copy: Vec[i64] = Vec.from_slice(rows[0]);
    println(copy.len());
    println(copy[0]);
    println(copy[1]);
}
"#,
            &["2", "11", "22"],
            "vec_from_slice_nested_index_source_clean",
        );
    }

    #[test]
    fn asan_vec_nested_indexed_write_clean() {
        // `rows[r][c] = val` on Vec[Vec[T]] — nested-index store path
        // (codegen `compile_nested_vec_vec_index_store`). The leaf
        // store overwrites a slot inside `rows.data[r].data` (the
        // pre-filled inner buffer); scope-exit cleanup walks rows
        // recursively and frees the inner buffers cleanly. Catches
        // any aliasing bug where the GEP arithmetic stomps past the
        // inner Vec aggregate (write goes into `rows.data` itself,
        // not the inner buffer).
        assert_clean_asan_run(
            r#"
fn main() {
    let mut rows: Vec[Vec[i64]] = Vec.new();
    let r0: Vec[i64] = Vec.filled(4, 0);
    let r1: Vec[i64] = Vec.filled(4, 0);
    rows.push(r0);
    rows.push(r1);
    rows[0][2] = 100;
    rows[1][3] = 200;
    println(rows[0][2]);
    println(rows[1][3]);
}
"#,
            &["100", "200"],
            "vec_nested_indexed_write_clean",
        );
    }

    #[test]
    fn asan_vec_extend_from_slice_nested_index_source_clean() {
        // Source is `rows[r]` on Vec[Vec[T]] — the kata-6 case.
        // The codegen fallback path compiles the Index expression
        // and reads its {ptr, len}. Memcpy aliases the source
        // pointer into the destination's buffer for the duration
        // of the memcpy, but the destination has independent
        // storage afterwards. Scope-exit cleanup of `rows`
        // recursively frees each inner Vec's buffer; `out`'s own
        // buffer is freed independently. Catches double-free if
        // the codegen accidentally aliases the source's buffer
        // into the destination's data pointer instead of memcpy.
        assert_clean_asan_run(
            r#"
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
}
"#,
            &["3"],
            "vec_extend_from_slice_nested_index_source_clean",
        );
    }

    // ── extend_from_slice: source-alias rejection (grow path) ────
    // When the source slice points into the receiver's own heap
    // buffer (e.g. `v.extend_from_slice(v.as_slice())`) and grow
    // fires, the grow path frees the old buffer before reading
    // from `src_data` — a use-after-free that previously silently
    // corrupted the extended elements (the read returned whatever
    // the allocator handed back from the recycled slot, often the
    // freshly-malloc'd new buffer's tail). The runtime overlap
    // guard in `extend_from_slice` detects the case before the
    // free and `emit_panic`s instead. Test verifies (a) the
    // guard fires with the expected message, and (b) the
    // disjoint-source counterpart still runs cleanly.

    #[test]
    fn asan_vec_extend_from_slice_self_alias_rejects() {
        assert_asan_panics_with(
            r#"
fn main() {
    let mut v: Vec[i64] = Vec.with_capacity(2);
    v.push(1);
    v.push(2);
    v.extend_from_slice(v.as_slice());
    println(v.len());
}
"#,
            "source slice aliases destination buffer",
            "vec_extend_from_slice_self_alias_rejects",
        );
    }

    #[test]
    fn asan_vec_extend_from_slice_disjoint_source_no_panic() {
        // Disjoint src/dst — guard must NOT fire even when the grow
        // path runs. dst cap=2, push one element so grow is required
        // mid-extend. Counterpart to the rejection test above.
        assert_clean_asan_run(
            r#"
fn main() {
    let src: Vec[i64] = Vec.filled(4, 5);
    let mut dst: Vec[i64] = Vec.with_capacity(2);
    dst.push(1);
    dst.extend_from_slice(src);
    println(dst.len());
}
"#,
            &["5"],
            "vec_extend_from_slice_disjoint_source_no_panic",
        );
    }

    // ── String: push_str + scope-exit free ────────────────────────
    // String shares the Vec-shaped layout; scope-exit cleanup should free
    // the UTF-8 buffer. Static literals have cap=0 and must NOT be freed —
    // catches bugs where the free path doesn't check the `cap > 0` guard.

    #[test]
    fn asan_string_new_push_str() {
        assert_clean_asan_run(
            r#"
fn main() {
    let mut s = String.new();
    s.push_str("hello ");
    s.push_str("world");
    println(s.len());
}
"#,
            &["11"],
            "string_new_push_str",
        );
    }

    // ── String literal: cap=0 must not be freed ───────────────────
    // A `let s = "static"` binds to a string-literal global with cap=0.
    // If scope-exit cleanup incorrectly frees it, ASAN catches the
    // invalid-free on a non-heap pointer.

    #[test]
    fn asan_string_literal_no_free() {
        assert_clean_asan_run(
            r#"
fn main() {
    let s = "static string never freed";
    println(s.len());
}
"#,
            &["25"],
            "string_literal_no_free",
        );
    }

    // ── Shared struct: rc_inc/rc_dec + final free ─────────────────
    // `shared struct Counter` heap-allocates with an RC header.
    // Scope-exit runs `emit_rc_dec`; when refcount hits zero, the free
    // branch inside `emit_rc_dec` must release the heap block.

    #[test]
    fn asan_shared_struct_single_owner() {
        assert_clean_asan_run(
            r#"
shared struct Counter { val: i64 }
fn main() {
    let c = Counter { val: 42 };
    println(c.val);
}
"#,
            &["42"],
            "shared_struct_single_owner",
        );
    }

    // ── Shared struct structural `==` (C1, B-2026-06-19-9) ────────
    // The field-walk comparator reads through the RC pointers (and a
    // String field's heap buffer) but allocates nothing; this pins that
    // it neither leaks nor double-frees the compared structs or their
    // String fields when they drop. A ≥36-byte String field defeats LSan's
    // short-string reachability blind spot (memory: lsan-reachability).
    #[test]
    fn asan_shared_struct_structural_eq_no_leak_no_double_free() {
        assert_clean_asan_run(
            r#"
#[derive(Eq, PartialEq)]
shared struct Tag { id: i64, name: String }
fn main() {
    let a = Tag { id: 1, name: "shared-struct-eq-asan-payload-0001" };
    let b = Tag { id: 1, name: "shared-struct-eq-asan-payload-0001" };
    let c = Tag { id: 2, name: "shared-struct-eq-asan-payload-XXXX" };
    if a == b { println("eq"); }
    if a != c { println("ne"); }
}
"#,
            &["eq", "ne"],
            "shared_struct_structural_eq",
        );
    }

    // ── Shared struct alias: refcount goes to 2, then 0 ───────────
    // Binding `b = a` triggers `rc_inc`. Scope-exit runs `rc_dec` twice
    // (once per binding); only the last one should free. Catches bugs
    // where the alias path double-frees or leaks.

    #[test]
    fn asan_shared_struct_alias_refcount_balance() {
        assert_clean_asan_run(
            r#"
shared struct Data { x: i64 }
fn main() {
    let a = Data { x: 100 };
    let b = a;
    println(a.x);
    println(b.x);
}
"#,
            &["100", "100"],
            "shared_struct_alias_refcount_balance",
        );
    }

    // ── Shared struct passed to a function ────────────────────────
    // The parameter binding inside the callee adds its own refcount
    // lifetime. Both caller- and callee-side rc_dec must balance.

    #[test]
    fn asan_shared_struct_passed_to_fn() {
        assert_clean_asan_run(
            r#"
shared struct Wrapper { val: i64 }
fn read_val(w: Wrapper) -> i64 { w.val }
fn main() {
    let w = Wrapper { val: 7 };
    println(read_val(w));
}
"#,
            &["7"],
            "shared_struct_passed_to_fn",
        );
    }

    // ── Vec inside a nested scope ─────────────────────────────────
    // Nested block scope — the inner Vec must be freed at the inner
    // block's close, not deferred to the outer `main` exit. ASAN alone
    // can't catch "freed at outer scope instead of inner" (both are
    // eventual free, no leak); combined with a later allocation that
    // reuses the same pool we at least smoke-test that nested cleanup
    // doesn't double-free or leak.

    #[test]
    fn asan_vec_nested_scope() {
        assert_clean_asan_run(
            r#"
fn main() {
    {
        let mut inner: Vec[i64] = Vec.new();
        inner.push(1);
        inner.push(2);
        println(inner.len());
    }
    let mut outer: Vec[i64] = Vec.new();
    outer.push(99);
    println(outer.len());
}
"#,
            &["2", "1"],
            "vec_nested_scope",
        );
    }

    // ── ? operator drains scope cleanup actions on the failure path ──────
    // The early-return emitted by `?` for `Result`/`Option` must run the
    // function's accumulated `scope_cleanup_actions` (free Vec/String buffers,
    // RC-dec shared values, free Map handles) before returning. Without the
    // drain, a Vec live at the `?` site leaks its data buffer when `?` fires.

    #[test]
    fn asan_question_drains_scope_cleanup_on_err() {
        assert_clean_asan_run(
            r#"
fn boom() -> Result[i64, i64] { Err(7_i64) }
fn use_vec() -> Result[i64, i64] {
    let mut v: Vec[i64] = Vec.new();
    v.push(1_i64);
    v.push(2_i64);
    v.push(3_i64);
    let _ = boom()?;
    Ok(v.len() as i64)
}
fn main() {
    match use_vec() {
        Ok(n) => println(n),
        Err(e) => println(e),
    }
}
"#,
            &["7"],
            "question_drains_scope_cleanup_on_err",
        );
    }

    #[test]
    fn asan_question_drains_scope_cleanup_on_none() {
        assert_clean_asan_run(
            r#"
fn maybe() -> Option[i64] { None }
fn use_vec() -> Option[i64] {
    let mut v: Vec[i64] = Vec.new();
    v.push(10_i64);
    v.push(20_i64);
    let _ = maybe()?;
    Some(v.len() as i64)
}
fn main() {
    match use_vec() {
        Some(n) => println(n),
        None => println(0),
    }
}
"#,
            &["0"],
            "question_drains_scope_cleanup_on_none",
        );
    }

    // ── Set[T]: scope-exit free ─────────────────────────────────────
    // Set lowers to Map[T, ()] and shares the karac_map_free cleanup
    // action. Verify the FreeMapHandle entry registered by
    // compile_set_new_stmt fires on scope exit, and that the Set's
    // backing buckets + heap-bearing String elements are released.

    #[test]
    fn asan_set_new_insert_scope_exit_free() {
        assert_clean_asan_run(
            r#"
fn main() {
    let mut s: Set[i64] = Set.new();
    s.insert(1_i64);
    s.insert(2_i64);
    s.insert(3_i64);
    println(s.len());
}
"#,
            &["3"],
            "set_new_insert_scope_exit_free",
        );
    }

    #[test]
    fn asan_set_string_scope_exit_free() {
        // Set[String] keeps the bucket array on the heap and references the
        // String literal's static buffer (cap = 0) by value-copy. The set
        // free should release the bucket array; static String buffers must
        // NOT be freed.
        assert_clean_asan_run(
            r#"
fn main() {
    let mut s: Set[String] = Set.new();
    s.insert("alice");
    s.insert("bob");
    s.insert("alice");
    println(s.len());
}
"#,
            &["2"],
            "set_string_scope_exit_free",
        );
    }

    // ── Clone trait surface (canonical: phase-8-stdlib-floor.md
    //    "Clone trait surface for collections") ───────────────────────────

    #[test]
    fn asan_vec_clone_independent_buffers() {
        // Both the source and the cloned Vec own heap buffers; both must
        // be freed exactly once on scope exit. ASAN catches double-free
        // (two frees of the same allocation) and leak (no free) — a
        // working clone keeps them independent.
        assert_clean_asan_run(
            r#"
fn main() {
    let mut v: Vec[i64] = Vec.new();
    v.push(1_i64);
    v.push(2_i64);
    v.push(3_i64);
    let w: Vec[i64] = v.clone();
    println(v.len());
    println(w.len());
}
"#,
            &["3", "3"],
            "vec_clone_independent_buffers",
        );
    }

    #[test]
    fn asan_vec_clone_empty_no_leak() {
        // Empty Vec clone hits the fast path (no malloc); the resulting
        // Vec has cap=0 and its scope-exit free must be a no-op rather
        // than calling free(null) repeatedly or leaking a placeholder
        // allocation.
        assert_clean_asan_run(
            r#"
fn main() {
    let v: Vec[i64] = Vec.new();
    let w: Vec[i64] = v.clone();
    println(w.len());
}
"#,
            &["0"],
            "vec_clone_empty_no_leak",
        );
    }

    #[test]
    fn asan_map_clone_independent_handle() {
        // Both maps allocate their own bucket arrays; both must be freed
        // exactly once on scope exit. ASAN catches handle-aliasing (one
        // map pointing at another's storage).
        assert_clean_asan_run(
            r#"
fn main() {
    let mut m: Map[i64, i64] = Map.new();
    m.insert(1_i64, 10_i64);
    m.insert(2_i64, 20_i64);
    let n: Map[i64, i64] = m.clone();
    println(m.len());
    println(n.len());
}
"#,
            &["2", "2"],
            "map_clone_independent_handle",
        );
    }

    // Regression for the kata 133 (`clone_graph` BFS) perf cliff
    // introduced by commit 2bd2dba ("per-iteration cleanup + null-
    // guarded RcDec for body-local lets", 2026-05-17). The per-iter
    // cleanup fires `rc_dec` on every body-local shared-struct let on
    // every loop iteration. `let n = visited.get(k).unwrap()` binds
    // an aliasing handle to the Map's stored ref because the runtime
    // `karac_map_get` byte-copies the bucket's value pointer without
    // touching its refcount, and the let-site's `rhs_yields_fresh_ref`
    // path treats MethodCall RHS as "fresh +1 ref" so it skips the
    // receive-side rc_inc. Pre-fix, the per-iter dec on `n` drove the
    // bucket's ref to zero, freeing the Node while the Map still held
    // a dangling pointer. Subsequent allocations reused the freed
    // chunk and every subsequent get-then-bind returned a node
    // aliasing the latest reuse — observable here as `visited.get(0).val`
    // reading the wrong value, and in kata 133 as a ~100× malloc-
    // freelist thrash on the next clone_graph call. The fix
    // (`compile_map_method` "get" arm) emits an rc_inc on the loaded
    // pointer when V is a shared struct, aligning Map.get with the
    // calling convention that shared-returning callees hand the
    // caller a fresh +1 ref. The Vec[Node] field in the Node type is
    // load-bearing for the repro — it bumps the heap allocation to
    // 40 bytes, putting it in a freelist bucket the next alloc reuses
    // deterministically; with no Vec field the 16-byte Node lands in
    // a sparser bucket and the corruption pattern doesn't surface.
    //
    // B-2026-07-14-3: the neighbor edges form a CHAIN (`node0->..->node4`),
    // NOT a ring. The original `(i + 1) % k` wrap built a reference CYCLE
    // (`node4 -> node0`); a `Vec[shared Node]` element is an OWNING ref (its
    // drop rc-dec's each element), so a cycle is uncollectable under RC and
    // leaks by construction — the whole map. That leak surfaced only on arm64
    // (LSan) once the reader-binding over-retain below was fixed; on x86 the
    // ring stayed reachable through a stale stack slot (an LSan false-negative),
    // which is why the ring "passed" there. The chain still exercises the exact
    // reader shape this test targets (`let a/b = m.get(k).unwrap()` + a
    // `neighbors.push(b)` consume) without conflating it with the separate,
    // known RC-cannot-collect-cycles limitation. The reader-binding over-retain
    // itself was a codegen double-inc: `m.get(k)` on a shared value rc-inc's the
    // aliased bucket ptr (get arm, maps.rs), and the consuming `.unwrap()`
    // let-site rc-inc'd it a SECOND time (`rhs_yields_fresh_ref` classed the
    // unwrap as non-fresh) — +2 per bind vs one per-iter dec, leaking every
    // node. Fixed in `rhs_yields_fresh_ref` (a shared-map-get `.unwrap()` IS
    // fresh — get already delivered the +1).
    #[test]
    fn asan_map_get_shared_value_in_loop_no_alias_collapse() {
        assert_clean_asan_run(
            r#"
shared struct Node {
    val: i64,
    mut neighbors: Vec[Node],
}

fn main() {
    let mut visited: Map[i64, Node] = Map.new();
    let k: i64 = 5;
    for i in 0..k {
        let fresh = Node { val: i, neighbors: Vec.new() };
        let _ = visited.insert(i, fresh);
    }
    // The push-into-Vec[Node] step is what triggers the per-iter
    // cleanup of `a` and `b` to free the Map's only ref; without
    // this second loop the bug doesn't surface because the inserts'
    // per-iter cleanup is already balanced by the existing
    // `suppress_source_vec_cleanup_for_arg` rc_inc.
    for i in 0..k {
        let a = visited.get(i).unwrap();
        if i + 1 < k {
            let b = visited.get(i + 1).unwrap();
            a.neighbors.push(b);
        }
    }
    // Read with let-bindings (not inline chains). The inline
    // `Map.get(k).unwrap().val` shape is covered separately in
    // `asan_map_get_unwrap_field_inline_chain` — together the
    // two tests pin both common reader shapes.
    let n0 = visited.get(0_i64).unwrap();
    let n1 = visited.get(1_i64).unwrap();
    let n4 = visited.get(4_i64).unwrap();
    println(n0.val);
    println(n1.val);
    println(n4.val);
}
"#,
            &["0", "1", "4"],
            "map_get_shared_value_in_loop_no_alias_collapse",
        );
    }

    // Regression for the inline `m.get(k).unwrap().val` chain
    // returning literal zero instead of the heap struct's val
    // field. Pre-fix, `shared_type_for_call_like` only handled
    // Identifier-receiver MethodCalls; a MethodCall whose object
    // is itself a MethodCall (the unwrap-on-Map.get chain) fell
    // through to the generic non-shared FieldAccess path, which
    // compiled `.val` as i64 zero. The fix recognises
    // `unwrap`/`expect` as a special case and recovers the inner
    // T from `method_unwrap_inner_types[span]`; the bug-#8
    // GEP+load+dec path then fires and the field is actually
    // read.
    //
    // Together with `asan_map_get_shared_value_in_loop_no_alias_collapse`
    // (which uses let-bindings) this covers both common reader
    // shapes for `Map[K, Shared]` values.
    #[test]
    fn asan_map_get_unwrap_field_inline_chain() {
        assert_clean_asan_run(
            r#"
shared struct Node {
    val: i64,
    mut neighbors: Vec[Node],
}

fn main() {
    let mut visited: Map[i64, Node] = Map.new();
    let _ = visited.insert(0_i64, Node { val: 100, neighbors: Vec.new() });
    let _ = visited.insert(1_i64, Node { val: 200, neighbors: Vec.new() });
    println(visited.get(0_i64).unwrap().val);
    println(visited.get(1_i64).unwrap().val);
}
"#,
            &["100", "200"],
            "map_get_unwrap_field_inline_chain",
        );
    }

    #[test]
    fn asan_set_clone_independent_handle() {
        // Set[i64] clone — both sets free independent bucket arrays.
        assert_clean_asan_run(
            r#"
fn main() {
    let mut s: Set[i64] = Set.new();
    s.insert(7_i64);
    s.insert(8_i64);
    let t: Set[i64] = s.clone();
    println(s.contains(7_i64));
    println(t.contains(8_i64));
}
"#,
            &["true", "true"],
            "set_clone_independent_handle",
        );
    }

    #[test]
    fn asan_set_union_string_independent_handles() {
        // Set[String].union — every surviving element is per-element-cloned
        // into a freshly-allocated bucket array. ASAN catches both the new
        // bucket-array leak (if `u` is not scope-tracked) and any UAF if
        // the per-element String clone aliases the source's heap buffer.
        assert_clean_asan_run(
            r#"
fn main() {
    let mut a: Set[String] = Set.new();
    a.insert("alpha");
    a.insert("beta");
    let mut b: Set[String] = Set.new();
    b.insert("beta");
    b.insert("gamma");
    let u: Set[String] = a.union(b);
    println(u.contains("alpha"));
    println(u.contains("beta"));
    println(u.contains("gamma"));
}
"#,
            &["true", "true", "true"],
            "set_union_string_independent_handles",
        );
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
            eprintln!("[{label}] parse errors: {:?}", parsed.errors);
            return None;
        }
        // Mirror the real CLI pipeline (lib.rs / cli.rs) and the codegen
        // harness (`tests/codegen.rs::run_program_capturing_inner`): desugar
        // runs between parse and resolve. It synthesizes `#[derive(...)]`
        // bodies (e.g. `#[derive(Default)]` → the inherent `Type.default`
        // impl) and expands comptime — without it a derive-dependent program
        // (std.mem `take[T: Default]`'s `T.default()` dispatch) miscompiles to
        // the const-0 fallback and double-frees, an ASAN-harness-only artifact
        // absent from shipped binaries.
        karac::desugar_program(&mut parsed.program);
        let resolved = karac::resolve(&parsed.program);
        let typed = karac::typecheck(&parsed.program, &resolved);
        karac::lower(&mut parsed.program, &typed);
        let ownership = karac::ownershipcheck(&parsed.program, &typed);
        super::common::assert_ownership_clean(&ownership, src);

        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let obj_path = format!("/tmp/karac_asan_ow_{}_{}.o", std::process::id(), id);
        let exe_path = format!("/tmp/karac_asan_ow_{}_{}", std::process::id(), id);

        if let Err(e) = compile_to_object(&parsed.program, &obj_path, Some(&ownership), None) {
            eprintln!("[{label}] compile_to_object failed: {e}");
            return None;
        }
        if !Path::new(&obj_path).exists() {
            eprintln!("[{label}] object file missing after compile_to_object");
            return None;
        }
        if let Err(e) =
            link_executable_with_sanitizer(&obj_path, &exe_path, &["-fsanitize=address"])
        {
            eprintln!("[{label}] link_executable_with_sanitizer failed: {e}");
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

    // ── Atomic-RC across par {}: refcount race detection ─────────
    // The `arc_values` subset of RC bindings crosses `par {}` thread
    // boundaries. With non-atomic load+add+store the refcount races
    // when both branches run concurrent inc/dec on the same heap block;
    // with atomic-RC (`atomicrmw add` / `atomicrmw sub`, `SeqCst`) the
    // increment is race-free. ASAN's standard run does not detect data
    // races on its own, but it *will* catch the secondary symptoms:
    // a UAF when the racing dec drops below zero and one branch tries
    // to free a still-live heap block, or a double-free when both
    // branches independently free. Pre-slice (substep 2 missing) this
    // test would manifest one of those errors under load; with the
    // atomic path it stays clean.

    #[test]
    fn asan_par_block_arc_promoted_no_double_free() {
        assert_clean_asan_run_with_ownership(
            r#"
shared struct Counter { val: i64 }
fn use_c(c: Counter) -> i64 { c.val }
fn main() {
    let cond: bool = false;
    let c = Counter { val: 7 };
    let d = c;
    if cond { use_c(d); }
    par {
        println(use_c(d));
        println(use_c(d));
    }
}
"#,
            "par_block_arc_promoted_no_double_free",
        );
    }

    /// B-2026-07-11-3: branch bindings that OWN HEAP (String) escape the
    /// `par {}` block into the enclosing scope and are consumed AFTER the
    /// block (no tail expression) — the join hoist. Each branch buffer now
    /// transfers into a parent return slot and is dropped at the enclosing
    /// scope's end like any other `let`, exactly once. Asserts no leak
    /// (LSan) and no use-after-free / double-free (ASAN) on the escaped
    /// heap owners — the class that broadening the codegen slot set to
    /// every branch binding could have regressed.
    #[test]
    fn asan_par_block_heap_bindings_escape_no_double_free() {
        assert_clean_asan_run_with_ownership(
            r#"
fn label(n: i64) -> String { f"v{n}" }
fn main() {
    par {
        let sa = label(1);
        let sb = label(2);
    }
    println(sa);
    println(sb);
}
"#,
            "par_block_heap_bindings_escape_no_double_free",
        );
    }

    /// B-2026-07-11-26: a fresh-temp HEAP-bearing enum scrutinee with a user
    /// `impl Drop`, matched in an if-let that MOVES the heap payload into a
    /// binding. The user Drop body runs (side effect `D`) AND the moved-out Vec
    /// is freed exactly once — the binding owns it, the enum user-drop wrapper
    /// runs only the body (its field handoff is struct-only), and item-B's
    /// field cleanup is suppressed for the moved-in field. Asserts no leak
    /// (LSan) and no use-after-free / double-free (ASAN) — the class the new
    /// user-drop registration on materialized enum scrutinees could regress.
    #[test]
    fn asan_freshtemp_enum_scrutinee_user_drop_no_double_free() {
        assert_clean_asan_run(
            "enum Msg { Text(Vec[i64]), Empty }\n\
             impl Drop for Msg { fn drop(mut ref self) { println(\"D\"); } }\n\
             fn mk(hit: bool) -> Msg { if hit { Msg.Text([1, 2, 3]) } else { Msg.Empty } }\n\
             fn main() {\n\
                 if let Msg.Text(v) = mk(true) { println(f\"{v.len()}\"); } else { println(\"m\"); }\n\
             }",
            &["3", "D"],
            "freshtemp_enum_scrutinee_user_drop_no_double_free",
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
            eprintln!("[{label}] parse errors: {:?}", parsed.errors);
            return None;
        }
        // Mirror the real CLI pipeline (lib.rs / cli.rs) and the codegen
        // harness (`tests/codegen.rs::run_program_capturing_inner`): desugar
        // runs between parse and resolve. It synthesizes `#[derive(...)]`
        // bodies (e.g. `#[derive(Default)]` → the inherent `Type.default`
        // impl) and expands comptime — without it a derive-dependent program
        // (std.mem `take[T: Default]`'s `T.default()` dispatch) miscompiles to
        // the const-0 fallback and double-frees, an ASAN-harness-only artifact
        // absent from shipped binaries.
        karac::desugar_program(&mut parsed.program);
        let resolved = karac::resolve(&parsed.program);
        let typed = karac::typecheck(&parsed.program, &resolved);
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
            eprintln!("[{label}] compile_to_object_with_options failed: {e}");
            return None;
        }
        if !Path::new(&obj_path).exists() {
            eprintln!("[{label}] object file missing after compile_to_object");
            return None;
        }
        if let Err(e) =
            link_executable_with_sanitizer(&obj_path, &exe_path, &["-fsanitize=address"])
        {
            eprintln!("[{label}] link_executable_with_sanitizer failed: {e}");
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

    // ── Slice A: auto-par return slots, move-only no-double-drop ──
    //
    // Phase-7 Slice A (Par codegen: return values, 2026-05-09) lifts
    // the slice-2 `group_defines_binding_used_outside` gate by
    // materializing a parent-allocated return struct and per-branch
    // slot writes. Decision (iii) of the slice locks in move-only
    // slot semantics — the branch's `scope_cleanup_actions` are
    // discarded on `emit_par_branch_fn` exit so destructor-bearing
    // values bit-copied through the slot don't double-drop, and the
    // parent's `track_vec_var` is the unique cleanup owner. This
    // test exercises that contract under ASAN with destructor-bearing
    // `Vec[i64]` slot values: four branches each construct a fresh
    // `Vec[i64]`, the parent reads each back from its slot via the
    // synthesized `__karac_ParGroup_*_Returns` struct, sums their
    // lengths into the printed result, and the parent's scope-exit
    // cleanup releases the four heap buffers exactly once.

    #[test]
    fn test_auto_par_returns_no_use_after_move_no_double_drop() {
        // Each `read_*` builds a fresh `Vec[i64]` of three elements;
        // the parent sums the four `.len()` values and prints `12`.
        // Disjoint resources (`R0`..`R3`) make the four reads eligible
        // for auto-par grouping; the typed return value forces the
        // slot mechanism to fire (slice 2 would have dropped the
        // group via the use-outside gate).
        assert_clean_asan_run_with_concurrency(
            r#"
effect resource R0;
effect resource R1;
effect resource R2;
effect resource R3;

fn make_v0() -> Vec[i64] reads(R0) {
    let mut v: Vec[i64] = Vec.new();
    v.push(10_i64);
    v.push(20_i64);
    v.push(30_i64);
    v
}
fn make_v1() -> Vec[i64] reads(R1) {
    let mut v: Vec[i64] = Vec.new();
    v.push(11_i64);
    v.push(21_i64);
    v.push(31_i64);
    v
}
fn make_v2() -> Vec[i64] reads(R2) {
    let mut v: Vec[i64] = Vec.new();
    v.push(12_i64);
    v.push(22_i64);
    v.push(32_i64);
    v
}
fn make_v3() -> Vec[i64] reads(R3) {
    let mut v: Vec[i64] = Vec.new();
    v.push(13_i64);
    v.push(23_i64);
    v.push(33_i64);
    v
}

fn main() {
    let v0 = make_v0();
    let v1 = make_v1();
    let v2 = make_v2();
    let v3 = make_v3();
    println(v0.len() + v1.len() + v2.len() + v3.len());
}
"#,
            &["12"],
            "auto_par_returns_no_use_after_move_no_double_drop",
        );
    }

    // ── Compound-payload enum drop-path (Phase 7.2 Slice DP, 2026-05-09) ──
    // Exercises `track_enum_var` + `emit_enum_drop_switch`: a value-type
    // enum binding that goes out of scope without being moved into a
    // downstream consumer must invoke its per-enum drop function, which
    // walks the variant's heap-bearing payload fields and frees their
    // data buffers. Without the slice's machinery these tests would leak
    // (Linux ASAN/LSan) or, on hosts with DP move-suppression bugs,
    // double-free at scope exit.

    #[test]
    fn asan_compound_enum_drop_invokes_string_destructor() {
        // Headline regression gate (DP5). A `String` payload's heap
        // buffer must be freed at scope exit — `__karac_drop_E` runs
        // the cap > 0 ? free(data) shape on the V variant's payload
        // words. Without DP4's drain hook the buffer leaks.
        assert_clean_asan_run(
            r#"
enum E { V(String) }
fn main() {
    let mut s: String = String.new();
    s.push_str("disk full");
    let _e = V(s);
    println(1);
}
"#,
            &["1"],
            "compound_enum_drop_invokes_string_destructor",
        );
    }

    #[test]
    fn asan_compound_enum_drop_invokes_vec_destructor() {
        // Vec[i64] payload — same `cap > 0 ? free(data)` cleanup
        // shape as String, exercised through the second drop-kind
        // entry in `field_drop_kinds`.
        assert_clean_asan_run(
            r#"
enum E { V(Vec[i64]) }
fn main() {
    let mut v: Vec[i64] = Vec.new();
    v.push(1);
    v.push(2);
    v.push(3);
    let _e = V(v);
    println(1);
}
"#,
            &["1"],
            "compound_enum_drop_invokes_vec_destructor",
        );
    }

    #[test]
    fn asan_compound_enum_drop_skips_no_payload_variant() {
        // No-payload variant lands on the default `ret` arm of the
        // tag-switch — no spurious free, no UAF on the unset payload
        // words. `V2` has zero heap-bearing fields, but the enum
        // itself has at least one heap-bearing variant so the drop
        // fn is still synthesized; verifies the per-variant arm
        // structure handles the trivial case correctly.
        assert_clean_asan_run(
            r#"
enum E { V1(String), V2 }
fn main() {
    let _e = V2;
    println(1);
}
"#,
            &["1"],
            "compound_enum_drop_skips_no_payload_variant",
        );
    }

    #[test]
    fn asan_compound_enum_drop_handles_mixed_width_variants() {
        // Mixed-width: V1(i64) at one tag, V2(String) at another.
        // Constructing each in turn must route through the right
        // cleanup arm — V1's primitive payload triggers no work,
        // V2's String payload frees the buffer. Each construction
        // is in a nested scope to test the per-scope drain timing
        // (the heap String buffer is freed at the inner block's
        // close, not deferred to `main`'s exit).
        assert_clean_asan_run(
            r#"
enum E { V1(i64), V2(String) }
fn main() {
    {
        let _a = V1(42);
    }
    {
        let mut s = String.new();
        s.push_str("hello");
        let _b = V2(s);
    }
    println(1);
}
"#,
            &["1"],
            "compound_enum_drop_handles_mixed_width_variants",
        );
    }

    #[test]
    fn asan_let_bound_enum_heap_payload_moved_out_no_double_free() {
        // #9 (phase-12 self-hosting): a bare `let`-bound enum whose active
        // variant carries a heap payload, moved OUT of the binding by `return`
        // (`fn make() { let e = E.A(..); e }`) or by `let g = f`, transfers
        // ownership — the source's `EnumDrop` is suppressed (cap-zeroed) so
        // only the consumer frees. Without the fix the source double-frees the
        // String buffer (use-after-free → SIGTRAP / ASAN double-free). Covers
        // BOTH fixed move paths plus the non-heap `N` variant and a loop (to
        // surface a missing source-suppression OR a missing consumer free).
        // NOTE: the by-value-call-arg-then-transfer path (passing the enum to
        // a fn that re-wraps it into its return) is the SEPARATE, general
        // blocker #14 (it double-frees for structs too) and is NOT covered.
        assert_clean_asan_run(
            r#"
enum E { A(String), B(i64, String), N(i64) }
fn make(tag: i64) -> E {
    if tag == 0 {
        let e = E.A("alpha".to_string());
        e
    } else if tag == 1 {
        let e = E.B(7, "beta".to_string());
        e
    } else {
        let e = E.N(99);
        e
    }
}
fn main() {
    // return-of-let-bound-enum, consumed by an INLINE match (not re-transferred
    // through another call — that transfer path is #14, not covered here).
    let r = make(0);
    match r { A(s) => println(s), B(n, s) => { println(n.to_string()); println(s); } N(x) => println(x.to_string()) }
    let r1 = make(1);
    match r1 { A(s) => println(s), B(n, s) => { println(n.to_string()); println(s); } N(x) => println(x.to_string()) }
    let r2 = make(2);
    match r2 { A(s) => println(s), B(n, s) => { println(n.to_string()); println(s); } N(x) => println(x.to_string()) }
    // `let g = f` enum move — g is the sole owner, f's drop suppressed.
    let f = make(0);
    let g = f;
    match g { A(s) => println(s), B(n, s) => { println(n.to_string()); println(s); } N(x) => println(x.to_string()) }
    // loop: each iteration's return-bound enum frees exactly once.
    let mut i = 0;
    while i < 3 {
        let x = make(0);
        match x { A(s) => println(s), B(n, s) => { println(n.to_string()); println(s); } N(y) => println(y.to_string()) }
        i = i + 1;
    }
}
"#,
            &[
                "alpha", "7", "beta", "99", "alpha", "alpha", "alpha", "alpha",
            ],
            "let_bound_enum_heap_payload_moved_out_no_double_free",
        );
    }

    #[test]
    fn asan_byvalue_aggregate_param_transferred_out_no_double_free() {
        // #14 (phase-12 self-hosting): an owned by-value aggregate (struct OR
        // enum) param moved into a call that transfers it OUT (into the callee's
        // return value) used to double-free — the caller's source binding and
        // the returned value aliased the same heap buffer and BOTH freed it.
        // The param is now entry-deep-copied + callee-owned (param_own.rs), so
        // each owns an independent buffer. Covers, under a loop (per-iteration
        // single-free):
        //   * direct enum return (`wrap(e) -> E { e }`),
        //   * struct consumed into a returned struct literal (`Wrap { t: t }`),
        //   * the lexer's bootstrap shape — an enum param wrapped into a returned
        //     struct then destructured (`make_spanned(token)`),
        //   * read-then-reuse of the source (`take(x); take(x)`) — entry-copy
        //     keeps the caller's binding live (the reason the fix is entry-copy,
        //     not a caller-side move).
        assert_clean_asan_run(
            r#"
enum E { A(String), N(i64) }
struct Inner { s: String }
struct Wrap { t: Inner }
struct Spanned { tok: E, off: i64 }
fn wrap_enum(e: E) -> E { e }
fn wrap_struct(t: Inner) -> Wrap { Wrap { t: t } }
fn make_spanned(t: E, o: i64) -> Spanned { Spanned { tok: t, off: o } }
fn read_struct(v: Inner) { if v.s.len() > 99999 { println(v.s); } }
fn main() {
    let mut i: i64 = 0;
    while i < 4 {
        let f = E.A(f"a-{i}");
        let g = wrap_enum(f);
        match g { A(s) => { if s.len() > 99999 { println(s); } } N(n) => println(n.to_string()) }

        let x = Inner { s: f"x-{i}" };
        let w = wrap_struct(x);
        if w.t.s.len() > 99999 { println(w.t.s); }

        let t = E.A(f"t-{i}");
        let sp = make_spanned(t, i);
        match sp.tok { A(name) => { if name.len() > 99999 { println(name); } } N(n) => println(n.to_string()) }

        let y = Inner { s: f"y-{i}" };
        read_struct(y);
        read_struct(y);

        i = i + 1;
    }
    println("done");
}
"#,
            &["done"],
            "byvalue_aggregate_param_transferred_out_no_double_free",
        );
    }

    #[test]
    fn asan_struct_with_direct_enum_field_no_leak_no_double_free() {
        // #15 (phase-12 self-hosting): a non-shared struct's synthesized drop
        // used to IGNORE enum-typed fields (an enum's LLVM layout is all-i64
        // words, invisible to the type-driven nested-aggregate pass), leaking
        // the live variant's String/Vec payload at the owning struct's scope
        // exit. `emit_struct_drop_synthesis` now invokes the enum's own
        // `__karac_drop_<E>` switch on a DIRECT enum field (Linux LSan catches a
        // regression of the leak; the `Span` shape mirrors the bootstrap's
        // `SpannedToken { tok: Token, .. }`).
        //
        // #15 is NOT coupled to a #14 double-free: under the caller-retains
        // model, a by-value aggregate param is UNTRACKED in the callee, so only
        // ONE tracked binding (the caller's source, or the transferred-out
        // result) ever frees the enum payload — freeing the enum field at drop
        // does not introduce an alias double-free. Verified empirically across
        // direct-return transfer-out, read-then-reuse, and consume-and-drop.
        // (The struct->struct->enum NESTED leak — `Wrap { sp: Span }` — is a
        // pre-existing, deeper instance left to #18; it is deliberately NOT
        // exercised here so Linux `detect_leaks=1` stays green.)
        assert_clean_asan_run(
            r#"
enum Tok { Id(String), Int(i64) }
struct Span { tok: Tok, off: i64 }
fn wrap(s: Span) -> Span { s }
fn make_spanned(t: Tok, o: i64) -> Span { Span { tok: t, off: o } }
fn peek(s: ref Span) -> i64 { s.off }
fn sink(s: String) -> i64 { s.len() }
fn drop_only(s: Span) { if s.off > 99999 { println(s.off.to_string()); } }
fn main() {
    let mut i: i64 = 0;
    let mut acc: i64 = 0;
    while i < 4 {
        // Leak path: built, kept live (off read), dropped without destructure.
        let a = Span { tok: Tok.Id(f"a-{i}"), off: i };
        if a.off > 99999 { println("never"); }

        // `match spanned.tok` that CONSUMES the bound payload (moves it into
        // `sink`) — the bootstrap pattern. The owning struct's drop must skip
        // the consumed field (the double-free #15 had to suppress); `sink`
        // owning + struct drop freeing the same buffer would abort under ASAN.
        let b = Span { tok: Tok.Id(f"b-{i}"), off: i };
        let c = wrap(b);
        match c.tok { Id(s) => { acc = acc + sink(s); } Int(n) => { acc = acc + n; } }

        // The `make_spanned(token)` shape: a callee-owned enum param wrapped
        // into a returned struct literal, then field-matched + consumed.
        let t = Tok.Id(f"t-{i}");
        let sp = make_spanned(t, i);
        match sp.tok { Id(s) => { acc = acc + sink(s); } Int(n) => { acc = acc + n; } }

        // Local struct, field-match + consume (no function in the path).
        let d = Span { tok: Tok.Id(f"d-{i}"), off: i };
        match d.tok { Id(s) => { acc = acc + sink(s); } Int(n) => { acc = acc + n; } }

        // Read-then-reuse: forces caller-retains aliasing (the source must
        // survive two by-value uses) — a missing entry-copy here would crash;
        // #15 freeing the enum field must still free it exactly once.
        let r = Span { tok: Tok.Id(f"r-{i}"), off: i };
        let x = peek(r);
        let y = peek(r);
        if x + y > 99999 { println("never"); }

        // Consume-and-drop (no transfer) of a struct-with-enum-field.
        let e = Span { tok: Tok.Id(f"e-{i}"), off: i };
        drop_only(e);

        i = i + 1;
    }
    if acc > 99999 { println("never"); }
    println("done");
}
"#,
            &["done"],
            "struct_with_direct_enum_field_no_leak_no_double_free",
        );
    }

    #[test]
    fn asan_struct_nested_enum_leaf_no_leak_no_double_free() {
        // #18 (phase-12 self-hosting): a struct whose only heap is TRANSITIVELY
        // inside an enum nested under ANOTHER struct field — `Wrap { sp: Span }`
        // where `Span { tok: Tok }` and `Tok` is heap-bearing. #15 freed only a
        // DIRECT enum field; the nested path went through the type-driven
        // `emit_aggregate_heap_field_frees`, which is enum-blind (an enum's
        // layout is all-i64 words). `emit_struct_drop_synthesis` now routes a
        // NAMED nested struct field through that struct's own
        // `__karac_drop_struct_<S>` (which post-#15 frees its enum fields), so
        // `Wrap`'s drop reaches `sp.tok`'s payload. Linux LSan catches a
        // regression of the leak; ASAN everywhere catches the double-free the
        // nested match-consume could introduce.
        //
        // Double-free coupling (mirrors #15's struct-field-match sub-fix, one
        // level deeper): once `Wrap`'s drop frees `sp.tok`, a `match c.sp.tok`
        // arm that CONSUMES the bound payload would double-free unless the match
        // suppression cap-zeros the consumed field in the SOURCE struct.
        // `suppress_destructured_struct_field_enum_cleanup` walks the full
        // `ident.f1.f2…` field-access chain for exactly this case.
        //
        // All payloads are the `Id(String)` variant and every `Int`/numeric arm
        // is guarded behind an impossible `> 99999`, so no `to_string` temp is
        // ever materialized — keeping Linux `detect_leaks=1` green against the
        // separate, pre-existing `println(x.to_string())` argument-temp leak.
        assert_clean_asan_run(
            r#"
enum Tok { Id(String), Int(i64) }
struct Span { tok: Tok, off: i64 }
struct Wrap { sp: Span, hi: i64 }
struct Deep { w: Wrap, tag: i64 }
fn sink(s: String) -> i64 { s.len() }
fn mk_wrap(n: i64) -> Wrap { Wrap { sp: Span { tok: Tok.Id(f"w-{n}"), off: n }, hi: n + 1 } }
fn fwd(w: Wrap) -> Wrap { w }
fn peek(w: ref Wrap) -> i64 { w.hi }
fn main() {
    let mut i: i64 = 0;
    let mut acc: i64 = 0;
    while i < 4 {
        // Nested leak path: Wrap built, kept live (hi read), dropped WITHOUT
        // destructure — Wrap's drop must free sp.tok's payload (the #18 leak).
        let a = Wrap { sp: Span { tok: Tok.Id(f"a-{i}"), off: i }, hi: i };
        if a.hi > 99999 { println("never"); }

        // Struct-literal MOVE: a Span local moved into a Wrap literal, then the
        // Wrap dropped undestructured. The source `span` must not also free the
        // enum payload (move-suppression vs the now-active nested drop).
        let span = Span { tok: Tok.Id(f"s-{i}"), off: i };
        let m = Wrap { sp: span, hi: i };
        if m.hi > 99999 { println("never"); }

        // Transfer-out + nested match-consume (the double-free risk): the bound
        // String moves into `sink`; `c`'s drop must skip the consumed field.
        let b = mk_wrap(i);
        let c = fwd(b);
        match c.sp.tok { Id(s) => { acc = acc + sink(s); } Int(n) => { if n > 99999 { println("never"); } } }

        // Local nested match-consume (no function in the path).
        let d = Wrap { sp: Span { tok: Tok.Id(f"d-{i}"), off: i }, hi: i };
        match d.sp.tok { Id(s) => { acc = acc + sink(s); } Int(n) => { if n > 99999 { println("never"); } } }

        // Read-then-reuse a Wrap (caller-retains aliasing), then drop it
        // undestructured — the nested payload must be freed exactly once.
        let r = mk_wrap(i);
        let x = peek(r);
        let y = peek(r);
        if x + y > 99999 { println("never"); }

        // Three-level nesting: Deep -> Wrap -> Span -> Tok, dropped
        // undestructured — the recursive struct-drop routing must descend.
        let deep = Deep { w: mk_wrap(i), tag: i };
        if deep.tag > 99999 { println("never"); }

        i = i + 1;
    }
    if acc > 99999 { println("never"); }
    println("done");
}
"#,
            &["done"],
            "struct_nested_enum_leaf_no_leak_no_double_free",
        );
    }

    #[test]
    fn asan_struct_tuple_enum_leaf_no_leak_no_double_free() {
        // #21 (phase-12 self-hosting): a struct field that is an anonymous TUPLE
        // whose only heap is inside an enum leaf — `struct H { pe: (Tok, i64) }`
        // with heap enum `Tok`. The struct drop's `NestedTuple` path now frees
        // the tuple's enum leaf (the bounded leak #21 reported), paired with
        // cap-zero suppression at every move-out site and entry-copy of
        // heap-bearing tuple params (the cross-function P6 case). This stresses
        // the whole move-out matrix in one loop: undestructured drop (the leak),
        // full-tuple destructure + consume, direct tuple-index match, tuple-index
        // let-move, whole-tuple let + cross-fn consume, whole-tuple arg, enum
        // arg, whole-struct move, nested struct/tuple, and a tuple-literal arg —
        // each clean on main; with the partial #21 fix the consume shapes
        // double-freed. Every numeric arm is guarded behind an impossible
        // `> 99999` so no `to_string` temp is materialized (keeps Linux LSan
        // green vs the separate println-temp leak).
        assert_clean_asan_run(
            r#"
enum Tok { Id(String), Int(i64) }
struct Inner { tok: Tok, k: i64 }
struct H { pe: (Tok, i64), tag: i64 }
struct Hn { pn: ((Tok, i64), i64), tag: i64 }
struct Hs { ps: (Inner, i64), tag: i64 }
struct Hv { pv: (Vec[i64], i64), tag: i64 }
fn mk(n: i64) -> H { H { pe: (Tok.Id(f"e-{n}"), n), tag: n } }
fn mkn(n: i64) -> Hn { Hn { pn: ((Tok.Id(f"n-{n}"), n), n), tag: n } }
fn mks(n: i64) -> Hs { Hs { ps: (Inner { tok: Tok.Id(f"s-{n}"), k: n }, n), tag: n } }
fn mkv(n: i64) -> Hv { let mut v: Vec[i64] = Vec.new(); v.push(n); Hv { pv: (v, n), tag: n } }
fn sink(s: String) -> i64 { s.len() }
fn sinkv(v: Vec[i64]) -> i64 { v.len() }
fn sinkt(p: (Tok, i64)) -> i64 { match p.0 { Id(s) => s.len(), Int(z) => { if z > 99999 { 1 } else { 0 } } } }
fn sinke(t: Tok) -> i64 { match t { Id(s) => s.len(), Int(z) => { if z > 99999 { 1 } else { 0 } } } }
fn sinki(p: (Inner, i64)) -> i64 { match p.0.tok { Id(s) => s.len(), Int(z) => { if z > 99999 { 1 } else { 0 } } } }
fn main() {
    let mut i: i64 = 0;
    let mut acc: i64 = 0;
    while i < 8 {
        // P0 — undestructured drop (the #21 leak target).
        let a = mk(i);
        if a.tag > 99999 { println("never"); }

        // P1 — full-tuple destructure then consume the enum leaf via match.
        let b = mk(i);
        let (t, n) = b.pe;
        match t { Id(s) => { acc = acc + sink(s); } Int(z) => { if z > 99999 { println("never"); } } }
        acc = acc + n;

        // P3 — direct tuple-index match scrutinee.
        let c = mk(i);
        match c.pe.0 { Id(s) => { acc = acc + sink(s); } Int(z) => { if z > 99999 { println("never"); } } }

        // P4 — tuple-index let-move into an enum binding, then consume.
        let d = mk(i);
        let x = d.pe.0;
        match x { Id(s) => { acc = acc + sink(s); } Int(z) => { if z > 99999 { println("never"); } } }

        // P5 / P6 — whole-tuple by value to a fn that matches an element
        // internally (cross-boundary; needs entry-copy of the tuple param).
        let e = mk(i);
        let pp = e.pe;
        acc = acc + sinkt(pp);
        let f = mk(i);
        acc = acc + sinkt(f.pe);

        // P7 — enum arg extracted from a tuple field (caller-retained).
        let g = mk(i);
        acc = acc + sinke(g.pe.0);

        // P8 — whole-struct move then drop undestructured.
        let h = mk(i);
        let h2 = h;
        if h2.tag > 99999 { println("never"); }

        // Tuple-literal arg with an enum element (caller-temp + entry-copy).
        acc = acc + sinkt((Tok.Id(f"L-{i}"), i));

        // Nested tuple / nested struct in a tuple, undestructured + consumed.
        let nt = mkn(i);
        if nt.tag > 99999 { println("never"); }
        match nt.pn.0.0 { Id(s) => { acc = acc + sink(s); } Int(z) => { if z > 99999 { println("never"); } } }
        let ns = mks(i);
        acc = acc + sinki(ns.ps);

        // Direct-Vec tuple regression (must stay clean).
        let v = mkv(i);
        let (vv, vn) = v.pv;
        acc = acc + sinkv(vv) + vn;

        i = i + 1;
    }
    if acc > 999999 { println("never"); }
    println("done");
}
"#,
            &["done"],
            "struct_tuple_enum_leaf_no_leak_no_double_free",
        );
    }

    #[test]
    fn asan_inline_enum_field_struct_arg_no_leak() {
        // #22 (phase-12 self-hosting) — the #19 fresh-temp tail. An enum-field
        // struct constructed INLINE at a call site (`consume(W { tok: Tok.Id(..) })`,
        // no caller binding) whose callee CONSUMES the enum internally (`match
        // w.tok`) triggers the callee's entry-copy (`make_aggregate_param_callee_
        // owned`): the callee deep-copies the enum payload at entry and frees only
        // its own copy, leaving the inline temp's original heap for the caller. A
        // let-bound arg gets that caller drop at its binding site, but the inline
        // temp had no owner and leaked once per call. The enum payload is invisible
        // to the LLVM-type `aggregate_has_heap_field` gate (all-i64 words), so
        // `track_inline_owned_aggregate_arg` skipped the struct-literal arm; the fix
        // adds a SOURCE-level drop-heap gate, restricted to copy-supported structs
        // (an independent copy provably exists → distinct buffers, never a
        // double-free). Stresses: the bare enum-leaf struct arg (free fn + method
        // site), an enum leaf nested one struct deeper, and a direct-Vec struct arg
        // (regression — already worked via the LLVM gate, must stay clean). f-string
        // payloads keep the heap non-foldable; numeric arms guard behind `> 99999`.
        assert_clean_asan_run(
            r#"
enum Tok { Id(String), Int(i64) }
struct W { tok: Tok, n: i64 }
struct Inner { tok: Tok, k: i64 }
struct Outer { inner: Inner, n: i64 }
struct V { xs: Vec[i64], n: i64 }
struct Sink { total: i64 }
fn consume(w: W) -> i64 { match w.tok { Id(s) => s.len(), Int(z) => { if z > 99999 { 1 } else { 0 } } } }
fn consume_outer(o: Outer) -> i64 { match o.inner.tok { Id(s) => s.len(), Int(z) => { if z > 99999 { 1 } else { 0 } } } }
fn consume_vec(v: V) -> i64 { v.xs.len() }
fn mkv(n: i64) -> Vec[i64] { let mut a: Vec[i64] = Vec.new(); a.push(n); a.push(n + 1); a }
impl Sink {
    fn take(mut ref self, w: W) -> i64 { match w.tok { Id(s) => s.len(), Int(z) => { if z > 99999 { 1 } else { 0 } } } }
}
fn main() {
    let mut i: i64 = 0;
    let mut acc: i64 = 0;
    let mut sk = Sink { total: 0 };
    while i < 8 {
        // Bare enum-leaf struct, constructed inline as a free-fn arg (the #22 leak).
        acc = acc + consume(W { tok: Tok.Id(f"a-{i}"), n: i });

        // Same shape at a METHOD call site (shared arg-lowering path).
        acc = acc + sk.take(W { tok: Tok.Id(f"m-{i}"), n: i });

        // Enum leaf nested one struct deeper, inline.
        acc = acc + consume_outer(Outer { inner: Inner { tok: Tok.Id(f"o-{i}"), k: i }, n: i });

        // Direct-Vec struct arg, inline (regression — must stay clean).
        acc = acc + consume_vec(V { xs: mkv(i), n: i });

        i = i + 1;
    }
    if acc > 999999 { println("never"); }
    println("done");
}
"#,
            &["done"],
            "inline_enum_field_struct_arg_no_leak",
        );
    }

    #[test]
    fn asan_struct_tuple_map_leaf_no_double_free() {
        // #23 (phase-12 self-hosting) — a `Map` leaf inside a tuple inside a struct
        // field, the one corruption-class residual of #21. `Map`s are caller-retains
        // (the origin binding frees; the callee never does), so #21's `NestedTuple`
        // struct drop added a SECOND freer for a Map tuple leaf and a local `Map`
        // folded into a tuple-in-struct-field double-freed (origin binding's
        // `FreeMapHandle` + the struct drop). The fix transfers the handle to the
        // tuple's owner at construction (drop the origin's `FreeMapHandle` — Part B),
        // gives a Map-owning tuple VAR a `TypeExpr`-driven drop (Part A) and
        // suppresses it when moved into a struct field (Part C1), so the struct's
        // drop is the sole freer. Coupled fix: the Map drop's
        // `karac_map_free_with_drop_vec` K/V flags are now derived from the element
        // types — the old hardcoded `(1, 1)` read offset-16 of an 8-byte scalar key
        // as a bogus `cap` and freed the key VALUE as a pointer (corruption on any
        // OCCUPIED `Map[i64, i64]`; `B-2026-06-13-18`, which also fixed the same bug
        // in the regular struct-field Map drop — covered here by the `Sp` field).
        //
        // Loop-stressed matrix: scalar tuple-Map field (origin-suppress + flag fix),
        // by-value consume (caller-retains, no second free), String-key field
        // (drop_key=1, leak if mis-flagged), Vec-value field (drop_val=1), a tuple
        // var moved into a struct field, a bare tuple var owning a Map (Part A drop),
        // and a plain `Map[String, i64]` struct field (regular drop path). The
        // f-string keys keep each entry's heap non-foldable so Linux LSan sees a real
        // leak if any flag/transfer is wrong; the inserts make the scalar maps
        // occupied so the old `(1, 1)` flags would abort here.
        assert_clean_asan_run(
            r#"
struct Hi { m: (Map[i64, i64], i64) }
struct Hs { m: (Map[String, i64], i64) }
struct Hv { m: (Map[i64, Vec[i64]], i64) }
struct Sp { m: Map[String, i64] }
fn ci(p: (Map[i64, i64], i64)) -> i64 { let (mm, n) = p; mm.len() + n }
fn main() {
    let mut i: i64 = 0;
    let mut acc: i64 = 0;
    while i < 8 {
        // Scalar tuple-Map field, local source, dropped undestructured
        // (#23 double-free + the occupied-scalar (1,1)-flag corruption).
        let mut a: Map[i64, i64] = Map.new();
        a.insert(i, i); a.insert(i + 100, i);
        let hi = Hi { m: (a, i) };

        // Same, but consumed by value (caller-retains; struct drop is sole freer).
        let mut a2: Map[i64, i64] = Map.new();
        a2.insert(i, i);
        let hi2 = Hi { m: (a2, i) };
        acc = acc + ci(hi2.m);

        // String-key tuple-Map field (drop_key=1 — free the key buffers, no leak).
        let mut s: Map[String, i64] = Map.new();
        s.insert(f"k-{i}", i); s.insert(f"j-{i}", i);
        let hs = Hs { m: (s, i) };

        // Vec-value tuple-Map field (drop_val=1 — free the value buffers).
        let mut v: Map[i64, Vec[i64]] = Map.new();
        let mut vv: Vec[i64] = Vec.new(); vv.push(i);
        v.insert(i, vv);
        let hv = Hv { m: (v, i) };

        // Tuple VAR moved into a struct field (Part A drop + Part C1 suppression).
        let mut b: Map[i64, i64] = Map.new();
        b.insert(i, i);
        let pair = (b, i);
        let hi3 = Hi { m: pair };

        // Bare tuple var owning a Map, dropped undestructured (Part A drop).
        let mut d: Map[i64, i64] = Map.new();
        d.insert(i, i);
        let t = (d, i);

        // Plain Map[String,i64] struct field — the regular MapOrSet drop + flag fix.
        let mut c: Map[String, i64] = Map.new();
        c.insert(f"c-{i}", i);
        let sp = Sp { m: c };

        i = i + 1;
    }
    if acc > 999999 { println("never"); }
    println("done");
}
"#,
            &["done"],
            "struct_tuple_map_leaf_no_double_free",
        );
    }

    #[test]
    fn asan_deep_tuple_index_match_no_double_free() {
        // #25 (phase-12 self-hosting, B-2026-06-14-4) — the read-path fix that
        // lets `match h.ps.0.tok { Id(s) => … }` (a `<struct>.tuplefield.0.<enum
        // field>` scrutinee) compile must not introduce a double-free: the arm
        // CONSUMES the enum payload (`s`) that the owning `h`'s tuple drop also
        // frees. The #21 tuple-index match suppression
        // (`suppress_destructured_struct_field_enum_cleanup` via the
        // `place_chain_type_name` TupleIndex hop) cap-zeros the source, so `s`
        // is the sole owner. Loop-stressed over consume (`s.len()`), borrow
        // (`println(s)`), and the scalar second element (`h.ps.1`, regression).
        // f-string payloads keep the heap non-foldable so Linux LSan sees a real
        // leak if the suppression mis-fires. The heap `let`-binding form
        // (`let inr = h.ps.0`) is a SEPARATE pre-existing move-out double-free
        // (tracker #27) and is deliberately not exercised here.
        assert_clean_asan_run(
            r#"
enum Tok { Id(String), Num(i64) }
struct Inner { tok: Tok, n: i64 }
struct Hs { ps: (Inner, i64) }
fn main() {
    let mut i: i64 = 0;
    let mut acc: i64 = 0;
    while i < 8 {
        let h = Hs { ps: (Inner { tok: Tok.Id(f"id-{i}"), n: i }, i) };
        // Consume arm — `s` owns the payload; h's tuple drop must skip it.
        match h.ps.0.tok {
            Id(s) => { acc = acc + s.len(); }
            Num(n) => { acc = acc + n; }
        }
        // Borrow arm over a fresh value.
        let h2 = Hs { ps: (Inner { tok: Tok.Id(f"v-{i}"), n: i }, i) };
        match h2.ps.0.tok {
            Id(s) => { acc = acc + s.len(); }
            Num(n) => { acc = acc + n; }
        }
        // Scalar second element (regression guard).
        acc = acc + h.ps.1;
        i = i + 1;
    }
    if acc > 999999 { println("never"); }
    println("done");
}
"#,
            &["done"],
            "deep_tuple_index_match_no_double_free",
        );
    }

    #[test]
    fn asan_tuple_index_map_receiver_no_leak() {
        // #26 (phase-12 self-hosting, B-2026-06-14-6) — methods on a Map TUPLE
        // element (`h.m.0.len()` / `.get` / `.insert`) route through a synth
        // identifier aliasing the owning struct's handle slot (the tuple element
        // is GEP'd in place via `field_chain_place_ptr`). The synth must NOT take
        // ownership: the owning `h` is the sole freer of the Map, and reads (len/
        // get/contains_key) borrow the handle while an in-place insert mutates it.
        // Loop-stressed with String keys (heap, non-foldable via f-strings) so
        // Linux LSan catches a leak if the synth mis-registers a second freer, and
        // an in-place insert each iteration so a copy-instead-of-alias would drop
        // the mutation (and leak/UAF the copy).
        assert_clean_asan_run(
            r#"
struct H { m: (Map[String, i64], i64) }
fn mkm(i: i64) -> Map[String, i64] {
    let mut m: Map[String, i64] = Map.new();
    m.insert(f"k{i}", i); m.insert(f"j{i}", i);
    return m;
}
fn main() {
    let mut i: i64 = 0;
    let mut acc: i64 = 0;
    while i < 8 {
        let mut h = H { m: (mkm(i), i) };
        // Read methods through the tuple element (borrow the handle).
        acc = acc + h.m.0.len();
        if h.m.0.contains_key(f"k{i}") { acc = acc + 1; }
        // In-place mutation through the tuple element.
        h.m.0.insert(f"x{i}", i);
        acc = acc + h.m.0.len();
        acc = acc + h.m.1;
        i = i + 1;
    }
    if acc > 999999 { println("never"); }
    println("done");
}
"#,
            &["done"],
            "tuple_index_map_receiver_no_leak",
        );
    }

    #[test]
    fn asan_map_local_bind_from_place_no_double_free() {
        // #28 (phase-12 self-hosting, B-2026-06-14-9) — a Map bound to a LOCAL
        // from a place source (`let mm = s.m`) now registers its dispatch
        // side-tables. The binding ALIASES the source handle (Maps are
        // caller-retains), so `mm` must NOT register a second `FreeMapHandle`:
        // the owning `s` is the sole freer. The fix only populates the dispatch
        // tables (`register_var_from_type_expr`), and the let path's
        // `track_map_var` is gated on a fresh-handle RHS (clone/union/…) which a
        // place source is not — so no second cleanup is queued. Loop-stressed with
        // String keys (heap, non-foldable) so Linux LSan catches a leak if the
        // owner's free were suppressed, and ASAN catches a double-free if `mm`
        // wrongly took a cleanup. Includes an in-place mutation through `mm`.
        assert_clean_asan_run(
            r#"
struct S { m: Map[String, i64] }
fn mkm(i: i64) -> Map[String, i64] {
    let mut m: Map[String, i64] = Map.new();
    m.insert(f"k{i}", i); m.insert(f"j{i}", i);
    return m;
}
fn main() {
    let mut i: i64 = 0;
    let mut acc: i64 = 0;
    while i < 8 {
        let s = S { m: mkm(i) };
        let mut mm = s.m;
        acc = acc + mm.len();
        if mm.contains_key(f"k{i}") { acc = acc + 1; }
        // In-place mutation through the bound local (mutates the shared handle).
        mm.insert(f"x{i}", i);
        acc = acc + mm.len();
        i = i + 1;
    }
    if acc > 999999 { println("never"); }
    println("done");
}
"#,
            &["done"],
            "map_local_bind_from_place_no_double_free",
        );
    }

    #[test]
    fn asan_tuple_elem_bind_move_out_no_double_free() {
        // #27 (phase-12 self-hosting, B-2026-06-14-8) — binding a heap-bearing
        // value OUT of a tuple element double-freed at scope exit: the binding's
        // drop AND the owning struct's `NestedTuple` tuple drop both freed the
        // shared buffer. `let inr = h.ps.0` (heap struct moved out — suppressed via
        // `suppress_tuple_index_move_source` → `zero_struct_move_caps`) and
        // `let tk = h.ps.0.tok` (enum field moved out through the tuple element —
        // suppressed via `suppress_place_field_enum_move_source` → place-chain GEP +
        // `zero_enum_payload_caps`). Loop-stressed over drop-only, field-read, and
        // match-consume of both forms; f-string payloads keep the heap non-foldable
        // so Linux LSan catches an over-suppression leak (if the source cap-zero
        // also orphaned a still-owned buffer) and ASAN catches the double-free.
        assert_clean_asan_run(
            r#"
enum Tok { Id(String), Num(i64) }
struct Inner { tok: Tok, n: i64 }
struct Hs { ps: (Inner, i64) }
fn main() {
    let mut i: i64 = 0;
    let mut acc: i64 = 0;
    while i < 8 {
        // Struct element moved out, dropped unused (the headline double-free).
        let ha = Hs { ps: (Inner { tok: Tok.Id(f"a{i}"), n: i }, 7) };
        let inr = ha.ps.0;

        // Struct element moved out, enum field CONSUMED via match.
        let hb = Hs { ps: (Inner { tok: Tok.Id(f"b{i}"), n: i }, 7) };
        let inr2 = hb.ps.0;
        match inr2.tok { Id(s) => { acc = acc + s.len(); } Num(n) => { acc = acc + n; } }

        // Enum field moved out THROUGH the tuple element, dropped unused.
        let hc = Hs { ps: (Inner { tok: Tok.Id(f"c{i}"), n: i }, 7) };
        let tk = hc.ps.0.tok;

        // Enum field moved out through the tuple element, CONSUMED.
        let hd = Hs { ps: (Inner { tok: Tok.Id(f"d{i}"), n: i }, 7) };
        let tk2 = hd.ps.0.tok;
        match tk2 { Id(s) => { acc = acc + s.len(); } Num(n) => { acc = acc + n; } }

        i = i + 1;
    }
    if acc > 999999 { println("never"); }
    println("done");
}
"#,
            &["done"],
            "tuple_elem_bind_move_out_no_double_free",
        );
    }

    #[test]
    fn asan_call_result_tuple_var_no_leak() {
        // #24 (phase-12 self-hosting) — a let-bound tuple VAR sourced from a CALL
        // (`let p = ret_tuple(i)`) whose only heap is an enum / Map leaf leaked: the
        // call-result source missed the annotation/literal arms of
        // `tuple_binding_elem_tes`, so no `TypeExpr`-driven drop was registered and
        // `track_tuple_var`'s LLVM walk is enum/Map-blind. The fix recovers the
        // element TEs from the callee's return type (`fn_return_type_exprs`). The
        // coupled fix (`B-2026-06-14-1`) makes the drop-fn memoization key
        // (`type_expr_sig`) generic-args-aware, so `Map[i64,i64]` and
        // `Map[String,i64]` no longer alias one drop fn — a scalar-first program had
        // leaked a later `Map[String,_]`'s keys; a String-first program ran a
        // `drop_key=1` over a scalar map (the #23 garbage-free class).
        //
        // Loop-stressed: call-sourced enum-leaf tuple UNUSED (the leak), the same
        // destructured + consumed (no double-free), call-sourced scalar-map and
        // String-key-map tuples (both UNUSED — the generic-args memo key), and both
        // map shapes used in one loop body so the memo collision would fire. The
        // f-string payloads/keys keep each iteration's heap non-foldable so Linux
        // LSan sees a real leak if any leaf's drop is missing or mis-keyed.
        assert_clean_asan_run(
            r#"
enum Tok { Id(String), Num(i64) }
fn ret_tuple(i: i64) -> (Tok, i64) { return (Tok.Id(f"id{i}"), i); }
fn ret_imap(i: i64) -> (Map[i64, i64], i64) {
    let mut m: Map[i64, i64] = Map.new();
    m.insert(i, i);
    return (m, i);
}
fn ret_smap(i: i64) -> (Map[String, i64], i64) {
    let mut m: Map[String, i64] = Map.new();
    m.insert(f"k{i}", i); m.insert(f"j{i}", i);
    return (m, i);
}
fn use_tok(t: Tok) -> i64 {
    match t { Id(s) => s.len(), Num(n) => n }
}
fn main() {
    let mut i: i64 = 0;
    let mut acc: i64 = 0;
    while i < 8 {
        // Call-sourced enum-leaf tuple var, UNUSED (the #24 leak).
        let p = ret_tuple(i);

        // Call-sourced enum-leaf tuple var, destructured + consumed (no double-free).
        let q = ret_tuple(i + 50);
        let (t, n) = q;
        acc = acc + use_tok(t) + n;

        // Call-sourced scalar-map tuple var, UNUSED (memo key (0,0) flags).
        let im = ret_imap(i);

        // Call-sourced String-key-map tuple var, UNUSED (memo key (1,0) flags) —
        // in the SAME loop body as the scalar map so the old shared memo key would
        // drop this with (0,0) and leak the f-string keys.
        let sm = ret_smap(i);

        i = i + 1;
    }
    if acc > 999999 { println("never"); }
    println("done");
}
"#,
            &["done"],
            "call_result_tuple_var_no_leak",
        );
    }

    #[test]
    fn asan_enum_field_struct_transfer_destructure_no_double_free() {
        // #19 (phase-12 self-hosting): a by-value TRANSFER of an enum-field struct
        // (`let b = wrap(a)`, `wrap(s: Span) -> Span { s }`) followed by a
        // destructure that USES the bound payload double-freed on the old
        // caller-retains path — the transferred result aliased the source's enum
        // buffer, and both struct drops freed it. Entry-copy for enum-field structs
        // (`field_copy_supported` user-enum arm) gives the callee an independent
        // copy, so source and result own distinct buffers. Exercised in a loop with
        // a BORROW arm (`println(s)` keeps the binding's cleanup) and a CONSUME arm
        // (`sink(s)` moves it), one-level and nested two-level (`b.sp.tok`). The
        // `Int` arms are guarded behind an impossible `> 99999` so no `to_string`
        // temp materializes (Linux `detect_leaks=1` stays green vs the separate
        // baseline temp leak).
        assert_clean_asan_run(
            r#"
enum Tok { Id(String), Int(i64) }
struct Span { tok: Tok, off: i64 }
struct Wrap { sp: Span, hi: i64 }
fn wrap(s: Span) -> Span { s }
fn fwd(w: Wrap) -> Wrap { w }
fn sink(s: String) -> i64 { s.len() }
fn main() {
    let mut i: i64 = 0;
    let mut acc: i64 = 0;
    while i < 4 {
        // One-level transfer + BORROW arm.
        let a = Span { tok: Tok.Id(f"a-{i}"), off: i };
        let b = wrap(a);
        match b.tok { Id(s) => println(s), Int(n) => { if n > 99999 { println("never"); } } }
        // One-level transfer + CONSUME arm.
        let c = Span { tok: Tok.Id(f"c-{i}"), off: i };
        let d = wrap(c);
        match d.tok { Id(s) => { acc = acc + sink(s); } Int(n) => { if n > 99999 { println("never"); } } }
        // Nested two-level transfer + BORROW arm.
        let e = Wrap { sp: Span { tok: Tok.Id(f"e-{i}"), off: i }, hi: i };
        let g = fwd(e);
        match g.sp.tok { Id(s) => println(s), Int(n) => { if n > 99999 { println("never"); } } }
        i = i + 1;
    }
    if acc > 99999 { println("never"); }
    println("done");
}
"#,
            &[
                "a-0", "e-0", "a-1", "e-1", "a-2", "e-2", "a-3", "e-3", "done",
            ],
            "enum_field_struct_transfer_destructure_no_double_free",
        );
    }

    #[test]
    fn asan_enum_field_struct_field_move_out_no_double_free() {
        // #19 (phase-12 self-hosting): the bootstrap lexer's `render()` shape —
        // iterate a `Vec[SpannedToken]` and pass each element BY VALUE to a fn that
        // moves the enum field OUT of its (now entry-copied) param into a local
        // (`let tk = t.token; match tk { … }`). The enum-field move-out cap-zeros
        // the source field in the owning struct's slot
        // (`suppress_struct_field_move_into_literal`'s enum arm) so the param's
        // struct drop and the moved-out local's drop free distinct buffers; without
        // it this double-freed (exit 133). All taken arms are `Id(String)` so no
        // `to_string` temp materializes.
        assert_clean_asan_run(
            r#"
enum Tok { Id(String), Eof }
struct Span2 { offset: i64, length: i64 }
struct Span { token: Tok, span: Span2 }
fn render(t: Span) -> String {
    let off = t.span.offset;
    let mut line = f"{off}:";
    let tk = t.token;
    match tk {
        Id(s) => line.push_str(s),
        Eof => line.push_str("eof"),
    }
    line
}
fn build(n: i64) -> Vec[Span] {
    let mut out: Vec[Span] = Vec.new();
    let mut i: i64 = 0;
    while i < n {
        out.push(Span { token: Tok.Id(f"t{i}"), span: Span2 { offset: i, length: 1 } });
        i = i + 1;
    }
    out.push(Span { token: Tok.Eof, span: Span2 { offset: n, length: 0 } });
    out
}
fn main() {
    let toks = build(3_i64);
    for t in toks {
        println(render(t));
    }
    println("done");
}
"#,
            &["0:t0", "1:t1", "2:t2", "3:eof", "done"],
            "enum_field_struct_field_move_out_no_double_free",
        );
    }

    #[test]
    fn asan_struct_pattern_destructure_no_double_free() {
        // #16 (phase-12 self-hosting): a plain struct-pattern match destructure of
        // an OWNED local struct (`match v { S { a, b: _ } => … }`) moves each
        // CONSUMED field's heap payload into the new binding; without
        // `suppress_destructured_struct_pattern_cleanup` the source struct's
        // `__karac_drop_<S>` re-frees the same buffer at scope exit → double-free
        // (exit 134 under guardmalloc). Exercises: flat String fields fully bound,
        // a partial bind (`b: _` — the unconsumed field must STILL be freed by the
        // source drop, so a too-eager suppression would leak it — Linux LSan
        // guards that), a nested-struct field moved whole (transitive cap-zero via
        // `zero_struct_move_caps`), and an enum field moved whole (`zero_enum_
        // payload_caps`). Looped so a per-iteration leak accumulates for LSan.
        assert_clean_asan_run(
            r#"
struct Inner { s: String }
enum Tok { Id(String), Eof }
struct S { a: String, b: String, inner: Inner, tok: Tok, n: i64 }
fn mk(i: i64) -> S {
    let mut x: String = String.new();
    x.push_str("a");
    x.push_str(i.to_string());
    let mut y: String = String.new();
    y.push_str("b");
    y.push_str(i.to_string());
    S { a: x, b: y, inner: Inner { s: "deep".to_string() }, tok: Tok.Id("id".to_string()), n: i }
}
fn main() {
    let mut i: i64 = 0;
    while i < 4 {
        let v = mk(i);
        match v {
            S { a, b: _, inner, tok, n } => {
                let Inner { s } = inner;
                let mut line: String = String.new();
                line.push_str(a);
                line.push_str("|");
                line.push_str(s);
                line.push_str("|");
                match tok { Id(t) => line.push_str(t), Eof => line.push_str("eof") }
                line.push_str("|");
                line.push_str(n.to_string());
                println(line);
            }
        }
        i = i + 1;
    }
    println("done");
}
"#,
            &[
                "a0|deep|id|0",
                "a1|deep|id|1",
                "a2|deep|id|2",
                "a3|deep|id|3",
                "done",
            ],
            "struct_pattern_destructure_no_double_free",
        );
    }

    #[test]
    fn asan_vec_clone_repeat_stresses_scope_cleanup() {
        // Clone in a fresh scope across multiple loop iterations —
        // verifies the scope-exit free fires for each loop-local clone
        // so allocations don't accumulate. ASAN catches a missing free.
        assert_clean_asan_run(
            r#"
fn main() {
    let mut v: Vec[i64] = Vec.new();
    v.push(1_i64);
    v.push(2_i64);
    v.push(3_i64);
    let mut iter: i64 = 0;
    let mut total: i64 = 0;
    while iter < 5_i64 {
        let w: Vec[i64] = v.clone();
        total = total + w.len();
        iter = iter + 1_i64;
    }
    println(total);
}
"#,
            &["15"], // 5 clones × 3 elements each
            "vec_clone_repeat_stresses_scope_cleanup",
        );
    }

    // ── Compound-payload tuple-payload destructure ────────────────
    // Theme 5 (2026-05-10) — heap-bearing element inside a tuple payload
    // survives destructure with no double-free / use-after-free. The
    // String element is constructed at the call site, moved into the
    // variant payload, then re-bound on the destructure side; per-element
    // word reconstruction must hand off ownership cleanly so the buffer
    // is freed exactly once at scope exit.

    #[test]
    fn asan_compound_tuple_payload_string_int() {
        assert_clean_asan_run(
            r#"
enum E { V((String, i64)) }
fn main() {
    let mut s = String.new();
    s.push_str("payload");
    let e = V((s, 7));
    match e {
        V((t, n)) => {
            println(t.len());
            println(n);
        }
    }
}
"#,
            &["7", "7"],
            "compound_tuple_payload_string_int",
        );
    }

    // ── Match-arm Vec/String cleanup (2026-05-13) ─────────────────
    // Per-arm scope frame + `track_vec_var` registration at
    // `bind_pattern_values` together close the leak where a Vec/String
    // extracted from an enum payload (`match opt { Some(v) => ... }`)
    // wasn't tracked for scope-exit cleanup. ASAN catches the leak
    // (Vec data buffer never freed) on the bound-then-discarded path
    // and double-free on the move-out path (`Some(v) => v` returns the
    // buffer; the per-arm move-aware suppression must zero the source's
    // cap so the caller's cleanup is the unique owner).
    //
    // Canonical bfs_sieve-style pattern: `bucket.remove(k)` extracts a
    // `Vec[i64]` from a Map, the match-arm binding receives it, the
    // arm body iterates it via `into_iter` (which doesn't drop in
    // karac today — see `compile_for` Vec/Slice arm), and the per-arm
    // drain frees the data buffer at end of arm.

    #[test]
    fn asan_match_arm_vec_binding_freed_on_arm_exit() {
        assert_clean_asan_run(
            r#"
fn inner() -> i64 {
    let mut bucket: Map[i64, Vec[i64]] = Map.new();
    let mut i = 0i64;
    while i < 50 {
        bucket.entry(i).or_insert(Vec.new()).push(i);
        i = i + 1;
    }
    let mut k = 0i64;
    while k < 50 {
        match bucket.remove(k) {
            Some(indices) => {
                let _len = indices.len();
            },
            None => {},
        }
        k = k + 1;
    }
    0i64
}
fn main() {
    let mut s = 0i64;
    let mut iter = 0i64;
    while iter < 10 {
        s = s + inner();
        iter = iter + 1;
    }
    println(s);
}
"#,
            &["0"],
            "match_arm_vec_binding_freed_on_arm_exit",
        );
    }

    // ── Map.remove heap-VALUE move-out under churn (2026-06-20) ───
    // The VALUE side of the `Map.remove` ownership class: `Map.remove`
    // moves the bucket's `{ptr,len,cap}` Vec/String value OUT (bitwise
    // copy into the `Some(old)` payload) and tombstones the slot WITHOUT
    // freeing it — the match binding becomes the sole owner and frees it
    // once at arm exit, while the eventual `karac_map_free_with_drop_vec`
    // walks only OCCUPIED slots (the tombstoned ones are skipped). The
    // stored-KEY side was fixed in B-2026-06-20-10 (the `drop_key` ABI).
    //
    // B-2026-06-20-14 logged an INTERMITTENT, seed-flavoured SEGV for
    // `asan_match_arm_vec_binding_freed_on_arm_exit` (below) on this same
    // value path. A follow-up audit (this batch) found the emitted IR for
    // that shape is single-owner and provably correct — one `@free` per
    // removed value, guarded by `cap > 0`, and the scope-exit map-free
    // touches no tombstoned slot — with no HashMap-order-dependent
    // decision anywhere in the path (no shared types are involved). Across
    // ~2400 real Linux ASAN+LSan runs (verified positive control) it did
    // not reproduce. The original symptom is consistent with a transient
    // stale-archive ABI mismatch during the `drop_key` rollout (codegen
    // passing the 4th arg to a not-yet-rebuilt 3-arg `karac_map_remove_old`
    // → garbage register), not a latent codegen bug. These three churn
    // stress cases lock the value path down permanently: large heap churn
    // exhausts the ASAN quarantine, so any real double-free/UAF surfaces as
    // a live-chunk corruption rather than a benign quarantined catch.

    #[test]
    fn asan_map_remove_vec_value_moveout_under_churn() {
        // Vec[i64] value, repeatedly grown then removed-and-bound across
        // 200 keys × 20 generations — the core move-out-then-free path at
        // a scale that cycles the allocator.
        assert_clean_asan_run(
            r#"
fn inner() -> i64 {
    let mut bucket: Map[i64, Vec[i64]] = Map.new();
    let mut i = 0i64;
    while i < 200 {
        let mut j = 0i64;
        while j < 20 {
            bucket.entry(i).or_insert(Vec.new()).push(i + j);
            j = j + 1;
        }
        i = i + 1;
    }
    let mut acc = 0i64;
    let mut k = 0i64;
    while k < 200 {
        match bucket.remove(k) {
            Some(indices) => {
                acc = acc + indices.len();
            },
            None => {},
        }
        k = k + 1;
    }
    acc
}
fn main() {
    let mut s = 0i64;
    let mut iter = 0i64;
    while iter < 20 {
        s = s + inner();
        iter = iter + 1;
    }
    println(s);
}
"#,
            &["80000"],
            "map_remove_vec_value_moveout_under_churn",
        );
    }

    #[test]
    fn asan_map_remove_vec_value_indexed_readback() {
        // Read every element of the moved-out Vec INSIDE the arm before it
        // is freed — exercises the reconstructed `{ptr,len,cap}` binding's
        // data buffer for reads, not just `len()`, under heap churn.
        assert_clean_asan_run(
            r#"
fn inner() -> i64 {
    let mut bucket: Map[i64, Vec[i64]] = Map.new();
    let mut i = 0i64;
    while i < 100 {
        let mut j = 0i64;
        while j < 16 {
            bucket.entry(i).or_insert(Vec.new()).push(i * 16i64 + j);
            j = j + 1;
        }
        i = i + 1;
    }
    let mut total = 0i64;
    let mut k = 0i64;
    while k < 100 {
        match bucket.remove(k) {
            Some(indices) => {
                let mut p = 0i64;
                while p < indices.len() {
                    total = total + indices[p];
                    p = p + 1;
                }
            },
            None => {},
        }
        k = k + 1;
    }
    total
}
fn main() {
    let mut s = 0i64;
    let mut iter = 0i64;
    while iter < 20 {
        s = s + inner();
        iter = iter + 1;
    }
    println(s);
}
"#,
            &["25584000"],
            "map_remove_vec_value_indexed_readback",
        );
    }

    #[test]
    fn asan_map_remove_string_key_vec_value_both_heap_halves() {
        // Heap KEY (String) + heap VALUE (Vec): the `drop_key=1` stored-key
        // free (B-2026-06-20-10) and the moved-out value free must each
        // fire exactly once, with no cross-talk, under churn.
        assert_clean_asan_run(
            r#"
fn keyfor(n: i64) -> String {
    let mut s = "k".to_string();
    s.push_str(n.to_string());
    s
}
fn inner() -> i64 {
    let mut bucket: Map[String, Vec[i64]] = Map.new();
    let mut i = 0i64;
    while i < 120 {
        let mut j = 0i64;
        while j < 12 {
            bucket.entry(keyfor(i)).or_insert(Vec.new()).push(i + j);
            j = j + 1;
        }
        i = i + 1;
    }
    let mut acc = 0i64;
    let mut k = 0i64;
    while k < 120 {
        match bucket.remove(keyfor(k)) {
            Some(indices) => {
                acc = acc + indices.len();
            },
            None => {},
        }
        k = k + 1;
    }
    acc
}
fn main() {
    let mut s = 0i64;
    let mut iter = 0i64;
    while iter < 15 {
        s = s + inner();
        iter = iter + 1;
    }
    println(s);
}
"#,
            &["21600"],
            "map_remove_string_key_vec_value_both_heap_halves",
        );
    }

    #[test]
    fn asan_match_arm_vec_move_out_no_double_free() {
        // Canonical `Option<Vec>::unwrap_or_default` shape: the arm binding
        // is the arm's tail expression, so the value is moved into the
        // match's result. The per-arm move-aware suppression must zero
        // the source's `cap` before the per-arm drain so the caller's
        // own scope cleanup is the unique owner. ASAN catches the
        // double-free that the naive "always track" change introduced
        // and required the suppress mechanism to prevent.
        assert_clean_asan_run(
            r#"
fn make() -> Vec[i64] {
    let mut v: Vec[i64] = Vec.new();
    v.push(1i64);
    v.push(2i64);
    v
}
fn unwrap_or_default(opt: Option[Vec[i64]]) -> Vec[i64] {
    match opt {
        Some(v) => v,
        None => Vec.new(),
    }
}
fn main() {
    let v = unwrap_or_default(Some(make()));
    println(v[0]);
    let w = unwrap_or_default(None);
    println(w.len());
}
"#,
            &["1", "0"],
            "match_arm_vec_move_out_no_double_free",
        );
    }

    // ── Early-return cleanup (2026-05-13) ─────────────────────────
    // `ExprKind::Return` historically built the LLVM return instruction
    // directly without draining `scope_cleanup_actions`, so early returns
    // (`if cond { return v; }` inside a function with tracked heap
    // locals) leaked every tracked binding's heap content. Fixed by
    // calling `emit_scope_cleanup()` before `build_return` and applying
    // the same `suppress_source_vec_cleanup_for_arg` move-aware
    // suppression on the return value that the function-end tail-return
    // path already applies. ASAN catches both halves: leak (no free
    // emitted on return path) and double-free (cleanup fires on the
    // moved-out buffer the caller now owns).

    #[test]
    fn asan_early_return_cleans_up_tracked_locals() {
        // The function has a tracked `Vec[i64]` local and exits via
        // `return 0` inside a conditional. Without the cleanup-on-return
        // fix, `v`'s data buffer would leak; ASAN reports it on exit.
        assert_clean_asan_run(
            r#"
fn process(short_circuit: bool) -> i64 {
    let mut v: Vec[i64] = Vec.new();
    v.push(1i64);
    v.push(2i64);
    v.push(3i64);
    if short_circuit {
        return 0;
    }
    v.len()
}
fn main() {
    let mut s = 0i64;
    let mut i = 0i64;
    while i < 5 {
        s = s + process(true);
        i = i + 1;
    }
    println(s);
}
"#,
            &["0"],
            "early_return_cleans_up_tracked_locals",
        );
    }

    #[test]
    fn asan_early_return_move_out_no_double_free() {
        // `return v` where `v` is a tracked Vec — the cleanup-on-return
        // path must apply move-aware suppression (zero the source's cap)
        // before draining so the caller's scope cleanup is the unique
        // owner of the buffer. Mirrors the function-end tail-return
        // suppress mechanism but for explicit `return expr`.
        assert_clean_asan_run(
            r#"
fn maybe_take(flag: bool) -> Vec[i64] {
    let mut v: Vec[i64] = Vec.new();
    v.push(42i64);
    if flag {
        return v;
    }
    v
}
fn main() {
    let v1 = maybe_take(true);
    let v2 = maybe_take(false);
    println(v1[0]);
    println(v2[0]);
}
"#,
            &["42", "42"],
            "early_return_move_out_no_double_free",
        );
    }

    // ── Map/Set heap-owning key + value drops (2026-05-14) ────────
    // Slice α + β of the recursive-drop work: `karac_map_free_with_drop_vec
    // (handle, drop_key, drop_val)` walks live buckets and frees per-entry
    // Vec/String content on both sides per the flags. Closes leaks for
    // `Set[String]` / `Set[Vec[T]]` (key only), `Map[String, V]` /
    // `Map[Vec[T], V]` (key only), and `Map[String, Vec[U]]` / similar
    // (both sides). Pre-fix these shapes leaked silently because the
    // narrower val-only helper missed every key-side allocation and the
    // primitive-only `karac_map_free` was used as a fallback.

    #[test]
    fn asan_set_string_keys_no_leak() {
        // `Set[String]` — the canonical pervasive shape. Each inserted
        // String is the bucket's KEY; on scope exit the runtime helper
        // must free each live key's data buffer. ASAN catches the leak
        // pre-fix (every inserted string's buffer leaked); post-fix the
        // set drops clean.
        assert_clean_asan_run(
            r#"
fn main() {
    let mut s: Set[String] = Set.new();
    let mut a = String.new();
    a.push_str("apple");
    s.insert(a);
    let mut b = String.new();
    b.push_str("banana");
    s.insert(b);
    let mut c = String.new();
    c.push_str("cherry");
    s.insert(c);
    println(s.len());
}
"#,
            &["3"],
            "set_string_keys_no_leak",
        );
    }

    #[test]
    fn asan_set_vec_keys_no_leak() {
        // `Set[Vec[i64]]` — Set of vecs. Each inserted Vec is the bucket's
        // KEY; the recursive-drop runtime helper frees each key's data
        // buffer before deallocating the bucket storage.
        assert_clean_asan_run(
            r#"
fn main() {
    let mut s: Set[Vec[i64]] = Set.new();
    let mut a: Vec[i64] = Vec.new();
    a.push(1i64);
    a.push(2i64);
    s.insert(a);
    let mut b: Vec[i64] = Vec.new();
    b.push(3i64);
    s.insert(b);
    println(s.len());
}
"#,
            &["2"],
            "set_vec_keys_no_leak",
        );
    }

    #[test]
    fn asan_set_vec_duplicate_element_dedup_no_leak_no_double_free() {
        // NEW ownership surface opened by the `Set[Vec[T]]` content-dedup fix
        // (B-2026-06-20-15): once two equal-CONTENTS vecs collapse, the second
        // `insert` takes the EXISTS (duplicate) path of `karac_map_insert_old`,
        // which keeps the bucket's existing element and does NOT adopt the
        // incoming one — so the incoming `{ptr,len,cap}` buffer must be freed
        // exactly once on the exists branch (B-2026-06-20-12's diamond), while
        // the bucket's adopted (first) buffer is freed exactly once at set drop.
        // Before this fix the dedup never happened (every insert was a fresh
        // bucket), so this exists-branch path was unreachable for `Set[Vec]`.
        // `b` is a moved local binding: its source scope-exit free is suppressed
        // (so it can't double-free with the exists-branch free) and the vacant
        // case would adopt it (so the exists-branch free can't run there). ≥6
        // i64s ⇒ a ≥48-byte data buffer (LSan misses sub-36-byte reachable
        // buffers). Must be clean under BOTH macOS ASAN (no double-free) and the
        // Linux LSan gate (no leak).
        assert_clean_asan_run(
            r#"
fn main() {
    let mut s: Set[Vec[i64]] = Set.new();
    let mut a: Vec[i64] = Vec.new();
    a.push(601i64); a.push(602i64); a.push(603i64);
    a.push(604i64); a.push(605i64); a.push(606i64);
    s.insert(a);
    let mut b: Vec[i64] = Vec.new();
    b.push(601i64); b.push(602i64); b.push(603i64);
    b.push(604i64); b.push(605i64); b.push(606i64);
    s.insert(b);
    println(s.len());
}
"#,
            &["1"],
            "set_vec_duplicate_element_dedup_no_leak_no_double_free",
        );
    }

    // ── `for w in vec` heap element BORROW consumed by a retaining sink
    //    (B-2026-06-20-13) ──
    // `for` over a Vec is borrow-iteration: `w` aliases `data[i]` and the
    // source Vec retains ownership (usable after the loop). A consume site that
    // RETAINS `w` (entry/push/insert) must deep-copy it — else the sink's drop
    // and the source Vec's drop free the same buffer (double-free; the
    // interpreter clones, so this was an A/B mismatch). The fix marks heap
    // for-loop element bindings (for_loop_borrow_vars) and routes them through
    // the same defensive copy as owned params.

    #[test]
    fn asan_for_loop_string_elem_into_entry_counter_no_double_free() {
        // The flagship histogram: `for w in words { *m.entry(w).or_insert(0) += 1 }`.
        // Repeated keys exercise vacant (adopt the copy) + occupied (free the
        // copy) paths; `words` stays live and frees its own elements once.
        assert_clean_asan_run(
            r#"
fn main() {
    let mut words: Vec[String] = Vec.new();
    words.push("alpha-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string());
    words.push("beta-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".to_string());
    words.push("alpha-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string());
    let mut m: Map[String, i64] = Map.new();
    for w in words {
        *m.entry(w).or_insert(0_i64) += 1_i64;
    }
    println(m.len());
    println(words.len());
}
"#,
            &["2", "3"],
            "for_loop_string_elem_into_entry_counter_no_double_free",
        );
    }

    #[test]
    fn asan_for_loop_string_elem_into_push_and_insert_no_double_free() {
        // Same borrow-element copy at `Vec.push` and `Map.insert` consume sites.
        assert_clean_asan_run(
            r#"
fn main() {
    let mut words: Vec[String] = Vec.new();
    words.push("one-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string());
    words.push("two-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".to_string());
    let mut out: Vec[String] = Vec.new();
    let mut m: Map[String, i64] = Map.new();
    for w in words {
        out.push(w);
    }
    for w in out {
        m.insert(w, 1_i64);
    }
    println(out.len());
    println(m.len());
    println(words.len());
}
"#,
            &["2", "2", "2"],
            "for_loop_string_elem_into_push_and_insert_no_double_free",
        );
    }

    #[test]
    fn asan_for_loop_borrow_then_rebind_let_no_leak() {
        // Shadow guard: after the loop, a `let w = <fresh owned>` reusing the
        // loop var name must NOT be defensive-copied (the `let` clears stale
        // for_loop_borrow_vars membership) — else the copy + source-suppress
        // would orphan the fresh String (LSan-only leak; ≥36-byte payload).
        assert_clean_asan_run(
            r#"
fn main() {
    let mut words: Vec[String] = Vec.new();
    words.push("loopelem-aaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string());
    let mut sink: Vec[String] = Vec.new();
    for w in words {
        sink.push(w);
    }
    let mut w = String.new();
    w.push_str("rebound-owned-bbbbbbbbbbbbbbbbbbbbbbbb");
    sink.push(w);
    println(sink.len());
}
"#,
            &["2"],
            "for_loop_borrow_then_rebind_let_no_leak",
        );
    }

    #[test]
    fn asan_for_loop_owned_agg_elem_direct_match_no_double_free() {
        // Direct `for it in items { match it { V(payload) => … } }` over a
        // heap-bearing user-enum element (registered `for_loop_owned_agg_vars`,
        // NOT `for_loop_borrow_vars`). The struct payload bound out of the
        // borrowed element aliases the container slot's heap (a String buffer +
        // a Vec buffer here); before the fix its scope-exit drop double-freed
        // against the container's per-element drop when `v` unwound. Guards
        // no-double-free (ASAN) / no-leak (LSan): every buffer reclaimed exactly
        // once. ≥36-byte String so the fault is loud.
        assert_clean_asan_run(
            r#"
struct Named { name: String, nums: Vec[i64] }
enum Item { A(Named), B }
fn total(items: ref Vec[Item]) -> i64 {
    let mut n = 0;
    for it in items {
        match it {
            A(x) => { n = n + x.name.len(); for t in x.nums { n = n + t; } }
            B => {}
        }
    }
    n
}
fn main() {
    let mut v: Vec[Item] = Vec.new();
    let mut i = 0;
    while i < 6 {
        let mut tg: Vec[i64] = Vec.new();
        tg.push(1); tg.push(2);
        v.push(Item.A(Named { name: "owned-agg-elem-direct-match-payload-xx".to_string(), nums: tg }));
        v.push(Item.B);
        i = i + 1;
    }
    println(total(v));
}
"#,
            &["246"],
            "for_loop_owned_agg_elem_direct_match",
        );
    }

    #[test]
    fn asan_module_scope_map_string_keys_no_double_free() {
        // Module-scope `Map.new()` (phase-8-stdlib-floor.md "Map.new() /
        // Set.new() as module-binding initialisers"). The handle lives in
        // a global filled by the `__karac_static_init` prologue and is
        // intentionally NEVER freed — a module binding lives for the whole
        // process, so there is no scope-exit `karac_map_free`. The handle
        // (and every heap key buffer it owns) stays reachable through the
        // global at exit, so LSan must NOT report it (Linux CI), and ASAN
        // must see no double-free / UAF here. Heap String keys exercise
        // the key_is_vec path under the static-init handle.
        assert_clean_asan_run(
            r#"
let mut REGISTRY: Map[String, i64] = Map.new();
fn put(k: String, v: i64) { REGISTRY.insert(k, v); }
fn main() {
    let mut k1 = String.new();
    k1.push_str("alpha-key-with-long-padding-to-exceed-sso-buffer");
    put(k1, 1i64);
    let mut k2 = String.new();
    k2.push_str("beta-key-with-long-padding-to-exceed-sso-buffer");
    put(k2, 2i64);
    println(REGISTRY.get("alpha-key-with-long-padding-to-exceed-sso-buffer").unwrap_or(0i64));
    println(REGISTRY.len());
}
"#,
            &["1", "2"],
            "module_scope_map_string_keys_no_double_free",
        );
    }

    #[test]
    fn asan_map_string_keys_no_leak() {
        // `Map[String, i64]` — the canonical `key_is_vec, !val_is_vec`
        // shape. Pre-fix the key buffers leaked because the val-only
        // helper never touched them and primitive-only `karac_map_free`
        // was used by default.
        assert_clean_asan_run(
            r#"
fn main() {
    let mut m: Map[String, i64] = Map.new();
    let mut k1 = String.new();
    k1.push_str("alpha");
    m.insert(k1, 1i64);
    let mut k2 = String.new();
    k2.push_str("beta");
    m.insert(k2, 2i64);
    println(m.len());
}
"#,
            &["2"],
            "map_string_keys_no_leak",
        );
    }

    #[test]
    fn asan_map_string_keys_vec_values_no_leak() {
        // `Map[String, Vec[i64]]` — both flags set. The runtime helper
        // must walk live buckets and free BOTH the key's String buffer
        // and the value's Vec buffer before deallocating bucket storage.
        // Catches the case where one side's drop fires correctly but
        // the other is silently skipped.
        assert_clean_asan_run(
            r#"
fn main() {
    let mut m: Map[String, Vec[i64]] = Map.new();
    let mut k = String.new();
    k.push_str("key");
    let mut v: Vec[i64] = Vec.new();
    v.push(7i64);
    v.push(8i64);
    m.insert(k, v);
    println(m.len());
}
"#,
            &["1"],
            "map_string_keys_vec_values_no_leak",
        );
    }

    #[test]
    fn asan_map_string_keys_values_entries_deep_copy_no_double_free() {
        // `keys()` / `values()` / `entries()` over a `Map[String,String]`
        // return OWNED Vecs whose heap halves are DEEP-CLONED from the bucket
        // (B-2026-06-20-11). A shallow `{ptr,len,cap}` copy aliased the map's
        // stored buffer, so the result Vec's scope-exit drop and the map's drop
        // freed the same allocation — a double-free (it crashed `keys()` even
        // before any read). ≥36-byte payloads; the result Vecs and the map all
        // drop independently and cleanly.
        assert_clean_asan_run(
            r#"
fn main() {
    let mut m: Map[String, String] = Map.new();
    m.insert("key-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(),
             "val-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".to_string());
    m.insert("key-cccccccccccccccccccccccccccccccccccc".to_string(),
             "val-dddddddddddddddddddddddddddddddddddd".to_string());
    let ks: Vec[String] = m.keys();
    let vs: Vec[String] = m.values();
    let es: Vec[(String, String)] = m.entries();
    // Counts only — entries() order is non-deterministic; a clean exit proves
    // no double-free (it crashed before any read pre-fix). Content correctness
    // is covered by the codegen E2E `test_e2e_map_string_values_entries_owned`.
    println(ks.len());
    println(vs.len());
    println(es.len());
}
"#,
            &["2", "2", "2"],
            "map_string_keys_values_entries_deep_copy_no_double_free",
        );
    }

    // ── Owned String/Vec PARAM moved into Map/Set insert (Cluster 1) ──
    // `m.insert(k, v)` / `set.insert(v)` where the key/value/element is an
    // owned `String`/`Vec` PARAMETER of the current function. Under the
    // by-value header ABI the CALLER retains the buffer's scope-exit free,
    // so a bucket that bit-copies the param's `{ptr,len,cap}` aliases the
    // caller's buffer — both the caller's `FreeVecBuffer` and the Map's
    // `karac_map_free_with_drop_vec` then free the same allocation
    // (double-free), and a post-call read of the bucket walks freed memory
    // (UAF). The fix wires `maybe_defensive_copy_param_arg` into the insert
    // arms so the collection owns a private copy. Same family as the
    // already-covered `Vec.push` / enum-payload / struct-field consume
    // sites (kata-22, 2026-06-06); these are the Map/Set siblings.

    #[test]
    fn asan_map_insert_owned_string_param_value() {
        // `Map[i64, String]`, VALUE side. The helper takes the String by
        // value (owned param) and inserts it; `m` lives in main and is
        // passed `mut ref`, so its bucket free and main's free of the
        // moved-in source double-hit the same buffer pre-fix. Allocation
        // churn between insert and read-back exposes a UAF if the bucket
        // aliased a freed buffer.
        assert_clean_asan_run(
            r#"
fn store(m: mut ref Map[i64, String], v: String) {
    m.insert(7i64, v);
}

fn main() {
    let mut m: Map[i64, String] = Map.new();
    let mut s = String.new();
    s.push_str("payload-string-value");
    store(mut m, s);
    let mut churn: Vec[String] = Vec.new();
    let mut i = 0i64;
    while i < 16i64 {
        let mut t = String.new();
        t.push_str("xxxxxxxxxxxxxxxxxxxx");
        churn.push(t);
        i = i + 1i64;
    }
    match m.get(7i64) { Some(g) => println(g), None => println("missing") }
}
"#,
            &["payload-string-value"],
            "map_insert_owned_string_param_value",
        );
    }

    #[test]
    fn asan_map_insert_owned_string_param_key() {
        // `Map[String, i64]`, KEY side. The owned String param is the
        // bucket key; the recursive-drop helper frees each live key, so an
        // aliased key buffer double-frees against the caller's source.
        assert_clean_asan_run(
            r#"
fn store(m: mut ref Map[String, i64], k: String) {
    m.insert(k, 1i64);
}

fn main() {
    let mut m: Map[String, i64] = Map.new();
    let mut a = String.new();
    a.push_str("alpha-key-string");
    store(mut m, a);
    let mut b = String.new();
    b.push_str("beta-key-string");
    store(mut m, b);
    println(m.len());
}
"#,
            &["2"],
            "map_insert_owned_string_param_key",
        );
    }

    #[test]
    fn asan_set_insert_owned_string_param() {
        // `Set[String]` (lowers to `Map[T, ()]`), element side. Owned
        // String param moved into a `mut ref` Set living in main.
        assert_clean_asan_run(
            r#"
fn add(set: mut ref Set[String], v: String) {
    set.insert(v);
}

fn main() {
    let mut s: Set[String] = Set.new();
    let mut a = String.new();
    a.push_str("apple-element");
    add(mut s, a);
    let mut b = String.new();
    b.push_str("banana-element");
    add(mut s, b);
    println(s.len());
}
"#,
            &["2"],
            "set_insert_owned_string_param",
        );
    }

    #[test]
    fn asan_map_insert_fstring_value() {
        // F-string sibling of the owned-param path: `m.insert(k, f"…")`.
        // The f-string's staged accumulator must be disarmed at the insert
        // (the bucket takes the buffer) — otherwise the acc's scope-exit
        // free and the Map's bucket free double-hit it. Covers the
        // `suppress_fstr_acc_if_moved_out` half of the fix.
        assert_clean_asan_run(
            r#"
fn store(m: mut ref Map[i64, String], n: i64) {
    m.insert(n, f"value-{n}-suffix");
}

fn main() {
    let mut m: Map[i64, String] = Map.new();
    store(mut m, 1i64);
    store(mut m, 2i64);
    println(m.len());
    match m.get(1i64) { Some(g) => println(g), None => println("missing") }
}
"#,
            &["2", "value-1-suffix"],
            "map_insert_fstring_value",
        );
    }

    // ── Vec[Map] / Vec[Set] owned-param defensive copy recurses into
    //    map/set handles (Cluster 1) ──
    // `emit_vecstr_defensive_copy` deep-copies the OUTER buffer of an owned
    // `Vec` param at a retaining consume site, then rewrites each element to
    // own its own heap. It recursed String/Vec elements but FLAT-COPIED
    // Map/Set elements — the copy and the source aliased the same opaque map
    // handles, so both the source's and the copy's scope-exit
    // `karac_map_free_with_drop_vec` freed the same map (double-free). Map
    // handles aren't LLVM-type-sniffable, but the element TypeExpr is
    // available, so the fix routes Map/Set elements through the synthesized
    // `karac_clone_<T>` deep-clone per element. Tail-returning the owned
    // `Vec[Map]` param is the canonical retaining site: the caller frees the
    // moved-in original AND the returned copy.

    #[test]
    fn asan_vec_map_param_deep_copy_no_double_free() {
        // `Vec[Map[i64, i64]]` owned param tail-returned. Pre-fix the
        // returned copy's element handles aliased the original's maps; both
        // freed at main scope exit → double-free. The read-backs prove the
        // cloned maps carry the same entries.
        assert_clean_asan_run(
            r#"
fn id(v: Vec[Map[i64, i64]]) -> Vec[Map[i64, i64]] {
    v
}

fn main() {
    let mut v: Vec[Map[i64, i64]] = Vec.new();
    let mut m: Map[i64, i64] = Map.new();
    m.insert(1i64, 10i64);
    m.insert(2i64, 20i64);
    v.push(m);
    let mut m2: Map[i64, i64] = Map.new();
    m2.insert(3i64, 30i64);
    v.push(m2);
    let r = id(v);
    println(r.len());
    match r[0].get(1i64) { Some(x) => println(x), None => println(-1i64) }
    match r[1].get(3i64) { Some(x) => println(x), None => println(-1i64) }
}
"#,
            &["2", "10", "30"],
            "vec_map_param_deep_copy_no_double_free",
        );
    }

    #[test]
    fn asan_vec_set_param_deep_copy_no_double_free() {
        // `Vec[Set[i64]]` sibling — Set lowers to `Map[T, ()]`, so the same
        // handle-aliasing double-free applies; the clone routes through
        // `emit_map_clone_fn` with the unit value half.
        assert_clean_asan_run(
            r#"
fn id(v: Vec[Set[i64]]) -> Vec[Set[i64]] {
    v
}

fn main() {
    let mut v: Vec[Set[i64]] = Vec.new();
    let mut s: Set[i64] = Set.new();
    s.insert(1i64);
    s.insert(2i64);
    v.push(s);
    let r = id(v);
    println(r.len());
    println(r[0].contains(1i64));
    println(r[0].contains(9i64));
}
"#,
            &["1", "true", "false"],
            "vec_set_param_deep_copy_no_double_free",
        );
    }

    #[test]
    fn asan_vec_map_string_value_param_deep_copy_no_double_free() {
        // `Vec[Map[i64, String]]` — the cloned maps must additionally
        // deep-copy their String VALUES (recursion lands in
        // `emit_map_clone_fn`'s val-clone). Catches an aliased inner String
        // buffer surviving the handle clone.
        assert_clean_asan_run(
            r#"
fn id(v: Vec[Map[i64, String]]) -> Vec[Map[i64, String]] {
    v
}

fn main() {
    let mut v: Vec[Map[i64, String]] = Vec.new();
    let mut m: Map[i64, String] = Map.new();
    let mut s = String.new();
    s.push_str("payload-in-nested-map");
    m.insert(7i64, s);
    v.push(m);
    let r = id(v);
    println(r.len());
    match r[0].get(7i64) { Some(g) => println(g), None => println("missing") }
}
"#,
            &["1", "payload-in-nested-map"],
            "vec_map_string_value_param_deep_copy_no_double_free",
        );
    }

    #[test]
    fn asan_map_entry_or_insert_counter_and_get_or_clean() {
        // Tier D entry write-through end to end: `*m.entry(k).or_insert(0) += 1`
        // builds a frequency table keyed by ≥36-byte Strings (LSan-visible if a
        // key buffer leaked), then `get_or` reads the counts back. The map owns
        // and frees each key exactly once; the scalar counter values carry no
        // heap. No leak / double-free / UAF.
        assert_clean_asan_run(
            r#"
fn main() {
    let mut m: Map[String, i64] = Map.new();
    let words = [
        "alpha-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "beta-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        "alpha-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "alpha-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    ];
    for w in words {
        *m.entry(w.to_string()).or_insert(0_i64) += 1;
    }
    println(m.get_or("alpha-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(), 0_i64));
    println(m.get_or("beta-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".to_string(), 0_i64));
}
"#,
            &["3", "1"],
            "map_entry_or_insert_counter_and_get_or_clean",
        );
    }

    #[test]
    fn asan_map_get_or_string_value_owned_copy_no_double_free() {
        // `get_or` returns an OWNED `V`, so a heap V (String, ≥36 bytes) must be
        // deep-cloned from the bucket on a hit — else the caller's scope-exit
        // drop and the map's drop free the same buffer (double-free). The miss
        // path returns the freshly-built default. Both bindings drop cleanly.
        assert_clean_asan_run(
            r#"
fn main() {
    let mut m: Map[String, String] = Map.new();
    let k = "key-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string();
    let v = "val-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".to_string();
    m.insert(k.clone(), v);
    let hit = m.get_or(k.clone(), "dflt-cccccccccccccccccccccccccccccccc".to_string());
    let miss = m.get_or("absent-dddddddddddddddddddddddddddddd".to_string(),
                        "dflt-eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee".to_string());
    println(hit);
    println(miss);
}
"#,
            &[
                "val-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                "dflt-eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee",
            ],
            "map_get_or_string_value_owned_copy_no_double_free",
        );
    }

    // ── Residual map-key no-adopt ownership (B-2026-06-20-9) ──
    // Map key methods route the key buffer through ownership paths the
    // fresh-temp-only handling of `B-2026-06-20-8` missed. `karac_map_entry`
    // ADOPTS the key (bit-copies its `{ptr,len,cap}`) only on the VACANT
    // insert; `and_modify`'s lookup variant, `get`/`get_or`/`remove`/
    // `contains_key`, and `insert` on the EXISTS path never adopt. For a
    // moved local binding / owned param / place key on a no-adopt path the
    // buffer was orphaned (leak); on `entry`'s vacant path the key was adopted
    // AND freed by the un-suppressed source (double-free). The fix mirrors
    // `Map.insert`'s consume-site dance in the entry chain (suppress source +
    // defensive-copy owned params + free on the no-adopt branch) and adds the
    // fresh-temp key free to `get`/`remove`/`contains_key` and the exists-path
    // key free to `insert`.

    #[test]
    fn asan_map_entry_moved_binding_key_no_double_free() {
        // `entry().or_insert` VACANT path with a moved LOCAL String binding.
        // The map adopts the buffer; the source binding's scope-exit free must
        // be suppressed, else both free it (double-free — caught even on macOS
        // ASAN without LSan).
        assert_clean_asan_run(
            r#"
fn main() {
    let mut m: Map[String, i64] = Map.new();
    let mut k = String.new();
    k.push_str("fresh-key-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
    *m.entry(k).or_insert(0_i64) += 1_i64;
    println(m.len());
}
"#,
            &["1"],
            "map_entry_moved_binding_key_no_double_free",
        );
    }

    #[test]
    fn asan_map_entry_moved_binding_key_occupied_no_leak() {
        // `entry().or_insert` OCCUPIED (no-adopt) path with a moved local
        // binding (pre-inserted via `insert`). The entry chain frees the
        // orphaned ≥36-byte key buffer (the source was suppressed).
        assert_clean_asan_run(
            r#"
fn main() {
    let mut m: Map[String, i64] = Map.new();
    m.insert("dup-key-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(), 5_i64);
    let mut k = String.new();
    k.push_str("dup-key-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
    *m.entry(k).or_insert(0_i64) += 1_i64;
    println(m.get_or("dup-key-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(), 0_i64));
}
"#,
            &["6"],
            "map_entry_moved_binding_key_occupied_no_leak",
        );
    }

    #[test]
    fn asan_map_entry_owned_param_key_no_double_free() {
        // `entry().or_insert` with an OWNED String PARAM key, exercised on both
        // the vacant (first call) and occupied (second call, same key) paths
        // via a `mut ref Map`. The param key is defensive-copied so the bucket
        // owns a private buffer; the caller frees the original. Vacant: copy
        // adopted, original freed by caller (no double-free). Occupied: copy
        // orphaned → freed by the entry chain, original freed by caller
        // (no leak, no double-free). Churn between calls exposes a UAF if a
        // bucket ever aliased a freed buffer.
        assert_clean_asan_run(
            r#"
fn bump(m: mut ref Map[String, i64], k: String) {
    *m.entry(k).or_insert(0_i64) += 1_i64;
}

fn main() {
    let mut m: Map[String, i64] = Map.new();
    let mut a1 = String.new();
    a1.push_str("param-key-aaaaaaaaaaaaaaaaaaaaaaaaaaaa");
    bump(mut m, a1);
    let mut a2 = String.new();
    a2.push_str("param-key-aaaaaaaaaaaaaaaaaaaaaaaaaaaa");
    bump(mut m, a2);
    let mut churn: Vec[String] = Vec.new();
    let mut i = 0i64;
    while i < 16i64 {
        let mut t = String.new();
        t.push_str("xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx");
        churn.push(t);
        i = i + 1i64;
    }
    println(m.get_or("param-key-aaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(), 0_i64));
}
"#,
            &["2"],
            "map_entry_owned_param_key_no_double_free",
        );
    }

    #[test]
    fn asan_map_entry_and_modify_moved_binding_key_no_leak() {
        // Bare `entry().and_modify` (the lookup-only variant — NEVER adopts)
        // with a moved local binding key, occupied. The entry chain always
        // frees the orphaned key on this path.
        assert_clean_asan_run(
            r#"
fn main() {
    let mut m: Map[String, i64] = Map.new();
    m.insert("am-key-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(), 10_i64);
    let mut k = String.new();
    k.push_str("am-key-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
    m.entry(k).and_modify(|v| { *v += 5_i64; });
    println(m.get_or("am-key-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(), 0_i64));
}
"#,
            &["15"],
            "map_entry_and_modify_moved_binding_key_no_leak",
        );
    }

    #[test]
    fn asan_map_get_moved_binding_key_no_leak() {
        // Moved local binding key into `get` (never adopts). `get` does NOT
        // suppress the source, so the source binding's scope-exit free releases
        // the buffer exactly once.
        assert_clean_asan_run(
            r#"
fn main() {
    let mut m: Map[String, i64] = Map.new();
    m.insert("look-key-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(), 9_i64);
    let mut k = String.new();
    k.push_str("look-key-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
    match m.get(k) { Some(x) => println(x), None => println(-1_i64) }
}
"#,
            &["9"],
            "map_get_moved_binding_key_no_leak",
        );
    }

    #[test]
    fn asan_map_get_fresh_temp_key_no_leak() {
        // Fresh-temp (`.clone()`) key into `get`. `get` now frees its
        // fresh-temp key (mirroring `get_or`); pre-fix it leaked one buffer
        // per call.
        assert_clean_asan_run(
            r#"
fn main() {
    let mut m: Map[String, i64] = Map.new();
    let base = "clone-key-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string();
    m.insert(base.clone(), 4_i64);
    match m.get(base.clone()) { Some(x) => println(x), None => println(-1_i64) }
}
"#,
            &["4"],
            "map_get_fresh_temp_key_no_leak",
        );
    }

    #[test]
    fn asan_map_remove_contains_fresh_temp_key_no_leak() {
        // Fresh-temp keys into `contains_key` (present) and `remove` (the
        // incoming key argument), both lookup-only — neither retains the
        // incoming key, so each now frees its fresh-temp key buffer. The
        // `remove` here targets an ABSENT key (miss path) on purpose, to
        // isolate the INCOMING-key residual: the distinct present-key STORED
        // key leak is exercised by the `asan_map_remove_present_*` tests below
        // (closed by the drop-flag ABI in B-2026-06-20-10).
        assert_clean_asan_run(
            r#"
fn main() {
    let mut m: Map[String, i64] = Map.new();
    let base = "rc-key-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string();
    m.insert(base.clone(), 7_i64);
    if m.contains_key(base.clone()) {
        println("present");
    }
    match m.remove("absent-key-bbbbbbbbbbbbbbbbbbbbbbbbbbbb".to_string()) {
        Some(x) => println(x),
        None => println(-1_i64),
    }
    println(m.len());
}
"#,
            &["present", "-1", "1"],
            "map_remove_contains_fresh_temp_key_no_leak",
        );
    }

    // ── Present-key remove STORED key/value ownership (B-2026-06-20-10) ──
    // Completes the map-key-ownership class B-2026-06-20-9 started. A
    // present-key `remove` of a HEAP key tombstones the bucket, and
    // `karac_map_free_with_drop_vec` only walks OCCUPIED slots — so the
    // bucket's STORED key buffer was orphaned (leak) until the runtime
    // learned to free it. `Map.remove` / `Set.remove` lower to
    // `karac_map_remove_old`, which now takes a codegen-set `drop_key` flag
    // (`llvm_ty_is_vec_struct(key_ty)`) and frees the stored key on the
    // tombstone path — the value half is MOVED OUT to the caller, so it is
    // never freed here. ≥36-byte keys per the LSan-reachability rule (LSan
    // misses short, still-reachable String buffers).

    #[test]
    fn asan_map_remove_present_heap_key_no_leak() {
        // `Map[String, i64].remove(present)` → Some. The incoming fresh-temp
        // key is freed by the no-adopt path (B-2026-06-20-9); the bucket's
        // STORED String key is freed by the runtime drop-flag (this fix).
        // Pre-fix the stored key buffer leaked under the Linux LSan gate.
        assert_clean_asan_run(
            r#"
fn main() {
    let mut m: Map[String, i64] = Map.new();
    m.insert("present-key-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(), 7_i64);
    match m.remove("present-key-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string()) {
        Some(x) => println(x),
        None => println(-1_i64),
    }
    println(m.len());
}
"#,
            &["7", "0"],
            "map_remove_present_heap_key_no_leak",
        );
    }

    #[test]
    fn asan_map_remove_present_heap_key_and_vec_value_no_leak() {
        // `Map[String, Vec[i64]].remove(present)` → Some(vec). Exercises BOTH
        // sides at once: the STORED String key is freed by the drop-flag
        // (this fix), while the Vec value is MOVED OUT into the match-arm
        // binding and freed exactly once at arm exit (the runtime must NOT
        // free a returned value). Pre-fix the stored key leaked; a naive
        // "free both" runtime change would instead double-free the value.
        assert_clean_asan_run(
            r#"
fn main() {
    let mut m: Map[String, Vec[i64]] = Map.new();
    let mut v: Vec[i64] = Vec.new();
    v.push(10_i64);
    v.push(20_i64);
    m.insert("vec-value-key-aaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(), v);
    match m.remove("vec-value-key-aaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string()) {
        Some(got) => println(got.len()),
        None => println(-1_i64),
    }
    println(m.len());
}
"#,
            &["2", "0"],
            "map_remove_present_heap_key_and_vec_value_no_leak",
        );
    }

    #[test]
    fn asan_map_insert_moved_binding_duplicate_key_no_leak() {
        // Moved local binding key into `insert` on the EXISTS (duplicate-key)
        // path. `karac_map_insert_old` keeps the bucket's existing key and does
        // not adopt the incoming one; `insert` suppressed the source — so the
        // incoming buffer is orphaned and now freed on the exists branch.
        assert_clean_asan_run(
            r#"
fn main() {
    let mut m: Map[String, i64] = Map.new();
    let mut k1 = String.new();
    k1.push_str("ins-key-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
    m.insert(k1, 1_i64);
    let mut k2 = String.new();
    k2.push_str("ins-key-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
    m.insert(k2, 2_i64);
    println(m.len());
    println(m.get_or("ins-key-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(), 0_i64));
}
"#,
            &["1", "2"],
            "map_insert_moved_binding_duplicate_key_no_leak",
        );
    }

    #[test]
    fn asan_map_insert_owned_param_duplicate_key_no_leak() {
        // Owned String PARAM key into `insert` on the EXISTS path via a
        // `mut ref Map`. The defensive copy is orphaned on the exists branch
        // and freed there; the caller frees each original param.
        assert_clean_asan_run(
            r#"
fn store(m: mut ref Map[String, i64], k: String) {
    m.insert(k, 1_i64);
}

fn main() {
    let mut m: Map[String, i64] = Map.new();
    let mut a1 = String.new();
    a1.push_str("ins-param-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
    store(mut m, a1);
    let mut a2 = String.new();
    a2.push_str("ins-param-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
    store(mut m, a2);
    println(m.len());
}
"#,
            &["1"],
            "map_insert_owned_param_duplicate_key_no_leak",
        );
    }

    #[test]
    fn asan_map_insert_duplicate_heap_value_discarded_no_leak() {
        // `Map[String, String]`, same key inserted twice with DISCARDED
        // `Option[V]` results. On the second (exists) insert the displaced OLD
        // String value is handed back as a `Some(old)` payload no one holds;
        // the discarded-Option-value cleanup already releases it. Companion to
        // the exists-path KEY free (the incoming duplicate key is freed by the
        // new exists-branch handling). Confirms both the key and the displaced
        // value are released exactly once on a duplicate heap-valued insert.
        assert_clean_asan_run(
            r#"
fn main() {
    let mut m: Map[String, String] = Map.new();
    let k = "vkey-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string();
    m.insert(k.clone(), "first-vvvvvvvvvvvvvvvvvvvvvvvvvvvvvvvv".to_string());
    m.insert(k.clone(), "second-wwwwwwwwwwwwwwwwwwwwwwwwwwww".to_string());
    println(m.get_or(k.clone(), "x".to_string()));
}
"#,
            &["second-wwwwwwwwwwwwwwwwwwwwwwwwwwww"],
            "map_insert_duplicate_heap_value_discarded_no_leak",
        );
    }

    // ── Set INCOMING-element NO-ADOPT ownership (B-2026-06-20-12) ──
    // Completes the map/set key-ownership class: B-2026-06-20-9 (c7b72bd4)
    // fixed the INCOMING key for Map's no-adopt paths but never applied it to
    // Set (`collections.rs` had zero `free_fresh_owned_str_arg` calls), and
    // B-2026-06-20-10 (a1b59c5e) fixed only the STORED element on a present-key
    // remove. The remaining gap is the INCOMING element argument: a fresh-owned
    // temp (`s.remove("x".to_string())`) or a moved binding on a no-adopt path
    // leaked one element buffer per call. Set lowers to `Map[T, ()]`, so these
    // arms call `karac_map_remove_old` / `karac_map_contains` / `karac_map_entry`
    // (insert). ≥36-byte elements per the LSan-reachability rule (LSan misses
    // short, still-reachable String/Vec buffers).

    #[test]
    fn asan_set_remove_present_fresh_temp_element_no_leak() {
        // `Set[String].remove(present)` with a fresh-temp element. TWO distinct
        // buffers must be freed exactly once: the bucket's STORED element (via
        // the runtime `drop_key` flag, B-2026-06-20-10) and the INCOMING fresh
        // temp (via `free_fresh_owned_str_arg`, this fix). Pre-fix the incoming
        // buffer leaked under the Linux LSan gate.
        assert_clean_asan_run(
            r#"
fn main() {
    let mut s: Set[String] = Set.new();
    s.insert("set-remove-element-aaaaaaaaaaaaaaaaaaaa".to_string());
    if s.remove("set-remove-element-aaaaaaaaaaaaaaaaaaaa".to_string()) {
        println("removed");
    }
    println(s.len());
}
"#,
            &["removed", "0"],
            "set_remove_present_fresh_temp_element_no_leak",
        );
    }

    #[test]
    fn asan_set_contains_present_fresh_temp_element_no_leak() {
        // `Set[String].contains(present)` with a fresh-temp element. The lookup
        // hashes/compares but never retains the incoming element, so the fresh
        // temp must be freed after the call. Pre-fix it leaked one buffer per
        // call (LSan-only).
        assert_clean_asan_run(
            r#"
fn main() {
    let mut s: Set[String] = Set.new();
    s.insert("set-contains-element-aaaaaaaaaaaaaaaaaa".to_string());
    if s.contains("set-contains-element-aaaaaaaaaaaaaaaaaa".to_string()) {
        println("present");
    }
    println(s.len());
}
"#,
            &["present", "1"],
            "set_contains_present_fresh_temp_element_no_leak",
        );
    }

    #[test]
    fn asan_set_insert_moved_binding_duplicate_element_no_leak() {
        // Moved local binding element into `Set[String].insert` on the EXISTS
        // (duplicate) path. `karac_map_insert_old` keeps the bucket's existing
        // element and does NOT adopt the incoming one, while the insert arm
        // suppressed the source binding's scope-exit free — so the incoming
        // buffer is orphaned and now freed on the exists branch.
        assert_clean_asan_run(
            r#"
fn main() {
    let mut s: Set[String] = Set.new();
    let mut a1 = String.new();
    a1.push_str("set-dup-element-aaaaaaaaaaaaaaaaaaaaaaaa");
    s.insert(a1);
    let mut a2 = String.new();
    a2.push_str("set-dup-element-aaaaaaaaaaaaaaaaaaaaaaaa");
    s.insert(a2);
    println(s.len());
}
"#,
            &["1"],
            "set_insert_moved_binding_duplicate_element_no_leak",
        );
    }

    #[test]
    fn asan_set_remove_absent_fresh_temp_vec_element_no_leak() {
        // `Set[Vec[i64]]` sibling on the lookup-only path: confirms the incoming
        // free's vec-struct gate (`free_fresh_owned_str_arg` → `cap > 0` free)
        // also fires for an actual `Vec` element, not just `String`. `make_vec()`
        // returns a fresh-owned temp; `remove` looks it up (ABSENT here — the
        // element `[701..706]` differs in both length and contents from the
        // set's lone `[1]`, so the explicit miss isolates the INCOMING-element
        // residual without depending on content equality, independent of the
        // `Set[Vec]` content-dedup now in place via B-2026-06-20-15) and never
        // retains it, so the returned Vec buffer must be freed after the call.
        // ≥6 i64s ⇒ a ≥48-byte data buffer (LSan misses sub-36-byte reachable
        // buffers). Pre-fix it leaked one buffer per call.
        assert_clean_asan_run(
            r#"
fn make_vec() -> Vec[i64] {
    let mut v: Vec[i64] = Vec.new();
    v.push(701i64);
    v.push(702i64);
    v.push(703i64);
    v.push(704i64);
    v.push(705i64);
    v.push(706i64);
    v
}

fn main() {
    let mut s: Set[Vec[i64]] = Set.new();
    let mut a: Vec[i64] = Vec.new();
    a.push(1i64);
    s.insert(a);
    if s.remove(make_vec()) {
        println("removed");
    } else {
        println("absent");
    }
    println(s.len());
}
"#,
            &["absent", "1"],
            "set_remove_absent_fresh_temp_vec_element_no_leak",
        );
    }

    // ── Vec[Map] ownership: a Map moved into a Vec transfers ownership
    //    to the Vec (Cluster 1) ──
    // The headline bug: a `Map` pushed into a `Vec` aliased a handle still
    // owned (and freed at scope exit) by the origin `m` binding, while the
    // Vec's own drop never freed map elements. So a `Vec[Map]` built from a
    // local map and RETURNED dangled — the local's `FreeMapHandle` freed the
    // handle the returned Vec still pointed at (use-after-free on the next
    // read; AOT printed `-1` vs interp's correct value). The fix makes the
    // Vec OWN its map elements: the push suppresses the source's
    // `FreeMapHandle` (ownership transfer) and the Vec drop frees each
    // element handle (`track_vec_of_maps_var`). Both halves are required —
    // either alone is a leak or a double-free.

    #[test]
    fn asan_vec_map_returned_from_helper_no_uaf() {
        // `make()` builds a `Vec[Map]` from a local map and returns it; the
        // read in `main` walks the returned map. Pre-fix the local's
        // scope-exit `FreeMapHandle` freed the handle inside `make`, so the
        // read in `main` is a heap-use-after-free (allocation churn between
        // forces the freed chunk's reuse). The value-correctness twin is
        // `tests/codegen.rs::test_e2e_vec_map_returned_from_helper`.
        assert_clean_asan_run(
            r#"
fn make() -> Vec[Map[i64, i64]] {
    let mut v: Vec[Map[i64, i64]] = Vec.new();
    let mut m: Map[i64, i64] = Map.new();
    m.insert(1i64, 777i64);
    v.push(m);
    v
}

fn main() {
    let v = make();
    let mut churn: Vec[Map[i64, i64]] = Vec.new();
    let mut i = 0i64;
    while i < 16i64 {
        let mut c: Map[i64, i64] = Map.new();
        c.insert(99i64, 1i64);
        churn.push(c);
        i = i + 1i64;
    }
    match v[0].get(1i64) { Some(x) => println(x), None => println(-1i64) }
}
"#,
            &["777"],
            "vec_map_returned_from_helper_no_uaf",
        );
    }

    #[test]
    fn asan_vec_map_push_then_read_same_scope_single_free() {
        // `v.push(m)` then a same-scope read + scope exit. The Vec now owns
        // the handle (push suppressed the source `m`'s `FreeMapHandle`); on
        // scope exit ONLY the Vec's element drop frees it. A missing
        // suppression here is a double-free (both `m` and the Vec free the
        // same handle) — this pins the all-frames suppression scan (the
        // moved binding's `FreeMapHandle` can sit one frame below the
        // push's transient arg frame).
        assert_clean_asan_run(
            r#"
fn main() {
    let mut v: Vec[Map[i64, i64]] = Vec.new();
    let mut m: Map[i64, i64] = Map.new();
    m.insert(1i64, 777i64);
    v.push(m);
    match v[0].get(1i64) { Some(x) => println(x), None => println(-1i64) }
}
"#,
            &["777"],
            "vec_map_push_then_read_same_scope_single_free",
        );
    }

    #[test]
    fn asan_vec_map_loop_built_returned_no_uaf() {
        // Loop-built `Vec[Map]` (the realistic scope-stack shape) returned
        // and indexed — exercises N>1 element handles through the drop loop.
        assert_clean_asan_run(
            r#"
fn make() -> Vec[Map[i64, i64]] {
    let mut v: Vec[Map[i64, i64]] = Vec.new();
    let mut i = 0i64;
    while i < 5i64 {
        let mut m: Map[i64, i64] = Map.new();
        m.insert(i, i * 100i64);
        v.push(m);
        i = i + 1i64;
    }
    v
}

fn main() {
    let v = make();
    println(v.len());
    match v[3].get(3i64) { Some(x) => println(x), None => println(-1i64) }
}
"#,
            &["5", "300"],
            "vec_map_loop_built_returned_no_uaf",
        );
    }

    // ── Struct field drop synthesis (2026-05-14, slice γ) ────────
    // `track_struct_var` + `emit_struct_drop_synthesis` emit a per-struct
    // `__karac_drop_struct_<Name>` function that frees each heap-owning
    // field's content on scope exit. Vec / String fields free their data
    // buffer (`cap > 0` guard); Map / Set fields call
    // `karac_map_free_with_drop_vec`. The move-aware
    // `suppress_source_vec_cleanup_for_arg` is extended for struct
    // identifiers — walks fields and zeros each Vec/String field's `cap`
    // — so `return h` / `let g = h` / `consume(h)` don't double-free
    // the inner buffer against the consumer's own tracking.

    #[test]
    fn asan_struct_with_vec_field_freed_on_scope_exit() {
        // Struct with Vec field — the canonical "compose a heap-owning
        // type into a value-type wrapper" pattern. Pre-fix the struct
        // had no scope-exit drop, so the inner Vec's data buffer leaked
        // when h went out of scope.
        assert_clean_asan_run(
            r#"
struct Holder { v: Vec[i64] }
fn build() -> i64 {
    let mut inner: Vec[i64] = Vec.new();
    inner.push(1i64);
    inner.push(2i64);
    inner.push(3i64);
    let h: Holder = Holder { v: inner };
    42i64
}
fn main() {
    let mut s = 0i64;
    let mut i = 0i64;
    while i < 10 {
        s = s + build();
        i = i + 1;
    }
    println(s);
}
"#,
            &["420"],
            "struct_with_vec_field_freed_on_scope_exit",
        );
    }

    #[test]
    fn asan_struct_with_vec_field_returned_no_double_free() {
        // `return h` where h has a Vec field — the move-aware suppress
        // in `suppress_source_vec_cleanup_for_arg` must walk h's fields
        // and zero each Vec field's cap so the function-end StructDrop
        // is a no-op for the returned value (the caller now owns it
        // and will run its own StructDrop). Pre-suppress, this
        // double-freed and SIGABRTed / hung on macOS allocator.
        assert_clean_asan_run(
            r#"
struct Holder { v: Vec[i64] }
fn build() -> Holder {
    let mut inner: Vec[i64] = Vec.new();
    inner.push(1i64);
    inner.push(2i64);
    let h: Holder = Holder { v: inner };
    h
}
fn first_elem(h: Holder) -> i64 {
    let inner = h.v;
    inner[0]
}
fn main() {
    let h = build();
    let f = first_elem(h);
    println(f);
}
"#,
            &["1"],
            "struct_with_vec_field_returned_no_double_free",
        );
    }

    // ── B-2026-06-10-2: moving a heap field OUT of a by-value struct PARAM ──
    // The param is a shallow copy whose field buffer aliases the caller's; the
    // moved-out local is deep-copied so it owns an independent buffer (the
    // caller's struct-drop frees the original exactly once). The repro above
    // (`asan_struct_with_vec_field_returned_no_double_free`) is the base case;
    // these pin the reuse / String-field / field-return shapes.

    #[test]
    fn asan_struct_param_field_move_reuse_no_double_free() {
        // Passing the same struct by value TWICE — each `first_elem(h)`
        // deep-copies `h.v`, so each callee frees its own copy and `main` frees
        // the original once. Pre-fix this double-freed (exit 134).
        assert_clean_asan_run(
            r#"
struct Holder { v: Vec[i64] }
fn build() -> Holder { let mut inner: Vec[i64] = Vec.new(); inner.push(7i64); let h: Holder = Holder { v: inner }; h }
fn first_elem(h: Holder) -> i64 { let inner = h.v; inner[0] }
fn main() { let h = build(); let a = first_elem(h); let b = first_elem(h); println(a + b); }
"#,
            &["14"],
            "struct_param_field_move_reuse",
        );
    }

    #[test]
    fn asan_struct_param_string_field_move_no_double_free() {
        // String field (layout-equivalent to `Vec[u8]`) moved out of a by-value
        // struct param — same deep-copy path, exercises the `String`/i8-elem arm.
        assert_clean_asan_run(
            r#"
struct Named { name: String }
fn build() -> Named { let mut s = String.new(); s.push_str("hi"); let n: Named = Named { name: s }; n }
fn firstn(n: Named) -> i64 { let inner = n.name; inner.len() }
fn main() { let n = build(); println(firstn(n)); }
"#,
            &["2"],
            "struct_param_string_field_move",
        );
    }

    #[test]
    fn asan_struct_param_field_returned_no_double_free() {
        // The moved-out field is RETURNED to the caller: the deep-copy is the
        // returned value (caller owns it), the param's original field is freed
        // by the outer `main`'s struct-drop — two independent buffers.
        assert_clean_asan_run(
            r#"
struct Holder { v: Vec[i64] }
fn build() -> Holder { let mut inner: Vec[i64] = Vec.new(); inner.push(9i64); let h: Holder = Holder { v: inner }; h }
fn takev(h: Holder) -> Vec[i64] { let inner = h.v; inner }
fn main() { let h = build(); let v = takev(h); println(v[0]); }
"#,
            &["9"],
            "struct_param_field_returned",
        );
    }

    #[test]
    fn asan_struct_with_string_field_freed_on_scope_exit() {
        // String is layout-equivalent to Vec[u8] (`{ptr, len, cap}`)
        // and is treated identically by the struct-drop synthesis.
        assert_clean_asan_run(
            r#"
struct Named { name: String }
fn build() -> i64 {
    let mut s = String.new();
    s.push_str("hello");
    let n: Named = Named { name: s };
    99i64
}
fn main() {
    let mut sum = 0i64;
    let mut i = 0i64;
    while i < 5 {
        sum = sum + build();
        i = i + 1;
    }
    println(sum);
}
"#,
            &["495"],
            "struct_with_string_field_freed_on_scope_exit",
        );
    }

    #[test]
    fn asan_struct_with_multiple_vec_fields_freed_on_scope_exit() {
        // Two Vec fields in one struct — verifies the per-field loop
        // in `emit_struct_drop_synthesis` correctly emits cleanup for
        // both, not just the first.
        assert_clean_asan_run(
            r#"
struct Pair { a: Vec[i64], b: Vec[i64] }
fn build() -> i64 {
    let mut x: Vec[i64] = Vec.new();
    x.push(10i64);
    let mut y: Vec[i64] = Vec.new();
    y.push(20i64);
    y.push(30i64);
    let p: Pair = Pair { a: x, b: y };
    0i64
}
fn main() {
    let mut s = 0i64;
    let mut i = 0i64;
    while i < 5 {
        s = s + build();
        i = i + 1;
    }
    println(s);
}
"#,
            &["0"],
            "struct_with_multiple_vec_fields_freed_on_scope_exit",
        );
    }

    // ── Auto-par scope cleanup ────────────────────────────────────
    //
    // Pre-fix the auto-par codegen path (`emit_par_branch_fn`) didn't
    // push a root cleanup frame at branch entry, so every
    // `track_vec_var` / `track_map_var` / `track_rc_var` call inside the
    // branch silently failed to queue (their bodies are `if let Some(frame)
    // = self.scope_cleanup_actions.last_mut()`). The branch's accumulated
    // cleanup queue was also discarded on normal completion — only the
    // cancel-path called `emit_scope_cleanup`. Result: every branch-local
    // heap allocation leaked at branch exit, and any class-(ii) slot
    // binding's heap buffer leaked at the parent function's scope-exit
    // (parent didn't `track_vec_var` the slot's loaded alloca).
    //
    // The kata-6 (zigzag) bench at K = 10,000 measured ~474 MiB peak RSS
    // from this leak. The fix:
    //   1. par_blocks.rs: push a fresh cleanup frame at branch entry;
    //      call `emit_scope_cleanup` before the branch's normal-completion
    //      `ret void`, with cap-zero suppression on slot-source allocas
    //      to prevent the slot's heap buffer from being freed twice
    //      (branch + parent).
    //   2. stmts.rs: re-enable `track_vec_var` on the parent's slot
    //      alloca so the buffer is freed at parent scope-exit.
    //
    // These tests exercise the shapes that surfaced the leak in the
    // 2026-05-17 kata-6 bench investigation; without the fix they
    // produced LeakSanitizer reports of ~10 MiB+ accumulated leak per
    // run.

    #[test]
    fn asan_auto_par_function_local_vec_freed_on_branch_exit() {
        // Bare Vec[i64] allocated inside a function called from a
        // 10-iter loop. Auto-par groups the let-stmts inside `build`,
        // dispatching the Vec allocation into a branch — without the
        // fix, the branch's track_vec_var no-ops and the slot's
        // parent-side alloca isn't tracked either; ~10 KB leak per
        // call. With the fix, the parent's `track_vec_var` runs at
        // function exit and frees the heap data.
        assert_clean_asan_run(
            r#"
fn build(n: i64) -> i64 {
    let mut v: Vec[i64] = Vec.new();
    let mut i = 0i64;
    while i < n {
        v.push(i);
        i = i + 1;
    }
    v.len()
}
fn main() {
    let mut sum = 0i64;
    let mut k = 0i64;
    while k < 10 {
        sum = sum + build(100);
        k = k + 1;
    }
    println(sum);
}
"#,
            &["1000"],
            "auto_par_function_local_vec_freed_on_branch_exit",
        );
    }

    #[test]
    fn asan_auto_par_vec_of_vec_freed_on_branch_exit() {
        // Vec[Vec[char]] built inside a function — the kata-6 zigzag
        // shape. Each call's per-row inner Vecs and outer Vec
        // allocate; without the fix all of these leak. The recursive-
        // drop fast path inside `FreeVecBuffer` handles the inner
        // Vec[char] buffers when the outer Vec drops; the fix routes
        // through that path correctly when the outer Vec is registered
        // via the parent-side `track_vec_var`.
        assert_clean_asan_run(
            r#"
fn build(n: i64) -> i64 {
    let mut rows: Vec[Vec[char]] = Vec.new();
    let mut r = 0i64;
    while r < 4 {
        let row: Vec[char] = Vec.new();
        rows.push(row);
        r = r + 1;
    }
    let mut i = 0i64;
    while i < n {
        rows[i % 4].push('A');
        i = i + 1;
    }
    rows[0].len()
}
fn main() {
    let mut sum = 0i64;
    let mut k = 0i64;
    while k < 10 {
        sum = sum + build(100);
        k = k + 1;
    }
    println(sum);
}
"#,
            &["250"],
            "auto_par_vec_of_vec_freed_on_branch_exit",
        );
    }

    #[test]
    fn asan_auto_par_vec_char_return_freed_on_caller_scope_exit() {
        // Function returns a Vec[char] consumed by the caller. The
        // class-(ii) slot machinery moves the branch's local Vec to a
        // parent-side alloca; with the fix that parent alloca is
        // `track_vec_var`-registered so the buffer is freed when the
        // surrounding function returns.
        assert_clean_asan_run(
            r#"
fn build_chars(n: i64) -> Vec[char] {
    let mut out: Vec[char] = Vec.new();
    let mut i = 0i64;
    while i < n {
        out.push('X');
        i = i + 1;
    }
    out
}
fn main() {
    let mut sum = 0i64;
    let mut k = 0i64;
    while k < 10 {
        let v = build_chars(100);
        sum = sum + v.len();
        k = k + 1;
    }
    println(sum);
}
"#,
            &["1000"],
            "auto_par_vec_char_return_freed_on_caller_scope_exit",
        );
    }

    // Regression for the "fn taking `Option[shared T]` chain hangs" bug
    // surfaced during kata 2 (add-two-numbers) reduction. Pre-fix, calling
    // a helper fn with `list: Option[Node]` argument on a linked list built
    // by a `from_arr` loop (`tail.next = Some(node); tail = node`) hung
    // indefinitely — somewhere in the scope-exit recursive drop path of
    // the chain. The companion test `asan_auto_par_shared_struct_option_
    // return_slot` below had to use inline match to avoid this codegen
    // bug. The hang is gone on current main as a side effect of the
    // intervening Option[shared T] refcount tracking + par-branch RC
    // suppression work (commits 3c77a10, 19b998d, codegen.rs §
    // `fn_return_option_inner_shared` / `track_rc_option_var`). Keep both
    // shapes — helper-fn and inline-match — to lock the fix in place.
    #[test]
    fn asan_option_shared_chain_through_helper_fn() {
        assert_clean_asan_run(
            r#"
shared struct Node {
    val: i64,
    mut next: Option[Node],
}

fn from_arr(arr: Vec[i64]) -> Option[Node] {
    let n = arr.len();
    if n == 0 {
        return None;
    }
    let head = Node { val: arr[0], next: None };
    let mut tail = head;
    let mut i = 1u64;
    while i < n {
        let node = Node { val: arr[i], next: None };
        tail.next = Some(node);
        tail = node;
        i = i + 1u64;
    }
    Some(head)
}

fn first_val(list: Option[Node]) -> i64 {
    match list {
        Some(n) => n.val,
        None => -1i64,
    }
}

fn main() {
    let mut a: Vec[i64] = Vec.new();
    a.push(10);
    a.push(20);
    a.push(30);
    let list = from_arr(a);
    println(first_val(list));
}
"#,
            &["10"],
            "option_shared_chain_through_helper_fn",
        );
    }

    #[test]
    fn asan_shared_struct_user_drop_recursive_chain_leak_free() {
        // phase-7 L938: a `shared struct` with a user `impl Drop` over a
        // recursive `Option[Self]` chain. The user body fires once per
        // link at that link's refcount→0 (the iterative self-chain fast
        // path is disabled when a user Drop exists, so each link routes
        // through `__karac_rc_drop_Node`). This must be leak-clean AND
        // free each node exactly once — no double-free from the body
        // running on top of the field walk + heap free.
        assert_clean_asan_run(
            r#"
shared struct Node { val: i64, mut next: Option[Node] }
impl Drop for Node {
    fn drop(mut ref self) {
        println(self.val);
    }
}
fn main() {
    let c = Node { val: 3, next: None };
    let b = Node { val: 2, next: Some(c) };
    let a = Node { val: 1, next: Some(b) };
    println(0);
}
"#,
            &["0", "1", "2", "3"],
            "shared_struct_user_drop_recursive_chain_leak_free",
        );
    }

    #[test]
    fn asan_user_drop_heap_enum_inline_temp_arg_leak_free() {
        // B-2026-06-10 carry-forward (enum arm): an inline enum temp with
        // BOTH a heap String payload and a user `impl Drop`, passed
        // directly as a call argument, registers the `karac_drop_<E>`
        // wrapper (user body) AND the payload-walking `__karac_drop_<E>`
        // on the same caller slot — complementary registrations, unlike
        // the struct case where the wrapper subsumes field cleanup. Must
        // be leak-clean (payload freed exactly once — the callee's entry
        // copy and the caller temp each free their own buffer) with the
        // user body firing once per temp. ≥36-byte payload defeats LSan's
        // short-string reachability masking.
        assert_clean_asan_run(
            r#"
enum Msg { Text(String), Nil }
impl Drop for Msg {
    fn drop(mut ref self) {
        println(1);
    }
}
fn consume(m: Msg) {}
fn main() {
    consume(Msg.Text("this is a long heap string payload over 36 bytes"));
    consume(Msg.Nil);
    println(0);
}
"#,
            &["1", "1", "0"],
            "user_drop_heap_enum_inline_temp_arg_leak_free",
        );
    }

    #[test]
    fn asan_auto_par_shared_struct_option_return_slot() {
        // A par group with two effectful stmts where one returns
        // `Option[shared T]` consumed in the parent scope. Pre-fix
        // (2026-05-17), the branch's `emit_scope_cleanup` ran the
        // queued `RcDecOption` on the slot-source local, dropping the
        // head Node's refcount to 0 → freed. The parent's load from
        // the return slot then yielded a dangling pointer; the
        // kata 2 add-two-numbers bench manifested as `node.val = 0`
        // (allocator-zeroed memory). Fix added RcDec/RcDecOption
        // suppression to the par-branch slot-source loop (analog to
        // the existing Vec `cap=0` suppression).
        //
        // The test geometry mirrors the kata 2 reduction: `make_vec`
        // returns `Vec[i64]`, `from_arr` returns `Option[Node]` where
        // `Node` is a `shared struct`. The analyzer parallelizes
        // `let b = make_vec(...)` and `let l1 = from_arr(...)` (both
        // effectful, independent vars, no effect-resource conflict).
        // The body prints `node.val` for the surviving head node —
        // 7 if the RC transfer worked, 0 (or ASAN error) if not.
        // Inline match on `l1` rather than passing through a helper fn
        // taking `Option[shared T]` by value — kept as-is for historical
        // continuity; the helper-fn shape that previously hung is now
        // covered separately by
        // `asan_option_shared_chain_through_helper_fn` above.
        assert_clean_asan_run(
            r#"
shared struct Node {
    val: i64,
    mut next: Option[Node],
}

fn make_vec(n: u64) -> Vec[i64] {
    let mut v: Vec[i64] = Vec.new();
    let mut i = 0u64;
    while i < n {
        v.push(7);
        i = i + 1u64;
    }
    v
}

fn from_arr(arr: Slice[i64]) -> Option[Node] {
    let n = arr.len();
    if n == 0 {
        return None;
    }
    let head = Node { val: arr[0], next: None };
    let mut tail = head;
    for i in 1..n {
        let node = Node { val: arr[i], next: None };
        tail.next = Some(node);
        tail = node;
    }
    Some(head)
}

fn main() {
    let a = make_vec(5u64);
    let b = make_vec(5u64);
    let l1 = from_arr(a.as_slice());

    match l1 {
        Some(n) => println(n.val),
        None => println(-1i64),
    }
    println(b.len());
}
"#,
            &["7", "5"],
            "auto_par_shared_struct_option_return_slot",
        );
    }

    // ── `ref T` arg from a non-place rvalue ──────────────────────
    //
    // C-followup (534c5b6 landed the materialization itself): when the
    // rvalue at a `ref T` arg position carries heap ownership — a
    // function returning an owned String/Vec, a `String + String`
    // concatenation, etc. — the materialized temp inside `compile_call`
    // owns that heap buffer. Without a cleanup registration, the
    // buffer is unreachable after the call returns (LeakSanitizer
    // would catch it on Linux; macOS ASAN can't surface leaks but can
    // still catch the inverse — a double-free if the registration
    // overshoots). Fix: temps whose value-type matches the
    // `{ptr,len,cap}` Vec/String layout are routed through
    // `track_vec_var`, picking up the same `FreeVecBuffer` cleanup
    // that `let`-bindings use. The walker's `cap > 0` guard makes
    // the registration safe for non-owning rvalues (string literals
    // are stored with `cap = 0` and short-circuit to no-op).

    // Observation: `println(s)` where `s: ref String` over a heap-
    // backed buffer trips an unrelated pre-existing
    // heap-buffer-overflow on macOS ASAN (puts reads 1 byte past
    // the buffer expecting a NUL; karac's heap-allocated Strings
    // don't NUL-terminate). The bug is shared by the let-binding
    // workaround too, so it's not part of this slice. The tests
    // below intentionally avoid `println(ref String)` of a heap
    // String — they observe via `.len()` instead so the ASAN check
    // focuses on whether the temp's FreeVecBuffer registration
    // handles the call-site materialization cleanly.

    #[test]
    fn asan_ref_arg_string_literal_no_double_free() {
        // Literal rvalue: cap=0, the FreeVecBuffer walker's `cap > 0`
        // guard must skip the free. A miss here would surface as a
        // double-free against the static buffer at scope exit.
        // `println(s.len())` avoids the println(ref String)
        // heap-buffer-overflow noted above.
        assert_clean_asan_run(
            r#"
fn show_len(s: ref String) {
    println(s.len());
}

fn main() {
    show_len("from literal rvalue");
}
"#,
            &["19"],
            "ref_arg_string_literal_no_double_free",
        );
    }

    #[test]
    fn asan_ref_arg_heap_string_concat_freed() {
        // Heap-owning rvalue from concatenation. The materialized
        // temp owns the joined buffer; cleanup must free it once at
        // scope exit (LeakSanitizer arm on Linux; UAF / double-free
        // on macOS). A double-free would fire if both the concat
        // helper and the temp's FreeVecBuffer registration ran.
        assert_clean_asan_run(
            r#"
fn show_len(s: ref String) {
    println(s.len());
}

fn main() {
    let a = "left ";
    let b = "right";
    show_len(a + b);
}
"#,
            &["10"],
            "ref_arg_heap_string_concat_freed",
        );
    }

    #[test]
    fn asan_ref_arg_function_return_string_freed() {
        // Function-return rvalue. Without this slice's track_vec_var
        // call on the materialized temp, the heap allocated inside
        // `make` would have no owner after the call returns. The
        // canonical case the commit message calls out.
        assert_clean_asan_run(
            r#"
fn make() -> String {
    let a = "made ";
    let b = "string";
    return a + b;
}

fn show_len(s: ref String) {
    println(s.len());
}

fn main() {
    show_len(make());
}
"#,
            &["11"],
            "ref_arg_function_return_string_freed",
        );
    }

    #[test]
    fn asan_ref_arg_repeated_calls_no_compound_leak() {
        // Calling `show_len(make())` in a loop. Each iteration's
        // materialized temp is in the same call-arg scope; without
        // proper cleanup, allocations would either pile up (leak
        // arm) or be freed against the wrong cap (double-free arm).
        // 8 iterations is small but enough that any per-iteration
        // imbalance would surface as a deterministic crash under
        // ASAN's quarantine.
        assert_clean_asan_run(
            r#"
fn make() -> String {
    let a = "hi ";
    let b = "there";
    return a + b;
}

fn show_len(s: ref String) {
    println(s.len());
}

fn main() {
    let mut i = 0;
    while i < 8 {
        show_len(make());
        i = i + 1;
    }
}
"#,
            &["8", "8", "8", "8", "8", "8", "8", "8"],
            "ref_arg_repeated_calls_no_compound_leak",
        );
    }

    #[test]
    fn asan_ref_arg_nested_vec_elem_freed() {
        // Slice 2 part B: a fresh `Vec[String]` rvalue passed to a `ref
        // Vec[String]` param. The prior `ref_rvalue_arg` path freed only the
        // outer buffer (`track_vec_var(temp, None)`), leaking each String
        // element's `{ptr,len,cap}` data. `queue_ref_rvalue_arg_cleanup` now
        // recovers the element type from `owned_temp_drops`, so the recursive
        // `FreeVecBuffer` walk frees the inner String buffers too. 8-iteration
        // loop: Linux `detect_leaks=1` is the leak oracle for the element
        // closure; macOS catches any double-free of the outer buffer.
        assert_clean_asan_run(
            r#"
fn make_vv() -> Vec[String] {
    let mut v: Vec[String] = Vec.new();
    v.push("alpha");
    v.push("beta");
    return v;
}

fn show(v: ref Vec[String]) {
    println(v.len());
}

fn main() {
    let mut i = 0;
    while i < 8 {
        show(make_vv());
        i = i + 1;
    }
}
"#,
            &["2", "2", "2", "2", "2", "2", "2", "2"],
            "ref_arg_nested_vec_elem_freed",
        );
    }

    #[test]
    fn asan_ref_arg_map_freed() {
        // Slice 2 part B: a fresh `Map[i64,i64]` handle passed to a `ref
        // Map[i64,i64]` param. The prior `ref_rvalue_arg` path only tracked
        // Vec/String-shaped temps, so a fresh Map handle passed by `ref`
        // leaked its whole control block. `queue_ref_rvalue_arg_cleanup`
        // recognizes the `Map[K,V]` TypeExpr via the hint table and queues a
        // `FreeMapHandle`. Loop amplifies any imbalance into a deterministic
        // macOS double-free fault; Linux catches the leak.
        assert_clean_asan_run(
            r#"
fn make_map() -> Map[i64, i64] {
    let mut m: Map[i64, i64] = Map.new();
    m.insert(1_i64, 2_i64);
    m.insert(3_i64, 4_i64);
    return m;
}

fn show(m: ref Map[i64, i64]) {
    println(m.len());
}

fn main() {
    let mut i = 0;
    while i < 8 {
        show(make_map());
        i = i + 1;
    }
}
"#,
            &["2", "2", "2", "2", "2", "2", "2", "2"],
            "ref_arg_map_freed",
        );
    }

    #[test]
    fn asan_method_chain_intermediate_vec_freed() {
        // Slice 3: `make_vec().len()` — a fresh-owned Vec temp is the receiver
        // of `len` (borrow). The receiver's heap buffer must drop after the
        // statement instead of leaking. 8-iteration loop: each iteration's
        // receiver temp is freed exactly once — Linux `detect_leaks=1` is the
        // leak oracle, macOS catches any double-free (e.g. a per-site reused
        // temp slot freed against a stale buffer, or compounding).
        assert_clean_asan_run(
            r#"
fn make_vec() -> Vec[i64] {
    let mut v: Vec[i64] = Vec.new();
    v.push(1_i64);
    v.push(2_i64);
    v.push(3_i64);
    return v;
}

fn main() {
    let mut total = 0i64;
    let mut i = 0;
    while i < 8 {
        total = total + make_vec().len();
        i = i + 1;
    }
    println(total);
}
"#,
            &["24"],
            "method_chain_intermediate_vec_freed",
        );
    }

    #[test]
    fn asan_method_chain_field_receiver_no_double_free() {
        // Double-free guard for slice 3's gate: a field-access receiver
        // (`h.items.len()`) reloads the buffer `h` owns — the receiver path
        // must NOT free it (only `h`'s scope-exit cleanup does). Looping the
        // read keeps `h` alive; a wrongful receiver-temp free would fault
        // under macOS ASAN (and `h`'s own free at scope exit would be the
        // second). Exercises the `expr_yields_fresh_owned_temp` exclusion.
        assert_clean_asan_run(
            r#"
struct Holder { items: Vec[i64] }

fn main() {
    let mut v: Vec[i64] = Vec.new();
    v.push(10_i64);
    v.push(20_i64);
    let h = Holder { items: v };
    let mut i = 0;
    while i < 8 {
        println(h.items.len());
        i = i + 1;
    }
}
"#,
            &["2", "2", "2", "2", "2", "2", "2", "2"],
            "method_chain_field_receiver_no_double_free",
        );
    }

    // ── 491: tail-expression temp drops before block-local lets ──
    //
    // phase-6-runtime.md line 491 — "Tail-expression temporary scope —
    // drop before block locals." The ordering rule is structural, not a
    // special case: a block's let-bindings and the materialized temp of
    // its tail expression share ONE scope-cleanup frame, pushed in
    // program order (the lets first, the tail-expr temp last because it
    // is later in source order). LIFO drain therefore frees the tail-
    // expr temp BEFORE every block-local let — the same unified-stack
    // mechanism pinned at IR level by
    // `test_ir_defer_drop_interleave_emission_order` (tests/codegen.rs).
    //
    // The only mid-expression temporary codegen tracks today is the
    // `ref T` Vec/String call-arg materialization (the `asan_ref_arg_*`
    // family above). This test puts one in TAIL position (`slen(make())`
    // with no trailing `;`) alongside a heap block-local `let v`, and
    // asserts ASAN-clean. A regression that hoisted the tail temp to the
    // outer scope, freed it against the wrong cap, or double-freed it
    // against the block-local `v` would surface here (leak arm on Linux;
    // UAF / double-free on macOS). The canonical MutexGuard *drop-order*
    // observation from the spec's test plan awaits a `MutexGuard` type
    // (mutex.kara is type-shape-only) and general method-chain temp
    // tracking; this pins the rule for every temporary tracked today.
    #[test]
    fn asan_tail_expr_temp_coexists_with_block_local_let() {
        assert_clean_asan_run(
            r#"
fn make() -> String {
    let a = "tail ";
    let b = "temp";
    return a + b;
}

fn slen(s: ref String) {
    println(s.len());
}

fn main() {
    let mut v: Vec[i64] = Vec.new();
    v.push(1_i64);
    v.push(2_i64);
    v.push(3_i64);
    println(f"v={v.len()}");
    slen(make())
}
"#,
            &["v=3", "9"],
            "tail_expr_temp_coexists_with_block_local_let",
        );
    }

    // ── general owned-temp tracking, slice 5 (phase-6 line 497) ──
    //
    // docs/spikes/general-owned-temp-tracking.md slice 5 closes the
    // *tail-expr temp leak*: a fresh owned temp produced in the tail of a
    // *discarded* block (`{ make() }` in statement position, or
    // `let _ = { make() };`) is the block's return value — its own frame
    // drops only the block-local lets, so the escaping tail temp was never
    // freed. `discarded_owned_temp_tail` peels the single-tail block wrapper
    // and routes the tail through the owned-temp chokepoint. On Linux the
    // unfreed buffer is the LeakSanitizer oracle; on macOS (no LSan) the
    // repeated discard in a loop is a *double-free* gate — a tail temp freed
    // against a buffer some other cleanup also owns would fault under ASAN.

    #[test]
    fn asan_discarded_block_tail_temp_freed() {
        // `{ make_vv() }` in statement position. `Vec[String]` so the
        // *nested* element buffers must also free (the element TypeExpr flows
        // from `owned_temp_drops` keyed on the peeled tail call's span); an
        // elem_ty: None regression would leak the inner Strings on Linux.
        // 8-iteration loop amplifies any per-iteration imbalance into a
        // deterministic macOS fault.
        assert_clean_asan_run(
            r#"
fn make_vv() -> Vec[String] {
    let mut v: Vec[String] = Vec.new();
    v.push("alpha");
    v.push("beta");
    return v;
}

fn main() {
    let mut i = 0;
    while i < 8 {
        { make_vv() }
        i = i + 1;
    }
    println(i);
}
"#,
            &["8"],
            "discarded_block_tail_temp_freed",
        );
    }

    #[test]
    fn asan_let_wildcard_block_tail_temp_freed() {
        // `let _ = { make_map() };` — wildcard-let discard of a block-tail
        // Map handle. Routes through the early Wildcard arm; the chokepoint
        // recognizes the `Map[K,V]` TypeExpr (hint table) and queues a
        // `karac_map_free` against the peeled tail. Looped to turn any
        // double-free of the map handle into a macOS fault.
        assert_clean_asan_run(
            r#"
fn make_map() -> Map[i64, i64] {
    let mut m: Map[i64, i64] = Map.new();
    m.insert(1_i64, 2_i64);
    return m;
}

fn main() {
    let mut i = 0;
    while i < 8 {
        let _ = { make_map() };
        i = i + 1;
    }
    println(i);
}
"#,
            &["8"],
            "let_wildcard_block_tail_temp_freed",
        );
    }

    #[test]
    fn asan_discarded_block_tail_temp_with_block_local_no_double_free() {
        // The block carries BOTH a heap-local `let` (`local`, dropped by the
        // block's own frame at block exit) AND a fresh-owned tail temp
        // (`make_vv()`, dropped by the discard arm's one-shot frame). Each
        // must free exactly once — a regression that materialized the tail
        // against the block-local's slot, or double-counted, would double-free
        // under macOS ASAN. (Drop *order* — tail before local — is a slice-6
        // observation concern and not asserted here; this pins leak/UAF
        // cleanliness only.)
        assert_clean_asan_run(
            r#"
fn make_vv() -> Vec[String] {
    let mut v: Vec[String] = Vec.new();
    v.push("x");
    return v;
}

fn main() {
    let mut i = 0;
    while i < 8 {
        {
            let mut local: Vec[String] = Vec.new();
            local.push("y");
            println(local.len());
            make_vv()
        }
        i = i + 1;
    }
    println(i);
}
"#,
            &["1", "1", "1", "1", "1", "1", "1", "1", "8"],
            "discarded_block_tail_temp_with_block_local_no_double_free",
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

    #[test]
    fn asan_iflet_freshtemp_enum_bound_field_no_double_free() {
        // `if let Full(v, n) = make()` — the Vec is moved into `v`. The
        // materialized temp's EnumDrop must SKIP that field (cap zeroed by
        // suppression); `v`'s own cleanup frees it once. Without suppression
        // this double-frees under macOS ASAN.
        let src = format!(
            "{B_ASAN_PRELUDE}\nfn main() {{\n    let mut i = 0;\n    while i < 8 {{\n        if let Holder.Full(v, n) = make() {{ println(v.len() + n); }}\n        i = i + 1;\n    }}\n    println(i);\n}}\n"
        );
        assert_clean_asan_run(
            &src,
            &["44", "44", "44", "44", "44", "44", "44", "44", "8"],
            "iflet_freshtemp_enum_bound_field_no_double_free",
        );
    }

    #[test]
    fn asan_iflet_freshtemp_enum_unbound_field_clean() {
        // `if let Full(_, n) = make()` — the Vec is UNBOUND. The enum drop walk
        // frees it (Linux leak oracle). macOS verifies the added drop doesn't
        // fault (e.g. freeing a garbage/aliased pointer).
        let src = format!(
            "{B_ASAN_PRELUDE}\nfn main() {{\n    let mut i = 0;\n    while i < 8 {{\n        if let Holder.Full(_, n) = make() {{ println(n); }}\n        i = i + 1;\n    }}\n    println(i);\n}}\n"
        );
        assert_clean_asan_run(
            &src,
            &["42", "42", "42", "42", "42", "42", "42", "42", "8"],
            "iflet_freshtemp_enum_unbound_field_clean",
        );
    }

    #[test]
    fn asan_match_freshtemp_enum_unbound_field_clean() {
        // `match make() { Full(_, n) => …, Empty => … }` — match surface of the
        // unbound-field drop. The matched `Full(_, _)` arm's unbound Vec is
        // freed by the materialized temp's EnumDrop.
        let src = format!(
            "{B_ASAN_PRELUDE}\nfn main() {{\n    let mut i = 0;\n    while i < 8 {{\n        match make() {{ Holder.Full(_, n) => println(n), Holder.Empty => println(0) }}\n        i = i + 1;\n    }}\n    println(i);\n}}\n"
        );
        assert_clean_asan_run(
            &src,
            &["42", "42", "42", "42", "42", "42", "42", "42", "8"],
            "match_freshtemp_enum_unbound_field_clean",
        );
    }

    #[test]
    fn asan_iflet_freshtemp_enum_miss_wholesale_clean() {
        // Miss edge: `make()` returns `Full(Vec, _)` but the pattern is
        // `Empty`, so the arm misses and the whole heap-bearing temp must drop
        // wholesale before/at the else. No suppression runs on the miss edge,
        // so the enum drop walk frees the entire payload. Looped leak/UAF gate.
        let src = format!(
            "{B_ASAN_PRELUDE}\nfn main() {{\n    let mut i = 0;\n    while i < 8 {{\n        if let Holder.Empty = make() {{ println(1); }} else {{ println(2); }}\n        i = i + 1;\n    }}\n    println(i);\n}}\n"
        );
        assert_clean_asan_run(
            &src,
            &["2", "2", "2", "2", "2", "2", "2", "2", "8"],
            "iflet_freshtemp_enum_miss_wholesale_clean",
        );
    }

    #[test]
    fn asan_letelse_freshtemp_enum_bound_field_no_double_free() {
        // let-else surface, bound field: `let Full(v, n) = make() else { … }`.
        // The escaped `v` binding frees the Vec; the materialized temp's
        // EnumDrop (drained at enclosing-scope exit) must skip it. macOS
        // double-free gate for the let-else suppression edge.
        let src = format!(
            "{B_ASAN_PRELUDE}\nfn count() -> i64 {{\n    let Holder.Full(v, n) = make() else {{ return 0 }}\n    return v.len() + n\n}}\nfn main() {{\n    let mut i = 0;\n    while i < 8 {{\n        println(count());\n        i = i + 1;\n    }}\n    println(i);\n}}\n"
        );
        assert_clean_asan_run(
            &src,
            &["44", "44", "44", "44", "44", "44", "44", "44", "8"],
            "letelse_freshtemp_enum_bound_field_no_double_free",
        );
    }

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

    #[test]
    fn asan_whilelet_freshtemp_enum_unbound_field_clean() {
        // `while let Full(_, n) = next(i)` — the Vec is unbound each iteration;
        // the per-iteration EnumDrop frees it before the next scrutinee eval.
        let src = format!(
            "{B_WHILELET_PRELUDE}\nfn main() {{\n    let mut i = 0;\n    while let Holder.Full(_, n) = next(i) {{\n        println(n);\n        i = i + 1;\n    }}\n    println(99);\n}}\n"
        );
        assert_clean_asan_run(
            &src,
            &["0", "1", "2", "3", "4", "5", "99"],
            "whilelet_freshtemp_enum_unbound_field_clean",
        );
    }

    #[test]
    fn asan_whilelet_freshtemp_enum_bound_field_no_double_free() {
        // `while let Full(v, n) = next(i)` — the Vec is moved into `v` each
        // iteration. Suppression zeroes the moved field's cap in the
        // (reused) alloca so the per-iteration EnumDrop skips it; `v`'s
        // per-iteration binding cleanup frees it once. A double-free would
        // fault on macOS.
        let src = format!(
            "{B_WHILELET_PRELUDE}\nfn main() {{\n    let mut i = 0;\n    while let Holder.Full(v, n) = next(i) {{\n        println(v.len() + n);\n        i = i + 1;\n    }}\n    println(99);\n}}\n"
        );
        assert_clean_asan_run(
            &src,
            &["2", "3", "4", "5", "6", "7", "99"],
            "whilelet_freshtemp_enum_bound_field_no_double_free",
        );
    }

    #[test]
    fn asan_whilelet_miss_variant_no_double_free() {
        // B follow-up #2: the loop terminates on a *heap-bearing* non-matching
        // variant (`Stop(Vec)` vs the matched `Go`). The final scrutinee is
        // freed wholesale on the new `whilelet.miss` edge. This guards the fix
        // against a double-free (macOS ASAN has no LeakSanitizer, so the leak
        // closure itself is pinned by the IR test; here we verify the
        // wholesale miss-drop doesn't double-free against the per-iteration
        // bound-field cleanup of the matched iterations). Several matches then
        // one miss.
        assert_clean_asan_run(
            r#"
enum Item { Go(Vec[i64]), Stop(Vec[i64]) }
fn mk(x: i64) -> Vec[i64] {
    let mut v: Vec[i64] = Vec.new();
    v.push(x);
    return v;
}
fn step(c: i64) -> Item {
    if c < 3 {
        return Item.Go(mk(c));
    }
    return Item.Stop(mk(99));
}
fn main() {
    let mut c: i64 = 0;
    while let Go(xs) = step(c) {
        println(xs.len() + c);
        c = c + 1;
    }
    println(c);
}
"#,
            &["1", "2", "3", "3"],
            "whilelet_miss_variant_no_double_free",
        );
    }

    #[test]
    fn asan_enum_nested_struct_payload_inplace_drop_no_leak() {
        // B-2026-06-13-13 part 1: an enum variant whose payload is a nested
        // non-shared user struct that carries heap (`Wrap(Inner { data: Vec, … })`
        // — the lexer's `CStringLiteral(CStr { bytes: Vec[u8], … })` shape). The
        // enum drop now recurses into the nested struct's `__karac_drop_struct_<S>`
        // (it previously classified the payload `None` and leaked the inner Vec).
        // Exercises the WHOLE-VALUE in-place drop path — the one the lexer hits
        // when it drops a `Vec[SpannedToken]` wholesale — via a non-consuming
        // wildcard match so the enum is dropped, not moved out. On Linux CI this
        // faults under LeakSanitizer if the nested-struct drop regresses; on macOS
        // (no LSan) it is the double-free gate — the deep-copy-on-entry keeps the
        // callee copy and caller original independent, so re-dropping would fault.
        assert_clean_asan_run(
            r#"
struct Inner { data: Vec[i64], tag: i64 }
enum E { Wrap(Inner), Empty }
fn mkvec(x: i64) -> Vec[i64] {
    let mut v: Vec[i64] = Vec.new();
    v.push(x);
    v.push(x + 1);
    return v;
}
fn main() {
    let mut sum: i64 = 0;
    let mut i = 0;
    while i < 50 {
        let e = E.Wrap(Inner { data: mkvec(i), tag: i });
        let k = match e {
            Wrap(_) => 1,
            Empty => 0,
        };
        sum = sum + k;
        i = i + 1;
    }
    println(sum);
}
"#,
            &["50"],
            "enum_nested_struct_payload_inplace_drop_no_leak",
        );
    }

    #[test]
    fn asan_enum_nested_struct_payload_moved_out_no_leak_no_double_free() {
        // B-2026-06-13-13 residual A: a nested-struct enum payload MOVED OUT of
        // the enum — bound by a `match` (`Wrap(inner)`), passed by value into a
        // fn that binds it out, returned as the arm tail, and re-used after a
        // consuming call. Each path now registers the moved-out struct binding
        // for `StructDrop` (pattern_binding.rs), kept symmetric with the source
        // move-suppression so it frees exactly once. On Linux CI this faults
        // under LeakSanitizer if the binding drop regresses (a leak); on macOS it
        // is the double-free gate — the copy-supported deep-copy keeps the caller
        // original valid after the callee frees its copy (the `sink+reuse` arm),
        // so a missed-suppression double-free or a stale-alias use-after-free
        // faults here. `Inner` is copy-supported (Vec + i64), so the binding IS
        // tracked; an `Option`/`Result` or Map-bearing payload is excluded
        // (covered by `asan_freshtemp_boxed_option_match_move_out_no_double_free`).
        assert_clean_asan_run(
            r#"
struct Inner { data: Vec[i64], n: i64 }
enum E { Wrap(Inner), Empty }
fn mk(x: i64) -> Vec[i64] {
    let mut v: Vec[i64] = Vec.new();
    v.push(x);
    v.push(x + 1);
    return v;
}
fn sink(e: E) -> i64 {
    match e {
        Wrap(inner) => inner.n,
        Empty => 0,
    }
}
fn unwrap_or(e: E) -> Inner {
    match e {
        Wrap(inner) => inner,
        Empty => Inner { data: mk(0), n: 0 },
    }
}
fn main() {
    let mut t: i64 = 0;
    let mut i = 0;
    while i < 20 {
        // match-bind move-out into a local, then consume
        let e1 = E.Wrap(Inner { data: mk(i), n: 1 });
        let r1 = match e1 {
            Wrap(inner) => inner.data.len() + inner.n,
            Empty => 0,
        };
        // by-value pass into a fn that binds the payload out
        let e2 = E.Wrap(Inner { data: mk(i), n: 2 });
        let r2 = sink(e2);
        // tail-return the moved-out struct binding
        let e3 = E.Wrap(Inner { data: mk(i), n: 3 });
        let got = unwrap_or(e3);
        // consuming call, then re-use the (copy-supported, callee-owned) original
        let e4 = E.Wrap(Inner { data: mk(i), n: 4 });
        let a = sink(e4);
        let b = match e4 {
            Wrap(inner) => inner.n,
            Empty => 0,
        };
        t = t + r1 + r2 + got.n + a + b;
        i = i + 1;
    }
    println(t);
}
"#,
            &["320"],
            "enum_nested_struct_payload_moved_out_no_leak_no_double_free",
        );
    }

    #[test]
    fn asan_inline_enum_ctor_call_arg_no_leak_no_double_free() {
        // B-2026-06-12-10: an inline enum-variant constructor passed by value as
        // a call argument (`wrap(Tok.V(mk()))`) — and the method form
        // (`m.wrap(Tok.V(mk()))`, the shape the self-hosted lexer hits via
        // `self.make_spanned(Token.StringLiteral(value))`) — is a fresh owned
        // temp the callee owns by deep-copy. The caller still owns the temp and
        // must drop it; that caller-side drop was missing, leaking the variant's
        // String payload once per call (the dominant self-hosted-lexer leak).
        // The let-bound form (`let t = Tok.V(mk()); wrap(t)`) was already clean,
        // so this guards the now-symmetric inline path. On Linux CI this faults
        // under LeakSanitizer if the drop regresses; on macOS (no LSan) it is the
        // double-free gate — re-dropping the callee-owned copy would fault here.
        // Loops so any per-iteration imbalance accumulates into a fault.
        assert_clean_asan_run(
            r#"
enum Tok { V(String), Empty }
struct Wrap { t: Tok, n: i64 }
struct Maker { id: i64 }
fn mk() -> String {
    let mut s = "".to_string();
    s.push_str("inline_enum_ctor_arg_payload");
    s
}
fn wrap_free(t: Tok) -> Wrap {
    Wrap { t: t, n: 1 }
}
impl Maker {
    fn wrap(ref self, t: Tok) -> Wrap {
        Wrap { t: t, n: self.id }
    }
}
fn tlen(w: Wrap) -> i64 {
    match w.t {
        V(s) => s.len(),
        Empty => 0,
    }
}
fn main() {
    let m = Maker { id: 1 };
    let mut total: i64 = 0;
    let mut i = 0;
    while i < 50 {
        let a = wrap_free(Tok.V(mk()));
        let b = m.wrap(Tok.V(mk()));
        total = total + tlen(a) + tlen(b);
        i = i + 1;
    }
    println(total);
}
"#,
            &["2800"],
            "inline_enum_ctor_call_arg_no_leak_no_double_free",
        );
    }

    #[test]
    fn asan_struct_destructure_bound_and_unbound_no_double_free() {
        // B follow-up #3: an owned struct destructure of a fresh temp where
        // one heap field is bound (`a`, freed via its binding) and another is
        // discarded (`b: _`, freed via a synthetic discard slot). Run in a
        // loop so any per-iteration imbalance — a double-free of `a` against a
        // whole-struct drop, or a missed/extra free of `b` — faults under
        // ASAN's quarantine. macOS has no LeakSanitizer, so the leak closure
        // is pinned by the IR tests; this is the double-free gate.
        assert_clean_asan_run(
            r#"
struct Pair { a: Vec[i64], b: Vec[i64], n: i64 }
fn mk(x: i64) -> Pair {
    let mut va: Vec[i64] = Vec.new();
    va.push(x);
    let mut vb: Vec[i64] = Vec.new();
    vb.push(x * 2);
    return Pair { a: va, b: vb, n: x };
}
fn main() {
    let mut i: i64 = 0;
    while i < 5 {
        let Pair { a, b: _, n } = mk(i);
        println(a.len() + n);
        i = i + 1;
    }
    println(99);
}
"#,
            &["1", "2", "3", "4", "5", "99"],
            "struct_destructure_bound_and_unbound_no_double_free",
        );
    }

    #[test]
    fn asan_struct_destructure_set_bound_and_unbound_no_double_free() {
        // Set leg of B follow-up #3 (closes the Set remaining-leak gap): a
        // fresh-temp struct destructure where one `Set` field is bound (`a`,
        // freed via its binding) and another discarded (`b: _`, freed via a
        // synthetic discard slot). Set lowers to `Map[T, ()]`, so both route
        // through `karac_map_free`. Looped so a double-free or missed/extra
        // free of either handle faults under ASAN's quarantine. macOS has no
        // LeakSanitizer, so the leak closure is pinned by the IR tests; this
        // is the double-free gate.
        assert_clean_asan_run(
            r#"
struct Pair { a: Set[i64], b: Set[i64], n: i64 }
fn mk(x: i64) -> Pair {
    let mut sa: Set[i64] = Set.new();
    sa.insert(x);
    let mut sb: Set[i64] = Set.new();
    sb.insert(x * 2);
    return Pair { a: sa, b: sb, n: x };
}
fn main() {
    let mut i: i64 = 0;
    while i < 5 {
        let Pair { a, b: _, n } = mk(i);
        println(a.len() + n);
        i = i + 1;
    }
    println(99);
}
"#,
            &["1", "2", "3", "4", "5", "99"],
            "struct_destructure_set_bound_and_unbound_no_double_free",
        );
    }

    #[test]
    fn asan_nested_struct_pattern_no_double_free() {
        // Nested struct pattern (`let Outer { inner: Inner { data }, n } = mk()`)
        // — the dispatch fix made `data.len()` compile; this confirms the
        // nested field's heap is freed exactly once. The enclosing `inner`
        // field is discard-freed as a unit (running Inner's drop → frees the
        // Vec), and `data` carries no separate cleanup, so looping faults under
        // ASAN if the Vec is freed twice (or aliased + freed).
        assert_clean_asan_run(
            r#"
struct Inner { data: Vec[i64] }
struct Outer { inner: Inner, n: i64 }
fn mk(x: i64) -> Outer {
    let mut v: Vec[i64] = Vec.new();
    v.push(x);
    v.push(x);
    return Outer { inner: Inner { data: v }, n: x };
}
fn main() {
    let mut i: i64 = 0;
    while i < 5 {
        let Outer { inner: Inner { data }, n } = mk(i);
        println(data.len() + n);
        i = i + 1;
    }
    println(99);
}
"#,
            &["2", "3", "4", "5", "6", "99"],
            "nested_struct_pattern_no_double_free",
        );
    }

    #[test]
    fn asan_set_local_moved_into_returned_struct_no_uaf() {
        // Pre-existing UAF (fixed 2026-06-08): a `Set` local moved into a
        // struct LITERAL that the function returns was freed at the source
        // function's scope exit — the Vec path had move-suppression, Map/Set
        // didn't — so the returned struct's handle dangled. Without the fix
        // this crashed even without ASAN (SIGSEGV / abort); here the caller
        // reads the moved-in handle (`contains`) in a loop, so a dangling /
        // double-freed handle faults under ASAN. Set lowers to `Map[T, ()]`.
        assert_clean_asan_run(
            r#"
struct Bag { tags: Set[i64], count: i64 }
fn make(x: i64) -> Bag {
    let mut s: Set[i64] = Set.new();
    s.insert(x);
    return Bag { tags: s, count: x };
}
fn main() {
    let mut i: i64 = 0;
    while i < 5 {
        let b = make(i);
        if b.tags.contains(i) { println(b.count); } else { println(-1); }
        i = i + 1;
    }
    println(99);
}
"#,
            &["0", "1", "2", "3", "4", "99"],
            "set_local_moved_into_returned_struct_no_uaf",
        );
    }

    #[test]
    fn asan_map_local_moved_into_returned_struct_no_uaf() {
        // Map sibling of the Set UAF above (was an abort/double-free, exit 134).
        // The caller derefs the moved-in handle via `b.m.len()`, so a dangling
        // handle faults under ASAN.
        assert_clean_asan_run(
            r#"
struct Box { m: Map[i64, i64], n: i64 }
fn make(x: i64) -> Box {
    let mut m: Map[i64, i64] = Map.new();
    m.insert(x, x * 2);
    return Box { m: m, n: x };
}
fn main() {
    let mut i: i64 = 0;
    while i < 5 {
        let b = make(i);
        println(b.n + b.m.len());
        i = i + 1;
    }
    println(99);
}
"#,
            &["1", "2", "3", "4", "5", "99"],
            "map_local_moved_into_returned_struct_no_uaf",
        );
    }

    #[test]
    fn asan_map_set_moved_into_returned_enum_variant_clean() {
        // ASAN-cleanliness guard for the enum-variant sibling of
        // `asan_map_local_moved_into_returned_struct_no_uaf` (phase-6 line
        // 562): a `Map`/`Set` local moved into an enum variant (`Some(m)`)
        // that the function returns must not free the handle at the source's
        // scope exit (the move-suppression at the enum-variant constructor),
        // and the match-arm leaf binding must dispatch `.len()`/`.contains()`
        // (the new dispatch wiring). Looped to exercise per-iteration alloca
        // reuse. This pins that the new dispatch + suppression paths stay
        // ASAN-clean and never double-free.
        //
        // NOTE — this ASAN test is NOT the non-vacuous proof of the UAF fix.
        // The bug here is a *single-free* UAF whose dangling read lands inside
        // the (non-ASAN-instrumented) runtime archive's `karac_map_len`, and
        // ASAN's free-quarantine preserves the freed bytes, so ASAN reads the
        // intact data and reports clean with OR without the fix. The
        // deterministic non-vacuous gate is the plain-build E2E
        // `test_e2e_match_arm_map_set_method_dispatch` in tests/codegen.rs,
        // which prints wrong/empty output without the suppression and `3\n2`
        // with it. This test guards the memory-safety dimension (no NEW
        // double-free introduced) and the looped dispatch path.
        assert_clean_asan_run(
            r#"
fn make_map(x: i64) -> Option[Map[i64, i64]] {
    let mut m: Map[i64, i64] = Map.new();
    m.insert(x, x * 2);
    return Some(m);
}
fn make_set(x: i64) -> Option[Set[i64]] {
    let mut s: Set[i64] = Set.new();
    s.insert(x);
    s.insert(x + 100);
    return Some(s);
}
fn main() {
    let mut i: i64 = 0;
    while i < 5 {
        match make_map(i) {
            Some(m) => println(i + m.len()),
            None => println(-1),
        }
        match make_set(i) {
            Some(s) => {
                if s.contains(i) { println(s.len()); } else { println(-1); }
            }
            None => println(-1),
        }
        i = i + 1;
    }
    println(99);
}
"#,
            &["1", "2", "2", "2", "3", "2", "4", "2", "5", "2", "99"],
            "map_set_moved_into_returned_enum_variant_clean",
        );
    }

    /// Oversized boxed enum payload — box-free double-free gate. A 4-word
    /// `Wide` exceeds Option's 3-word area, so `Some(Wide)` heap-boxes it
    /// (`coerce_to_payload_words`); the annotated `let o: Option[Wide]`
    /// frees the box at scope exit. The matched-out `e` is a scalar copy
    /// (no inner heap), so the only owner of the box is `o`'s slot —
    /// looping faults under ASAN if the box is freed twice or the
    /// matched-out copy aliases it. macOS has no LeakSanitizer, so the
    /// leak side is pinned by the IR free-count test; this is the
    /// double-free gate. See docs/spikes/oversized-enum-payload.md.
    #[test]
    fn asan_boxed_option_let_no_double_free() {
        assert_clean_asan_run(
            r#"
struct Wide { a: i64, b: i64, c: i64, d: i64 }
fn main() {
    let mut i: i64 = 0;
    while i < 5 {
        let o: Option[Wide] = Some(Wide { a: i, b: 2, c: 3, d: 4 });
        match o {
            Some(e) => println(e.a + e.d),
            None => println(-1),
        }
        i = i + 1;
    }
    println(99);
}
"#,
            &["4", "5", "6", "7", "8", "99"],
            "boxed_option_let_no_double_free",
        );
    }

    /// Boxed payload whose `T` itself owns heap: `Option[H]` with a `Vec`
    /// field (5 words → boxed). Scope exit runs the inner struct drop
    /// (frees the Vec buffer) then frees the box. The `vv` moved into
    /// `Some(H { v: vv, .. })` must have its own scope cleanup suppressed
    /// — otherwise the Vec buffer is freed by both `vv`'s cleanup and the
    /// box's inner drop. Looping turns any such imbalance into an ASAN
    /// fault. (No `match` here, to isolate the box-drop + move-in
    /// suppression from the move-OUT-of-box path, a separate follow-up.)
    #[test]
    fn asan_boxed_option_inner_heap_no_double_free() {
        assert_clean_asan_run(
            r#"
struct H { v: Vec[i64], a: i64, b: i64 }
fn main() {
    let mut i: i64 = 0;
    while i < 5 {
        let mut vv: Vec[i64] = Vec.new();
        vv.push(i);
        let o: Option[H] = Some(H { v: vv, a: 1, b: 2 });
        println(i);
        i = i + 1;
    }
    println(99);
}
"#,
            &["0", "1", "2", "3", "4", "99"],
            "boxed_option_inner_heap_no_double_free",
        );
    }

    /// Oversized-enum-payload §1/§2 (fresh-temp scrutinee box-free, move-OUT):
    /// `match make(i) { Some(h) => … }` over a fresh-temp boxed `Option[H]`
    /// where `H` owns a `Vec` (5 words → boxed). The bound `h` now owns the
    /// inner Vec and frees it via its own scope cleanup; the fresh-temp
    /// `BoxedEnumDrop` must free ONLY the box (no inner struct drop) or the
    /// Vec buffer is freed twice. The loop turns any imbalance into a
    /// deterministic ASAN double-free. Complements
    /// `asan_boxed_option_inner_heap_no_double_free` (which isolates the
    /// move-IN suppression with no `match`).
    #[test]
    fn asan_freshtemp_boxed_option_match_move_out_no_double_free() {
        assert_clean_asan_run(
            r#"
struct H { v: Vec[i64], a: i64, b: i64 }
fn make(i: i64) -> Option[H] {
    let mut vv: Vec[i64] = Vec.new();
    vv.push(i);
    return Some(H { v: vv, a: 1, b: 2 });
}
fn main() {
    let mut i: i64 = 0;
    while i < 5 {
        match make(i) {
            Some(h) => println(h.v[0] + h.a),
            None => println(-1),
        }
        i = i + 1;
    }
    println(99);
}
"#,
            &["1", "2", "3", "4", "5", "99"],
            "freshtemp_boxed_option_match_move_out_no_double_free",
        );
    }

    /// Oversized-enum-payload §3 (untyped-let inference, inner heap): an
    /// untyped `let o = make(i)` over a boxed `Option[H]` (`H` owns a `Vec`,
    /// 5 words → boxed). The box drop is inferred from the callee's return
    /// type — it must free the inner Vec (via the inner struct drop) and the
    /// box exactly once each. The loop turns any imbalance into a
    /// deterministic ASAN leak/double-free. Untyped analogue of
    /// `asan_boxed_option_inner_heap_no_double_free`.
    #[test]
    fn asan_untyped_let_boxed_option_inner_heap_no_double_free() {
        assert_clean_asan_run(
            r#"
struct H { v: Vec[i64], a: i64, b: i64 }
fn make(i: i64) -> Option[H] {
    let mut vv: Vec[i64] = Vec.new();
    vv.push(i);
    return Some(H { v: vv, a: 1, b: 2 });
}
fn main() {
    let mut i: i64 = 0;
    while i < 5 {
        let o = make(i);
        println(i);
        i = i + 1;
    }
    println(99);
}
"#,
            &["0", "1", "2", "3", "4", "99"],
            "untyped_let_boxed_option_inner_heap_no_double_free",
        );
    }

    // ── general owned-temp tracking, slice 1 (phase-6 line 489/497) ──
    //
    // docs/spikes/general-owned-temp-tracking.md slice 1: a fresh-owned
    // Vec/String produced in statement-discard position (`make_vec();`) has
    // no binding to drop it; the owned-temp chokepoint materializes it into an
    // `__owned_tmp` slot and frees it at the `;`. On Linux this is a leak gate
    // (LeakSanitizer flags the unfreed buffer); on macOS (no LSan) it is a
    // *double-free* gate — if the chokepoint wrongly freed a buffer that some
    // other cleanup also owns, the repeated discard in a loop faults under
    // ASAN's quarantine. The `make()` call in a loop amplifies any
    // per-iteration imbalance into a deterministic crash, mirroring
    // `asan_ref_arg_repeated_calls_no_compound_leak`.
    #[test]
    fn asan_discarded_vec_temp_freed_no_double_free() {
        assert_clean_asan_run(
            r#"
fn make_vec() -> Vec[i64] {
    let mut v: Vec[i64] = Vec.new();
    v.push(1_i64);
    v.push(2_i64);
    return v;
}

fn main() {
    let mut i = 0;
    while i < 8 {
        make_vec();
        i = i + 1;
    }
    println("done");
}
"#,
            &["done"],
            "discarded_vec_temp_freed_no_double_free",
        );
    }

    // A discarded fresh String from a MethodCall (`s.to_upper();` shape —
    // here `concat()`-style via `+` wrapped in a returning fn, called and
    // discarded). Confirms the chokepoint covers String (same `{ptr,len,cap}`
    // layout as Vec) and that draining the one-shot statement frame does not
    // double-free the *bound* `keep` String living in the same function.
    #[test]
    fn asan_discarded_string_temp_coexists_with_bound_string() {
        assert_clean_asan_run(
            r#"
fn make_str() -> String {
    let a = "discarded ";
    let b = "temp";
    return a + b;
}

fn main() {
    let keep = "kept value";
    make_str();
    println(keep);
}
"#,
            &["kept value"],
            "discarded_string_temp_coexists_with_bound_string",
        );
    }

    // ── General owned-temp tracking, slice 2: Map / RC / nested-elem ──
    // (docs/spikes/general-owned-temp-tracking.md). Map handles and RC
    // boxes are plain pointers — slice 1 (LLVM-type Vec/String detection)
    // leaked them; the lowering-pass `owned_temp_drops` hint table now lets
    // `materialize_owned_temp` classify and drop them. The 8-iteration loop
    // amplifies any per-iteration imbalance into a deterministic double-free
    // fault under macOS ASAN; Linux `detect_leaks=1` is the leak oracle.

    #[test]
    fn asan_discarded_map_temp_freed() {
        // A discarded fresh `Map[i64, i64]` handle: no binding to drop it,
        // recognized only via the hint table's `Map[K, V]` TypeExpr. Both
        // halves primitive → `karac_map_free` (no per-entry vec walk). Faults
        // on macOS if the handle is double-freed; leaks on Linux if untracked.
        assert_clean_asan_run(
            r#"
fn make_map() -> Map[i64, i64] {
    let mut m: Map[i64, i64] = Map.new();
    m.insert(1_i64, 2_i64);
    m.insert(3_i64, 4_i64);
    return m;
}

fn main() {
    let mut i = 0;
    while i < 8 {
        make_map();
        i = i + 1;
    }
    println("done");
}
"#,
            &["done"],
            "discarded_map_temp_freed",
        );
    }

    #[test]
    fn asan_discarded_nested_vec_string_temp_freed() {
        // A discarded `Vec[String]`: slice 1 freed the outer buffer but
        // leaked the inner String element buffers (elem_ty was `None`). The
        // hint table supplies the element type, so the recursive `FreeVecBuffer`
        // walk frees each element. The bound `keep` String in the same frame
        // pins that draining the one-shot discard frame doesn't double-free a
        // live binding. Leak oracle (Linux) is the real gate for the element
        // closure; macOS catches any double-free.
        assert_clean_asan_run(
            r#"
fn make_vv() -> Vec[String] {
    let mut v: Vec[String] = Vec.new();
    v.push("alpha");
    v.push("beta");
    return v;
}

fn main() {
    let keep = "kept";
    let mut i = 0;
    while i < 8 {
        make_vv();
        i = i + 1;
    }
    println(keep);
}
"#,
            &["kept"],
            "discarded_nested_vec_string_temp_freed",
        );
    }

    #[test]
    fn asan_vec_of_vec_of_string_scope_exit_drop_no_leak() {
        // Two-level nested heap: a `Vec[Vec[String]]` dropped at scope exit. The
        // inline `FreeVecBuffer` cleanup's vec-struct fast path is ONE level deep —
        // it frees each inner `Vec[String]`'s data buffer but treats that buffer's
        // elements as opaque, so the innermost String char-buffers leak (documented
        // one-level limit; the recursive `emit_vec_drop_fn` family existed but was
        // unwired). This routes the `Vec[heap-inner]` element through the recursive
        // per-element drop (`karac_drop_Vec_String`), which drops every level. The
        // binding is only `.len()`-read (never consumed), so it drops whole at
        // scope exit. ≥36-byte innermost strings defeat LSan short-string
        // reachability; the loop re-materializes each pass. Leak (innermost
        // Strings) is the Linux-LSan gate; a double-free would show on macOS ASAN.
        assert_clean_asan_run(
            r#"
fn build(n: i64) -> Vec[Vec[String]] {
    let mut outer: Vec[Vec[String]] = Vec.new();
    let mut a: Vec[String] = Vec.new();
    a.push(f"alpha string padded out well beyond thirty-six bytes {n}");
    a.push(f"beta string padded out well beyond thirty-six bytes {n}");
    outer.push(a);
    let mut b: Vec[String] = Vec.new();
    b.push(f"gamma string padded out well beyond thirty-six byte {n}");
    outer.push(b);
    return outer;
}
fn main() {
    let mut i = 0;
    while i < 4 {
        let vv = build(i);
        println(vv.len());
        i = i + 1;
    };
}
"#,
            &["2", "2", "2", "2"],
            "vec_of_vec_of_string_scope_exit_drop_no_leak",
        );
    }

    #[test]
    fn asan_vec_of_vec_of_struct_scope_exit_drop_no_leak() {
        // Slice 3o: a `Vec[Vec[Rec]]` where `Rec` owns a heap `String` field,
        // dropped at scope exit, looped. 3n's recursive drop handled collection
        // inners; 3o threads the struct-field drop (`__karac_drop_struct_Rec`)
        // through the recursive `karac_drop_Vec_Rec` so each element's `name`
        // String frees. Leak (innermost field Strings) is the Linux-LSan gate;
        // a double-free (aliased element vs its clone) shows on macOS ASAN.
        // ≥36-byte field strings defeat LSan short-string reachability.
        assert_clean_asan_run(
            r#"
struct Rec { name: String, n: i64 }
fn build(n: i64) -> Vec[Vec[Rec]] {
    let mut outer: Vec[Vec[Rec]] = Vec.new();
    let mut a: Vec[Rec] = Vec.new();
    a.push(Rec { name: f"alpha string padded out beyond thirty-six bytes {n}", n: 1_i64 });
    a.push(Rec { name: f"beta string padded out beyond thirty-six bytes {n}", n: 2_i64 });
    outer.push(a);
    return outer;
}
fn main() {
    let mut i = 0;
    while i < 3 {
        let vv = build(i);
        println(vv.len());
        i = i + 1;
    };
}
"#,
            &["1", "1", "1"],
            "vec_of_vec_of_struct_scope_exit_drop_no_leak",
        );
    }

    #[test]
    fn asan_vec_of_vec_of_enum_scope_exit_drop_no_leak() {
        // Slice 3o enum sibling: a `Vec[Vec[Tok]]` where `Tok` has a heap variant
        // (`Word(String)`). The enum drop-switch (`__karac_drop_Tok`) threaded
        // through the recursive `karac_drop_Vec_Tok` frees each live `Word`
        // payload String. Same leak/double-free hazards as the struct case.
        assert_clean_asan_run(
            r#"
enum Tok { Word(String), Num(i64) }
fn build(n: i64) -> Vec[Vec[Tok]] {
    let mut outer: Vec[Vec[Tok]] = Vec.new();
    let mut a: Vec[Tok] = Vec.new();
    a.push(Tok.Word(f"alpha string padded out beyond thirty-six bytes {n}"));
    a.push(Tok.Num(2_i64));
    outer.push(a);
    return outer;
}
fn main() {
    let mut i = 0;
    while i < 3 {
        let vv = build(i);
        println(vv.len());
        i = i + 1;
    };
}
"#,
            &["1", "1", "1"],
            "vec_of_vec_of_enum_scope_exit_drop_no_leak",
        );
    }

    #[test]
    fn asan_vec_of_vec_of_shared_struct_scope_exit_drop_no_leak() {
        // Slice 3o shared sibling: a `Vec[Vec[Node]]` where `Node` is a `shared
        // struct` owning a `String`. A shared element's per-element drop is an
        // RC-dec (`__karac_vec_elem_rc_dec_Node`), threaded through the recursive
        // `karac_drop_Vec_Node`; at rc→0 the box (and its String) frees. A missing
        // dec leaks the whole box (Linux LSan); a double-dec frees the box twice
        // (macOS ASAN). The `te_recursive_drop_fully_supported` gate admits shared
        // types via `shared_types`.
        assert_clean_asan_run(
            r#"
shared struct Node { label: String }
fn build(n: i64) -> Vec[Vec[Node]] {
    let mut outer: Vec[Vec[Node]] = Vec.new();
    let mut a: Vec[Node] = Vec.new();
    a.push(Node { label: f"alpha string padded out beyond thirty-six bytes {n}" });
    outer.push(a);
    return outer;
}
fn main() {
    let mut i = 0;
    while i < 3 {
        let vv = build(i);
        println(vv.len());
        i = i + 1;
    };
}
"#,
            &["1", "1", "1"],
            "vec_of_vec_of_shared_struct_scope_exit_drop_no_leak",
        );
    }

    #[test]
    fn asan_vec_of_option_string_scope_exit_drop_no_leak() {
        // Slice 3p: a `Vec[Option[String]]` dropped at scope exit, looped. An
        // `Option[String]` ELEMENT is the type-erased `{tag, w0, w1, w2}` layout
        // whose `Some` payload {ptr,len,cap} overlays w0..w2 — not a vec-struct,
        // so the one-level fast path skipped it, and `vec_elem_agg_drop_for_-
        // type_expr` early-returned None for Option (the type-erased enum drop
        // switch can't know the payload type — B-2026-06-10-6's concrete-typed
        // binding cleanup covers only BINDINGS, not Vec elements). The `Some`
        // payload Strings leaked. Fixed by the payload-type-aware
        // `karac_drop_Option_String` threaded through the agg-drop loop:
        // tag-guarded (None elements skipped), payload dropped via the recursive
        // family. Leak is the Linux-LSan gate; a double-free (payload freed by
        // both the element drop and a binding) shows on macOS ASAN. Payloads are
        // runtime f-strings so the heap allocation actually happens — a constant
        // literal folds to a static cap=0 string and hides the path
        // (B-2026-06-10-6's discipline).
        assert_clean_asan_run(
            r#"
fn build(n: i64) -> Vec[Option[String]] {
    let mut v: Vec[Option[String]] = Vec.new();
    v.push(Some(f"alpha string padded out beyond thirty-six bytes {n}"));
    v.push(None);
    v.push(Some(f"beta string padded out beyond thirty-six bytes {n}"));
    return v;
}
fn main() {
    let mut i = 0;
    while i < 3 {
        let v = build(i);
        println(v.len());
        i = i + 1;
    };
}
"#,
            &["3", "3", "3"],
            "vec_of_option_string_scope_exit_drop_no_leak",
        );
    }

    #[test]
    fn asan_vec_of_vec_of_option_string_scope_exit_drop_no_leak() {
        // Slice 3p two-level sibling: `Vec[Vec[Option[String]]]`. The recursive
        // `karac_drop_Vec_Option_String` (3n's family) calls the tag-guarded
        // Option drop per innermost element. Guards that the Option arm composes
        // with the nested-Vec recursion. Runtime f-string payloads (real heap).
        assert_clean_asan_run(
            r#"
fn build(n: i64) -> Vec[Vec[Option[String]]] {
    let mut outer: Vec[Vec[Option[String]]] = Vec.new();
    let mut a: Vec[Option[String]] = Vec.new();
    a.push(Some(f"alpha string padded out beyond thirty-six bytes {n}"));
    a.push(None);
    outer.push(a);
    return outer;
}
fn main() {
    let mut i = 0;
    while i < 3 {
        let vv = build(i);
        println(vv.len());
        i = i + 1;
    };
}
"#,
            &["1", "1", "1"],
            "vec_of_vec_of_option_string_scope_exit_drop_no_leak",
        );
    }

    #[test]
    fn asan_vec_push_option_binding_no_double_free() {
        // Slice 3p double-free regression (caught by this exact probe during
        // development, exit 133): `let o = Some(f"..."); v.push(o)` — the push
        // bit-copies the option aggregate into the vec, whose per-element
        // `karac_drop_Option_String` now frees the payload; the source binding
        // `o`'s `FreeInlineOptionPayload` would free the SAME buffer. The push
        // family (push/push_back/try_push/push_front/try_push_front) disarms the
        // source via `suppress_inline_option_payload_cleanup_for_moved_arg`
        // (cap-zeroes option field 3), making the container the unique owner.
        // macOS ASAN catches the double-free; Linux LSan the leak if the element
        // drop went missing instead.
        assert_clean_asan_run(
            r#"
fn main() {
    let mut i = 0;
    while i < 4 {
        let mut v: Vec[Option[String]] = Vec.new();
        let o = Some(f"a payload string padded beyond thirty-six bytes {i}");
        v.push(o);
        println(v.len());
        i = i + 1;
    };
}
"#,
            &["1", "1", "1", "1"],
            "vec_push_option_binding_no_double_free",
        );
    }

    #[test]
    fn asan_vec_of_option_vec_scope_exit_drop_no_leak() {
        // Slice 3p Vec-payload sibling: `Vec[Option[Vec[i64]]]`. The payload
        // drop recurses through `karac_drop_Vec_i64` (the payload's own family
        // fn) — the inner Vec's data buffer frees once per `Some` element.
        assert_clean_asan_run(
            r#"
fn build(n: i64) -> Vec[Option[Vec[i64]]] {
    let mut v: Vec[Option[Vec[i64]]] = Vec.new();
    let mut inner: Vec[i64] = Vec.new();
    inner.push(n);
    inner.push(n + 1);
    v.push(Some(inner));
    v.push(None);
    return v;
}
fn main() {
    let mut i = 0;
    while i < 3 {
        let v = build(i);
        println(v.len());
        i = i + 1;
    };
}
"#,
            &["2", "2", "2"],
            "vec_of_option_vec_scope_exit_drop_no_leak",
        );
    }

    #[test]
    fn asan_vec_of_result_string_scope_exit_drop_no_leak() {
        // Slice 3q: a `Vec[Result[String, String]]` dropped at scope exit,
        // looped. The tag-dispatching `karac_drop_Result_<ok>_<err>` frees the
        // live side's inline payload overlay per element (Ok and Err overlay the
        // same w0..w2). LSan-RED pre-fix (every payload leaked, 6 allocs).
        // Runtime f-string payloads per the 3p spelling-trap discipline.
        assert_clean_asan_run(
            r#"
fn build(n: i64) -> Vec[Result[String, String]] {
    let mut v: Vec[Result[String, String]] = Vec.new();
    v.push(Ok(f"alpha ok payload padded out beyond thirty-six bytes {n}"));
    v.push(Err(f"beta err payload padded out beyond thirty-six bytes {n}"));
    return v;
}
fn main() {
    let mut i = 0;
    while i < 3 {
        let v = build(i);
        println(v.len());
        i = i + 1;
    };
}
"#,
            &["2", "2", "2"],
            "vec_of_result_string_scope_exit_drop_no_leak",
        );
    }

    #[test]
    fn asan_vec_push_result_binding_no_double_free() {
        // Slice 3q: `let r = Ok(f"..."); v.push(r)` — the push family disarms
        // the source binding's `FreeInlineResultPayload` (cap-zero, the Result
        // sibling of the Option moved-arg suppression) so the container's
        // element drop is the unique owner.
        assert_clean_asan_run(
            r#"
fn main() {
    let mut i = 0;
    while i < 4 {
        let mut v: Vec[Result[String, i64]] = Vec.new();
        let r: Result[String, i64] = Ok(f"a payload padded out beyond thirty-six bytes {i}");
        v.push(r);
        println(v.len());
        i = i + 1;
    };
}
"#,
            &["1", "1", "1", "1"],
            "vec_push_result_binding_no_double_free",
        );
    }

    #[test]
    fn asan_for_match_vec_option_element_no_double_free() {
        // Slice 3q regression pin — this exact shape SIGTRAP'd (exit 133) after
        // slice 3p armed the `Vec[Option[String]]` element drop: `for o in v`
        // copies the element into the loop binding, and `match o { Some(s) => …
        // }` bound the payload OUT of the copy, registering its own free — a
        // double-free against the container's element drop. The loop binding is
        // now marked in `for_loop_borrow_vars` (Option/Result-with-heap-payload
        // elements) and `scrutinee_is_borrowed_binding` treats it as a borrow,
        // so the arm binding aliases and the container's element drop is the
        // single owner. Output must also match the interpreter (188).
        assert_clean_asan_run(
            r#"
fn build(n: i64) -> Vec[Option[String]] {
    let mut v: Vec[Option[String]] = Vec.new();
    v.push(Some(f"alpha payload padded beyond thirty-six bytes {n}"));
    v.push(None);
    return v;
}
fn main() {
    let mut total = 0;
    let mut i = 0;
    while i < 4 {
        let v = build(i);
        for o in v {
            match o {
                Some(s) => { total = total + s.len(); },
                None => { total = total + 1; },
            };
        }
        i = i + 1;
    };
    println(total);
}
"#,
            &["188"],
            "for_match_vec_option_element_no_double_free",
        );
    }

    #[test]
    fn asan_for_match_vec_result_element_no_double_free() {
        // Slice 3q: the Result sibling of the loop-element match pin, with a
        // mixed heap/scalar Result (Ok(String) / Err(i64)) — the Err arm binds a
        // scalar (no free either way), the Ok arm reads the borrowed payload.
        assert_clean_asan_run(
            r#"
fn build(n: i64) -> Vec[Result[String, i64]] {
    let mut v: Vec[Result[String, i64]] = Vec.new();
    v.push(Ok(f"alpha ok payload padded out beyond thirty-six bytes {n}"));
    v.push(Err(7_i64));
    return v;
}
fn main() {
    let mut total = 0;
    let mut i = 0;
    while i < 4 {
        let v = build(i);
        for r in v {
            match r {
                Ok(s) => { total = total + s.len(); },
                Err(e) => { total = total + e; },
            };
        }
        i = i + 1;
    };
    println(total);
}
"#,
            &["240"],
            "for_match_vec_result_element_no_double_free",
        );
    }

    #[test]
    fn asan_for_iflet_vec_option_element_no_double_free() {
        // Slice 3q: the `if let` sibling — the if-let/while-let/let-else bind
        // sites never consulted `scrutinee_is_borrowed_binding` at all (only
        // `match` set `pattern_binding_is_borrow`), so
        // `for o in v { if let Some(s) = o { … } }` double-freed even after the
        // match path was fixed. All three bind sites now set the flag for a
        // borrowed identifier scrutinee.
        assert_clean_asan_run(
            r#"
fn build(n: i64) -> Vec[Option[String]] {
    let mut v: Vec[Option[String]] = Vec.new();
    v.push(Some(f"alpha payload padded beyond thirty-six bytes {n}"));
    v.push(None);
    return v;
}
fn main() {
    let mut total = 0;
    let mut i = 0;
    while i < 4 {
        let v = build(i);
        for o in v {
            if let Some(s) = o {
                total = total + s.len();
            }
        }
        i = i + 1;
    };
    println(total);
}
"#,
            &["184"],
            "for_iflet_vec_option_element_no_double_free",
        );
    }

    #[test]
    fn asan_discarded_rc_temp_freed() {
        // A discarded fresh shared-struct (RC box): the producing call returns
        // one owned reference, so `materialize_owned_temp` queues a single
        // `rc_dec` at the `;` (refcount → 0 frees via the recursive drop fn).
        // Faults on macOS if the box is double-freed (e.g. the return-move-out
        // also decs); leaks on Linux if the discard goes untracked.
        assert_clean_asan_run(
            r#"
shared struct Counter { val: i64 }

fn make_counter() -> Counter {
    return Counter { val: 7 };
}

fn main() {
    let mut i = 0;
    while i < 8 {
        make_counter();
        i = i + 1;
    }
    println("done");
}
"#,
            &["done"],
            "discarded_rc_temp_freed",
        );
    }

    #[test]
    fn asan_returned_map_explicit_return_no_double_free() {
        // Regression for the explicit-`return m;` map-suppression gap fixed
        // alongside slice 2 (src/codegen/exprs.rs `ExprKind::Return`): the
        // tail-expression path suppressed a returned Map's `FreeMapHandle`,
        // but the explicit-`return` path did not — so a callee returning a
        // map via `return m;` freed the handle *and* returned it, and the
        // caller's binding then freed the dangling pointer (double-free under
        // AOT). Here the callee uses `return m;` and the caller binds and
        // reads it; without the fix this double-frees. Sibling to the
        // discarded-map case, pinning the *bound* return shape.
        assert_clean_asan_run(
            r#"
fn make_map() -> Map[i64, i64] {
    let mut m: Map[i64, i64] = Map.new();
    m.insert(1_i64, 2_i64);
    return m;
}

fn main() {
    let m2 = make_map();
    println(m2.len());
}
"#,
            &["1"],
            "returned_map_explicit_return_no_double_free",
        );
    }

    // ── first-class fn values (B-2026-06-20-1) ───────────────────

    #[test]
    fn asan_named_fn_value_heap_arg_no_leak() {
        // B-2026-06-20-1: a bare named `fn` passed in `Fn(...)` position is
        // reified into a `{trampoline, null env}` fat pointer. The trampoline
        // is a transparent env-ignoring forwarder, so a heap-carrying
        // (String) arg moved through the higher-order call must be owned and
        // freed exactly once — no leak, no double-free. A ≥36-byte payload
        // keeps the buffer off the short-String reachable path so LSan sees a
        // genuine leak if one is introduced.
        assert_clean_asan_run(
            r#"
fn shout(s: String) -> String { f"{s}!" }
fn apply(f: Fn(String) -> String, x: String) -> String { f(x) }
fn main() {
    let r = apply(shout, "hello-this-is-a-fairly-long-payload-string");
    println(r);
}
"#,
            &["hello-this-is-a-fairly-long-payload-string!"],
            "named_fn_value_heap_arg_no_leak",
        );
    }

    #[test]
    fn asan_let_bound_fn_value_heap_arg_no_leak() {
        // B-2026-06-21-1: the same transparent-trampoline guarantee through a
        // fn value bound to a LOCAL first (`let g = shout`) and then passed to a
        // `Fn(...)` parameter. The reified fat pointer's env is null and the
        // trampoline is a module global (no heap), so the only heap is the
        // String arg — it must move through and free exactly once. ≥36-byte
        // payload to defeat the short-String reachable-leak blind spot.
        assert_clean_asan_run(
            r#"
fn shout(s: String) -> String { f"{s}!" }
fn apply(f: Fn(String) -> String, x: String) -> String { f(x) }
fn main() {
    let g = shout;
    let r = apply(g, "hello-this-is-a-fairly-long-payload-string");
    println(r);
}
"#,
            &["hello-this-is-a-fairly-long-payload-string!"],
            "let_bound_fn_value_heap_arg_no_leak",
        );
    }

    #[test]
    fn asan_returned_fn_value_heap_arg_no_leak() {
        // B-2026-06-21-2: a fn value flowed through a `-> Fn(...)` return, then
        // invoked on a heap (String) arg. The returned value is a heap-free fat
        // pointer (null env, module-global trampoline); the only heap is the
        // String arg, which must move through the transparent trampoline and
        // free exactly once. ≥36-byte payload to defeat the short-String
        // reachable-leak blind spot.
        assert_clean_asan_run(
            r#"
fn shout(s: String) -> String { f"{s}!" }
fn pick() -> Fn(String) -> String { shout }
fn main() {
    let f = pick();
    let r = f("hello-this-is-a-fairly-long-payload-string");
    println(r);
}
"#,
            &["hello-this-is-a-fairly-long-payload-string!"],
            "returned_fn_value_heap_arg_no_leak",
        );
    }

    // ── while let / let else drop paths (phase-6 line 489) ───────

    #[test]
    fn asan_while_let_per_iteration_heap_local_freed() {
        // `compile_while_let` pushes a per-iteration scope-cleanup frame.
        // A heap String created inside the loop body must be freed at each
        // iteration's exit — not leaked across iterations, not double-freed
        // when the next iteration reuses the binding's slot.
        assert_clean_asan_run(
            r#"
fn pop(v: mut ref Vec[i64]) -> Option[i64] {
    if v.len() == 0 {
        return Option.None;
    }
    let last = v.len() - 1;
    let x = v[last];
    v.remove(last);
    return Option.Some(x);
}

fn main() {
    let mut v: Vec[i64] = Vec.new();
    v.push(1_i64);
    v.push(2_i64);
    v.push(3_i64);
    while let Some(x) = pop(mut v) {
        let prefix = "n=";
        let line = prefix + "x";
        println(f"{line} {x}");
    }
    println("done");
}
"#,
            &["n=x 3", "n=x 2", "n=x 1", "done"],
            "while_let_per_iteration_heap_local_freed",
        );
    }

    #[test]
    fn asan_let_else_binding_and_else_heap_clean() {
        // let-else: a heap String bound on the match edge drops at scope
        // exit; a heap String built in the diverging else path drops on
        // the `return`. Exercises both edges of `compile_let_else`.
        assert_clean_asan_run(
            r#"
fn make(empty: bool) -> Option[String] {
    if empty {
        return Option.None;
    }
    let s = "hello";
    return Option.Some(s + "!");
}

fn run(empty: bool) {
    let Some(s) = make(empty) else {
        let msg = "was ";
        let full = msg + "empty";
        println(full);
        return
    }
    println(s);
}

fn main() {
    run(false);
    run(true);
    println("done");
}
"#,
            &["hello!", "was empty", "done"],
            "let_else_binding_and_else_heap_clean",
        );
    }

    // ── kara-katas leetcode #8 (atoi) end-to-end ─────────────────
    //
    // The kata that surfaced the interpreter Cast no-op (commit
    // 6a79ae2) and motivated `String.bytes()` (commit 517aa1d).
    // Locks in: the shipped kata source compiles, runs, prints the
    // 20 expected integers, and exits ASAN-clean. Source kept
    // in-sync with `kara-katas/.../atoi.kara` (~80 lines verbatim);
    // if the kara-katas file drifts, this test stays a fixed
    // regression target. Output matches what `python3 atoi.py`
    // emits — see kara-katas/leetcode/1-100/8-string-to-integer-atoi.

    #[test]
    fn asan_kata_8_atoi_bytes_one_pass() {
        assert_clean_asan_run(
            r#"
fn my_atoi(s: ref String) -> i32 {
    let bytes = s.bytes();
    let n = bytes.len();

    let space: u8 = ' ' as u32 as u8;
    let plus:  u8 = '+' as u32 as u8;
    let minus: u8 = '-' as u32 as u8;
    let zero:  u8 = '0' as u32 as u8;
    let nine:  u8 = '9' as u32 as u8;

    let mut i = 0i64;
    while i < n and bytes[i] == space {
        i = i + 1;
    }

    let mut sign: i32 = 1i32;
    if i < n and bytes[i] == plus {
        i = i + 1;
    } else if i < n and bytes[i] == minus {
        sign = -1i32;
        i = i + 1;
    }

    let int_max: i32 = 2147483647i32;
    let int_min: i32 = -2147483648i32;
    let max_div: i32 = int_max / 10i32;

    let mut result: i32 = 0i32;
    while i < n {
        let b = bytes[i];
        if b < zero or b > nine {
            break;
        }
        let digit: i32 = (b as i32) - (zero as i32);
        if result > max_div or (result == max_div and digit > 7i32) {
            if sign == 1i32 {
                return int_max;
            }
            return int_min;
        }
        result = result * 10i32 + digit;
        i = i + 1;
    }

    sign * result
}

fn report(s: ref String) {
    println(my_atoi(s));
}

fn main() {
    report("42");
    report("   -42");
    report("4193 with words");
    report("words and 987");
    report("-91283472332");
    report("91283472332");
    report("+1");
    report("");
    report("   ");
    report("+-12");
    report("-+12");
    report("  0000000000012345678");
    report("2147483647");
    report("-2147483648");
    report("2147483648");
    report("-2147483649");
    report("  +0 123");
    report("00000-42a1234");
    report("  -0012a42");
    report("+");
}
"#,
            &[
                "42",
                "-42",
                "4193",
                "0",
                "-2147483648",
                "2147483647",
                "1",
                "0",
                "0",
                "0",
                "0",
                "12345678",
                "2147483647",
                "-2147483648",
                "2147483647",
                "-2147483648",
                "0",
                "0",
                "-12",
                "0",
            ],
            "kata_8_atoi_bytes_one_pass",
        );
    }

    // ── SoA-laid-out Vec drop ────────────────────────────────────
    // `layout entities: Vec[Entity]` lowers to multi-allocation storage —
    // one buffer per hot group plus an optional cold-group buffer — and
    // the outer struct shape is `{ ptr_g0, ..., ptr_g(N-1), [ptr_cold,]
    // i64 len, i64 cap }` rather than the plain Vec `{ptr, len, cap}`.
    // Before the `FreeSoaGroups` cleanup variant landed, the scope-exit
    // walker routed SoA through `FreeVecBuffer`, which both (a) read the
    // `cap > 0` guard from the wrong slot (offset 16 in a 2-hot-group
    // SoA is the `len` field, not cap) and (b) freed only the first
    // group pointer, leaking every other hot group and the cold buffer.
    // These tests are the load-bearing ASAN coverage for that fix.

    #[test]
    fn asan_soa_drop_two_hot_groups_primitive() {
        assert_clean_asan_run(
            r#"
struct Entity { x: f64, y: f64, hp: i64 }
layout entities: Vec[Entity] {
    group physics { x, y }
    group combat { hp }
}
fn main() {
    let mut entities: Vec[Entity] = Vec.new();
    entities.push(Entity { x: 1.0, y: 2.0, hp: 100 });
    entities.push(Entity { x: 3.0, y: 4.0, hp: 200 });
    entities.push(Entity { x: 5.0, y: 6.0, hp: 300 });
    entities.push(Entity { x: 7.0, y: 8.0, hp: 400 });
    entities.push(Entity { x: 9.0, y: 10.0, hp: 500 });
    println(entities.len());
}
"#,
            &["5"],
            "soa_drop_two_hot_groups_primitive",
        );
    }

    #[test]
    fn asan_soa_by_value_param_caller_retains_no_leak_or_double_free() {
        // B-2026-06-19-14 slice 1: a SoA `Vec[Entity]` passed BY VALUE to a
        // reader fn whose param (`entities`) matches `layout entities`. The
        // param's signature is the 4-field SoA struct; the callee borrows it
        // (CALLER-RETAINS — no callee-side FreeSoaGroups), so the caller's
        // per-iteration binding frees both group buffers exactly once. Looped
        // 20× so a per-call leak (callee never frees AND caller suppressed) or
        // a double-free (both free) would surface under LSan/ASAN. 600/iter ×
        // 20 = 12000.
        assert_clean_asan_run(
            r#"
struct Entity { x: f64, y: f64, hp: i64 }
layout entities: Vec[Entity] {
    group physics { x, y }
    group combat { hp }
}
fn total(entities: Vec[Entity]) -> i64 {
    let mut t = 0;
    let mut i = 0;
    while i < entities.len() {
        let e = entities[i];
        t = t + e.hp;
        i = i + 1;
    }
    t
}
fn main() {
    let mut sum = 0;
    let mut k = 0;
    while k < 20 {
        let mut entities: Vec[Entity] = Vec.new();
        entities.push(Entity { x: 1.0, y: 2.0, hp: 100 });
        entities.push(Entity { x: 3.0, y: 4.0, hp: 200 });
        entities.push(Entity { x: 5.0, y: 6.0, hp: 300 });
        sum = sum + total(entities);
        k = k + 1;
    }
    println(sum);
}
"#,
            &["12000"],
            "soa_by_value_param_caller_retains",
        );
    }

    #[test]
    fn asan_soa_by_value_param_caller_different_name_caller_retains() {
        // Per-layout monomorphization slice 2: same caller-retains ownership as
        // the sibling above, but the callee param (`rows`) does NOT match the
        // `layout entities` block — the call is served by the on-demand layout
        // monomorph `total$soa_entities` (forward layout-flow inference), not
        // the name-keyed by-value path. The mono's SoA param prologue must keep
        // CALLER-RETAINS (no callee-side FreeSoaGroups), so the caller's
        // per-iteration `entities` frees both group buffers exactly once.
        // Looped 20× so a per-call leak or double-free surfaces under LSan/ASAN.
        assert_clean_asan_run(
            r#"
struct Entity { x: f64, y: f64, hp: i64 }
layout entities: Vec[Entity] {
    group physics { x, y }
    group combat { hp }
}
fn total(rows: Vec[Entity]) -> i64 {
    let mut t = 0;
    let mut i = 0;
    while i < rows.len() {
        let e = rows[i];
        t = t + e.hp;
        i = i + 1;
    }
    t
}
fn main() {
    let mut sum = 0;
    let mut k = 0;
    while k < 20 {
        let mut entities: Vec[Entity] = Vec.new();
        entities.push(Entity { x: 1.0, y: 2.0, hp: 100 });
        entities.push(Entity { x: 3.0, y: 4.0, hp: 200 });
        entities.push(Entity { x: 5.0, y: 6.0, hp: 300 });
        sum = sum + total(entities);
        k = k + 1;
    }
    println(sum);
}
"#,
            &["12000"],
            "soa_by_value_param_caller_different_name",
        );
    }

    #[test]
    fn asan_soa_return_value_caller_owns_no_leak_or_double_free() {
        // Per-layout monomorphization slice 3 (SoA returns): the OPPOSITE
        // ownership of the by-value param. A builder `make_entities()` builds a
        // SoA `Vec[Entity]` and RETURNS it — bound by a differently-named local
        // `out` and received into the caller's `entities` (`layout entities`).
        // The return is a MOVE OUT: the callee suppresses its own
        // `FreeSoaGroups` for the returned local (it no longer owns the group
        // buffers), and the caller's `entities` binding frees both buffers
        // exactly once at scope exit. Get the ownership transfer wrong and it's
        // either a double-free (callee frees + caller frees → ASAN) or a leak
        // (neither frees → LSan). Looped 20× to amplify either. The struct
        // carries ≥36 bytes of live payload across the groups so a reachable
        // leak isn't masked by LSan's short-allocation blind spot.
        assert_clean_asan_run(
            r#"
struct Entity { x: f64, y: f64, hp: i64 }
layout entities: Vec[Entity] {
    group physics { x, y }
    group combat { hp }
}
fn make_entities() -> Vec[Entity] {
    let mut out: Vec[Entity] = Vec.new();
    out.push(Entity { x: 1.0, y: 2.0, hp: 100 });
    out.push(Entity { x: 3.0, y: 4.0, hp: 200 });
    out.push(Entity { x: 5.0, y: 6.0, hp: 300 });
    out
}
fn main() {
    let mut sum = 0;
    let mut k = 0;
    while k < 20 {
        let entities: Vec[Entity] = make_entities();
        let mut i = 0;
        while i < entities.len() {
            let e = entities[i];
            sum = sum + e.hp;
            i = i + 1;
        }
        k = k + 1;
    }
    println(sum);
}
"#,
            &["12000"],
            "soa_return_value_caller_owns",
        );
    }

    #[test]
    fn asan_soa_mut_ref_fill_borrow_no_leak_or_double_free() {
        // Per-layout monomorphization slice 4 (multi-buffer WRITE, by mut ref):
        // a differently-named SoA buffer (`entities`, `layout entities`) is
        // FILLED through a shared `fill(buf: mut ref Vec[Entity])` helper that
        // pushes. The push reallocs each group buffer and writes the new
        // pointers / len / cap back through the deref'd caller-struct pointer
        // (`ref_params`). Ownership is BORROW: the mono must NOT queue a
        // `FreeSoaGroups` for the `mut ref` param — only `main`'s `entities`
        // binding owns the buffers and frees both groups once at scope exit.
        // Get it wrong and it's a double-free (callee + caller both free → ASAN)
        // or a leak (the realloc'd group buffers from a prior iteration never
        // freed → LSan). Looped 20× — each iteration builds a fresh `entities`,
        // fills it via mut-ref, reads it, drops it — to amplify either fault.
        // ≥36 bytes of live payload per element across the groups so a reachable
        // leak isn't masked by LSan's short-allocation blind spot.
        assert_clean_asan_run(
            r#"
struct Entity { x: f64, y: f64, hp: i64 }
layout entities: Vec[Entity] {
    group physics { x, y }
    group combat { hp }
}
fn fill(buf: mut ref Vec[Entity]) {
    buf.push(Entity { x: 1.0, y: 2.0, hp: 100 });
    buf.push(Entity { x: 3.0, y: 4.0, hp: 200 });
    buf.push(Entity { x: 5.0, y: 6.0, hp: 300 });
}
fn main() {
    let mut sum = 0;
    let mut k = 0;
    while k < 20 {
        let mut entities: Vec[Entity] = Vec.new();
        fill(mut entities);
        let mut i = 0;
        while i < entities.len() {
            let e = entities[i];
            sum = sum + e.hp;
            i = i + 1;
        }
        k = k + 1;
    }
    println(sum);
}
"#,
            &["12000"],
            "soa_mut_ref_fill_borrow",
        );
    }

    #[test]
    fn asan_soa_layout_named_param_base_aos_and_mono_soa_no_leak() {
        // Per-layout monomorphization slice 5 (origin-only `soa_layouts`): one
        // by-value helper `total(entities: Vec[Entity])` whose param NAME matches
        // the `layout entities` block is called BOTH ways per iteration —
        //   - with the SoA local `entities` → routed to a SoA monomorph
        //     (caller-retains: no callee-side FreeSoaGroups, the SoA local frees
        //     both group buffers once), and
        //   - with an ordinary AoS `plain: Vec[Entity]` → routed to the AoS BASE
        //     symbol (caller-owns: the plain Vec frees its single buffer once).
        // Retiring the name-keyed by-value param ABI moved BOTH routes onto their
        // correct ownership paths; a regression on either is a double-free (ASAN)
        // or a leak (LSan). Looped 20× to amplify. ≥36 bytes of live payload per
        // element across the groups so a reachable leak isn't masked by LSan's
        // short-allocation blind spot.
        assert_clean_asan_run(
            r#"
struct Entity { x: f64, y: f64, hp: i64 }
layout entities: Vec[Entity] {
    group physics { x, y }
    group combat { hp }
}
fn total(entities: Vec[Entity]) -> i64 {
    let mut t = 0;
    let mut i = 0;
    while i < entities.len() {
        let e = entities[i];
        t = t + e.hp;
        i = i + 1;
    }
    t
}
fn main() {
    let mut sum = 0;
    let mut k = 0;
    while k < 20 {
        let mut entities: Vec[Entity] = Vec.new();
        entities.push(Entity { x: 1.0, y: 2.0, hp: 100 });
        entities.push(Entity { x: 3.0, y: 4.0, hp: 200 });
        entities.push(Entity { x: 5.0, y: 6.0, hp: 300 });
        let mut plain: Vec[Entity] = Vec.new();
        plain.push(Entity { x: 7.0, y: 8.0, hp: 7 });
        plain.push(Entity { x: 9.0, y: 1.0, hp: 11 });
        sum = sum + total(entities) + total(plain);
        k = k + 1;
    }
    println(sum);
}
"#,
            &["12360"],
            "soa_layout_named_param_base_aos_and_mono_soa",
        );
    }

    #[test]
    fn asan_soa_field_index_store_no_overflow() {
        // B-2026-06-20-7: the buggy field-level SoA index-store strided the SoA
        // struct as a contiguous AoS element, so a store at index >= 1 wrote PAST
        // the target group's buffer — a heap-buffer-overflow ASAN catches. The
        // fix addresses the field's own group buffer at [i] by the group
        // sub-struct stride. This scatters field writes across BOTH groups
        // (`physics.x` and `combat.hp`) at indices 0..2, then reads them back,
        // looped 20x so any stray address trips ASAN. ≥36 bytes of live payload
        // per element so a reachable leak isn't masked by LSan's blind spot.
        assert_clean_asan_run(
            r#"
struct Body { x: f64, y: f64, hp: i64 }
layout bodies: Vec[Body] {
    group physics { x, y }
    group combat { hp }
}
fn main() {
    let mut sum = 0;
    let mut k = 0;
    while k < 20 {
        let mut bodies: Vec[Body] = Vec.new();
        bodies.push(Body { x: 1.0, y: 2.0, hp: 100 });
        bodies.push(Body { x: 3.0, y: 4.0, hp: 200 });
        bodies.push(Body { x: 5.0, y: 6.0, hp: 300 });
        let mut i = 0;
        while i < bodies.len() {
            bodies[i].hp = bodies[i].hp + 1;
            i = i + 1;
        }
        let mut j = 0;
        while j < bodies.len() {
            sum = sum + bodies[j].hp;
            j = j + 1;
        }
        k = k + 1;
    }
    println(sum);
}
"#,
            &["12060"],
            "soa_field_index_store_no_overflow",
        );
    }

    #[test]
    fn asan_soa_drop_with_cold_group_primitive() {
        // Cold group adds an extra buffer that pre-fix codegen never
        // freed (the cold pointer sits between the hot pointers and the
        // len/cap pair; the legacy free path read field 0 only). Five
        // pushes cross the cap 0 → 4 → 8 realloc boundary so the prior
        // cold-buffer free path is also exercised.
        assert_clean_asan_run(
            r#"
struct Entity { x: f64, y: f64, hp: i64, label: i64 }
layout entities: Vec[Entity] {
    group physics { x, y }
    group combat { hp }
    cold { label }
}
fn main() {
    let mut entities: Vec[Entity] = Vec.new();
    entities.push(Entity { x: 1.0, y: 2.0, hp: 100, label: 11 });
    entities.push(Entity { x: 3.0, y: 4.0, hp: 200, label: 22 });
    entities.push(Entity { x: 5.0, y: 6.0, hp: 300, label: 33 });
    entities.push(Entity { x: 7.0, y: 8.0, hp: 400, label: 44 });
    entities.push(Entity { x: 9.0, y: 10.0, hp: 500, label: 55 });
    println(entities.len());
}
"#,
            &["5"],
            "soa_drop_with_cold_group_primitive",
        );
    }

    #[test]
    fn asan_soa_pop_remove_no_leak_or_uaf() {
        // Exercises every SoA mutator together (pop, pop_front, remove)
        // alongside the scope-exit FreeSoaGroups cleanup. The shift-
        // memmoves run against the same group buffers the cleanup will
        // later free, so a wrong shift pointer / wrong byte count
        // would surface as ASAN heap-buffer-overflow or UAF. Two hot
        // groups exercise the per-group shift loop. (Primitive fields
        // here; the heap-field SoA element drops are covered by the
        // dedicated `asan_soa_string_field_*` / `asan_soa_vec_pod_field_*`
        // tests above.)
        //
        // The struct is 4 i64 words (`label` in a cold group): `pop()`
        // returns `Option[Entity]`, whose payload area is only 3 words, so
        // the popped 4-word `Entity` is heap-BOXED (see
        // docs/spikes/oversized-enum-payload.md). Re-widened from the 3-word
        // B#1 stop-gap now that the fresh-temp-scrutinee box-free (§1) lands:
        // `match entities.pop() { Some(e) => … }` reads the 4th word `label`
        // back through the box (was truncated/garbage before boxing) AND the
        // `BoxedEnumDrop` queued for this fresh-temp scrutinee frees the box,
        // so the run must stay ASAN-clean (a leaked box or a double-free with
        // the SoA group cleanup would surface here). Cold-group *layout*
        // codegen is covered separately in tests/codegen.rs.
        assert_clean_asan_run(
            r#"
struct Entity { x: i64, y: i64, hp: i64, label: i64 }
layout entities: Vec[Entity] {
    group physics { x, y }
    group combat { hp }
    group meta { label }
}
fn main() {
    let mut entities: Vec[Entity] = Vec.new();
    let mut i: i64 = 0;
    while i < 6 {
        entities.push(Entity { x: i, y: i * 10, hp: i * 100, label: i * 1000 + 7 });
        i = i + 1;
    }
    let _front = entities.pop_front();
    let _middle = entities.remove(2);
    match entities.pop() {
        Some(e) => {
            println(e.x);
            println(e.label);
        }
        None => println(-1),
    }
    println(entities.len());
}
"#,
            &["5", "5007", "3"],
            "soa_pop_remove_no_leak_or_uaf",
        );
    }

    #[test]
    fn asan_soa_drop_empty_collection() {
        // Empty SoA — never pushed, so cap stays 0 and the cleanup
        // should short-circuit at the `is_heap` guard without freeing
        // anything. Catches a regression where the cap check reads the
        // wrong slot and accidentally calls free on undef group ptrs.
        assert_clean_asan_run(
            r#"
struct Entity { x: f64, y: f64, hp: i64 }
layout entities: Vec[Entity] {
    group physics { x, y }
    group combat { hp }
}
fn main() {
    let entities: Vec[Entity] = Vec.new();
    println(entities.len());
}
"#,
            &["0"],
            "soa_drop_empty_collection",
        );
    }

    #[test]
    fn asan_soa_reassign_carried_buffer_no_leak_or_double_free() {
        // Slice 6 (the carried-grid double-buffer): a SoA `grid` is built
        // (`init()` returns a counted-loop-filled SoA Vec — the `with_capacity`
        // form), then REASSIGNED each "frame" from a layout-returning call
        // (`grid = bump(grid)`), the exact shape of a stateful sim's per-frame
        // loop. `compile_soa_assign_from_call` frees the OLD group buffers (the
        // by-value param is caller-retains, so the displaced buffers are owned
        // here) before storing the new header; the binding's queued
        // `FreeSoaGroups` frees the final frame's buffers at scope exit. Get the
        // double-buffer accounting wrong and it's a double-free (free old AND
        // scope-free the same buffers → ASAN) or a per-frame leak (never free the
        // displaced buffers → LSan). The grid is rebuilt + reassigned 5× inside a
        // 20× outer loop to amplify either; `Cell` is 40 bytes (two SoA groups,
        // both group buffers well over LSan's short-allocation blind spot). Sum
        // of `a` (0, bumped +1 ×5) over 8 cells × 20 = 800.
        assert_clean_asan_run(
            r#"
struct Cell { a: f64, b: f64, c: f64, d: f64, e: f64 }
layout grid: Vec[Cell] { group lo { a, b } group hi { c, d, e } }
fn bump(g: Vec[Cell]) -> Vec[Cell] {
    let mut out: Vec[Cell] = Vec.new();
    let mut i = 0;
    while i < g.len() {
        let c = g[i];
        out.push(Cell { a: c.a + 1.0, b: c.b, c: c.c, d: c.d, e: c.e });
        i = i + 1;
    }
    out
}
fn init() -> Vec[Cell] {
    let mut grid: Vec[Cell] = Vec.new();
    let mut i = 0;
    while i < 8 { grid.push(Cell { a: 0.0, b: 1.0, c: 2.0, d: 3.0, e: 4.0 }); i = i + 1; }
    grid
}
fn main() {
    let mut sum = 0.0;
    let mut k = 0;
    while k < 20 {
        let mut grid: Vec[Cell] = init();
        let mut f = 0;
        while f < 5 { grid = bump(grid); f = f + 1; }
        let mut i = 0;
        while i < grid.len() { sum = sum + grid[i].a; i = i + 1; }
        k = k + 1;
    }
    println(sum);
}
"#,
            &["800"],
            "soa_reassign_carried_buffer",
        );
    }

    #[test]
    fn asan_soa_whole_element_index_store_no_overflow() {
        // Follow-on (whole-element SoA index store `grid[i] = E { … }`): the
        // pre-fix store wrote the full AoS element over a SINGLE group's narrower
        // stride — a heap-buffer-overflow at the last element of every group
        // buffer (write 40-byte Cell at offset i*16 into the `lo` group's
        // 16-byte stride). ASAN flags the over-stride write directly; this is the
        // regression guard for the silent-overflow class. Scatter all 8 elements
        // by whole-element assignment each of 20 frames, then read back across
        // both groups so a dropped/over-stride write also changes the sum. Cell
        // is 40 bytes (two groups, both buffers past LSan's short-alloc blind
        // spot). grid[i] = {i+1, …}: sum of `a` (i+1, i 0..8) = 36, ×20 = 720.
        assert_clean_asan_run(
            r#"
struct Cell { a: f64, b: f64, c: f64, d: f64, e: f64 }
layout grid: Vec[Cell] { group lo { a, b } group hi { c, d, e } }
fn main() with panics {
    let mut sum = 0.0;
    let mut k = 0;
    while k < 20 {
        let mut grid: Vec[Cell] = Vec.new();
        let mut i = 0;
        while i < 8 { grid.push(Cell { a: 0.0, b: 0.0, c: 0.0, d: 0.0, e: 0.0 }); i = i + 1; }
        let mut j = 0;
        while j < grid.len() {
            grid[j] = Cell { a: (j + 1) as f64, b: 1.0, c: 2.0, d: 3.0, e: 4.0 };
            j = j + 1;
        }
        let mut r = 0;
        while r < grid.len() { sum = sum + grid[r].a; r = r + 1; }
        k = k + 1;
    }
    println(sum);
}
"#,
            &["720"],
            "soa_whole_element_index_store",
        );
    }

    #[test]
    fn asan_soa_early_return_fall_through_no_leak_or_uaf() {
        // Follow-on (branch-leaf / multi-`return` SoA returns): a return-SoA
        // helper with an EARLY `return early;` guarded by a flag, then a tail
        // `late`. Two ownership paths share one cleanup frame:
        //   flag=true  → `early` moved out (must NOT be freed here — the caller
        //                owns it; freeing pre-return is a UAF/double-free ASAN
        //                catches), `late` never allocated.
        //   flag=false → `early` allocated but NOT returned (must be freed at
        //                scope exit — a compile-time cleanup removal would leak
        //                it on this path; LSan catches), `late` moved out.
        // The early move-out uses a runtime `cap = 0` sentinel
        // (`neutralize_moved_soa_groups_slot`), branch-safe precisely because
        // the frame is shared. Both paths run every iteration (×20); `Cell` is
        // 40 bytes (two group buffers past LSan's short-alloc blind spot).
        // g1[0].a (1.0) + g2[0].a (2.0) = 3.0 × 20 = 60.
        assert_clean_asan_run(
            r#"
struct Cell { a: f64, b: f64, c: f64, d: f64, e: f64 }
layout grid: Vec[Cell] { group lo { a, b } group hi { c, d, e } }
fn build(v: f64) -> Vec[Cell] {
    let mut g: Vec[Cell] = Vec.new();
    let mut i = 0;
    while i < 8 { g.push(Cell { a: v, b: 1.0, c: 2.0, d: 3.0, e: 4.0 }); i = i + 1; }
    g
}
fn pick(flag: bool) -> Vec[Cell] {
    let early: Vec[Cell] = build(1.0);
    if flag {
        return early;
    }
    let late: Vec[Cell] = build(2.0);
    late
}
fn main() {
    let mut sum = 0.0;
    let mut k = 0;
    while k < 20 {
        let g1: Vec[Cell] = pick(true);
        let g2: Vec[Cell] = pick(false);
        sum = sum + g1[0].a + g2[0].a;
        k = k + 1;
    }
    println(sum);
}
"#,
            &["60"],
            "soa_early_return_fall_through",
        );
    }

    #[test]
    fn asan_soa_branch_leaf_tail_returns_no_leak_or_uaf() {
        // Follow-on sibling: branch-leaf BARE tails (`if flag { a } else { b }`,
        // no `return` keyword) — both `a` and `b` are returned locals the
        // recursive `soa_return_local_names` seeds SoA, and each block-scoped
        // leaf is moved out of its branch as the function value. Get the
        // per-branch move-out wrong and the unselected branch's buffers either
        // free early (UAF, ASAN) or never (leak, LSan). pick(true)/pick(false)
        // each iteration (×20); 40-byte Cell. g1[0].a (1.0) + g2[0].a (2.0) =
        // 3.0 × 20 = 60.
        assert_clean_asan_run(
            r#"
struct Cell { a: f64, b: f64, c: f64, d: f64, e: f64 }
layout grid: Vec[Cell] { group lo { a, b } group hi { c, d, e } }
fn fill(v: f64) -> Vec[Cell] {
    let mut g: Vec[Cell] = Vec.new();
    let mut i = 0;
    while i < 8 { g.push(Cell { a: v, b: 1.0, c: 2.0, d: 3.0, e: 4.0 }); i = i + 1; }
    g
}
fn pick(flag: bool) -> Vec[Cell] {
    if flag {
        let a: Vec[Cell] = fill(1.0);
        a
    } else {
        let b: Vec[Cell] = fill(2.0);
        b
    }
}
fn main() {
    let mut sum = 0.0;
    let mut k = 0;
    while k < 20 {
        let g1: Vec[Cell] = pick(true);
        let g2: Vec[Cell] = pick(false);
        sum = sum + g1[0].a + g2[0].a;
        k = k + 1;
    }
    println(sum);
}
"#,
            &["60"],
            "soa_branch_leaf_tail_returns",
        );
    }

    // ── SoA heap-field (String / Vec) element drops ───────────────
    // String / Vec[POD] element fields are now allowed in SoA layouts.
    // Their per-element heap buffers are freed by the synthesized
    // `__karac_soa_drop_<layout>` at scope exit, on overwrite (index /
    // field store drop-old), and on the carried-grid reassignment. Each
    // String payload is ≥36 bytes so a missed free is past LSan's
    // short-allocation reachability blind spot. All paths run ×20 frames
    // to amplify a per-frame leak; the Linux-CI LSan job is the gate.

    #[test]
    fn asan_soa_string_field_scope_cleanup_no_leak() {
        // The CORE leak fix: a SoA Vec whose element has a heap String
        // field, built fresh each frame and dropped at scope exit. Pre-fix
        // the FreeSoaGroups cleanup freed only the group buffers (POD
        // assumption), leaking every element's String payload — 8 × 20 =
        // 160 heap strings. Reads only the primitive `id` (the heap field
        // read-back is a separate read-path concern). Sum of ids =
        // 28/frame × 20 = 560.
        assert_clean_asan_run(
            r#"
struct Cell { id: i64, name: String }
layout cells: Vec[Cell] { group ids { id } group names { name } }
fn main() with panics {
    let mut total = 0;
    let mut k = 0;
    while k < 20 {
        let mut cells: Vec[Cell] = Vec.new();
        let mut i = 0;
        while i < 8 {
            cells.push(Cell { id: i, name: f"soa-heap-owning-string-payload-element-{i}" });
            i = i + 1;
        }
        let mut j = 0;
        while j < cells.len() { total = total + cells[j].id; j = j + 1; }
        k = k + 1;
    }
    println(total);
}
"#,
            &["560"],
            "soa_string_field_scope_cleanup",
        );
    }

    #[test]
    fn asan_soa_string_field_index_store_overwrite_no_leak() {
        // Whole-element overwrite `cells[i] = Cell { … }` over a String
        // field: `compile_soa_index_store` drops the OLD element's String
        // buffer before scattering the new one. Pre-fix the old "initial-…"
        // strings leaked (8 × 20 = 160). After overwrite ids = i+10, so the
        // sum = (28 + 80)/frame × 20 = 2160.
        assert_clean_asan_run(
            r#"
struct Cell { id: i64, name: String }
layout cells: Vec[Cell] { group ids { id } group names { name } }
fn main() with panics {
    let mut total = 0;
    let mut k = 0;
    while k < 20 {
        let mut cells: Vec[Cell] = Vec.new();
        let mut i = 0;
        while i < 8 {
            cells.push(Cell { id: i, name: f"initial-soa-heap-string-payload-element-{i}" });
            i = i + 1;
        }
        let mut j = 0;
        while j < cells.len() {
            cells[j] = Cell { id: j + 10, name: f"rewritten-soa-heap-string-payload-element-{j}" };
            j = j + 1;
        }
        let mut r = 0;
        while r < cells.len() { total = total + cells[r].id; r = r + 1; }
        k = k + 1;
    }
    println(total);
}
"#,
            &["2160"],
            "soa_string_field_index_store_overwrite",
        );
    }

    #[test]
    fn asan_soa_string_field_field_store_overwrite_no_leak() {
        // Field-level overwrite `cells[i].name = f"…"` over a heap String:
        // `compile_soa_field_store` frees the displaced buffer before the
        // store, and the f-string accumulator's own cleanup is suppressed
        // (else the acc + the SoA drop double-free — the SIGTRAP guard).
        // Pre-fix the old "initial-…" strings leaked. ids unchanged
        // (i = 0..8), sum = 28/frame × 20 = 560.
        assert_clean_asan_run(
            r#"
struct Cell { id: i64, name: String }
layout cells: Vec[Cell] { group ids { id } group names { name } }
fn main() with panics {
    let mut total = 0;
    let mut k = 0;
    while k < 20 {
        let mut cells: Vec[Cell] = Vec.new();
        let mut i = 0;
        while i < 8 {
            cells.push(Cell { id: i, name: f"initial-soa-heap-string-payload-element-{i}" });
            i = i + 1;
        }
        let mut j = 0;
        while j < cells.len() {
            cells[j].name = f"replaced-soa-heap-string-payload-element-{j}";
            j = j + 1;
        }
        let mut r = 0;
        while r < cells.len() { total = total + cells[r].id; r = r + 1; }
        k = k + 1;
    }
    println(total);
}
"#,
            &["560"],
            "soa_string_field_field_store_overwrite",
        );
    }

    #[test]
    fn asan_soa_string_field_reassign_carried_no_leak() {
        // Carried-grid double-buffer (`cells = rebuild(cells)`) where the
        // element has a heap String field: the reassignment's inline
        // group-buffer free must FIRST drop the old generation's String
        // payloads (via the synthesized drop fn), else every rebuilt frame
        // leaks the prior generation's strings. The by-value param is
        // caller-retains, so the old buffers are owned at the assignment
        // and the displaced strings are this site's to free. Rebuilt 5×
        // per frame × 20. `id` is bumped +1 each rebuild: final id = i + 5,
        // sum = (28 + 40)/frame × 20 = 1360.
        assert_clean_asan_run(
            r#"
struct Cell { id: i64, name: String }
layout cells: Vec[Cell] { group ids { id } group names { name } }
fn rebuild(src: Vec[Cell]) -> Vec[Cell] {
    let mut dst: Vec[Cell] = Vec.new();
    let mut i = 0;
    while i < src.len() {
        dst.push(Cell { id: src[i].id + 1, name: f"rebuilt-soa-heap-string-payload-element-{i}" });
        i = i + 1;
    }
    dst
}
fn init() -> Vec[Cell] {
    let mut cells: Vec[Cell] = Vec.new();
    let mut i = 0;
    while i < 8 {
        cells.push(Cell { id: i, name: f"initial-soa-heap-string-payload-element-{i}" });
        i = i + 1;
    }
    cells
}
fn main() with panics {
    let mut total = 0;
    let mut k = 0;
    while k < 20 {
        let mut cells: Vec[Cell] = init();
        let mut f = 0;
        while f < 5 { cells = rebuild(cells); f = f + 1; }
        let mut j = 0;
        while j < cells.len() { total = total + cells[j].id; j = j + 1; }
        k = k + 1;
    }
    println(total);
}
"#,
            &["1360"],
            "soa_string_field_reassign_carried",
        );
    }

    #[test]
    fn asan_soa_string_push_named_binding_no_double_free() {
        // Move-in of a NAMED owned struct binding into push
        // (`let c = Cell{…}; cells.push(c)`): push bit-copies `c`'s String
        // header into the group buffer, so the SoA Vec owns it. Without the
        // move-in cap-zero, `c`'s own StructDrop AND the SoA cleanup free
        // the same buffer — a double-free ASAN catches on every host (not a
        // leak, so even the macOS run flags it). Sum of ids = 560.
        assert_clean_asan_run(
            r#"
struct Cell { id: i64, name: String }
layout cells: Vec[Cell] { group ids { id } group names { name } }
fn main() with panics {
    let mut total = 0;
    let mut k = 0;
    while k < 20 {
        let mut cells: Vec[Cell] = Vec.new();
        let mut i = 0;
        while i < 8 {
            let c: Cell = Cell { id: i, name: f"named-binding-soa-heap-string-payload-{i}" };
            cells.push(c);
            i = i + 1;
        }
        let mut j = 0;
        while j < cells.len() { total = total + cells[j].id; j = j + 1; }
        k = k + 1;
    }
    println(total);
}
"#,
            &["560"],
            "soa_string_push_named_binding",
        );
    }

    #[test]
    fn asan_soa_vec_pod_field_no_leak() {
        // A `Vec[i64]` (Vec over a POD element) SoA field: the per-element
        // drop frees each element's outer Vec buffer at scope exit. Each
        // `make(12)` is a 96-byte buffer (past LSan's blind spot); 8 per
        // frame × 20 = 160 buffers that pre-fix leaked. Sum of `tag` = 560.
        assert_clean_asan_run(
            r#"
struct Row { tag: i64, data: Vec[i64] }
layout rows: Vec[Row] { group tags { tag } group bulk { data } }
fn make(n: i64) -> Vec[i64] {
    let mut v: Vec[i64] = Vec.new();
    let mut i = 0;
    while i < n { v.push(i); i = i + 1; }
    v
}
fn main() with panics {
    let mut total = 0;
    let mut k = 0;
    while k < 20 {
        let mut rows: Vec[Row] = Vec.new();
        let mut i = 0;
        while i < 8 { rows.push(Row { tag: i, data: make(12) }); i = i + 1; }
        let mut j = 0;
        while j < rows.len() { total = total + rows[j].tag; j = j + 1; }
        k = k + 1;
    }
    println(total);
}
"#,
            &["560"],
            "soa_vec_pod_field",
        );
    }

    #[test]
    fn asan_vec_of_shared_push_drop_singleton() {
        // B-2026-07-11-33 guard (+ the B-36 investigation): a `Vec[shared]` /
        // `Vec[Option[shared]]` rc-dec's its elements and frees its buffer at
        // scope exit, for the small SINGLE-element shape. Under Linux LSan this
        // is CLEAN — macOS `leaks` over-reported this shape (a false positive on
        // the `karac_realloc_or_panic` buffer, whose custom-allocator wrapper
        // the `leaks` tool doesn't track; B-36 was closed as a macOS-`leaks`
        // artifact, not a real leak). This test is the authoritative (LSan) guard.
        assert_clean_asan_run(
            r#"
shared struct N { val: i64, mut next: Option[N] }
fn main() {
    let n1 = N { val: 1, next: None };
    let mut v: Vec[N] = Vec.new();
    v.push(n1);
    let a = N { val: 2, next: None };
    let mut w: Vec[Option[N]] = Vec.new();
    w.push(Some(a));
    println(99);
}
"#,
            &["99"],
            "vec_of_shared_push_drop_singleton",
        );
    }

    #[test]
    fn asan_shared_list_build_remove_repeat() {
        // Regression for the `shared struct` RC over-dec (2026-05-30): a
        // tail-cursor-built list, removed via `remove_nth_from_end`
        // (returns `dummy.next`, which aliases the `head` param), repeated
        // in a loop. Pre-fix the caller's binding shared the source's single
        // ref and the second scope-exit dec drove the refcount negative — a
        // double-free ASAN flags (and the build leaked). Must run clean.
        assert_clean_asan_run(
            r#"
shared struct ListNode { val: i64, mut next: Option[ListNode] }
fn from_array(arr: Slice[i64]) -> Option[ListNode] {
    let n = arr.len();
    if n == 0 { return None; }
    let head = ListNode { val: arr[0], next: None };
    let mut tail = head;
    for i in 1..n {
        let node = ListNode { val: arr[i], next: None };
        tail.next = Some(node);
        tail = node;
    }
    Some(head)
}
fn remove_nth_from_end(head: Option[ListNode], n: i64) -> Option[ListNode] {
    let dummy = ListNode { val: 0, next: head };
    let mut fast = head;
    let mut i = 0i64;
    while i < n { if let Some(node) = fast { fast = node.next; } i = i + 1i64; }
    let mut slow = dummy;
    loop {
        match fast {
            Some(node) => { fast = node.next; if let Some(s) = slow.next { slow = s; } }
            None => break,
        }
    }
    if let Some(target) = slow.next { slow.next = target.next; }
    dummy.next
}
fn head_val(list: Option[ListNode]) -> i64 {
    match list { Some(node) => node.val, None => 0i64 }
}
fn main() {
    let data: Array[i64, 8] = [1, 2, 3, 4, 5, 6, 7, 8];
    let mut sum: i64 = 0i64;
    let mut k: i64 = 0i64;
    while k < 64i64 {
        let list = from_array(data);
        let n: i64 = (k % 8i64) + 1i64;
        let out = remove_nth_from_end(list, n);
        sum = sum + head_val(out);
        k = k + 1i64;
    }
    println(sum);
}
"#,
            &["72"],
            "shared_list_build_remove_repeat",
        );
    }

    #[test]
    fn asan_reshaper_headerless_dummy_free_repeat() {
        // Headerless "reshaper" elision (KARAC_HEADERLESS_RESHAPER, default-OFF):
        // an in-place link-permuting transform (reverse a sublist, LeetCode #92)
        // that owns its input list, permutes links via head-insertion splices,
        // and returns `dummy.next`. Under the flag the whole ListNode goes
        // headerless (16 B, no rc word); the sentinel `dummy` (a fresh node NOT
        // in the returned chain) must get a single-node free at scope exit. A
        // prior bug leaked that dummy once per reversal when the walk reassigned
        // `prev` off it (left > 1). Build + reverse + fold repeated 40× with a
        // shifting left>1 window, so a per-iteration dummy leak trips
        // LeakSanitizer (Linux) and any double-free trips ASAN. Runs clean under
        // BOTH layouts: headered by default, headerless when the env flag is set
        // (the flag-on leak gate is the point — run this test under
        // `KARAC_HEADERLESS_RESHAPER=1` in the Linux-LSan harness).
        assert_clean_asan_run(
            r#"
shared struct ListNode { val: i64, mut next: Option[ListNode] }
fn build(m: i64, seed: i64) -> Option[ListNode] {
    let dummy = ListNode { val: -1, next: None };
    let mut tail = dummy; let mut j = 0i64;
    while j < m { let node = ListNode { val: (j + seed) % 97i64, next: None }; tail.next = Some(node); tail = node; j = j + 1i64; }
    dummy.next
}
fn reverse_between(head: Option[ListNode], left: i64, right: i64) -> Option[ListNode] {
    let dummy = ListNode { val: 0, next: head };
    let mut prev = dummy; let mut i = 1i64;
    while i < left { match prev.next { Some(n) => { prev = n; } None => {} } i = i + 1i64; }
    match prev.next { Some(cur) => { let mut j = left;
        while j < right { match cur.next { Some(nxt) => { cur.next = nxt.next; nxt.next = prev.next; prev.next = Some(nxt); } None => {} } j = j + 1i64; } } None => {} }
    dummy.next
}
fn fold(list: Option[ListNode], seed: i64) -> i64 { let mut a = seed; let mut c = list;
    loop { match c { Some(n) => { a = (a * 131i64 + (n.val + 1i64)) % 1000000007i64; c = n.next; } None => break, } } a }
fn main() {
    let mut sum = 0i64; let mut k = 0i64;
    while k < 40i64 {
        let list = build(30i64, k);
        let r = reverse_between(list, 2i64 + (k % 5i64), 12i64);
        sum = (sum * 131i64 + fold(r, k)) % 1000000007i64;
        k = k + 1i64;
    }
    println(sum);
}
"#,
            &["530882893"],
            "reshaper_headerless_dummy_free_repeat",
        );
    }

    #[test]
    fn asan_option_shared_walk_unwrap_cursor_repeat() {
        // Regression for the walk-cursor refcount pair (2026-06-05):
        // (1) `Option[shared T]` variable-assign released the old inner
        // BEFORE retaining the new — `cur = node.next` freed the chain
        // out from under the cursor (UAF); (2) `let node = cur.unwrap()`
        // skipped the receive-inc (MethodCall misclassified as a fresh
        // +1 source) while still queueing the scope-exit dec — one
        // over-dec per iteration. Build + walk + drop repeated so a leak
        // (inverse failure: over-retain) trips LeakSanitizer too.
        assert_clean_asan_run(
            r#"
shared struct ListNode { val: i64, mut next: Option[ListNode] }
fn make() -> Option[ListNode] {
    let mut head = ListNode { val: 1, next: None };
    let second = ListNode { val: 2, next: None };
    head.next = Some(second);
    Some(head)
}
fn walk(head: Option[ListNode]) -> i64 {
    let mut cur = head;
    let mut sum = 0;
    while cur.is_some() {
        let node = cur.unwrap();
        sum = sum + node.val;
        cur = node.next;
    }
    sum
}
fn main() {
    let mut total: i64 = 0i64;
    let mut k: i64 = 0i64;
    while k < 64i64 {
        total = total + walk(make());
        k = k + 1i64;
    }
    println(total);
}
"#,
            &["192"],
            "option_shared_walk_unwrap_cursor_repeat",
        );
    }

    #[test]
    fn asan_option_shared_prepend_builder_rc_fallback_repeat() {
        // Regression for the RC-fallback boxing / `Option[shared T]`
        // collision (2026-06-05). The ownership checker flags the
        // prepend-builder's `head` for RC fallback; boxing redirected
        // the slot to a `{rc, Option}` heap ptr that the Option-assign /
        // arg-share / scope-exit paths misread as a raw Option struct
        // (32-byte store into the 8-byte slot — stack smash, then UAF
        // on the decoded-garbage tag). Option[shared] bindings are now
        // excluded from boxing. MUST run via the ownership-loaded
        // harness — the plain run never populates the RC-fallback set.
        assert_clean_asan_run_with_ownership(
            r#"
shared struct ListNode { val: i64, mut next: Option[ListNode] }
fn make(n: i64) -> Option[ListNode] {
    let mut head: Option[ListNode] = None;
    let mut i = 0;
    while i < n {
        let node = ListNode { val: i, next: head };
        head = Some(node);
        i = i + 1;
    }
    head
}
fn walk(head: Option[ListNode]) -> i64 {
    let mut cur = head;
    let mut sum = 0;
    while cur.is_some() {
        let node = cur.unwrap();
        sum = sum + node.val;
        cur = node.next;
    }
    sum
}
fn main() {
    let mut total: i64 = 0i64;
    let mut k: i64 = 0i64;
    while k < 32i64 {
        let chain = make(50);
        total = total + walk(chain);
        k = k + 1i64;
    }
    println(total);
}
"#,
            "option_shared_prepend_builder_rc_fallback_repeat",
        );
    }

    #[test]
    fn asan_option_shared_method_tail_field_step_repeat() {
        // Method niche-ABI extension (2026-06-05): `node.step()` where
        // `step(ref self) -> Option[ListNode] { self.next }` is a tail
        // field return from a BORROWED receiver, looped with fresh
        // chains, with the receiver's chain summed afterwards. Pins
        // three fixes under ASAN:
        //   1. ref-rooted tail field returns are NOT move-out zeroed
        //      (the zeroing also wrote through the un-deref'd ref-param
        //      slot into the caller's stack frame);
        //   2. the returned alias carries its own +1 (the ref-rooted
        //      FieldAccess arm in `compile_tail_final_expr`);
        //   3. method arg loops share-inc tracked `Option[shared]` args
        //      (`m.total(...)` consumes through the niche-ABI method
        //      param without stealing the caller's ref).
        assert_clean_asan_run(
            r#"
shared struct ListNode { val: i64, mut next: Option[ListNode] }
shared struct Merger { count: i64 }
impl ListNode {
    fn build(n: i64) -> Option[ListNode] {
        let mut head: Option[ListNode] = None;
        let mut i = n;
        while i > 0 {
            let node = ListNode { val: i, next: head };
            head = Some(node);
            i = i - 1;
        }
        head
    }
    fn step(ref self) -> Option[ListNode] { self.next }
}
impl Merger {
    fn total(ref self, head: Option[ListNode]) -> i64 {
        let mut t = 0;
        let mut cur = head;
        while cur.is_some() {
            let n = cur.unwrap();
            t = t + n.val;
            cur = n.next;
        }
        t
    }
}
fn main() {
    let m = Merger { count: 0 };
    let mut total = 0;
    let mut iter = 0;
    while iter < 50 {
        let chain = ListNode.build(50);
        let node = chain.unwrap();
        let stepped = node.step();
        total = total + m.total(stepped);
        total = total + m.total(chain);
        iter = iter + 1;
    }
    println(total);
}
"#,
            &["127450"],
            "option_shared_method_tail_field_step_repeat",
        );
    }

    #[test]
    fn asan_option_shared_field_let_alias_repeat() {
        // `let stepped = node.next;` — Identifier-object field read
        // bound by an untyped let (case (c)). The registration queued a
        // scope-exit dec with no balancing inc: stepped's dec freed the
        // sub-chain the field still owned, and the owner's drop walked
        // freed memory — LATENT on main (masked by garbage rc-words
        // stopping the walk) until the niche-ABI allocation shift made
        // it trap. Now takes the case-(d) aliasing-acquire +1. Summing
        // both the alias and the original chain catches both failure
        // directions under ASAN.
        assert_clean_asan_run(
            r#"
shared struct ListNode { val: i64, mut next: Option[ListNode] }
fn build(n: i64) -> Option[ListNode] {
    let mut head: Option[ListNode] = None;
    let mut i = n;
    while i > 0 {
        let node = ListNode { val: i, next: head };
        head = Some(node);
        i = i - 1;
    }
    head
}
fn sum(head: Option[ListNode]) -> i64 {
    let mut t = 0;
    let mut cur = head;
    while cur.is_some() {
        let n = cur.unwrap();
        t = t + n.val;
        cur = n.next;
    }
    t
}
fn main() {
    let mut total = 0;
    let mut iter = 0;
    while iter < 50 {
        let chain = build(50);
        let node = chain.unwrap();
        let stepped = node.next;
        total = total + sum(stepped);
        total = total + sum(chain);
        iter = iter + 1;
    }
    println(total);
}
"#,
            &["127450"],
            "option_shared_field_let_alias_repeat",
        );
    }

    #[test]
    fn asan_option_shared_owned_self_receiver_repeat() {
        // Owned-`self` shared receiver (the bugs.md receiver-move
        // segfault): the usermethod dispatch used to pass the stack-slot
        // address where owned-shared `self` expects the heap pointer —
        // the callee's receive-inc corrupted a stack word; and the tail
        // `self.next` zeroing severed the caller's list. Fixed pair
        // pinned under ASAN: receiver discriminated via the source-level
        // ref flag, tail field returns take the loaded-inner inc. The
        // post-call `m_total(chain)` read proves non-destructive reads;
        // the loop catches drift both directions.
        assert_clean_asan_run(
            r#"
shared struct ListNode { val: i64, mut next: Option[ListNode] }
impl ListNode {
    fn step(self) -> Option[ListNode] { self.next }
    fn value(self) -> i64 { self.val }
}
fn make(n: i64) -> Option[ListNode] {
    let mut head: Option[ListNode] = None;
    let mut i = n;
    while i > 0 {
        let node = ListNode { val: i, next: head };
        head = Some(node);
        i = i - 1;
    }
    head
}
fn sum(head: Option[ListNode]) -> i64 {
    let mut t = 0;
    let mut cur = head;
    while cur.is_some() {
        let n = cur.unwrap();
        t = t + n.val;
        cur = n.next;
    }
    t
}
fn main() {
    let mut total = 0;
    let mut iter = 0;
    while iter < 50 {
        let chain = make(50);
        let node = chain.unwrap();
        total = total + node.value();
        let rest = node.step();
        total = total + sum(rest);
        total = total + sum(chain);
        iter = iter + 1;
    }
    println(total);
}
"#,
            &["127500"],
            "option_shared_owned_self_receiver_repeat",
        );
    }

    #[test]
    fn asan_rc_elision_scratch_loop_repeat() {
        // RC elision phase A: per-iteration elided scratch objects.
        // The elided cleanup is an unconditional free — ASAN catches
        // a free of a still-referenced object (analysis unsound) and
        // LeakSanitizer (linux CI) catches a skipped free. Includes a
        // conditional-branch let (null-guard path) and the read-only
        // declared-owned callee (the inferred-Ref would-be-mode gate).
        assert_clean_asan_run(
            r#"
shared struct Stats { mut count: i64, mut total: i64 }
fn read_only(s: Stats) -> i64 {
    s.count
}
impl Stats {
    fn bump(mut ref self, n: i64) {
        self.count = self.count + 1;
        self.total = self.total + n;
    }
}
fn main() {
    let mut grand = 0;
    let mut iter = 0;
    while iter < 100 {
        let s = Stats { count: 0, total: 0 };
        s.bump(iter);
        grand = grand + s.total + read_only(s);
        if iter > 50 {
            let extra = Stats { count: 1, total: iter };
            grand = grand + extra.total;
        }
        iter = iter + 1;
    }
    println(grand);
}
"#,
            &["8725"],
            "rc_elision_scratch_loop_repeat",
        );
    }

    #[test]
    fn asan_cluster_append_builder_repeat() {
        // Phase B1 cluster free-walk under ASAN: the root's cleanup
        // frees every chain node WITHOUT consulting refcounts — a
        // wrong analysis (any node with a second owner) is an
        // immediate ASAN double-free; a missed node is a leak (linux
        // CI LeakSanitizer). Covers the canonical append builder +
        // inline walk + a link-displacement orphan (freed through
        // normal RC mid-build, unreachable from the walk).
        assert_clean_asan_run(
            r#"
shared struct ListNode { val: i64, mut next: Option[ListNode] }
fn build_and_sum(n: i64) -> i64 {
    let dummy = ListNode { val: 0, next: None };
    let mut tail = dummy;
    let mut i = 1;
    while i <= n {
        let node = ListNode { val: i, next: None };
        tail.next = Some(node);
        tail = node;
        i = i + 1;
    }
    let mut sum = 0;
    let mut cur = dummy.next;
    while cur.is_some() {
        let x = cur.unwrap();
        sum = sum + x.val;
        cur = x.next;
    }
    sum
}
fn displaced() -> i64 {
    let dummy = ListNode { val: 0, next: None };
    let a = ListNode { val: 10, next: None };
    let b = ListNode { val: 20, next: None };
    dummy.next = Some(a);
    dummy.next = Some(b);
    let mut sum = 0;
    let mut cur = dummy.next;
    while cur.is_some() {
        let x = cur.unwrap();
        sum = sum + x.val;
        cur = x.next;
    }
    sum
}
fn main() {
    let mut total = 0;
    let mut iter = 0;
    while iter < 50 {
        total = total + build_and_sum(50);
        total = total + displaced();
        iter = iter + 1;
    }
    println(total);
}
"#,
            &["64750"],
            "cluster_append_builder_repeat",
        );
    }

    #[test]
    fn asan_headerless_cluster_repeat() {
        // Phase D headerless members under ASAN: the type-pure
        // canonical builder allocates 16-byte nodes (no rc word) and
        // the root free-walk geps the SHIFTED link slot — a missed
        // layout conversion at any consumer site reads/writes 8 bytes
        // off and trips ASAN heap-buffer-overflow immediately; a
        // free-walk against the wrong slot is a wild-pointer free.
        // 100 iterations x 100 nodes; sum(1..=100) = 5050 per call.
        // Mixed-layout half: `lone()` uses the same type headered
        // (free literal, no cluster) in the same binary.
        assert_clean_asan_run(
            r#"
shared struct ListNode { val: i64, mut next: Option[ListNode] }
fn build_and_sum(n: i64) -> i64 {
    let dummy = ListNode { val: 0, next: None };
    let mut tail = dummy;
    let mut i = 1;
    while i <= n {
        let node = ListNode { val: i, next: None };
        tail.next = Some(node);
        tail = node;
        i = i + 1;
    }
    let mut sum = 0;
    let mut cur = dummy.next;
    while cur.is_some() {
        let x = cur.unwrap();
        sum = sum + x.val;
        cur = x.next;
    }
    sum
}
fn lone() -> i64 {
    let a = ListNode { val: 3, next: None };
    a.val
}
fn main() {
    let mut total = 0;
    let mut iter = 0;
    while iter < 100 {
        total = total + build_and_sum(100);
        total = total + lone();
        iter = iter + 1;
    }
    println(total);
}
"#,
            &["505300"],
            "headerless_cluster_repeat",
        );
    }

    #[test]
    fn asan_fresh_return_builders_repeat() {
        // Phase C1b fresh-return transfer under ASAN: both sanctioned
        // tail shapes (SomeRoot `Some(head)` and RootLink `dummy.next`)
        // hand the b2 count-free chain to the caller at rc==1 per node.
        // A missed suppression (tail compensation inc / Some transfer
        // inc) leaks every chain head; an over-eager root cleanup
        // (free-walk instead of root-only / none) is an immediate ASAN
        // double-free when the caller's dec-drop walks the chain.
        // 100 iterations x two 100-node chains; sum(1..=100) = 5050.
        assert_clean_asan_run(
            r#"
shared struct ListNode { val: i64, mut next: Option[ListNode] }
fn build_someroot(n: i64) -> Option[ListNode] {
    let head = ListNode { val: 1, next: None };
    let mut tail = head;
    let mut i = 2;
    while i <= n {
        let node = ListNode { val: i, next: None };
        tail.next = Some(node);
        tail = node;
        i = i + 1;
    }
    Some(head)
}
fn build_rootlink(n: i64) -> Option[ListNode] {
    let dummy = ListNode { val: 0, next: None };
    let mut tail = dummy;
    let mut i = 1;
    while i <= n {
        let node = ListNode { val: i, next: None };
        tail.next = Some(node);
        tail = node;
        i = i + 1;
    }
    dummy.next
}
fn sum_chain(head: Option[ListNode]) -> i64 {
    let mut sum = 0;
    let mut cur = head;
    while cur.is_some() {
        let x = cur.unwrap();
        sum = sum + x.val;
        cur = x.next;
    }
    sum
}
fn main() {
    let mut total = 0;
    let mut iter = 0;
    while iter < 100 {
        total = total + sum_chain(build_someroot(100));
        total = total + sum_chain(build_rootlink(100));
        iter = iter + 1;
    }
    println(total);
}
"#,
            &["1010000"],
            "fresh_return_builders_repeat",
        );
    }

    #[test]
    fn asan_headerless_abi_full_pipeline_repeat() {
        // Phase C2b under ASAN: program-wide headerless ListNode —
        // 16-byte nodes with NO rc word — through the full kata-#2
        // composition, 200 iterations with chain reuse. The failure
        // modes are vicious and deterministic: any survived count op
        // corrupts val/next (wrong total), any layout disagreement
        // GEPs off by 8 (ASAN OOB on the trailing field), an
        // unbalanced borrow/adoption double-frees or leaks per
        // iteration. Exact total: 200*15 + 9 + 15 = 3024.
        assert_clean_asan_run(
            r#"
shared struct ListNode { val: i64, mut next: Option[ListNode] }
fn from_three(a: i64, b: i64, c: i64) -> Option[ListNode] {
    let dummy = ListNode { val: 0, next: None };
    let mut tail = dummy;
    let mut i = 0;
    while i < 3 {
        let mut v = a;
        if i == 1 { v = b; }
        if i == 2 { v = c; }
        let node = ListNode { val: v, next: None };
        tail.next = Some(node);
        tail = node;
        i = i + 1;
    }
    dummy.next
}
fn add_two_numbers(l1: Option[ListNode], l2: Option[ListNode]) -> Option[ListNode] {
    let dummy = ListNode { val: 0, next: None };
    let mut tail = dummy;
    let mut a = l1;
    let mut b = l2;
    let mut carry: i64 = 0;
    loop {
        let mut s: i64 = carry;
        let mut done = true;
        if let Some(n) = a {
            s = s + n.val;
            a = n.next;
            done = false;
        }
        if let Some(n) = b {
            s = s + n.val;
            b = n.next;
            done = false;
        }
        if done and s == 0 {
            break;
        }
        let node = ListNode { val: s % 10, next: None };
        tail.next = Some(node);
        tail = node;
        carry = s / 10;
    }
    dummy.next
}
fn sum_chain(head: Option[ListNode]) -> i64 {
    let mut sum = 0;
    let mut cur = head;
    while cur.is_some() {
        let x = cur.unwrap();
        sum = sum + x.val;
        cur = x.next;
    }
    sum
}
fn main() {
    let l1 = from_three(2, 4, 3);
    let l2 = from_three(5, 6, 4);
    let mut total = 0;
    let mut iter = 0;
    while iter < 200 {
        let r = add_two_numbers(l1, l2);
        total = total + sum_chain(r);
        iter = iter + 1;
    }
    total = total + sum_chain(l1) + sum_chain(l2);
    println(total);
}
"#,
            &["3024"],
            "headerless_abi_full_pipeline_repeat",
        );
    }

    #[test]
    fn asan_borrowed_param_walks_repeat() {
        // Phase C2a under ASAN: two long-lived chains walked by a
        // borrowing adder 200 times. The borrow contract is balanced
        // per call (caller arg-site head inc / callee exit RcDecOption)
        // while ALL walk traffic is count-free — an unbalanced cursor
        // (a stray alias-acquire inc, a counted advance, an over-eager
        // family cleanup) frees a reused chain mid-loop (ASAN UAF) or
        // leaks per call (LeakSanitizer / RSS). Exact total pins the
        // arithmetic: 200*15 + 9 + 15 = 3024.
        assert_clean_asan_run(
            r#"
shared struct ListNode { val: i64, mut next: Option[ListNode] }
fn from_three(a: i64, b: i64, c: i64) -> Option[ListNode] {
    let dummy = ListNode { val: 0, next: None };
    let mut tail = dummy;
    let mut i = 0;
    while i < 3 {
        let mut v = a;
        if i == 1 { v = b; }
        if i == 2 { v = c; }
        let node = ListNode { val: v, next: None };
        tail.next = Some(node);
        tail = node;
        i = i + 1;
    }
    dummy.next
}
fn add_two_numbers(l1: Option[ListNode], l2: Option[ListNode]) -> Option[ListNode] {
    let dummy = ListNode { val: 0, next: None };
    let mut tail = dummy;
    let mut a = l1;
    let mut b = l2;
    let mut carry: i64 = 0;
    loop {
        let mut s: i64 = carry;
        let mut done = true;
        if let Some(n) = a {
            s = s + n.val;
            a = n.next;
            done = false;
        }
        if let Some(n) = b {
            s = s + n.val;
            b = n.next;
            done = false;
        }
        if done and s == 0 {
            break;
        }
        let node = ListNode { val: s % 10, next: None };
        tail.next = Some(node);
        tail = node;
        carry = s / 10;
    }
    dummy.next
}
fn sum_chain(head: Option[ListNode]) -> i64 {
    let mut sum = 0;
    let mut cur = head;
    while cur.is_some() {
        let x = cur.unwrap();
        sum = sum + x.val;
        cur = x.next;
    }
    sum
}
fn main() {
    let l1 = from_three(2, 4, 3);
    let l2 = from_three(5, 6, 4);
    let mut total = 0;
    let mut iter = 0;
    while iter < 200 {
        let r = add_two_numbers(l1, l2);
        total = total + sum_chain(r);
        iter = iter + 1;
    }
    total = total + sum_chain(l1) + sum_chain(l2);
    println(total);
}
"#,
            &["3024"],
            "borrowed_param_walks_repeat",
        );
    }

    #[test]
    fn asan_adopted_builders_repeat() {
        // Phase C1c under ASAN: both adopted-family shapes — the
        // sanctioned match head-read and the non-owning cursor walk —
        // dropping per iteration via the option-guarded free-walk. An
        // adoption miscount has both signatures: an over-eager walk
        // double-frees against a still-counted ref (immediate ASAN
        // UAF); a missed adoption / suppressed-cleanup mismatch leaks
        // a 100-node chain per iteration (LeakSanitizer where
        // available, RSS blowup otherwise). 100 iterations, exact
        // total: (1 + 5050) * 100.
        assert_clean_asan_run(
            r#"
shared struct ListNode { val: i64, mut next: Option[ListNode] }
fn build_someroot(n: i64) -> Option[ListNode] {
    let head = ListNode { val: 1, next: None };
    let mut tail = head;
    let mut i = 2;
    while i <= n {
        let node = ListNode { val: i, next: None };
        tail.next = Some(node);
        tail = node;
        i = i + 1;
    }
    Some(head)
}
fn build_rootlink(n: i64) -> Option[ListNode] {
    let dummy = ListNode { val: 0, next: None };
    let mut tail = dummy;
    let mut i = 1;
    while i <= n {
        let node = ListNode { val: i, next: None };
        tail.next = Some(node);
        tail = node;
        i = i + 1;
    }
    dummy.next
}
fn main() {
    let mut total = 0;
    let mut iter = 0;
    while iter < 100 {
        let a = build_someroot(100);
        match a {
            Some(node) => { total = total + node.val; }
            None => {}
        }
        let b = build_rootlink(100);
        let mut cur = b;
        while cur.is_some() {
            let x = cur.unwrap();
            total = total + x.val;
            cur = x.next;
        }
        iter = iter + 1;
    }
    println(total);
}
"#,
            &["505100"],
            "adopted_builders_repeat",
        );
    }

    #[test]
    fn asan_param_coexisting_builders_repeat() {
        // Phase C1a under ASAN: kata #2's exact pipeline — C1b
        // builders feed a param-walking adder whose own cluster
        // transfers out (member-type params coexist with the cluster,
        // keeping full RC). A wall failure has both signatures: a
        // param node entering the cluster double-frees against its RC
        // drop; a fresh node leaking under a param chain over-frees on
        // the param's dec-walk. 200 iterations, exact total pins the
        // arithmetic (342+465=807 → digit sum 15 → 3000).
        assert_clean_asan_run(
            r#"
shared struct ListNode { val: i64, mut next: Option[ListNode] }
fn from_three(a: i64, b: i64, c: i64) -> Option[ListNode] {
    let dummy = ListNode { val: 0, next: None };
    let mut tail = dummy;
    let mut i = 0;
    while i < 3 {
        let mut v = a;
        if i == 1 { v = b; }
        if i == 2 { v = c; }
        let node = ListNode { val: v, next: None };
        tail.next = Some(node);
        tail = node;
        i = i + 1;
    }
    dummy.next
}
fn add_two_numbers(l1: Option[ListNode], l2: Option[ListNode]) -> Option[ListNode] {
    let dummy = ListNode { val: 0, next: None };
    let mut tail = dummy;
    let mut a = l1;
    let mut b = l2;
    let mut carry: i64 = 0;
    loop {
        let mut s: i64 = carry;
        let mut done = true;
        if let Some(n) = a {
            s = s + n.val;
            a = n.next;
            done = false;
        }
        if let Some(n) = b {
            s = s + n.val;
            b = n.next;
            done = false;
        }
        if done and s == 0 {
            break;
        }
        let node = ListNode { val: s % 10, next: None };
        tail.next = Some(node);
        tail = node;
        carry = s / 10;
    }
    dummy.next
}
fn sum_chain(head: Option[ListNode]) -> i64 {
    let mut sum = 0;
    let mut cur = head;
    while cur.is_some() {
        let x = cur.unwrap();
        sum = sum + x.val;
        cur = x.next;
    }
    sum
}
fn main() {
    let mut total = 0;
    let mut iter = 0;
    while iter < 200 {
        let l1 = from_three(2, 4, 3);
        let l2 = from_three(5, 6, 4);
        let r = add_two_numbers(l1, l2);
        total = total + sum_chain(r);
        iter = iter + 1;
    }
    println(total);
}
"#,
            &["3000"],
            "param_coexisting_builders_repeat",
        );
    }

    #[test]
    fn asan_option_shared_niche_abi_convergence_repeat() {
        // Niche call ABI for `Option[shared T]` signatures (Slice 1,
        // 2026-06-05) + the explicit-return alias compensation it
        // surfaced. One loop exercising every convergence point under
        // ASAN: chained call-result args (`ident(make(...))` packs and
        // unpacks at each boundary), explicit `return head;` /
        // `return node.next;` aliases (each needs the Return-arm +1 so
        // the param's scope-exit dec doesn't free the returned chain),
        // recursion (`nth`), and the `?` operator (shared-typed `let`
        // from `q_w0` + null early-return through the niche). Repeats
        // catch both failure directions: UAF (under-count) trips ASAN,
        // leak (over-count) trips LeakSanitizer on platforms that have
        // it.
        assert_clean_asan_run(
            r#"
shared struct ListNode { val: i64, mut next: Option[ListNode] }
fn make(n: i64) -> Option[ListNode] {
    let mut head: Option[ListNode] = None;
    let mut i = n;
    while i > 0 {
        let node = ListNode { val: i, next: head };
        head = Some(node);
        i = i - 1;
    }
    head
}
fn ident(head: Option[ListNode]) -> Option[ListNode] { head }
fn ret_field(head: Option[ListNode]) -> Option[ListNode] {
    if head.is_some() {
        let node = head.unwrap();
        return node.next;
    }
    return None;
}
fn nth(head: Option[ListNode], k: i64) -> Option[ListNode] {
    if k == 0 {
        return head;
    }
    if head.is_none() {
        return None;
    }
    let node = head.unwrap();
    nth(node.next, k - 1)
}
fn second(head: Option[ListNode]) -> Option[ListNode] {
    let first = head?;
    let rest = first.next?;
    Some(rest)
}
fn sum(head: Option[ListNode]) -> i64 {
    let mut total = 0;
    let mut cur = head;
    while cur.is_some() {
        let node = cur.unwrap();
        total = total + node.val;
        cur = node.next;
    }
    total
}
fn main() {
    let mut total: i64 = 0i64;
    let mut k: i64 = 0i64;
    while k < 32i64 {
        total = total + sum(ident(make(10)));
        total = total + sum(ret_field(make(10)));
        total = total + sum(nth(make(10), 4));
        total = total + sum(second(make(10)));
        k = k + 1i64;
    }
    println(total);
}
"#,
            &["6656"],
            "option_shared_niche_abi_convergence_repeat",
        );
    }
    // ── Auto-par slot-ownership transfer (2026-06-05) ─────────────

    /// The Map-handle slot-publication UAF: auto-par groups
    /// `String.add` + `Map.new()`, the Map-producing branch writes the
    /// handle into the parent's return slot, and pre-fix ALSO ran its
    /// queued `FreeMapHandle` at branch end — the parent's `m.insert`
    /// then operated on freed memory (SIGSEGV in release, UAF under
    /// ASAN). Threads the full pipeline (ownership + concurrency) so
    /// the auto-par lowering actually fires — the default harness's
    /// `None, None` compile never reaches this code path.
    #[test]
    fn asan_auto_par_map_slot_published_handle_clean() {
        let label = "auto_par_map_slot_published_handle";
        if !asan_available() {
            eprintln!("[{label}] ASAN unavailable on this host — skipping");
            return;
        }
        let Some((stdout, status)) = run_under_asan_with_full_pipeline(
            r#"
fn main() {
    let name = "ka" + "ra";
    let mut m: Map[String, i64] = Map.new();
    m.insert("a", 1);
    m.insert("b", 2);
    let b = m.get("b");
    match b {
        Some(val) => println(val),
        None => println(0),
    }
    println(name);
}
"#,
            label,
        ) else {
            eprintln!("[{label}] setup failed — skipping");
            return;
        };
        assert!(
            status.success(),
            "[{label}] ASAN reported a memory error (exit code {:?}) — \
             look for heap-use-after-free on the slot-published Map handle",
            status.code()
        );
        assert_eq!(
            stdout.trim().lines().collect::<Vec<_>>(),
            vec!["2", "kara"],
            "[{label}] unexpected stdout (ASAN passed, output mismatched)"
        );
    }

    /// A3a leak regression: two independent allocating calls (each builds a
    /// fresh Vec) now AUTO-parallelize — `(Allocates,Allocates)` is no longer a
    /// conflict. Each Vec is built in its own par branch, published to the
    /// parent's slot, then MOVED into `sum` which owns and frees it at scope
    /// exit. A wrongly-skipped or doubled free on the grouped branch's owned
    /// buffer is exactly the leak/double-free class this gate exists for.
    /// Threads the full pipeline (ownership + concurrency) so auto-par actually
    /// fires — the default `None, None` harness leaves the grouping dead. Each
    /// Vec carries 8 i64s (64 bytes), above the LSan reachability threshold, and
    /// both are consumed (not live at exit), so a missing free is a detectable
    /// non-reachable leak rather than one LSan masks.
    #[test]
    fn asan_par_ref_string_arg_network_call_no_double_free() {
        // A2b-2 variable-arg groundwork: a network call whose param is `ref
        // String` BORROWS its argument (no move), so lifting it into a par
        // branch must NOT drop the branch's view of the parent's owned `String`
        // — the parent stays the unique owner and frees each once. Uses an
        // explicit `par {}` (same capture machinery auto-par reuses) to prove
        // the borrow-capture is double-free-clean BEFORE the predicate is
        // relaxed to admit ref-param args to auto-par. Loop so any double-free
        // accumulates under ASan.
        assert_clean_asan_run(
            r#"
fn fetch(u: ref String) -> i64 with reads(Network) suspends { return u.len(); }
fn main() {
    let mut i: i64 = 0i64;
    while i < 3i64 {
        let a = "aaaaaaaaaaaaaaaaaaaa";
        let b = "bbbbbbbbbbbbbbbbbbbb";
        let r = par {
            let x = fetch(a);
            let y = fetch(b);
            x + y
        };
        println(r);
        i = i + 1i64;
    }
}
"#,
            &["40", "40", "40"],
            "asan_par_ref_string_arg_network_call_no_double_free",
        );
    }

    #[test]
    fn asan_a2b2_autopar_ref_param_arg_no_double_free() {
        // A2b-2 variable-arg (end-to-end): two `reads(Network) suspends` calls
        // whose `ref String` param BORROWS an owned parent binding are now
        // grouped by AUTO-PAR (no explicit `par {}`) — the relaxed
        // `is_safe_network_fanout` admits identifier args at borrow positions.
        // The parent stays the unique owner of each `String`; the borrow into
        // the branch must not double-free. Pins the full admission path
        // (analysis groups -> codegen fans out), LSan/ASan-clean. Companion to
        // the explicit-`par {}` `asan_par_ref_string_arg_network_call_no_double_free`.
        assert_clean_asan_run(
            r#"
fn fetch(u: ref String) -> i64 with reads(Network) suspends { return u.len(); }
fn main() {
    let a = "aaaaaaaaaaaaaaaaaaaa";
    let b = "bbbbbbbbbbbbbbbbbbbb";
    let x = fetch(a);
    let y = fetch(b);
    println(x);
    println(y);
}
"#,
            &["20", "20"],
            "asan_a2b2_autopar_ref_param_arg_no_double_free",
        );
    }

    #[test]
    fn asan_auto_par_network_effect_owned_heap_fanout_clean() {
        // A2b-2: two independent `reads(Network) suspends` fns returning owned
        // heap (String) are now grouped by auto-par via the arg-safe
        // network-fanout exemption (`is_safe_network_fanout`) and fanned out
        // through the return-slot move-only path. Each String must be freed
        // exactly once — the parent is the unique drop owner after the branch
        // bit-copies through its slot and the branch's own cleanup is
        // discarded. Distinct from `asan_auto_par_allocating_calls_clean`
        // (grouped via `allocates`): this exercises the `reads(Network)`-driven
        // admission end-to-end, so a leak/double-free surfaces under LSan/ASan.
        assert_clean_asan_run(
            r#"
fn fetch_a() -> String with reads(Network) suspends { return "aaaaaaaaaaaaaaaaaaaa"; }
fn fetch_b() -> String with reads(Network) suspends { return "bbbbbbbbbbbbbbbbbbbb"; }
fn main() {
    let x = fetch_a();
    let y = fetch_b();
    println(x);
    println(y);
}
"#,
            &["aaaaaaaaaaaaaaaaaaaa", "bbbbbbbbbbbbbbbbbbbb"],
            "asan_auto_par_network_effect_owned_heap_fanout_clean",
        );
    }

    #[test]
    fn asan_a2b2_ephemeral_send_recv_owned_param_fanout_clean() {
        // A2b-2 Phase 1: two *ephemeral* network calls that `sends(Network)`
        // AND `receives(Network)` — the real `http_get` shape — with an OWNED
        // `String` param fed a literal arg. Before Phase 1 the send/recv
        // `Network` conflict kept this pair serial, so the fanned-out codegen
        // path was never reached for the send/recv shape; Phase 1's ephemeral
        // relaxation now groups it. Memory-safety proof: each coroutine takes
        // ownership of the moved-in `String` (a heap value materialized from
        // the literal) and returns it, so the value must be freed EXACTLY once
        // across the fork/join — it flows param → return-slot bit-copy → parent
        // (sole drop owner), with the branch's own cleanup discarded. A literal
        // arg names no parent binding, so there is no caller-side drop to
        // double-cancel (the coroutine-owned-param hazard cannot fire). A
        // double-free or leak surfaces under LSan/ASan. Companion to the
        // `reads(Network)` variant `asan_auto_par_network_effect_owned_heap_fanout_clean`,
        // which could not exercise the send/recv-conflict path.
        assert_clean_asan_run(
            r#"
fn get_a(u: String) -> String with sends(Network) receives(Network) { return u; }
fn get_b(u: String) -> String with sends(Network) receives(Network) { return u; }
fn main() {
    let x = get_a("aaaaaaaaaaaaaaaaaaaa");
    let y = get_b("bbbbbbbbbbbbbbbbbbbb");
    println(x);
    println(y);
}
"#,
            &["aaaaaaaaaaaaaaaaaaaa", "bbbbbbbbbbbbbbbbbbbb"],
            "asan_a2b2_ephemeral_send_recv_owned_param_fanout_clean",
        );
    }

    #[test]
    fn asan_a2b2_associated_network_opener_owned_param_fanout_clean() {
        // A2b-2 Phase 2 Slice 1: two *associated* (receiver-less) network
        // openers — `Net.open("a"); Net.open("b")`, the `TcpStream.connect`
        // shape — with an OWNED `String` param fed a literal arg, moved through
        // to the return. Extends the Phase 1 ephemeral proof to the 2-segment
        // associated-call codegen path (a fresh path that Phase 1 never fanned
        // out). Memory-safety proof is identical to the free-fn variant: the
        // coroutine owns the moved-in `String` and returns it, so it flows param
        // → return-slot bit-copy → parent (sole drop owner) and is freed EXACTLY
        // once across the fork/join; the literal arg names no parent binding, so
        // no caller-side drop can double-cancel. A double-free or leak surfaces
        // under LSan/ASan.
        assert_clean_asan_run(
            r#"
struct Net { id: i64 }
impl Net {
    fn open(u: String) -> String with sends(Network) receives(Network) { return u; }
}
fn main() {
    let x = Net.open("aaaaaaaaaaaaaaaaaaaa");
    let y = Net.open("bbbbbbbbbbbbbbbbbbbb");
    println(x);
    println(y);
}
"#,
            &["aaaaaaaaaaaaaaaaaaaa", "bbbbbbbbbbbbbbbbbbbb"],
            "asan_a2b2_associated_network_opener_owned_param_fanout_clean",
        );
    }

    #[test]
    fn asan_a2b2_method_distinct_receivers_fanout_clean() {
        // A2b-2 Phase 2 Slice 2: two `mut ref self` network method calls on
        // DISTINCT non-shared local receivers (`s1.fetch(); s2.fetch()`) fan out.
        // Memory-safety proof for the method-receiver path: each receiver is
        // BORROWED (mut ref self — not moved into the coroutine, so no
        // receiver double-drop), the mutation is written back through the
        // captured-mutation machinery, and each returned owned `String` flows
        // through its own return slot with the parent as sole drop owner. The
        // `Stream` locals drop exactly once at `main` scope exit. A double-free
        // or leak surfaces under LSan/ASan.
        assert_clean_asan_run(
            r#"
struct Stream { n: i64 }
impl Stream {
    fn fetch(mut ref self) -> String with sends(Network) receives(Network) {
        self.n = self.n + 1;
        return "aaaaaaaaaaaaaaaaaaaa";
    }
}
fn main() {
    let mut s1 = Stream { n: 0 };
    let mut s2 = Stream { n: 0 };
    let a = s1.fetch();
    let b = s2.fetch();
    println(a);
    println(b);
}
"#,
            &["aaaaaaaaaaaaaaaaaaaa", "aaaaaaaaaaaaaaaaaaaa"],
            "asan_a2b2_method_distinct_receivers_fanout_clean",
        );
    }

    #[test]
    fn asan_auto_par_allocating_calls_clean() {
        let label = "auto_par_allocating_calls";
        if !asan_available() {
            eprintln!("[{label}] ASAN unavailable on this host — skipping");
            return;
        }
        let Some((stdout, status)) = run_under_asan_with_full_pipeline(
            r#"
fn make(seed: i64) -> Vec[i64] {
    let mut v: Vec[i64] = Vec.new();
    let mut i = 0;
    while i < 8 {
        v.push(seed + i);
        i = i + 1;
    }
    return v;
}
fn sum(xs: Vec[i64]) -> i64 {
    let mut t = 0;
    let mut i = 0;
    while i < 8 {
        t = t + xs[i];
        i = i + 1;
    }
    return t;
}
fn main() {
    let a = make(100);
    let b = make(200);
    println(sum(a) + sum(b));
}
"#,
            label,
        ) else {
            eprintln!("[{label}] setup failed — skipping");
            return;
        };
        assert!(
            status.success(),
            "[{label}] ASAN reported a memory error (exit code {:?}) — \
             look for a LeakSanitizer report or double-free on a grouped \
             branch's owned Vec buffer",
            status.code()
        );
        // make(100)=sum 100..107=828; make(200)=sum 200..207=1628; total 2456.
        assert_eq!(
            stdout.trim().lines().collect::<Vec<_>>(),
            vec!["2456"],
            "[{label}] unexpected stdout (ASAN passed, output mismatched)"
        );
    }

    /// B-2026-07-03-32: the Column-handle slot-publication UAF. Auto-par
    /// groups the `print(hd(av))` read and the `let c = Column.from_vec(…)`
    /// producer into sibling par branches. The producing branch writes the
    /// column control-block pointer into the parent's return slot, then
    /// pre-fix ALSO ran its queued `FreeColumn` at branch end — freeing the
    /// three buffers (data / null-bitmap / control) it had just published.
    /// The parent's `c.len()` after the join then read a dangling control
    /// block: `0` under `karac build` (correct `4` under `karac run` and
    /// `KARAC_AUTO_PAR=0`), or an out-of-bounds panic on the first element
    /// access — a SILENT wrong-output miscompile, the worst class. The fix
    /// transfers `FreeColumn` (and its `DataFrame`/`Tensor` siblings) from
    /// the branch to the parent via `SlotOwnership`, exactly like the
    /// Map/Struct/SoA handles already were. Threads the full pipeline
    /// (ownership + concurrency) so auto-par actually fires — the default
    /// `None, None` harness leaves the grouping dead. The 4-element i64
    /// column is 32 data bytes + a bitmap + a control block; a double-free
    /// on the published control block trips ASAN, a skipped parent free is a
    /// LeakSanitizer report on Linux.
    #[test]
    fn asan_b32_auto_par_column_slot_published_handle_clean() {
        let label = "auto_par_column_slot_published_handle";
        if !asan_available() {
            eprintln!("[{label}] ASAN unavailable on this host — skipping");
            return;
        }
        let Some((stdout, status)) = run_under_asan_with_full_pipeline(
            r#"
fn hd(v: Vec[i64]) -> i64 { v[0] }
fn main() {
    let av: Vec[i64] = [4, 2, 7, 1];
    println(hd(av));
    let c: Column[i64] = Column.from_vec([5, 9, 3, 1]);
    println(c.len());
    println(c.iter_valid()[3]);
}
"#,
            label,
        ) else {
            eprintln!("[{label}] setup failed — skipping");
            return;
        };
        assert!(
            status.success(),
            "[{label}] ASAN reported a memory error (exit code {:?}) — \
             look for heap-use-after-free / double-free on the slot-published \
             Column control block, or a LeakSanitizer report on a skipped \
             parent free",
            status.code()
        );
        // hd([4,2,7,1])=4; from_vec([5,9,3,1]).len()=4; iter_valid()[3]=1.
        assert_eq!(
            stdout.trim().lines().collect::<Vec<_>>(),
            vec!["4", "4", "1"],
            "[{label}] unexpected stdout — a `0` for len (or a panic) is the \
             dangling-control-block miscompile this gate exists for"
        );
    }

    /// Tensor heap lifecycle (phase-11 codegen core slice): one malloc'd
    /// `[rank][dims][data]` block per tensor, freed once at scope exit
    /// via `FreeTensor`'s null-guard. Exercises every ownership-transfer
    /// shape in one program — construction (all four constructors,
    /// including the temporary-dims-Vec eager free), mutation, `let b =
    /// a;` move (source slot nulled — double-free would trip ASAN),
    /// fn-boundary moves (owned arg + tail return), and `shape()`'s
    /// fresh Vec (its own FreeVecBuffer). Leak detection on Linux
    /// (detect_leaks=1) additionally catches a missing free.
    #[test]
    fn asan_tensor_lifecycle_clean() {
        let label = "tensor_lifecycle";
        if !asan_available() {
            eprintln!("[{label}] ASAN unavailable on this host — skipping");
            return;
        }
        let Some((stdout, status)) = run_under_asan(
            r#"
fn make() -> Tensor[f64, [2, 2]] {
    let t: Tensor[f64, [2, 2]] = Tensor.full([2, 2], 9.0);
    t
}

fn first(t: Tensor[f64, [2, 2]]) -> f64 {
    t[0, 0]
}

fn main() {
    let z: Tensor[f64, [2, 3]] = Tensor.zeros([2, 3]);
    println(z[1, 2]);
    let o: Tensor[i64, [4]] = Tensor.ones([4]);
    println(o[3]);
    let mut f = Tensor.from([[1, 2], [3, 4]]);
    f[0, 1] = 42;
    println(f[0, 1]);
    let s = f.shape();
    println(s[0]);
    let moved = f;
    println(moved[1, 0]);
    let m = make();
    println(m[1, 1]);
    println(first(make()));
}
"#,
            label,
        ) else {
            eprintln!("[{label}] setup failed — skipping");
            return;
        };
        assert!(
            status.success(),
            "[{label}] ASAN reported a memory error (exit code {:?}) — \
             check FreeTensor double-free/leak on the move-suppression paths",
            status.code()
        );
        assert_eq!(
            stdout.trim().lines().collect::<Vec<_>>(),
            vec!["0", "1", "42", "2", "3", "9", "9"],
            "[{label}] unexpected stdout (ASAN passed, output mismatched)"
        );
    }

    #[test]
    fn asan_iter_axis_row_view_bind_no_double_free() {
        // B-2026-07-13-7: `t.iter_axis(n)` returns a `Vec[Tensor]` of freshly
        // malloc'd sub-tensor blocks, freed per-element by
        // `track_vec_of_tensors_var`. Binding an element out — `let r = rows[i]`
        // — shallow-copied the 8-byte tensor pointer (no Tensor arm in the
        // clone dispatcher), so the binding's `FreeTensor` and the container's
        // per-element free hit the SAME block: `free(): double free detected in
        // tcache 2` under JIT/native (interpreter was correct). The fix
        // deep-clones the whole tensor block so the binding owns an independent
        // copy. 300 iters, two row views bound per iter: a missed clone
        // double-frees (ASAN), a leaked clone accumulates (LSan on Linux).
        assert_clean_asan_run(
            r#"
fn main() {
    let mut i: i64 = 0;
    let mut total: f32 = 0.0f32;
    while i < 300 {
        let m: Tensor[f32, [2, 3]] = Tensor.from([[1.0f32, 2.0f32, 3.0f32], [4.0f32, 5.0f32, 6.0f32]]);
        let rows = m.iter_axis(0);
        let r0 = rows[0];
        let r1 = rows[1];
        total = total + r0.sum() + r1.sum();
        i = i + 1;
    }
    println(total.to_string());
}
"#,
            // Each iter: r0.sum()=1+2+3=6, r1.sum()=4+5+6=15 → 21. 300*21 = 6300.
            &["6300"],
            "iter_axis_row_view_bind_no_double_free",
        );
    }

    #[test]
    fn asan_shared_vec_field_index_field_mutation_no_leak() {
        // B-2026-07-13-10 leak/UAF gate. The read/store fixes GEP a shared
        // element's heap field through a Vec that is itself a FIELD of a shared
        // struct (`root.kids[i].val`). Both paths reach the element handle via
        // `compile_expr(Index)`, which is a PURE read (the field-access-rooted
        // index mints a synth Vec identifier and recurses — no rc_inc), so
        // neither adds an owned ref that would need a matching dec. This churns
        // the shape 300× — each iter builds a shared root + two shared children,
        // pushes the children into the `kids` Vec field (RC co-ownership),
        // mutates them through the chained store, and reads them back — so a
        // stray inc on the chain (leak) or a missed dec of the pushed children
        // at Vec-drop (leak) is caught by LSan on Linux, and a double-free of a
        // child box would trip ASAN.
        assert_clean_asan_run(
            r#"
shared struct Node { mut val: i64, mut kids: Vec[Node] }
fn main() {
    let mut i: i64 = 0;
    let mut total: i64 = 0;
    while i < 300 {
        let root = Node { val: 1, kids: Vec.new() };
        let a = Node { val: 10, kids: Vec.new() };
        let b = Node { val: 20, kids: Vec.new() };
        root.kids.push(a);
        root.kids.push(b);
        root.kids[0].val = root.kids[0].val + 5;
        root.kids[1].val = 99;
        total = total + root.kids[0].val + root.kids[1].val;
        i = i + 1;
    }
    println(total.to_string());
}
"#,
            // Each iter: kids[0]=15, kids[1]=99 → 114. 300*114 = 34200.
            &["34200"],
            "shared_vec_field_index_field_mutation_no_leak",
        );
    }

    #[test]
    fn asan_shared_struct_map_shared_value_field_no_leak() {
        // B-2026-07-13-12. A `shared struct Owner { cache: Map[i64, Node] }`
        // (Node shared) dropped the Map's bucket storage via the type-erased
        // `karac_map_free_with_drop_vec` but NEVER dec'd the shared VALUES — the
        // shared-struct RC-drop's MapOrSet arm lacked the per-bucket
        // `emit_map_shared_half_rc_dec_walk` that the non-shared struct-drop peer
        // already runs. One ref leaked per live entry (LSan: 16-byte Node boxes).
        // 50 iters × 2 entries: the walk now dec's each shared value before the
        // bucket free.
        assert_clean_asan_run(
            r#"
shared struct Node { mut val: i64 }
shared struct Owner { mut cache: Map[i64, Node] }
fn main() {
    let mut i: i64 = 0;
    let mut total: i64 = 0;
    while i < 50 {
        let o = Owner { cache: Map.new() };
        o.cache.insert(1, Node { val: 7 });
        o.cache.insert(2, Node { val: 8 });
        total = total + 1;
        i = i + 1;
    }
    println(total.to_string());
}
"#,
            &["50"],
            "shared_struct_map_shared_value_field_no_leak",
        );
    }

    #[test]
    fn asan_shared_enum_vec_shared_payload_drop_no_leak() {
        // B-2026-07-13-13 (the shared-enum sibling of B-2026-07-13-11). A
        // `shared enum Tree { Branch(Vec[Node]) }` (Node shared) dropped with its
        // payload INTACT (RC → 0 while the Branch still holds the Vec) froze the
        // `{ptr,len,cap}` buffer but never dec'd the shared ELEMENTS —
        // `emit_shared_enum_field_drop`'s Vec/String arm had no element-drain
        // loop. One ref leaked per element (LSan: 16-byte Node boxes). This
        // churns the DIRECT-DROP path (construct, never match out the payload,
        // drop at scope exit) — the path this fix targets; the arm now drains
        // each element (the shared element's `emit_vec_elem_rc_dec_fn`) before
        // the buffer free. (The separate match-move shape `Branch(xs) => …`,
        // where the payload Vec is bound out, is tracked by B-2026-07-13-14.)
        assert_clean_asan_run(
            r#"
shared struct Node { mut val: i64 }
shared enum Tree { Leaf, Branch(Vec[Node]) }
fn build(n: i64) -> Tree {
    let mut v: Vec[Node] = Vec.new();
    v.push(Node { val: n });
    v.push(Node { val: n + 1 });
    Tree.Branch(v)
}
fn main() {
    let mut i: i64 = 0;
    let mut total: i64 = 0;
    while i < 50 {
        let t = build(i);
        total = total + 1;
        i = i + 1;
    }
    println(total.to_string());
}
"#,
            &["50"],
            "shared_enum_vec_shared_payload_drop_no_leak",
        );
    }

    #[test]
    fn asan_match_move_vec_shared_enum_payload_no_leak() {
        // B-2026-07-13-14. Matching a shared enum and binding a `Vec[shared T]`
        // payload OUT (`match t { Branch(xs) => … }`) leaked the shared elements.
        // The move-out suppression zeros the box's Vec `cap` so the enum's own
        // rc-drop skips the payload (the binding is the sole owner), but the
        // binding was tracked buffer-only (`track_vec_var`) — it freed the Vec
        // buffer and left every element's RC box unreferenced (LSan: 16-byte Node
        // boxes). `bind_pattern_values` now upgrades a `Vec[shared]` payload
        // binding to the element-draining tracker (`track_vec_of_aggs_var` → the
        // shared element's `emit_vec_elem_rc_dec_fn`) at the same single-owner
        // point, so no double-free (the source's drain is suppressed exactly as
        // its buffer-free already is — verified against move-further shapes:
        // return-out, re-move to another local, and a String payload which stays
        // buffer-only). 50 iters × 2 elements.
        assert_clean_asan_run(
            r#"
shared struct Node { mut val: i64 }
shared enum Tree { Leaf, Branch(Vec[Node]) }
fn main() {
    let mut i: i64 = 0;
    let mut total: i64 = 0;
    while i < 50 {
        let mut v: Vec[Node] = Vec.new();
        v.push(Node { val: 1 });
        v.push(Node { val: 2 });
        let t = Tree.Branch(v);
        match t {
            Tree.Leaf => {}
            Tree.Branch(xs) => { total = total + xs.len(); }
        }
        i = i + 1;
    }
    println(total.to_string());
}
"#,
            &["100"],
            "match_move_vec_shared_enum_payload_no_leak",
        );
    }

    #[test]
    fn asan_shared_enum_map_shared_value_payload_drop_no_leak() {
        // B-2026-07-13-15 (the shared-enum sibling of B-2026-07-13-12). A
        // `shared enum Store { Full(Map[i64, Node]) }` (Node shared) dropped with
        // its payload intact released the Map's bucket storage via the type-erased
        // `karac_map_free_with_drop_vec` but never dec'd the shared VALUES —
        // `emit_shared_enum_field_drop`'s Map/Set arm lacked the per-bucket
        // `emit_map_shared_half_rc_dec_walk` the struct-drop peer runs. One ref
        // leaked per live entry (LSan: 16-byte Node boxes). 50 iters × 2 entries:
        // the arm now walks each shared value before the bucket free.
        assert_clean_asan_run(
            r#"
shared struct Node { mut val: i64 }
shared enum Store { Empty, Full(Map[i64, Node]) }
fn build(n: i64) -> Store {
    let mut m: Map[i64, Node] = Map.new();
    m.insert(1, Node { val: n });
    m.insert(2, Node { val: n + 1 });
    Store.Full(m)
}
fn main() {
    let mut i: i64 = 0;
    let mut total: i64 = 0;
    while i < 50 {
        let s = build(i);
        total = total + 1;
        i = i + 1;
    }
    println(total.to_string());
}
"#,
            &["50"],
            "shared_enum_map_shared_value_payload_drop_no_leak",
        );
    }

    #[test]
    fn asan_channel_send_recv_heap_payload_no_double_free() {
        // B-2026-07-13-16. `tx.send(v)` for an owned heap payload (`Vec`/`String`)
        // memcpy'd the value's `{ptr,len,cap}` header into the type-erased queue
        // but never neutralized the SOURCE binding's scope-exit free — so the
        // source `v` freed the buffer AND the `recv`'d binding freed the same
        // (aliased) buffer: `free(): double free detected in tcache 2` under
        // JIT/native (the interpreter moves the value into the channel, so it was
        // correct). A string LITERAL source stayed clean by luck (`cap == 0`).
        // `send` is now a MOVE — the source's Vec/String/Map/fstr cleanup is
        // suppressed, so the queue is the sole owner until `recv` transfers to the
        // receiver. This churns balanced send→recv→use of both a `Vec[i64]` and an
        // owned `String` (100 iters each): a missed suppression double-frees
        // (ASAN), a stray extra owner leaks (LSan).
        assert_clean_asan_run(
            r#"
fn main() {
    let mut i: i64 = 0;
    let mut total: i64 = 0;
    while i < 100 {
        let (vtx, vrx): (Sender[Vec[i64]], Receiver[Vec[i64]]) = Channel.new();
        let mut v: Vec[i64] = Vec.new();
        v.push(i);
        v.push(i + 1);
        vtx.send(v);
        let gv = vrx.recv();
        total = total + gv.len();
        let (stx, srx): (Sender[String], Receiver[String]) = Channel.new();
        let s = f"msg-{i}";
        stx.send(s);
        let gs = srx.recv();
        total = total + gs.len();
        i = i + 1;
    }
    println(total.to_string());
}
"#,
            // Vec len 2 × 100 = 200; String "msg-{i}" lengths: i=0..9 → 5 (×10=50),
            // i=10..99 → 6 (×90=540) = 590. Total = 790.
            &["790"],
            "channel_send_recv_heap_payload_no_double_free",
        );
    }

    #[test]
    fn asan_vec_insert_heap_no_double_free() {
        // `Vec[String].insert(idx, value)` MOVES the heap value into the
        // container (the `insert` codegen arm carries push's ownership-
        // suppression set), so the source binding must not also free the buffer.
        // Churns 100× — each iter builds a fresh owned String and inserts it at
        // the front (forcing a full memmove of the growing tail) — so a missed
        // source-cleanup suppression double-frees (ASAN) and a stray extra owner
        // leaks (LSan). The tail memmove also exercises the grow/realloc path.
        assert_clean_asan_run(
            r#"
fn main() {
    let mut v: Vec[String] = Vec.new();
    let mut i: i64 = 0;
    while i < 100 {
        let s = f"item-{i}";
        v.insert(0, s);
        i = i + 1;
    }
    println(v.len().to_string());
}
"#,
            &["100"],
            "vec_insert_heap_no_double_free",
        );
    }

    /// Column heap lifecycle (phase-11 data-science stdlib, Arrow codegen
    /// core slice): each `Column[T]` is a control block + a separate data
    /// buffer + a separate validity bitmap, all freed once at scope exit
    /// via `FreeColumn`'s null-guard (three `free`s). Exercises every
    /// ownership-transfer shape — construction (new / with_capacity /
    /// from_vec, the last with a temporary-Vec eager free), push growth
    /// (realloc of both buffers), `let b = a;` move (source slot nulled —
    /// double-free would trip ASAN), and fn-boundary moves (owned arg +
    /// tail return). Leak detection on Linux (detect_leaks=1) additionally
    /// catches a missing free of the data buffer / bitmap / control block.
    #[test]
    fn asan_column_lifecycle_clean() {
        let label = "column_lifecycle";
        if !asan_available() {
            eprintln!("[{label}] ASAN unavailable on this host — skipping");
            return;
        }
        let Some((stdout, status)) = run_under_asan(
            r#"
fn make() -> Column[i64] {
    let mut c: Column[i64] = Column.new();
    c.push(7);
    c.push_null();
    c.push(9);
    c
}

fn take(c: Column[f64]) -> i64 {
    c.null_count()
}

fn main() {
    // new() + push growth (forces realloc of data + bitmap from cap 0).
    let mut a: Column[i64] = Column.new();
    a.push(1);
    a.push(2);
    a.push_null();
    a.push(4);
    a.push(5);
    println(a.len());
    println(a.null_count());
    // with_capacity (no growth) + indexing.
    let mut w: Column[i64] = Column.with_capacity(8);
    w.push(11);
    match w[0] { Some(v) => println(v), None => println(-1) }
    // from_vec with a temporary Vec arg (eager-free of the source buffer).
    let v: Column[f64] = Column.from_vec([1.0, 2.0, 3.0]);
    println(take(v));
    // let-rebind move (source slot nulled — no double-free).
    let h: Column[i64] = Column.from_vec([8, 9]);
    let k = h;
    println(k.len());
    // fn-return move (tail return owns the control block).
    let m = make();
    println(m.null_count());
}
"#,
            label,
        ) else {
            eprintln!("[{label}] setup failed — skipping");
            return;
        };
        assert!(
            status.success(),
            "[{label}] ASAN reported a memory error (exit code {:?}) — \
             check FreeColumn double-free/leak on the move-suppression paths",
            status.code()
        );
        assert_eq!(
            stdout.trim().lines().collect::<Vec<_>>(),
            vec!["5", "1", "11", "0", "2", "1"],
            "[{label}] unexpected stdout (ASAN passed, output mismatched)"
        );
    }

    /// DataFrame heap lifecycle (phase-11 Arrow Q6 codegen): `insert`
    /// copies the argument column *in* (freeing a fresh-temp original) and
    /// grows the entries buffer from cap 0; a same-name `insert` replaces
    /// (frees the old column); `column` copies *out* a fresh independent
    /// column (its own `FreeColumn`); the frame is moved (`let df2 = df`,
    /// source slot nulled — the `FreeDataFrame` drop runs once); and the
    /// `FreeDataFrame` drop loop frees every column (data + bitmap +
    /// control) + name buffer, then the entries buffer + control. A
    /// missing free leaks (Linux detect_leaks); a double free is caught
    /// everywhere.
    #[test]
    fn asan_dataframe_lifecycle_clean() {
        let label = "dataframe_lifecycle";
        if !asan_available() {
            eprintln!("[{label}] ASAN unavailable on this host — skipping");
            return;
        }
        let Some((stdout, status)) = run_under_asan(
            r#"
fn main() {
    let mut df: DataFrame = DataFrame.new();
    df.insert("age", Column.from_vec([30, 25, 40]));
    df.insert("score", Column.from_vec([1.5, 2.5, 3.5]));
    // Replace an existing column (frees the old column's allocations).
    df.insert("age", Column.from_vec([31, 26, 41]));
    println(df.width());
    println(df.height());
    // Copy-out: a fresh independent column, mutated, then dropped.
    let mut a: Column[i64] = df.column("age");
    a.push(99);
    println(a.len());
    println(df.height());
    // Move the frame (source slot nulled — drop runs exactly once).
    let df2: DataFrame = df;
    println(df2.width());
}
"#,
            label,
        ) else {
            eprintln!("[{label}] setup failed — skipping");
            return;
        };
        assert!(
            status.success(),
            "[{label}] ASAN reported a memory error (exit code {:?}) — \
             check FreeDataFrame drop loop + insert copy-in / replace frees",
            status.code()
        );
        assert_eq!(
            stdout.trim().lines().collect::<Vec<_>>(),
            vec!["2", "3", "4", "3", "2"],
            "[{label}] unexpected stdout (ASAN passed, output mismatched)"
        );
    }

    /// DataFrame `column_names` / `select` heap lifecycle (phase-11 Arrow
    /// Q6 codegen, slice 2c): `column_names` mallocs a fresh `Vec[String]`
    /// whose elements are independent name copies (freed by the Vec's own
    /// drop, never the frame's name buffers); `select` mallocs a fresh
    /// frame holding column copies (its own `FreeDataFrame` drop). The
    /// `cols` literal arg is freed by the caller's owned-temp drop (freeing
    /// it in `select` would double-free). A missing free leaks (Linux
    /// detect_leaks); a double free is caught everywhere.
    #[test]
    fn asan_dataframe_column_names_select_clean() {
        let label = "dataframe_names_select";
        if !asan_available() {
            eprintln!("[{label}] ASAN unavailable on this host — skipping");
            return;
        }
        let Some((stdout, status)) = run_under_asan(
            r#"
fn main() {
    let mut df: DataFrame = DataFrame.new();
    df.insert("a", Column.from_vec([1, 2]));
    df.insert("b", Column.from_vec([3, 4]));
    let names: Vec[String] = df.column_names();
    println(names.len());
    let sub: DataFrame = df.select(["b", "a"]);
    println(sub.width());
    let col: Column[i64] = sub.column("b");
    println(col.len());
    println(df.width());
}
"#,
            label,
        ) else {
            eprintln!("[{label}] setup failed — skipping");
            return;
        };
        assert!(
            status.success(),
            "[{label}] ASAN reported a memory error (exit code {:?}) — \
             check column_names Vec[String] drop + select fresh-frame drop",
            status.code()
        );
        assert_eq!(
            stdout.trim().lines().collect::<Vec<_>>(),
            vec!["2", "2", "2", "2"],
            "[{label}] unexpected stdout (ASAN passed, output mismatched)"
        );
    }

    /// `DataFrame` holding a `Column[String]` heap lifecycle (phase-11
    /// DataFrame-String integration). A String column inside a frame must
    /// keep value semantics with independent String heaps: `insert`
    /// deep-clones the column's strings IN (a fresh-temp original is fully
    /// freed incl. its strings; an identifier source keeps its own drop),
    /// `column(name)` deep-clones OUT, `select` deep-clones into the new
    /// frame, `insert`-replace frees the old column's strings, and the
    /// frame drop frees every column's per-element strings (`elem_size == 24`
    /// runtime branch). A shared heap (memcpy without re-clone) double-frees;
    /// a missing per-element free leaks (Linux detect_leaks). Long payloads
    /// (>= 23 bytes) force real heap allocation.
    #[test]
    fn asan_dataframe_string_column_lifecycle_clean() {
        let label = "dataframe_string_column";
        if !asan_available() {
            eprintln!("[{label}] ASAN unavailable on this host — skipping");
            return;
        }
        let Some((stdout, status)) = run_under_asan(
            r#"
fn main() {
    let mut df: DataFrame = DataFrame.new();
    // insert from a fresh-temp Column[String] (from_vec of an identifier Vec).
    let names: Vec[String] = ["alpha_padding_aaaaaaaaaaaaaaa", "beta_padding_bbbbbbbbbbbbbbbb"];
    df.insert("name", Column.from_vec(names));
    df.insert("age", Column.from_vec([20, 30]));
    // copy a String column OUT (independent clone; dropped after use).
    let back: Column[String] = df.column("name");
    for s in back.iter_valid() { println(s.len()); }
    // select reorders both a numeric and a String column into a fresh frame.
    let sub: DataFrame = df.select(["age", "name"]);
    let sn: Column[String] = sub.column("name");
    println(sn.valid_count());
    // replace the String column (frees the old column's strings).
    let repl: Vec[String] = ["gamma_padding_ccccccccccccccc", "delta_padding_ddddddddddddddd"];
    df.insert("name", Column.from_vec(repl));
    let back2: Column[String] = df.column("name");
    for s in back2.iter_valid() { println(s.len()); }
    println(df.width());
}
"#,
            label,
        ) else {
            eprintln!("[{label}] setup failed — skipping");
            return;
        };
        assert!(
            status.success(),
            "[{label}] ASAN reported a memory error (exit code {:?}) — \
             check deep_copy String re-clone / column_free_allocations String drain / replace",
            status.code()
        );
        assert_eq!(
            stdout.trim().lines().collect::<Vec<_>>(),
            vec!["29", "29", "2", "29", "29", "2"],
            "[{label}] unexpected stdout (ASAN passed, output mismatched)"
        );
    }

    /// `DataFrame.describe()` heap lifecycle (phase-11 describe codegen). The
    /// result is a fresh frame: a `statistic` `Column[String]` of static
    /// labels (`cap == 0`, so drop skips them — no rodata free) and one
    /// `Column[f64]` per numeric source column (fresh control / data / bitmap,
    /// freed by the result frame's `FreeDataFrame`). Each stats column
    /// allocates and frees an f64 scratch buffer. The source frame is borrowed
    /// (its own drop unaffected). A missing free leaks (Linux detect_leaks);
    /// the scratch buffer or a stats column freed twice trips ASAN. Reuse of
    /// the source frame after describe pins it wasn't consumed.
    #[test]
    fn asan_dataframe_describe_lifecycle_clean() {
        let label = "dataframe_describe_lifecycle";
        if !asan_available() {
            eprintln!("[{label}] ASAN unavailable on this host — skipping");
            return;
        }
        let Some((stdout, status)) = run_under_asan(
            r#"
fn main() {
    let mut df: DataFrame = DataFrame.new();
    df.insert("age", Column.from_vec([20, 30, 40, 50]));
    // a String column (skipped by describe) + a null-bearing float column.
    let names: Vec[String] = ["alpha_padding_aaaaaaaaaaaaaaa", "beta_padding_bbbbbbbbbbbbbbbb", "gamma_padding_ccccccccccccccc", "delta_padding_ddddddddddddddd"];
    df.insert("name", Column.from_vec(names));
    let score: Column[f64] = Column.from_iter_nullable([Some(1.0), None, Some(3.0), Some(5.0)]);
    df.insert("score", score);
    // describe builds a fresh frame (statistic + age + score).
    let d: DataFrame = df.describe();
    println(d.width());
    println(d.height());
    let lab: Column[String] = d.column("statistic");
    println(lab.valid_count());
    let a: Column[f64] = d.column("age");
    for v in a.iter_valid() { println(v); }
    // source frame still usable (borrowed by describe, not consumed).
    println(df.width());
}
"#,
            label,
        ) else {
            eprintln!("[{label}] setup failed — skipping");
            return;
        };
        assert!(
            status.success(),
            "[{label}] ASAN reported a memory error (exit code {:?}) — \
             check describe fresh-frame build / static-label cap=0 / f64 scratch free",
            status.code()
        );
        assert_eq!(
            stdout.trim().lines().collect::<Vec<_>>(),
            vec![
                "3",
                "8",
                "8",
                "4",
                "35",
                "12.909944487358056",
                "20",
                "27.5",
                "35",
                "42.5",
                "50",
                "3"
            ],
            "[{label}] unexpected stdout (ASAN passed, output mismatched)"
        );
    }

    /// Column transform heap lifecycle (phase-11 follow-on slice): the
    /// Column-returning transforms `fillna` / `dropna` and the
    /// `from_iter_nullable` constructor each malloc a *fresh* control
    /// block + data buffer + bitmap; the receiver is borrowed (keeps its
    /// own `FreeColumn`), and the fresh result is freed once via the
    /// let-binding's `FreeColumn`. A missing free leaks (Linux
    /// detect_leaks); a wrong free double-frees (caught everywhere).
    /// Receiver reuse after the transforms pins it wasn't consumed.
    #[test]
    fn asan_column_transforms_lifecycle_clean() {
        let label = "column_transforms_lifecycle";
        if !asan_available() {
            eprintln!("[{label}] ASAN unavailable on this host — skipping");
            return;
        }
        let Some((stdout, status)) = run_under_asan(
            r#"
fn main() {
    let mut c: Column[i64] = Column.new();
    c.push(10);
    c.push_null();
    c.push(30);
    c.push_null();
    let f = c.fillna(99);
    println(f.len());
    let d = c.dropna();
    println(d.len());
    // receiver still usable (borrowed, not consumed).
    println(c.null_count());
    let opts: Vec[Option[i64]] = [Some(1), None, Some(3)];
    let e: Column[i64] = Column.from_iter_nullable(opts);
    println(e.null_count());
    match e[2] { Some(v) => println(v), None => println(-1) }
}
"#,
            label,
        ) else {
            eprintln!("[{label}] setup failed — skipping");
            return;
        };
        assert!(
            status.success(),
            "[{label}] ASAN reported a memory error (exit code {:?}) — \
             check FreeColumn on fresh transform results / receiver-borrow",
            status.code()
        );
        assert_eq!(
            stdout.trim().lines().collect::<Vec<_>>(),
            vec!["4", "2", "2", "1", "3"],
            "[{label}] unexpected stdout (ASAN passed, output mismatched)"
        );
    }

    /// Column `fillna(value, treat_nan_as_null)` float-NaN normalization heap
    /// lifecycle (phase-11 follow-on): each `fillna` mallocs a fresh control
    /// block + data buffer + all-ones bitmap, freed once via the let-binding's
    /// `FreeColumn`; the float receiver is borrowed and reused after both
    /// transforms. The NaN-normalizing arm doesn't change the allocation
    /// shape, so this pins the float fill loop frees cleanly too.
    #[test]
    fn asan_column_fillna_nan_lifecycle_clean() {
        let label = "column_fillna_nan_lifecycle";
        if !asan_available() {
            eprintln!("[{label}] ASAN unavailable on this host — skipping");
            return;
        }
        let Some((stdout, status)) = run_under_asan(
            r#"
fn main() {
    let z: f64 = 0.0;
    let nan: f64 = z / z;
    let mut c: Column[f64] = Column.new();
    c.push(1.5);
    c.push_null();
    c.push(nan);
    c.push(4.0);
    let a = c.fillna(0.0);
    println(a.null_count());
    let b = c.fillna(0.0, treat_nan_as_null: true);
    println(b.null_count());
    // receiver still usable (borrowed, not consumed).
    println(c.null_count());
}
"#,
            label,
        ) else {
            eprintln!("[{label}] setup failed — skipping");
            return;
        };
        assert!(
            status.success(),
            "[{label}] ASAN reported a memory error (exit code {:?}) — \
             check FreeColumn on the fresh fillna results / receiver-borrow",
            status.code()
        );
        assert_eq!(
            stdout.trim().lines().collect::<Vec<_>>(),
            vec!["0", "0", "1"],
            "[{label}] unexpected stdout (ASAN passed, output mismatched)"
        );
    }

    /// Column Vec-returning iterators heap lifecycle (phase-11 follow-on):
    /// `iter() -> Vec[Option[T]]` and `iter_valid() -> Vec[T]` each malloc
    /// a fresh Vec buffer (POD elements — no per-element drop), freed once
    /// via the result binding's `FreeVecBuffer` / the for-loop's owned-temp
    /// materialization. The source column is borrowed (keeps its own
    /// `FreeColumn`). Both the let-bound and direct-for-source forms run;
    /// a missing free leaks (Linux detect_leaks), a wrong free double-frees.
    #[test]
    fn asan_column_iter_lifecycle_clean() {
        let label = "column_iter_lifecycle";
        if !asan_available() {
            eprintln!("[{label}] ASAN unavailable on this host — skipping");
            return;
        }
        let Some((stdout, status)) = run_under_asan(
            r#"
fn main() {
    let mut c: Column[i64] = Column.new();
    c.push(10);
    c.push_null();
    c.push(30);
    let all: Vec[Option[i64]] = c.iter();
    println(all.len());
    let mut sum = 0;
    for o in all { match o { Some(v) => { sum = sum + v; }, None => { sum = sum - 1; } } }
    println(sum);
    let valid: Vec[i64] = c.iter_valid();
    println(valid.len());
    let mut vs = 0;
    for x in c.iter_valid() { vs = vs + x; }
    println(vs);
    // source column still usable (borrowed, not consumed).
    println(c.null_count());
}
"#,
            label,
        ) else {
            eprintln!("[{label}] setup failed — skipping");
            return;
        };
        assert!(
            status.success(),
            "[{label}] ASAN reported a memory error (exit code {:?}) — \
             check FreeVecBuffer on the iter results / column borrow",
            status.code()
        );
        assert_eq!(
            stdout.trim().lines().collect::<Vec<_>>(),
            vec!["3", "39", "2", "40", "1"],
            "[{label}] unexpected stdout (ASAN passed, output mismatched)"
        );
    }

    /// Column 3VL-arithmetic heap lifecycle (phase-11 follow-on): every
    /// element-wise `+ - * /` / comparison / unary `-` mallocs a fresh
    /// result column; operands are borrowed (keep their own `FreeColumn`),
    /// and a fresh-temp intermediate in `a + b + c` / `-a` chains is freed
    /// after the copy (`column_free_if_fresh_temp`) — a missing free leaks
    /// (Linux detect_leaks), a wrong free double-frees. Operand reuse after
    /// the ops pins that nothing was wrongly consumed.
    #[test]
    fn asan_column_arithmetic_lifecycle_clean() {
        let label = "column_arithmetic_lifecycle";
        if !asan_available() {
            eprintln!("[{label}] ASAN unavailable on this host — skipping");
            return;
        }
        let Some((stdout, status)) = run_under_asan(
            r#"
fn fst(c: Column[i64], i: i64) -> i64 { match c[i] { Some(v) => v, None => -1 } }
fn main() {
    let mut a: Column[i64] = Column.new();
    a.push(10); a.push_null(); a.push(30);
    let mut b: Column[i64] = Column.new();
    b.push(1); b.push(2); b.push(3);
    // chained col-col: a + b + b — the (a + b) intermediate is a fresh
    // temp freed after the second op.
    let s = a + b + b;
    println(fst(s, 2));
    // col-scalar + unary neg chain.
    let m = -(a * 2);
    println(fst(m, 0));
    // comparison -> fresh Column[bool].
    let eq = a == b;
    println(eq.null_count());
    // operands still usable (borrowed, not consumed).
    println(a.null_count());
    println(b.len());
}
"#,
            label,
        ) else {
            eprintln!("[{label}] setup failed — skipping");
            return;
        };
        assert!(
            status.success(),
            "[{label}] ASAN reported a memory error (exit code {:?}) — \
             check FreeColumn on fresh 3VL results / fresh-temp operand free",
            status.code()
        );
        assert_eq!(
            stdout.trim().lines().collect::<Vec<_>>(),
            vec!["36", "-20", "1", "1", "3"],
            "[{label}] unexpected stdout (ASAN passed, output mismatched)"
        );
    }

    /// Column statistical reductions heap lifecycle (phase-11 stats codegen
    /// slice): the scalar reductions (`sum`/`mean`/`var`/`std`/`min`/`max`)
    /// allocate no heap and only read the column's buffers, so the column
    /// stays intact and is freed once at scope exit. `corr`'s argument may be
    /// a *fresh-temp* column (`a.corr(a + a)`) — the temporary is freed after
    /// the read via `column_free_if_fresh_temp` (a missing free leaks on
    /// Linux detect_leaks; a wrong free double-frees). The receiver stays
    /// borrowed and usable after every call.
    #[test]
    fn asan_column_stats_lifecycle_clean() {
        let label = "column_stats_lifecycle";
        if !asan_available() {
            eprintln!("[{label}] ASAN unavailable on this host — skipping");
            return;
        }
        let Some((stdout, status)) = run_under_asan(
            r#"
fn main() {
    let a: Column[f64] = Column.from_vec([2.0, 4.0, 6.0]);
    // Scalar reductions allocate no heap; the column is untouched.
    println(a.sum());
    println(a.mean());
    println(a.var());
    println(a.std());
    // corr with a fresh-temp argument (a + a) — the temporary column is
    // freed after the read; `a` is borrowed.
    println(a.corr(a + a));
    // a still usable after all of the above (borrowed, not consumed).
    println(a.min());
    println(a.max());
}
"#,
            label,
        ) else {
            eprintln!("[{label}] setup failed — skipping");
            return;
        };
        assert!(
            status.success(),
            "[{label}] ASAN reported a memory error (exit code {:?}) — \
             check corr's fresh-temp arg free / reductions not freeing the receiver",
            status.code()
        );
        assert_eq!(
            stdout.trim().lines().collect::<Vec<_>>(),
            vec!["12", "4", "4", "2", "1", "2", "6"],
            "[{label}] unexpected stdout (ASAN passed, output mismatched)"
        );
    }

    /// Column `median` / `quantile` heap lifecycle (phase-11 stats codegen
    /// slice 3): each call mallocs a fresh `f64` scratch buffer, sorts it
    /// in place, reads the interpolated result, then frees the buffer — a
    /// missing free leaks (Linux detect_leaks), a double-free or read past
    /// the free trips ASAN. Several calls in a row on the same (borrowed,
    /// untouched) column pin allocate-sort-free balance across iterations.
    #[test]
    fn asan_column_median_quantile_lifecycle_clean() {
        let label = "column_median_quantile_lifecycle";
        if !asan_available() {
            eprintln!("[{label}] ASAN unavailable on this host — skipping");
            return;
        }
        let Some((stdout, status)) = run_under_asan(
            r#"
fn main() {
    let c: Column[f64] = Column.from_vec([4.0, 1.0, 3.0, 2.0, 5.0]);
    // Repeated median/quantile — each mallocs + frees its own scratch buffer.
    println(c.median());
    println(c.quantile(0.25));
    println(c.quantile(0.75));
    // Null-skipping median over an integer column (separate buffer).
    let o: Column[i64] = Column.from_iter_nullable([Some(9), None, Some(1), Some(5)]);
    println(o.median());
    // c still usable afterward (borrowed, not consumed).
    println(c.len());
}
"#,
            label,
        ) else {
            eprintln!("[{label}] setup failed — skipping");
            return;
        };
        assert!(
            status.success(),
            "[{label}] ASAN reported a memory error (exit code {:?}) — \
             check the median/quantile sort-buffer malloc/free balance",
            status.code()
        );
        assert_eq!(
            stdout.trim().lines().collect::<Vec<_>>(),
            vec!["3", "2", "4", "5", "5"],
            "[{label}] unexpected stdout (ASAN passed, output mismatched)"
        );
    }

    /// `Stats.*` free-function codegen lifecycle (phase-11). `Stats.median`
    /// mallocs + frees a scratch f64 buffer per call (memcpy-sort-read-free);
    /// a fresh `vec![…]` temp argument is read and then freed via
    /// `materialize_owned_temp` (the early-dispatch owned-temp leak guard —
    /// `builtin-method-early-dispatch-skips-owned-temp-arg-free`). A borrowed
    /// `Vec` argument is NOT consumed (still usable after). A missing free
    /// leaks (Linux detect_leaks); a double free trips ASAN everywhere.
    #[test]
    fn asan_stats_free_functions_lifecycle_clean() {
        let label = "stats_free_functions_lifecycle";
        if !asan_available() {
            eprintln!("[{label}] ASAN unavailable on this host — skipping");
            return;
        }
        let Some((stdout, status)) = run_under_asan(
            r#"
fn main() {
    let v: Vec[f64] = vec![4.0, 1.0, 3.0, 2.0, 5.0];
    // median mallocs/frees a scratch buffer; the others read in place.
    println(Stats.median(v));
    println(Stats.mean(v));
    println(Stats.stddev(v));
    // v borrowed, not consumed — still usable.
    println(Stats.sum(v));
    // fresh vec![…] temp argument: read then freed (no leak).
    println(Stats.median(vec![30.0, 10.0, 20.0, 40.0]));
}
"#,
            label,
        ) else {
            eprintln!("[{label}] setup failed — skipping");
            return;
        };
        assert!(
            status.success(),
            "[{label}] ASAN reported a memory error (exit code {:?}) — \
             check the median scratch malloc/free + fresh-temp arg free",
            status.code()
        );
        assert_eq!(
            stdout.trim().lines().collect::<Vec<_>>(),
            vec!["3", "3", "1.4142135623730951", "15", "25"],
            "[{label}] unexpected stdout (ASAN passed, output mismatched)"
        );
    }

    /// `Stats.percentile`/`sort`/`argsort` codegen lifecycle (phase-11).
    /// `percentile` mallocs + frees an f64 scratch (memcpy-sort-read-free).
    /// `sort` / `argsort` each malloc a buffer and hand it back as an OWNED
    /// `Vec` whose `let`-binding frees it at scope exit — a missing free leaks
    /// (Linux detect_leaks), a double free trips ASAN. The source `Vec` is
    /// borrowed (still usable after), and a fresh `vec![…]` temp argument is
    /// freed via `materialize_owned_temp`.
    #[test]
    fn asan_stats_methods_lifecycle_clean() {
        let label = "stats_methods_lifecycle";
        if !asan_available() {
            eprintln!("[{label}] ASAN unavailable on this host — skipping");
            return;
        }
        let Some((stdout, status)) = run_under_asan(
            r#"
fn main() {
    let v: Vec[f64] = vec![40.0, 10.0, 30.0, 20.0, 50.0];
    println(Stats.percentile(v, 50.0));
    let s: Vec[f64] = Stats.sort(v);
    println(s[0]);
    println(s[4]);
    let a: Vec[i64] = Stats.argsort(v);
    println(a[0]);
    println(a[4]);
    // fresh vec![…] temp argument: read then freed (no leak).
    let s2: Vec[f64] = Stats.sort(vec![3.0, 1.0, 2.0]);
    println(s2[0]);
}
"#,
            label,
        ) else {
            eprintln!("[{label}] setup failed — skipping");
            return;
        };
        assert!(
            status.success(),
            "[{label}] ASAN reported a memory error (exit code {:?}) — \
             check the percentile scratch + sort/argsort owned-Vec free",
            status.code()
        );
        assert_eq!(
            stdout.trim().lines().collect::<Vec<_>>(),
            vec!["30", "10", "50", "1", "4", "1"],
            "[{label}] unexpected stdout (ASAN passed, output mismatched)"
        );
    }

    /// `Column[String]` heap-element lifecycle (phase-11 Column[String]
    /// codegen slice): each String column owns its element heaps. `from_vec`
    /// deep-clones the source Vec's strings IN (the source Vec is borrowed and
    /// drops its own); `iter_valid` deep-clones them OUT into a fresh
    /// `Vec[String]` (which drops its own); the `FreeColumn` drain frees every
    /// valid slot's String (cap-guarded) before the buffers; a move
    /// (`let d = c`) nulls the source slot so only the new owner frees. A
    /// missing free leaks (Linux detect_leaks), a double free / use-after-free
    /// trips ASAN everywhere. The moved column's reuse pins that the move
    /// transferred ownership cleanly (no double free of the shared heaps).
    #[test]
    fn asan_column_string_lifecycle_clean() {
        let label = "column_string_lifecycle";
        if !asan_available() {
            eprintln!("[{label}] ASAN unavailable on this host — skipping");
            return;
        }
        let Some((stdout, status)) = run_under_asan(
            r#"
fn main() {
    // Long payloads (>= 23 bytes) force real heap allocation (LSan misses
    // short reachable strings) — see lsan-reachability-short-string-leaks.
    // The source Vec is moved into from_vec (ownership), but its scope drop
    // still frees its own element strings; from_vec deep-clones independent
    // copies into the column, so the two never share a heap.
    let v: Vec[String] = ["alpha_aaaaaaaaaaaaaaaaaaaaaaaa", "beta_bbbbbbbbbbbbbbbbbbbbbbbbb", "gamma_ccccccccccccccccccccccc"];
    let c: Column[String] = Column.from_vec(v);
    // iter_valid clones out into a fresh Vec[String] (dropped after the loop).
    for s in c.iter_valid() { println(s.len()); }
    // Move the column: `d` owns the heaps, `c`'s FreeColumn is suppressed.
    let d = c;
    println(d.valid_count());
    for s in d.iter_valid() { println(s.len()); }
}
"#,
            label,
        ) else {
            eprintln!("[{label}] setup failed — skipping");
            return;
        };
        assert!(
            status.success(),
            "[{label}] ASAN reported a memory error (exit code {:?}) — \
             check from_vec clone-in / iter_valid clone-out / FreeColumn String drain / move",
            status.code()
        );
        assert_eq!(
            stdout.trim().lines().collect::<Vec<_>>(),
            vec!["30", "30", "29", "3", "30", "30", "29"],
            "[{label}] unexpected stdout (ASAN passed, output mismatched)"
        );
    }

    /// Tensor element-wise arithmetic heap lifecycle (phase-11 line 47):
    /// every `+ - * /` / unary `-` mallocs a fresh result; operands are
    /// borrowed (keep their own `FreeTensor`); a fresh-temp intermediate in
    /// `a + b + c` / `(a + b) * (b + c)` / `-a + b` is freed after the copy
    /// (the `tensor_operand_is_owned_fresh_temp` path) — a missing free leaks
    /// (Linux detect_leaks), a wrong free double-frees (caught everywhere).
    /// Operand reuse after the ops pins that nothing was wrongly consumed.
    #[test]
    fn asan_tensor_arithmetic_lifecycle_clean() {
        let label = "tensor_arithmetic_lifecycle";
        if !asan_available() {
            eprintln!("[{label}] ASAN unavailable on this host — skipping");
            return;
        }
        let Some((stdout, status)) = run_under_asan(
            r#"
fn main() {
    let a: Tensor[i64, [3]] = Tensor.from([1, 2, 3]);
    let b: Tensor[i64, [3]] = Tensor.from([10, 20, 30]);
    let c: Tensor[i64, [3]] = Tensor.from([100, 200, 300]);
    let r = a + b + c;
    println(r[0]);
    let r2 = (a + b) * (b + c);
    println(r2[1]);
    let n = -a + b;
    println(n[0]);
    let s = a + 5;
    println(s[2]);
    let m = a * 3 - b;
    println(m[1]);
    println(a[0]);
    println(b[2]);
}
"#,
            label,
        ) else {
            eprintln!("[{label}] setup failed — skipping");
            return;
        };
        assert!(
            status.success(),
            "[{label}] ASAN reported a memory error (exit code {:?}) — \
             check the fresh-temp operand free / FreeTensor double-free paths",
            status.code()
        );
        assert_eq!(
            stdout.trim().lines().collect::<Vec<_>>(),
            vec!["111", "4840", "9", "8", "-14", "1", "30"],
            "[{label}] unexpected stdout (ASAN passed, output mismatched)"
        );
    }

    /// Tensor broadcasting heap lifecycle (phase-11 "Explicit broadcasting
    /// methods"). Each `broadcast_*` mallocs a fresh result block the
    /// let-binding `FreeTensor` reclaims; the identifier receiver and an
    /// identifier argument are borrowed (keep their own frees); a fresh-temp
    /// argument (`one + one`) is freed after the copy (the
    /// `tensor_operand_is_owned_fresh_temp` path) — a missing free leaks
    /// (Linux detect_leaks), a wrong free double-frees (caught everywhere).
    /// Receiver + argument reuse after the ops pins borrow-not-move.
    #[test]
    fn asan_tensor_broadcast_lifecycle_clean() {
        let label = "tensor_broadcast_lifecycle";
        if !asan_available() {
            eprintln!("[{label}] ASAN unavailable on this host — skipping");
            return;
        }
        let Some((stdout, status)) = run_under_asan(
            r#"
fn main() {
    let m: Tensor[i64, [2, 3]] = Tensor.from([[1, 2, 3], [4, 5, 6]]);
    let row: Tensor[i64, [1, 3]] = Tensor.from([[10, 20, 30]]);
    let r = m.broadcast_add(row);
    println(r[1, 2]);
    let col: Tensor[i64, [2, 1]] = Tensor.from([[100], [200]]);
    let c = m.broadcast_mul(col);
    println(c[1, 0]);
    let one: Tensor[i64, [1, 3]] = Tensor.from([[1, 1, 1]]);
    let h = m.broadcast_add(one + one);
    println(h[0, 0]);
    println(m[0, 0]);
    println(row[0, 1]);
}
"#,
            label,
        ) else {
            eprintln!("[{label}] setup failed — skipping");
            return;
        };
        assert!(
            status.success(),
            "[{label}] ASAN reported a memory error (exit code {:?}) — \
             check the broadcast result FreeTensor / fresh-temp-arg free paths",
            status.code()
        );
        assert_eq!(
            stdout.trim().lines().collect::<Vec<_>>(),
            vec!["36", "800", "3", "1", "20"],
            "[{label}] unexpected stdout (ASAN passed, output mismatched)"
        );
    }

    /// Tensor reduction heap lifecycle (phase-11 line 47, Slice B). Full
    /// reduces return a scalar (no malloc); axis reduces malloc a fresh
    /// rank-1-lower block that the let-binding `FreeTensor` must reclaim. A
    /// chained `m.sum_axis(0)` on a let-bound axis-reduce result and receiver
    /// reuse after the reduces pin that nothing is double-freed or read after
    /// free.
    #[test]
    fn asan_tensor_reduce_lifecycle_clean() {
        let label = "tensor_reduce_lifecycle";
        if !asan_available() {
            eprintln!("[{label}] ASAN unavailable on this host — skipping");
            return;
        }
        let Some((stdout, status)) = run_under_asan(
            r#"
fn main() {
    let a: Tensor[i64, [2, 3]] = Tensor.from([[1, 2, 3], [4, 5, 6]]);
    println(a.sum());
    println(a.max());
    let s0 = a.sum_axis(0);
    println(s0[1]);
    let s1 = a.sum_axis(1);
    println(s1[0]);
    let m = a.mean_axis(0);
    println(m[2]);
    let chained = m.sum_axis(0);
    println(chained);
    println(a[1, 2]);
}
"#,
            label,
        ) else {
            eprintln!("[{label}] setup failed — skipping");
            return;
        };
        assert!(
            status.success(),
            "[{label}] ASAN reported a memory error (exit code {:?}) — \
             check the axis-reduce result FreeTensor / double-free paths",
            status.code()
        );
        assert_eq!(
            stdout.trim().lines().collect::<Vec<_>>(),
            vec!["21", "6", "7", "6", "4.5", "10.5", "6"],
            "[{label}] unexpected stdout (ASAN passed, output mismatched)"
        );
    }

    /// Tensor shape-transform heap lifecycle (phase-11 follow-on slice):
    /// reshape / permute / slice / squeeze each malloc a fresh result
    /// block and copy the data; the receiver is borrowed (keeps its own
    /// `FreeTensor`). A chained `permute(..).reshape(..)` additionally
    /// exercises the fresh-temporary free of the intermediate (the
    /// `receiver_is_fresh_temp` path) — a missing free leaks (Linux
    /// detect_leaks), a wrong free double-frees (caught everywhere).
    #[test]
    fn asan_tensor_shape_transform_lifecycle_clean() {
        let label = "tensor_shape_transform_lifecycle";
        if !asan_available() {
            eprintln!("[{label}] ASAN unavailable on this host — skipping");
            return;
        }
        let Some((stdout, status)) = run_under_asan(
            r#"
fn main() {
    let a = Tensor.from([[1, 2, 3], [4, 5, 6]]);
    let r = a.reshape([3, 2]);
    println(r[2, 1]);
    let p = a.permute([1, 0]);
    println(p[2, 1]);
    let sl = a.slice(1, 1, 3);
    println(sl[1, 1]);
    let b = Tensor.from([[[7], [8], [9]]]);
    let sq = b.squeeze();
    println(sq[2]);
    let chained = a.permute([1, 0]).reshape([6]);
    println(chained[5]);
}
"#,
            label,
        ) else {
            eprintln!("[{label}] setup failed — skipping");
            return;
        };
        assert!(
            status.success(),
            "[{label}] ASAN reported a memory error (exit code {:?}) — \
             check the fresh-result free and the chained-intermediate free",
            status.code()
        );
        assert_eq!(
            stdout.trim().lines().collect::<Vec<_>>(),
            vec!["6", "6", "6", "9", "6"],
            "[{label}] unexpected stdout (ASAN passed, output mismatched)"
        );
    }

    /// `iter_axis` Vec[Tensor] heap lifecycle (phase-11 follow-on slice):
    /// the result `Vec` holds a buffer of tensor `ptr`s, each a separate
    /// `[rank][dims][data]` block. The `Vec[Tensor]` cleanup
    /// (`track_vec_of_tensors_var` → `cleanup.tdrop`) must free every
    /// element block and the outer buffer exactly once — a missing free
    /// leaks (Linux detect_leaks), a double free trips ASAN everywhere.
    /// Exercises the `let`-bound result (indexed) and the for-loop
    /// method-source materialization (which queues the synth temp's
    /// cleanup), plus the rank-1 `Vec[T]` form (a plain buffer).
    #[test]
    fn asan_tensor_iter_axis_lifecycle_clean() {
        let label = "tensor_iter_axis_lifecycle";
        if !asan_available() {
            eprintln!("[{label}] ASAN unavailable on this host — skipping");
            return;
        }
        let Some((stdout, status)) = run_under_asan(
            r#"
fn main() {
    let a = Tensor.from([[1, 2, 3], [4, 5, 6]]);
    let rows = a.iter_axis(0);
    println(rows.len());
    println(rows[1][2]);
    for c in a.iter_axis(1) {
        println(c[0]);
    }
    let v = Tensor.from([10, 20, 30]);
    let scal = v.iter_axis(0);
    println(scal[2]);
}
"#,
            label,
        ) else {
            eprintln!("[{label}] setup failed — skipping");
            return;
        };
        assert!(
            status.success(),
            "[{label}] ASAN reported a memory error (exit code {:?}) — \
             check the Vec[Tensor] element drop (track_vec_of_tensors_var)",
            status.code()
        );
        assert_eq!(
            stdout.trim().lines().collect::<Vec<_>>(),
            vec!["2", "6", "1", "2", "3", "30"],
            "[{label}] unexpected stdout (ASAN passed, output mismatched)"
        );
    }

    /// Owned fn-return / method-return receiver of a shape transform
    /// (phase-11 line 39). `make().reshape(..)` and `f.build().slice(..)`
    /// each produce a fresh OWNED tensor temporary that the transform
    /// copies out of and must then free exactly once
    /// (`tensor_receiver_is_owned_fresh_temp` → the `receiver_is_fresh_temp`
    /// free). A missing free leaks the intermediate (Linux detect_leaks);
    /// a free applied to a *borrowed* receiver, or a double free of the
    /// result, trips ASAN everywhere. The identifier receivers in the
    /// other tensor ASAN tests pin the negative (don't-free) side; this
    /// pins the fresh-temp positive side for both the free-fn and method
    /// return sources.
    #[test]
    fn asan_tensor_fnret_receiver_free_clean() {
        let label = "tensor_fnret_receiver_free";
        if !asan_available() {
            eprintln!("[{label}] ASAN unavailable on this host — skipping");
            return;
        }
        let Some((stdout, status)) = run_under_asan(
            r#"
fn make() -> Tensor[i64, [2, 3]] {
    Tensor.from([[1, 2, 3], [4, 5, 6]])
}
struct Factory {}
impl Factory {
    fn build(ref self) -> Tensor[i64, [2, 3]] {
        Tensor.from([[10, 20, 30], [40, 50, 60]])
    }
}
fn main() {
    let r = make().reshape([3, 2]);
    println(r[2, 1]);
    let f = Factory {};
    let m = f.build().slice(0, 1, 2);
    println(m[0, 2]);
    let p = make().permute([1, 0]);
    println(p[2, 1]);
    let sq = make().squeeze();
    println(sq[1, 2]);
}
"#,
            label,
        ) else {
            eprintln!("[{label}] setup failed — skipping");
            return;
        };
        assert!(
            status.success(),
            "[{label}] ASAN reported a memory error (exit code {:?}) — \
             check the fresh fn-return/method-return receiver free",
            status.code()
        );
        assert_eq!(
            stdout.trim().lines().collect::<Vec<_>>(),
            vec!["6", "60", "6", "6"],
            "[{label}] unexpected stdout (ASAN passed, output mismatched)"
        );
    }

    /// `ref Tensor` returns are BORROWS — the by-value ref ABI hands back
    /// the owner's block pointer, so the borrow must never be freed
    /// (phase-11 line 40). Exercises the free-fn return (inline transform
    /// receiver + let-bound) and a user `-> ref Tensor` accessor method
    /// (inline transform receiver + let-bound). The ordering is adversarial:
    /// `h.view().permute(..)` (an inline borrow receiver) is followed by a
    /// later `h.view()` and `a[0,0]` — if a borrow receiver were wrongly
    /// freed (the chained-method span-collision hazard), the later reads
    /// would be use-after-free (ASAN) and the owner's scope-exit drop a
    /// double free. Clean here means every owner block is freed exactly
    /// once and no borrow frees anything.
    #[test]
    fn asan_tensor_ref_return_borrow_clean() {
        let label = "tensor_ref_return_borrow";
        if !asan_available() {
            eprintln!("[{label}] ASAN unavailable on this host — skipping");
            return;
        }
        let Some((stdout, status)) = run_under_asan(
            r#"
fn firstrow(t: ref Tensor[i64, [2, 3]]) -> ref Tensor[i64, [2, 3]] {
    t
}
struct Holder { t: Tensor[i64, [2, 3]] }
impl Holder {
    fn view(ref self) -> ref Tensor[i64, [2, 3]] {
        self.t
    }
}
fn main() {
    let a = Tensor.from([[1, 2, 3], [4, 5, 6]]);
    let r = firstrow(a).reshape([3, 2]);
    println(r[2, 1]);
    let b = firstrow(a);
    println(b[1, 2]);
    let h = Holder { t: Tensor.from([[10, 20, 30], [40, 50, 60]]) };
    let m = h.view().permute([1, 0]);
    println(m[2, 1]);
    let v = h.view();
    println(v[1, 2]);
    println(a[0, 0]);
}
"#,
            label,
        ) else {
            eprintln!("[{label}] setup failed — skipping");
            return;
        };
        assert!(
            status.success(),
            "[{label}] ASAN reported a memory error (exit code {:?}) — \
             a borrowed ref-Tensor return must not be freed",
            status.code()
        );
        assert_eq!(
            stdout.trim().lines().collect::<Vec<_>>(),
            vec!["6", "6", "60", "60", "1"],
            "[{label}] unexpected stdout (ASAN passed, output mismatched)"
        );
    }

    #[test]
    fn asan_borrow_local_read_methods_no_double_free() {
        // B-2026-06-07-5 residue: read-only methods beyond len/is_empty on a
        // borrow-LOCAL now route through `compile_vec_method` (the receiver is
        // registered in `vec_elem_types`). Reading through the borrow must NOT
        // free the source's heap buffer — only the source frees it, once, at
        // scope exit. The String source is a heap concat (not a static
        // literal) and the Vec is heap, so a stray free of the borrow would
        // double-free (ASAN abort) and an early free would leave the trailing
        // `s.len()`/`xs.len()` reading freed memory.
        let label = "borrow_local_read_methods";
        if !asan_available() {
            eprintln!("[{label}] ASAN unavailable on this host — skipping");
            return;
        }
        let Some((stdout, status)) = run_under_asan(
            r#"
fn sid(s: ref String) -> ref String { s }
fn vid(v: ref Vec[i64]) -> ref Vec[i64] { v }
fn main() {
    let s: String = "hello " + "world";
    let n = sid(s);
    println(n.starts_with("hello"));
    let xs: Vec[i64] = [10, 20, 30];
    let m = vid(xs);
    match m.get(1) { Some(x) => println(x), None => println(0 - 1) }
    match m.last() { Some(x) => println(x), None => println(0 - 1) }
    println(s.len());
    println(xs.len());
}
"#,
            label,
        ) else {
            eprintln!("[{label}] setup failed — skipping");
            return;
        };
        assert!(
            status.success(),
            "[{label}] ASAN reported a memory error (exit code {:?}) — \
             a read method on a borrow-local must not free the source's buffer",
            status.code()
        );
        assert_eq!(
            stdout.trim().lines().collect::<Vec<_>>(),
            vec!["true", "20", "30", "11", "3"],
            "[{label}] unexpected stdout (ASAN passed, output mismatched)"
        );
    }

    /// Mono-body owned-local cleanup (phase-11): a monomorphized
    /// (shape-generic) body that binds an owned `Tensor` local must free it
    /// exactly once at scope exit when it's dropped, and NOT free it when
    /// it's moved out as the return value (the caller frees). Exercises both
    /// in one program: `build_id` returns its `out` local (moved out → caller
    /// frees once), `diag_then_drop` drops its `t` local (freed at the mono's
    /// scope exit). Both have auto-par-eligible loops, so this also guards
    /// the `branch_cancel_ptr` reset (a stale ptr would mis-compile, not
    /// leak). A missing drain leaks `t` (Linux detect_leaks); a double-free
    /// of a moved-out tensor trips ASAN everywhere.
    #[test]
    fn asan_mono_body_owned_tensor_local_clean() {
        let label = "mono_body_owned_tensor_local";
        if !asan_available() {
            eprintln!("[{label}] ASAN unavailable on this host — skipping");
            return;
        }
        let Some((stdout, status)) = run_under_asan(
            r#"
fn build_id[N](n: i64) -> Tensor[f64, [N, N]] {
    let out: Tensor[f64, [?, ?]] = Tensor.zeros([n, n]);
    for i in 0..n { out[i, i] = 1.0; }
    out
}
fn diag_then_drop[N](n: i64) -> f64 {
    let t: Tensor[f64, [?, ?]] = Tensor.zeros([n, n]);
    for i in 0..n { t[i, i] = 2.0; }
    let mut s = 0.0;
    for i in 0..n { s = s + t[i, i]; }
    s
}
fn main() {
    let a = build_id(3);
    println(a[2, 2]);
    let b = build_id(2);
    println(b[0, 0]);
    println(diag_then_drop(4));
}
"#,
            label,
        ) else {
            eprintln!("[{label}] setup failed — skipping");
            return;
        };
        assert!(
            status.success(),
            "[{label}] ASAN reported a memory error (exit code {:?}) — \
             check the mono-body FreeTensor drain (drop vs move-out)",
            status.code()
        );
        assert_eq!(
            stdout.trim().lines().collect::<Vec<_>>(),
            vec!["1", "1", "8"],
            "[{label}] unexpected stdout (ASAN passed, output mismatched)"
        );
    }

    // ── Owned Vec/String param moved into a local (kata-23, 2026-06-07) ──
    //
    // `let mut work = lists;` where `lists` is a bare by-value Vec/String
    // param: the caller retains the buffer's scope-exit free (kata-22
    // owned-param ABI), so the let-move must deep-copy — pre-fix the moved
    // binding's `FreeVecBuffer` and the caller's free double-freed the
    // same buffer. macOS malloc only trapped on some heap layouts (kata-23's
    // ten cases split unpredictably); ASAN catches it deterministically.

    #[test]
    // B-2026-07-12-29 (FIXED): the compound index-assign of a shared/
    // Option[shared] Vec element — `work[i] = merge_two(work[i], …)` — orphaned
    // the OVERWRITTEN old node with no rc-dec, an ARM64-only leak (balanced on
    // x86 via an arch-dependent struct-move/ABI path, unbalanced on arm64). The
    // fix adds the ARC setter rule (retain-new → store → release-old) to
    // `compile_vec_index_store`, releasing the old value via the same per-element
    // drop the scope-exit drain uses. Previously `#[cfg_attr(aarch64, ignore)]`;
    // the ignore is removed now that the arm64 leg is clean.
    fn asan_owned_vec_param_let_move_interval_merge() {
        // kata-23 merge_k_lists shape: param Vec[Option[shared]] moved to
        // a mut local, in-place interval element reads/assignments, slot 0
        // returned, caller walks the spliced chain.
        assert_clean_asan_run(
            r#"
shared struct ListNode {
    val: i64,
    mut next: Option[ListNode],
}

fn merge_two(l1: Option[ListNode], l2: Option[ListNode]) -> Option[ListNode] {
    let dummy = ListNode { val: 0, next: None };
    let mut tail = dummy;
    let mut a = l1;
    let mut b = l2;
    loop {
        if let Some(na) = a {
            if let Some(nb) = b {
                if na.val <= nb.val {
                    tail.next = Some(na);
                    tail = na;
                    a = na.next;
                } else {
                    tail.next = Some(nb);
                    tail = nb;
                    b = nb.next;
                }
            } else {
                tail.next = a;
                break;
            }
        } else {
            tail.next = b;
            break;
        }
    }
    dummy.next
}

fn merge_k(lists: Vec[Option[ListNode]]) -> Option[ListNode] {
    let mut work = lists;
    let k = work.len();
    if k == 0 {
        return None;
    }
    let mut interval = 1;
    while interval < k {
        let mut i = 0;
        while i + interval < k {
            work[i] = merge_two(work[i], work[i + interval]);
            i = i + 2 * interval;
        }
        interval = 2 * interval;
    }
    work[0]
}

fn main() {
    let n1 = ListNode { val: 1, next: None };
    let n2 = ListNode { val: 2, next: None };
    let n3 = ListNode { val: 3, next: None };
    let mut v: Vec[Option[ListNode]] = Vec.new();
    v.push(Some(n1));
    v.push(Some(n2));
    v.push(Some(n3));
    let mut cur = merge_k(v);
    loop {
        match cur {
            Some(node) => {
                println(node.val);
                cur = node.next;
            }
            None => break,
        }
    }
}
"#,
            &["1", "2", "3"],
            "owned_vec_param_let_move_interval_merge",
        );
    }

    #[test]
    fn asan_vec_option_shared_index_overwrite_place_rhs_no_leak() {
        // B-2026-07-12-29 minimal repro (Option[shared] element, place RHS):
        // `work[0] = work[1]` overwrites slot 0's OLD node with slot 1's — the
        // overwritten node must be rc-dec'd or it leaks on arm64. The retain of
        // the RHS is upstream (the index-read clone rc-incs), so the store site
        // must release the old. slot0 holds a 2-node chain so BOTH orphaned
        // nodes would leak pre-fix; the driver keeps slot 1 alive and walks it.
        assert_clean_asan_run(
            r#"
shared struct ListNode {
    val: i64,
    mut next: Option[ListNode],
}

fn probe(lists: Vec[Option[ListNode]]) -> Option[ListNode] {
    let mut work = lists;
    work[0] = work[1];
    work[0]
}

fn main() {
    let a2 = ListNode { val: 10, next: None };
    let a1 = ListNode { val: 11, next: Some(a2) };
    let b = ListNode { val: 22, next: None };
    let mut v: Vec[Option[ListNode]] = Vec.new();
    v.push(Some(a1));
    v.push(Some(b));
    let mut cur = probe(v);
    loop {
        match cur {
            Some(node) => {
                println(node.val);
                cur = node.next;
            }
            None => break,
        }
    }
}
"#,
            &["22"],
            "vec_option_shared_index_overwrite_place_rhs",
        );
    }

    #[test]
    fn asan_vec_plain_shared_index_overwrite_place_rhs_no_leak() {
        // B-2026-07-12-29 sibling — plain `Vec[shared T]` (non-Option) element.
        // The element clone is a SHALLOW pointer copy (no rc-inc), so
        // `work[0] = work[1]` pre-fix BOTH double-freed slot 1's box (two slots
        // aliasing one un-inc'd box → two scope-exit decs) AND leaked slot 0's
        // old box. The setter rule retains the new (place RHS) and releases the
        // old, balancing both.
        assert_clean_asan_run(
            r#"
shared struct Node { val: i64 }

fn probe(xs: Vec[Node]) -> i64 {
    let mut work = xs;
    work[0] = work[1];
    work[0].val
}

fn main() {
    let a = Node { val: 7 };
    let b = Node { val: 9 };
    let mut v: Vec[Node] = Vec.new();
    v.push(a);
    v.push(b);
    println(probe(v));
}
"#,
            &["9"],
            "vec_plain_shared_index_overwrite_place_rhs",
        );
    }

    #[test]
    fn asan_owned_string_param_let_move_grow() {
        // String sibling with a realloc after the move — without the
        // deep copy the caller frees a stale (realloc-moved) pointer.
        assert_clean_asan_run(
            r#"
fn bang(s: String) -> String {
    let mut t = s;
    t.push_str("!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!");
    t
}

fn main() {
    let a = f"abc{1}";
    let b = bang(a);
    println(b.len());
}
"#,
            &["36"],
            "owned_string_param_let_move_grow",
        );
    }

    #[test]
    fn asan_owned_vec_param_assign_move() {
        // Assign-arm sibling: `work = v;` deep-copies; the LHS's prior
        // buffer is eagerly freed (no leak), the caller's free stays
        // valid.
        assert_clean_asan_run(
            r#"
fn second(v: Vec[i64]) -> i64 {
    let mut work: Vec[i64] = Vec.new();
    work.push(0);
    work = v;
    work[1]
}

fn main() {
    let mut v: Vec[i64] = Vec.new();
    v.push(7);
    v.push(9);
    println(second(v));
}
"#,
            &["9"],
            "owned_vec_param_assign_move",
        );
    }

    // ── kata-#24: pattern-binding alias acquire ───────────────────

    #[test]
    fn asan_if_let_shared_binding_field_displacement() {
        // The kata-#24 minimal UAF: `if let Some(second) = first.next`
        // bound a NON-retained alias; `first.next = second.next`
        // released the field's only ref to that node, freeing it under
        // the live binding — the `second.val` read below is a
        // heap-use-after-free pre-fix. `bind_pattern_values`' alias
        // acquire (+1 at bind, scope-exit RcDec) keeps it alive.
        assert_clean_asan_run(
            r#"
shared struct ListNode { val: i64, mut next: Option[ListNode] }
fn from3() -> Option[ListNode] {
    let head = ListNode { val: 1, next: None };
    let n2 = ListNode { val: 2, next: None };
    let n3 = ListNode { val: 3, next: None };
    n2.next = Some(n3);
    head.next = Some(n2);
    Some(head)
}
fn poke(head: Option[ListNode]) {
    if let Some(first) = head {
        if let Some(second) = first.next {
            first.next = second.next;
            println(second.val);
        }
    }
}
fn main() {
    poke(from3());
}
"#,
            &["2"],
            "if_let_shared_binding_field_displacement",
        );
    }

    #[test]
    fn asan_swap_pairs_pair_relink_loop() {
        // Full kata-#24 iterative pair-swap over a fresh 6-node chain:
        // per-pair three-store re-link with `break` exits from inside
        // `if let` arms holding live bindings. Catches both halves of
        // the fix under ASAN — the binding acquire (UAF on `second`)
        // and the break-drain (whose absence leaks; whose
        // over-aggressive form would double-free on the fall-through
        // path).
        assert_clean_asan_run(
            r#"
shared struct ListNode { val: i64, mut next: Option[ListNode] }
fn build(n: i64) -> Option[ListNode] {
    let head = ListNode { val: 1, next: None };
    let mut tail = head;
    for i in 2..n + 1 {
        let node = ListNode { val: i, next: None };
        tail.next = Some(node);
        tail = node;
    }
    Some(head)
}
fn swap_pairs(head: Option[ListNode]) -> Option[ListNode] {
    let dummy = ListNode { val: 0, next: head };
    let mut prev = dummy;
    loop {
        if let Some(first) = prev.next {
            if let Some(second) = first.next {
                first.next = second.next;
                second.next = Some(first);
                prev.next = Some(second);
                prev = first;
            } else {
                break;
            }
        } else {
            break;
        }
    }
    dummy.next
}
fn main() {
    let mut cur = swap_pairs(build(6));
    let mut sum = 0;
    loop {
        match cur {
            Some(node) => {
                sum = sum + node.val;
                cur = node.next;
            }
            None => break,
        }
    }
    println(sum);
}
"#,
            &["21"],
            "swap_pairs_pair_relink_loop",
        );
    }

    // ── `ref name @ PATTERN` borrow bindings (phase-8 @ slice 4) ──
    //
    // `ref x @ Foo { a }` under an owned scrutinee: the subtree
    // borrows — pattern bindings must NOT register heap cleanup
    // (`pattern_binding_is_borrow` suppression in the by_ref
    // AtBinding bind path) while the source keeps its own drop
    // (`pattern_consumes_field` → false for by_ref). If either half
    // regresses, the String buffer is freed twice (binding cleanup +
    // source drop) and ASAN flags it here.

    #[test]
    fn asan_ref_at_binding_struct_string_field_single_free() {
        assert_clean_asan_run(
            r#"
struct Foo { a: String, n: i64 }
fn main() {
    let foo = Foo { a: "heap-owned string content", n: 7 };
    match foo {
        ref x @ Foo { a, n } => {
            println(a);
            println(n);
            println(x.n);
        }
    }
    println(foo.a);
}
"#,
            &[
                "heap-owned string content",
                "7",
                "7",
                "heap-owned string content",
            ],
            "ref_at_binding_struct_string_field_single_free",
        );
    }

    #[test]
    fn asan_ref_at_binding_option_string_payload_single_free() {
        assert_clean_asan_run(
            r#"
fn main() {
    let opt = Some("payload string on the heap");
    match opt {
        ref x @ Some(y) => { println(y); }
        None => { println("none"); }
    }
    match opt {
        Some(z) => { println(z); }
        None => { }
    }
}
"#,
            &["payload string on the heap", "payload string on the heap"],
            "ref_at_binding_option_string_payload_single_free",
        );
    }

    #[test]
    fn asan_channel_send_recv_clone_single_free() {
        // Phase 6 "Channel AOT codegen lowering": the refcount Drop
        // (`CleanupAction::DropChannelEnd`) must reclaim the channel exactly
        // once. `Channel.new()` mints refcount 2 (the destructured `tx`/`rx`),
        // `tx.clone()` increments to 3, and the three scope-exit drops bring
        // it to 0 — a single free of the `KaracChannel`. A miscount would
        // surface here as an ASAN double-free (over-drop) or, on Linux CI's
        // LeakSanitizer, a leak (under-drop). Run through the full pipeline
        // (concurrency on) so the `stmt_has_channel_op` auto-par exclusion is
        // exercised — without it the `send`/`recv` fan into branch workers and
        // the channel-end allocas land in a captured scope, which would also
        // trip ASAN. String payloads exercise the multi-word transfer too.
        assert_clean_asan_run_with_concurrency(
            r#"
fn main() {
    let (tx, rx): (Sender[String], Receiver[String]) = Channel.new();
    tx.send("first");
    let tx2 = tx.clone();
    tx2.send("second");
    println(rx.recv());
    println(rx.recv());
    match rx.try_recv() {
        Some(v) => println(v),
        None => println("drained"),
    }
}
"#,
            &["first", "second", "drained"],
            "channel_send_recv_clone_single_free",
        );
    }

    #[test]
    fn asan_channel_move_into_spawn_single_free() {
        // Move-across-spawn: `tx` is captured into the spawned closure and
        // consumed by `worker(tx)`. The channel `new` mints refcount 2 (`tx`
        // / `rx`); `main` drops both at scope exit (the moved-in `tx` param
        // isn't a `bind_pattern` binding, so the worker registers no second
        // drop), balancing to a single free AFTER `h.join()` guarantees the
        // worker's `send` already ran (no use-after-free on the worker side,
        // no double-free on `main`'s). Verified leak-balanced at runtime
        // (1 alloc / 1 free); ASAN guards the double-free / UAF edges.
        assert_clean_asan_run_with_concurrency(
            r#"
fn worker(tx: Sender[i64]) -> i64 {
    tx.send(42);
    0
}
fn main() {
    let (tx, rx): (Sender[i64], Receiver[i64]) = Channel.new();
    let h: TaskHandle[i64] = spawn(|| worker(tx));
    h.join();
    println(rx.recv());
}
"#,
            &["42"],
            "channel_move_into_spawn_single_free",
        );
    }

    #[test]
    fn asan_channel_producer_consumer_close_single_free() {
        // Cross-task sender-drop: the producer (spawned task) sends 2 values
        // then finishes — its moved `Sender` is dropped BY THE TASK (the
        // wrapper), which both closes the channel (terminating the consumer's
        // blocking `recv` drain) and releases exactly one reference. The
        // parent's drop of the moved `Sender` is suppressed, so the channel is
        // freed exactly once (no double-free here, no leak on Linux LSan) —
        // and the program terminates rather than deadlocking.
        assert_clean_asan_run_with_concurrency(
            r#"
fn producer(tx: Sender[i64]) -> i64 {
    tx.send(10);
    tx.send(20);
    0
}
fn consume(rx: Receiver[i64]) -> i64 {
    let mut sum = 0;
    let mut go = true;
    while go {
        let v = rx.recv();
        if v == 0 { go = false; } else { sum = sum + v; }
    }
    sum
}
fn main() {
    let (tx, rx): (Sender[i64], Receiver[i64]) = Channel.new();
    let h: TaskHandle[i64] = spawn(|| producer(tx));
    println(consume(rx));
    h.join();
}
"#,
            &["30"],
            "channel_producer_consumer_close_single_free",
        );
    }

    #[test]
    fn asan_channel_end_returned_from_fn_single_free() {
        // Regression for the channel-end MOVE-OUT-ON-RETURN double-drop
        // (recv-out-slot-read-race root cause): a factory `fn mk() ->
        // Receiver[T] { let (tx, rx) = Channel.new(); ...; rx }` returns the
        // `Receiver` as its tail expression — moving it into the caller's
        // binding. `bind_pattern` queues a `DropChannelEnd` for `rx` at the
        // destructure site; without move-out suppression at the return, that
        // drop fires at `mk`'s scope exit (decrementing the channel's `total`)
        // AND again when the caller's `r` goes out of scope — a double-drop
        // that frees the `KaracChannel` early. Here `mk` keeps a cloned sender
        // alive across the return (mirroring the host-async `pointer_moves()`
        // shape, where the host owns a clone), so the over-drop frees the
        // channel while a live sender reference still points at it: ASAN flags
        // the heap-use-after-free / double-free. With the fix, `mk` drops only
        // its local `tx`, the caller's `r` drops `rx` once, and `s` (the
        // surviving clone) drops last → exactly one free. Covers the tail-
        // expression return; a sibling `return rx;` form is exercised below.
        assert_clean_asan_run(
            r#"
fn mk() -> Receiver[i64] {
    let (tx, rx): (Sender[i64], Receiver[i64]) = Channel.new();
    let keep = tx.clone();
    keep.send(7);
    tx.send(9);
    rx
}
fn main() {
    let r = mk();
    println(r.recv());
    println(r.recv());
}
"#,
            &["7", "9"],
            "channel_end_returned_from_fn_single_free",
        );
    }

    #[test]
    fn asan_channel_end_explicit_return_single_free() {
        // Sibling of `asan_channel_end_returned_from_fn_single_free` for the
        // explicit `return rx;` shape (vs the tail-expression form). Same
        // move-out-on-return double-drop class; the suppression lives in the
        // `ExprKind::Return` arm. `cond` is always true so `mk` always returns
        // `rx` (the moved-out end) — a single free with the fix, an ASAN
        // double-free without it.
        assert_clean_asan_run(
            r#"
fn mk(cond: bool) -> Sender[i64] {
    let (tx, rx): (Sender[i64], Receiver[i64]) = Channel.new();
    tx.send(11);
    println(rx.recv());
    if cond {
        return tx;
    }
    tx
}
fn main() {
    let s = mk(true);
    s.send(22);
}
"#,
            &["11"],
            "channel_end_explicit_return_single_free",
        );
    }

    #[test]
    fn asan_channel_end_let_rebind_single_free() {
        // Regression for the channel-end LET-REBIND move double-drop — the
        // let-binding sibling of `asan_channel_end_returned_from_fn_single_free`
        // (which covers the tail-expression return) and
        // `asan_channel_end_explicit_return_single_free` (the `return rx;` form).
        // Here the `Receiver` is moved into a NEW `let` binding first
        // (`let keep = rx;`), then that rebind is returned. The destructure
        // queues a `DropChannelEnd` for `rx`; `bind_pattern` queues a SECOND for
        // `keep`. Without move-suppression at the let-rebind, BOTH fire — `rx`'s
        // at `mk`'s scope exit AND the caller's `r` at `main`'s — double-dropping
        // the channel's refcount and freeing the `KaracChannel` early while the
        // caller still reads from it: ASAN flags the heap-use-after-free /
        // double-free. With the fix the source `rx`'s `DropChannelEnd` is
        // suppressed, `keep` carries the single live drop (itself suppressed at
        // the return as the new owner moves to `main`), and `main`'s `r` frees
        // exactly once. The two buffered sends arrive in order → "7","9".
        assert_clean_asan_run(
            r#"
fn mk() -> Receiver[i64] {
    let (tx, rx): (Sender[i64], Receiver[i64]) = Channel.new();
    let keep = rx;
    tx.send(7);
    tx.send(9);
    keep
}
fn main() {
    let r = mk();
    println(r.recv());
    println(r.recv());
}
"#,
            &["7", "9"],
            "channel_end_let_rebind_single_free",
        );
    }

    #[test]
    fn asan_channel_end_let_rebind_branch_buried_no_leak() {
        // Guards the BRANCH-BURIED corner of the let-rebind suppression: only
        // ONE arm rebinds the channel end (`if cond { let keep = rx; ... }`),
        // while the OTHER arm keeps using the source `rx`. A compile-time
        // retraction of `rx`'s `DropChannelEnd` (the terminal-site
        // `suppress_channel_drop_for_var`) would remove it unconditionally, so
        // the non-rebinding `else` path would never drop `rx` and leak the
        // `KaracChannel` (`total` stuck at 1) — caught by LeakSanitizer on Linux.
        // The branch-safe in-slot null sentinel
        // (`neutralize_moved_channel_end_slot`) only neutralizes `rx` on the path
        // that actually executes the move, so BOTH arms free exactly once: no
        // leak, no double-free. `cond` is exercised both ways from `main`.
        // (On macOS — no LSan — this asserts the no-double-free / no-UAF half;
        // the Linux LSan gate asserts the no-leak half.)
        assert_clean_asan_run(
            r#"
fn pick(cond: bool) -> i64 {
    let (tx, rx): (Sender[i64], Receiver[i64]) = Channel.new();
    tx.send(100);
    if cond {
        let keep = rx;
        keep.recv()
    } else {
        rx.recv()
    }
}
fn main() {
    let a = pick(true);
    let b = pick(false);
    println(a + b);
}
"#,
            &["200"],
            "channel_end_let_rebind_branch_buried_no_leak",
        );
    }

    #[test]
    fn asan_discarded_taskgroup_spawn_loop_eager_reap_no_double_free() {
        // B-2026-06-17-2 — the canonical server shape `loop { tg.spawn(|| …) }`
        // discards each child's `TaskHandle`. Codegen now marks the discarded
        // handle detached (`karac_runtime_task_detach`), and the runtime
        // eager-reaps detached, completed children inside
        // `karac_runtime_taskgroup_register`'s sweep — bounding the group's
        // `children` Vec instead of leaking ~100 B/conn unbounded.
        //
        // This E2E drives the FULL path (codegen detach emission + register-time
        // sweep + scope-exit `join_and_free`) under ASAN/LSan. Its job is to
        // pin the UAF-prone hazard the spike flagged: the sweep and the
        // scope-exit join must never both free the same child (double-free), and
        // the sweep's terminal-peek must never free a still-running child (UAF).
        // The `join_and_free` barrier at the group's scope exit makes the run
        // deterministic — every child is reclaimed before exit, so Linux LSan
        // also confirms no leak. (The fails-before-fix leak *regression* lives in
        // the runtime unit test `taskgroup_register_reaps_detached_completed_
        // children`, which asserts the Vec stays bounded; an at-exit LSan check
        // can't catch the leak because `join_and_free` reclaims everything when
        // a finite scope exits.)
        assert_clean_asan_run_with_concurrency(
            r#"
fn work(n: i64) -> i64 {
    n + 1
}
fn main() {
    let mut tg = TaskGroup.new();
    let mut i = 0;
    while i < 2000 {
        let c = i;
        tg.spawn(|| work(c));
        i = i + 1;
    }
    println(0);
}
"#,
            &["0"],
            "discarded_taskgroup_spawn_loop_eager_reap_no_double_free",
        );
    }

    #[test]
    fn asan_bounded_channel_scope_exit_single_free() {
        // `BoundedChannel.new` allocates a runtime queue; the `BoundedChannel`
        // Drop frees it (and any undrained payloads) exactly once at scope
        // exit. String elements exercise the heap-payload copy path (the
        // queue owns the byte blobs; the source String's own drop is
        // independent). ASAN proves: no leak (queue + undrained "world" blob
        // freed), no double-free (single-owner, no refcount).
        assert_clean_asan_run(
            r#"
fn main() {
    let bc: BoundedChannel[String] = BoundedChannel.new(2, OnFull.FailFast);
    match bc.send("hello") { Ok(_) => println(1), Err(_) => println(0), }
    match bc.send("world") { Ok(_) => println(1), Err(_) => println(0), }
    match bc.recv() { Some(s) => println(s), None => println("none"), }
    // "world" left undrained — its blob is freed by the channel's Drop.
}
"#,
            &["1", "1", "hello"],
            "bounded_channel_scope_exit_single_free",
        );
    }

    // ── Borrowed String-slice map keys (allocation-free lookups) ──────
    //
    // `m.get(s[a..b])` / `m.insert(s[a..b], v)` pass a borrowed
    // `{ptr, len, cap=0}` view into `s` instead of a freshly-allocated
    // owned `String`. Lookups never retain the key; `insert` deep-copies it
    // only on a *fresh* insertion (`karac_map_insert_borrowed_str_old`). This
    // test proves the deep-copy happens: the source heap string is freed
    // (reassigned) and the allocator churned *before* the map is read, so a
    // borrowed key that was wrongly stored verbatim would be a use-after-free
    // ASAN catches. Also exercises the empty-slice key (`s[0..0]` → null ptr,
    // len 0) and scope-exit free of the deep-copied keys.
    #[test]
    fn asan_borrowed_string_slice_map_keys_deep_copy() {
        assert_clean_asan_run(
            r#"
fn main() {
    let mut m: Map[String, i64] = Map.new();
    let mut s = String.new();
    s.push_str("foo");
    s.push_str("bar");        // heap buffer "foobar"
    m.insert(s[0..3], 1);     // borrowed slice -> deep-copied into the map
    m.insert(s[3..6], 2);
    s = String.new();         // frees the old "foobar" buffer
    let mut junk = String.new();
    let mut i = 0i64;
    while i < 2000 { junk.push_str("zzzz"); i = i + 1; }
    match m.get("foo") { Some(v) => println(v), None => println(-1) }
    match m.get("bar") { Some(v) => println(v), None => println(-1) }
    m.insert(s[0..0], 9);     // empty borrowed key (null ptr, len 0)
    println(m.len())
}
"#,
            &["1", "2", "3"],
            "borrowed_string_slice_map_keys_deep_copy",
        );
    }

    // ── `Map[String, _].clear()` frees heap key buffers ───────────────
    //
    // Plain `karac_map_clear` only zeroed the bucket status bytes, leaking
    // every live String key's heap buffer (the map-free frees only occupied
    // slots, and a clear leaves none). With many insert→clear rounds this
    // leaks unboundedly. The fix routes heap-keyed/valued maps through
    // `karac_map_clear_with_drop_vec`. LeakSanitizer fails this test pre-fix.
    #[test]
    fn asan_map_string_key_clear_frees_heap_keys() {
        assert_clean_asan_run(
            r#"
fn main() {
    let base = "abcdefghij";
    let mut m: Map[String, i64] = Map.new();
    let mut round = 0i64;
    while round < 50 {
        let mut v = 0i64;
        while v < 5 { m.insert(base[v*2 .. v*2+2], v); v = v + 1; }
        m.clear();
        round = round + 1;
    }
    println(m.len());
}
"#,
            &["0"],
            "map_string_key_clear_frees_heap_keys",
        );
    }

    // B-2026-06-10-1: `Vec.contains` / `String.contains` codegen lowering.
    // `contains` is read-only — it loads each element (or memcmp's a window)
    // but never moves out of, frees, or aliases the receiver's buffer. This
    // exercises both over genuinely heap-allocated sources (a Vec[String]
    // whose elements are f-string heap buffers, and a heap String built via
    // push_str) so a stray free / double-free / over-read in the scan would
    // trip ASAN. The needle is also a heap f-string for the String case.
    #[test]
    fn asan_contains_heap_sources_no_uaf() {
        assert_clean_asan_run(
            r#"
fn main() {
    let mut names: Vec[String] = Vec.new();
    let mut i = 0i64;
    while i < 4 { names.push(f"name:{i}"); i = i + 1; }
    println(names.contains(f"name:2"));
    println(names.contains(f"name:9"));

    let mut s: String = "";
    s.push_str("hello ");
    s.push_str("world");
    println(s.contains(f"o w"));
    println(s.contains(f"zzz"));
}
"#,
            &["true", "false", "true", "false"],
            "contains_heap_sources_no_uaf",
        );
    }

    #[test]
    fn asan_push_str_borrowed_slice_no_uaf() {
        // `push_str(src[a..b])` borrows a zero-copy view into `src` instead of
        // allocating a temp String (the 30× #405 fix). The view points into
        // `src`'s buffer; `out` grows repeatedly (the destination buffer is
        // freed/reallocated each grow). ASAN confirms the borrowed source —
        // which is `hexd`/`words[k]`, NOT `out` — stays valid across `out`'s
        // grows (no use-after-free), and that nothing leaks (the cap-0 view is
        // never freed; no temp is allocated to leak). Literal- and
        // heap-element-sourced slices both exercise the path.
        assert_clean_asan_run(
            r#"
fn main() {
    let hexd: String = "0123456789abcdef";
    let mut out: String = "";
    let mut k = 0i64;
    while k < 2000 {
        let d = k & 0xfi64;
        out.push_str(hexd[d..d + 1i64]);   // literal-sourced borrow, out grows
        k = k + 1i64;
    }
    println(out.bytes().len());

    let mut words: Vec[String] = Vec.new();
    let mut i = 0i64;
    while i < 8 { words.push(f"token-{i}-payload"); i = i + 1i64; }
    let mut joined: String = "";
    let mut j = 0i64;
    while j < 500 {
        joined.push_str(words[j & 7i64][0..5i64]);   // heap-element-sourced borrow + grow
        j = j + 1i64;
    }
    println(joined.bytes().len());
}
"#,
            &["2000", "2500"],
            "push_str_borrowed_slice_no_uaf",
        );
    }

    #[test]
    fn asan_let_bound_heap_vec_element_no_double_free() {
        // `let w = v[i]` where v: Vec[String] with heap-owned (f-string) elements
        // — the index returns a SHALLOW element struct aliasing v's buffer.
        // Binding it owned must DEEP-CLONE (B-2026-06-14-11): both w's drop and
        // v's element-drop run at scope exit, so without the clone they free the
        // same buffer (double-free). The element stays in v (interp clones), so v[i]
        // is reused afterward to confirm it wasn't consumed/corrupted. Loops so the
        // clone path runs many times (each w dropped per-iteration).
        assert_clean_asan_run(
            r#"
fn main() {
    let mut v: Vec[String] = Vec.new();
    let mut i = 0i64;
    while i < 8 { v.push(f"token-{i}-payload"); i = i + 1i64; }
    let mut total = 0i64;
    let mut j = 0i64;
    while j < 4000 {
        let w = v[j & 7i64];          // deep-clone bind; v[j&7] stays valid
        total = total + w.bytes().len();
        j = j + 1i64;
    }
    println(total);
    println(v[0i64]);                 // v still intact after 4000 binds
    println(v.len());
}
"#,
            &["60000", "token-0-payload", "8"],
            "let_bound_heap_vec_element_no_double_free",
        );
    }

    #[test]
    fn asan_let_bound_vec_enum_struct_element_no_double_free() {
        // B-2026-06-14-12 (sibling of B-11): a `Vec` element that is a user ENUM
        // or STRUCT carrying a heap String payload. Indexing returns a SHALLOW
        // copy aliasing v's buffer, so it must be deep-cloned (emit_enum_clone_fn
        // / emit_struct_clone_fn) — otherwise the binding and v's element-drop
        // free the same buffer (double-free). Covers all three element-read shapes
        // the fix touches: (1) `let e = es[i]` + `match e` move-out (enum), (2)
        // `let p = ps[i]` + field access (struct), and (3) the DIRECT
        // `match es[i] { Word(s) => … }` scrutinee (the lexer's token-consume
        // shape), which clones `scrut` itself so the arm extracts the clone and
        // the freshtemp materialization drop-tracks it. Each source vec is reused
        // after the binds to confirm it stayed intact; the loop runs the
        // synthesized clone + suppression paths thousands of times.
        assert_clean_asan_run(
            r#"
enum Tok { Word(String), End }
struct Pair { name: String, n: i64 }
fn main() {
    let mut es: Vec[Tok] = Vec.new();
    let mut ps: Vec[Pair] = Vec.new();
    let mut i = 0i64;
    while i < 8 {
        es.push(Tok.Word(f"w-{i}-payload"));
        ps.push(Pair { name: f"p-{i}-payload", n: i });
        i = i + 1i64;
    }
    let mut total = 0i64;
    let mut j = 0i64;
    while j < 4000 {
        let e = es[j & 7i64];            // deep-clone bind of enum element
        match e { Tok.Word(s) => { total = total + s.bytes().len(); }, Tok.End => {} }
        let p = ps[j & 7i64];            // deep-clone bind of struct element
        total = total + p.name.bytes().len() + p.n;
        j = j + 1i64;
    }
    println(total);
    match es[0i64] { Tok.Word(s) => println(s), Tok.End => println("end") }
    println(ps[0i64].name);             // both vecs intact after 4000 binds
    println(es.len());
}
"#,
            &["102000", "w-0-payload", "p-0-payload", "8"],
            "let_bound_vec_enum_struct_element_no_double_free",
        );
    }

    #[test]
    fn asan_try_clone_vec_string_deep_independent_free() {
        // phase-8-stdlib-floor item 8: `Vec[String].try_clone()` deep-clones
        // every element into a fresh buffer. Source and clone own independent
        // String buffers, so both must free exactly once with no double-free /
        // leak. Both go out of scope here (source + the `Ok`-bound clone).
        assert_clean_asan_run(
            r#"
fn main() {
    let mut src: Vec[String] = Vec.new();
    src.push(f"alpha{1}");
    src.push(f"beta{2}");
    match src.try_clone() {
        Ok(c) => {
            println(c.len());
            println(c[0]);
            println(c[1]);
        }
        Err(_) => println("err"),
    }
    src.push(f"gamma{3}");
    println(src.len());
}
"#,
            &["2", "alpha1", "beta2", "3"],
            "try_clone_vec_string_deep_independent_free",
        );
    }

    #[test]
    fn asan_try_clone_question_unwrap_single_free() {
        // The `?`-unwrap of a `try_clone` result yields an owned Vec the callee
        // returns through; the source and the unwrapped clone each free once.
        assert_clean_asan_run(
            r#"
fn dup() -> Result[i64, AllocError] {
    let mut v: Vec[String] = Vec.new();
    v.push(f"x{1}");
    v.push(f"y{2}");
    let c: Vec[String] = v.try_clone()?;
    Ok(c.len())
}

fn main() {
    match dup() {
        Ok(n) => println(n),
        Err(_) => println("err"),
    }
}
"#,
            &["2"],
            "try_clone_question_unwrap_single_free",
        );
    }

    #[test]
    fn asan_try_clone_vec_tuple_scalar_deep_free() {
        // Non-heap tuple element (`Vec[(i64, i64)]`) routes through the tuple
        // fallible-clone fn (per-field recursion) nested inside the Vec
        // fallible-clone loop. No inner heap, so the only allocation is the
        // buffer; source and clone free their buffers exactly once.
        //
        // The tuple-WITH-heap-element variant (`Vec[(i64, String)]`) is NOT
        // covered: it UAFs identically under the *panicking* `.clone()` too —
        // a pre-existing defect in the Vec-of-tuple-with-heap-element clone path
        // (bugs.md B-2026-06-10-5), independent of `try_clone`.
        assert_clean_asan_run(
            r#"
fn main() {
    let mut src: Vec[(i64, i64)] = Vec.new();
    src.push((1i64, 2i64));
    src.push((3i64, 4i64));
    match src.try_clone() {
        Ok(c) => {
            println(c[0].0); println(c[0].1);
            println(c[1].0); println(c[1].1);
        }
        Err(_) => println("err"),
    }
    src.push((5i64, 6i64));
    println(src.len());
}
"#,
            &["1", "2", "3", "4", "3"],
            "try_clone_vec_tuple_scalar_deep_free",
        );
    }

    #[test]
    fn asan_vec_tuple_heap_element_push_read() {
        // B-2026-06-10-5 (core UAF): pushing a tuple with an inline f-string
        // heap field into a `Vec[(i64, String)]` and reading it back. Before
        // the fix `compile_tuple` left the f-string accumulator's
        // `FreeVecBuffer` armed; it freed the String buffer right after the
        // push, leaving the Vec element dangling (heap-use-after-free on the
        // read / scope-exit). The fix suppresses the inline-f-string acc as
        // the tuple takes ownership, and the Vec's scope-exit drain recurses
        // into the tuple element's owned String field (one free, at scope
        // exit). Multiple pushes exercise the grow path too.
        assert_clean_asan_run(
            r#"
fn main() {
    let mut src: Vec[(i64, String)] = Vec.new();
    src.push((1i64, f"p{1}"));
    src.push((2i64, f"q{2}"));
    println(src[0].1);
    println(src[1].1);
    println(src.len());
}
"#,
            &["p1", "q2", "2"],
            "vec_tuple_heap_element_push_read",
        );
    }

    #[test]
    fn asan_vec_tuple_heap_element_string_var_push() {
        // Sibling of the push-read case where the String field arrives as an
        // IDENTIFIER (an owned binding) rather than an inline f-string. The
        // tuple-construction move-suppression (identifier source) + the Vec
        // scope-exit recursive drop must keep this single-free clean.
        assert_clean_asan_run(
            r#"
fn main() {
    let mut src: Vec[(i64, String)] = Vec.new();
    let s = f"p{1}";
    src.push((1i64, s));
    println(src[0].1);
    println(src.len());
}
"#,
            &["p1", "1"],
            "vec_tuple_heap_element_string_var_push",
        );
    }

    #[test]
    fn asan_vec_tuple_heap_element_clone_deep_free() {
        // B-2026-06-10-5 (the headline `.clone()` repro): build a
        // `Vec[(i64, String)]` by pushing inline-f-string tuples, then
        // `.clone()` it. The reported UAF (`karac_string_clone` reading a
        // freed pointer) was NOT a shallow clone — `emit_vec_clone_fn`
        // already deep-clones per element. The real cause was upstream: the
        // push left each inline f-string accumulator's `FreeVecBuffer` armed,
        // so it freed the source String right after the push; `clone` then
        // read that freed buffer. The fix suppresses the inline-f-string acc
        // at tuple construction (source valid for clone) and recurses the
        // Vec scope-exit drain into the tuple element's String (one free per
        // Vec). Source and clone own independent buffers; each frees once.
        assert_clean_asan_run(
            r#"
fn main() {
    let mut src: Vec[(i64, String)] = Vec.new();
    src.push((1i64, f"p{1}"));
    src.push((2i64, f"q{2}"));
    let c = src.clone();
    println(c[0].1);
    println(c[1].1);
    println(src.len());
}
"#,
            &["p1", "q2", "2"],
            "vec_tuple_heap_element_clone_deep_free",
        );
    }

    #[test]
    fn asan_vec_tuple_heap_element_try_clone_deep_free() {
        // B-2026-06-10-5 closes the original tracker's deferred coverage:
        // `Vec[(i64, String)].try_clone()` (the fallible sibling of the
        // `.clone()` case above) over a heap tuple element. Same root cause
        // and fix; source + the `Ok`-bound clone each free their own buffers
        // exactly once. The scalar-tuple variant
        // (`asan_try_clone_vec_tuple_scalar_deep_free`) guarded the non-heap
        // path; this guards the heap-element path it deferred.
        assert_clean_asan_run(
            r#"
fn main() {
    let mut src: Vec[(i64, String)] = Vec.new();
    src.push((1i64, f"p{1}"));
    src.push((2i64, f"q{2}"));
    match src.try_clone() {
        Ok(c) => {
            println(c[0].1);
            println(c[1].1);
            println(c.len());
        }
        Err(_) => println("err"),
    }
    println(src.len());
}
"#,
            &["p1", "q2", "2", "2"],
            "vec_tuple_heap_element_try_clone_deep_free",
        );
    }

    #[test]
    fn asan_rc_fallback_tuple_heap_field_drop_no_leak() {
        // B-2026-06-10-8: a let-bound tuple with a heap (String) field that
        // the ownership checker routes to RC-fallback boxing leaked the
        // field's buffer at scope exit — the box `{i64 rc, value}` was freed
        // at rc==0 without recursing into the boxed value's heap fields. The
        // fix synthesizes a per-box value-drop fn (`register_rc_fallback_box_drop`)
        // that `emit_rc_dec` invokes before the box free. Non-foldable
        // (loop-index) strings so each `t` is a real heap allocation; macOS
        // ASAN proves no double-free, Linux `detect_leaks=1` proves no leak
        // (the leak this closes was LSan-visible, invisible to macOS ASAN).
        assert_clean_asan_run(
            r#"
fn main() {
    let mut i = 0i64;
    while i < 3i64 {
        let t = (i, f"item-{i}");
        println(t.1);
        i = i + 1i64;
    }
}
"#,
            &["item-0", "item-1", "item-2"],
            "rc_fallback_tuple_heap_field_drop",
        );
    }

    #[test]
    fn asan_rc_fallback_struct_heap_field_drop_no_leak() {
        // B-2026-06-10-8, the non-shared-struct sibling of the tuple case: a
        // let-bound `struct Pair { n: i64, s: String }` routed to RC-fallback
        // boxing leaked its `String` field. The structural heap-field walk
        // (`emit_aggregate_heap_field_frees`) handles tuples and structs
        // uniformly — both lower to an LLVM struct whose String fields are
        // `vec_struct_type()`-shaped.
        assert_clean_asan_run(
            r#"
struct Pair { n: i64, s: String }
fn main() {
    let mut i = 0i64;
    while i < 3i64 {
        let t = Pair { n: i, s: f"item-{i}" };
        println(t.s);
        i = i + 1i64;
    }
}
"#,
            &["item-0", "item-1", "item-2"],
            "rc_fallback_struct_heap_field_drop",
        );
    }

    #[test]
    fn asan_rc_fallback_tuple_moved_no_double_free() {
        // B-2026-06-10-8 move-out safety: the new box value-drop recursion
        // must fire exactly once for the binding's last owner. A returned
        // boxed tuple (moved out of the producer), a whole-binding move
        // (`let u = t`), and a partial field read (`let s = t.1`) each go
        // through the refcounted box; the rc gates the field-free to rc==0,
        // so none double-frees the String. macOS ASAN is the double-free
        // oracle here (Linux additionally checks no leak).
        assert_clean_asan_run(
            r#"
fn make(i: i64) -> (i64, String) { (i, f"made-{i}") }
fn main() {
    let r = make(7i64);
    println(r.1);
    let t = (1i64, f"x-{1}");
    let u = t;
    println(u.1);
    let p = (2i64, f"y-{2}");
    let s = p.1;
    println(s);
}
"#,
            &["made-7", "x-1", "y-2"],
            "rc_fallback_tuple_moved",
        );
    }

    #[test]
    fn asan_user_enum_field_in_struct_heap_payload() {
        // Memory-safety companion to the `enum-in-struct-field` codegen
        // blocker fix (two-pass struct declaration). A struct field whose
        // type is a user enum with a HEAP (String) payload, held in a Vec of
        // such structs, matched + read, then dropped at scope exit. The fix
        // makes the field lower at the enum's real tagged-union shape (not
        // the i64 fall-through); this guards that the heap payload inside the
        // enum inside the struct inside the Vec is freed exactly once — no
        // UAF on the read, no double-free at scope exit. Non-foldable
        // (loop-index) strings so the buffers are real heap allocations.
        assert_clean_asan_run(
            r#"
enum Token { Ident(String), Eof }
struct Spanned { start: i64, tok: Token }
fn main() {
    let mut toks: Vec[Spanned] = Vec.new();
    let mut i = 0i64;
    while i < 3i64 {
        toks.push(Spanned { start: i, tok: Token.Ident(f"id-{i}") });
        i = i + 1i64;
    }
    let mut j = 0;
    while j < toks.len() {
        match toks[j].tok {
            Ident(name) => println(name),
            Eof => println("eof"),
        }
        j = j + 1;
    }
}
"#,
            &["id-0", "id-1", "id-2"],
            "user_enum_field_in_struct_heap_payload",
        );
    }

    // ── Raw-pointer deref load/store (B-2026-06-11-3) ─────────────
    //
    // `unsafe { *p }` on a `*const T` / `*mut T` now emits a real `load`
    // of the pointee (it previously yielded the address), and `*p = val`
    // stores through the pointer (it previously clobbered the pointer
    // variable's own alloca). Both addresses point into a live stack-owned
    // `Array[u8, N]`, so a mis-emitted load/store (reading or writing the
    // wrong address) would trip ASAN with a stack-buffer over/underflow.
    // The loaded/stored value is a scalar `u8`, so there is no heap
    // ownership to double-free; this guards the addressing, not a free.

    #[test]
    fn asan_raw_ptr_deref_load_no_bad_access() {
        assert_clean_asan_run(
            r#"
fn main() {
    let a: Array[u8, 3] = [65u8, 66u8, 67u8];
    let p = a.as_ptr();
    // Safety: `p` addresses element 0 of the live owned array.
    let b: u8 = unsafe { *p };
    println(b);
}
"#,
            &["65"],
            "raw_ptr_deref_load_no_bad_access",
        );
    }

    #[test]
    fn asan_raw_ptr_deref_store_no_bad_access() {
        assert_clean_asan_run(
            r#"
fn main() {
    let mut a: Array[u8, 3] = [65u8, 66u8, 67u8];
    let p = a.as_mut_ptr();
    // Safety: `p` addresses element 0 of the live mutable owned array.
    unsafe { *p = 90u8; }
    let b: u8 = unsafe { *p };
    println(b);
    println(a[0]);
}
"#,
            &["90", "90"],
            "raw_ptr_deref_store_no_bad_access",
        );
    }

    // ── Parser pre-port: recursive-heap gate (AST tree shape) ──────
    //
    // The self-hosting parser builds the AST at scale. karac v1 forbids a
    // direct nested-enum payload (`E_ENUM_NESTED_ENUM_PAYLOAD`), so the AST
    // port wraps recursive edges as `shared enum` (RC pointer = the
    // `Box<Expr>` analog), tagged-union operands as plain `struct`, and
    // sequence children as `Vec[Expr]`:
    //
    //     shared enum Expr { Num(i64), Add(BinOp), Neg(Unary), Call(CallExpr) }
    //     struct BinOp { left: Expr, right: Expr }
    //     struct Unary { operand: Expr }
    //     struct CallExpr { callee: Expr, args: Vec[Expr] }
    //
    // The existing `asan_*_recursive_shared_enum_*` cases above use the
    // DIRECT-payload shape (`Add(Expr, Expr)`); these exercise the
    // struct-wrapped shape the port actually uses, plus the operations the
    // parser hammers: deep build, RC-share fan-out, move-out of Vec
    // elements, and a by-value transform that returns a NEW tree (the
    // parser-rewrite shape). They are the durable artifact of the
    // "recursive-heap family quiet" green-light for the parser port. The
    // Linux-CI LSan job is the authoritative leak gate (mac ASAN catches
    // double-free / UAF only).

    #[test]
    fn asan_struct_wrapped_recursive_tree_freed_once() {
        // Build/drop a struct-wrapped recursive `shared enum` tree (the AST
        // wrapping convention). Each `Expr` child is an RC handle inside a
        // plain-`struct` operand wrapper; the whole tree must be freed
        // exactly once (no leak of the per-node boxes, no double-free of the
        // RC-shared children).
        assert_clean_asan_run(
            r#"
shared enum Expr { Num(i64), Add(BinOp), Neg(Unary) }
struct BinOp { left: Expr, right: Expr }
struct Unary { operand: Expr }
fn eval(e: Expr) -> i64 {
    match e {
        Num(n) => n,
        Add(b) => eval(b.left) + eval(b.right),
        Neg(u) => 0 - eval(u.operand),
    }
}
fn main() {
    let inner = Add(BinOp { left: Num(2), right: Num(3) });
    let sum = Add(BinOp { left: Num(1), right: inner });
    let t = Neg(Unary { operand: sum });
    println(eval(t));
}
"#,
            &["-6"],
            "struct_wrapped_recursive_tree_freed_once",
        );
    }

    #[test]
    fn asan_struct_wrapped_deep_build_and_vec_children_no_leak() {
        // Recursive builder to depth N, trees built in a loop and pushed into
        // a `Vec[Expr]`, and a `Call` variant with a `Vec[Expr]` of
        // heterogeneous children. Looping makes any per-iteration leak of a
        // tree (or its RC children) visible to the Linux-CI LSan gate.
        assert_clean_asan_run(
            r#"
shared enum Expr { Num(i64), Add(BinOp), Neg(Unary), Call(CallExpr) }
struct BinOp { left: Expr, right: Expr }
struct Unary { operand: Expr }
struct CallExpr { callee: Expr, args: Vec[Expr] }
fn eval(e: Expr) -> i64 {
    match e {
        Num(n) => n,
        Add(b) => eval(b.left) + eval(b.right),
        Neg(u) => 0 - eval(u.operand),
        Call(c) => {
            let mut acc = eval(c.callee);
            for a in c.args { acc = acc + eval(a); }
            acc
        }
    }
}
fn build(n: i64) -> Expr {
    if n <= 0 { Num(0) }
    else { Neg(Unary { operand: Add(BinOp { left: Num(n), right: build(n - 1) }) }) }
}
fn main() {
    let mut args: Vec[Expr] = Vec.new();
    let mut i: i64 = 0;
    while i < 10 { args.push(Num(i)); i = i + 1; }
    let call = Call(CallExpr { callee: Num(100), args: args });
    let mut forest: Vec[Expr] = Vec.new();
    let mut j: i64 = 0;
    while j < 30 { forest.push(build(j)); j = j + 1; }
    let mut total: i64 = eval(call);
    for t in forest { total = total + eval(t); }
    println(total);
}
"#,
            &["-80"],
            "struct_wrapped_deep_build_and_vec_children",
        );
    }

    #[test]
    fn asan_struct_wrapped_byvalue_transform_returns_new_tree_no_double_free() {
        // The parser-rewrite shape: consume a tree BY VALUE in a recursive
        // transformer that returns a NEW tree (moving the children through),
        // including move-out of `Vec[Expr]` elements into the transformer in
        // a for-loop. The original tree is consumed exactly once and the
        // rebuilt tree freed exactly once — no double-free of a child that
        // was moved into the new tree, no leak of the consumed original.
        assert_clean_asan_run(
            r#"
shared enum Expr { Num(i64), Add(BinOp), Neg(Unary) }
struct BinOp { left: Expr, right: Expr }
struct Unary { operand: Expr }
fn eval(e: Expr) -> i64 {
    match e {
        Num(n) => n,
        Add(b) => eval(b.left) + eval(b.right),
        Neg(u) => 0 - eval(u.operand),
    }
}
fn fold(e: Expr) -> Expr {
    match e {
        Num(n) => Num(n),
        Neg(u) => Neg(Unary { operand: fold(u.operand) }),
        Add(b) => {
            let l = fold(b.left);
            let r = fold(b.right);
            Add(BinOp { left: l, right: r })
        }
    }
}
fn build(n: i64) -> Expr {
    if n <= 0 { Num(1) }
    else { Add(BinOp { left: Num(n), right: build(n - 1) }) }
}
fn main() {
    let t = build(15);
    let t2 = fold(t);
    let mut v: Vec[Expr] = Vec.new();
    let mut i: i64 = 0;
    while i < 12 { v.push(build(i)); i = i + 1; }
    let mut total: i64 = eval(t2);
    for e in v {
        let folded = fold(e);
        total = total + eval(folded);
    }
    println(total);
}
"#,
            &["419"],
            "struct_wrapped_byvalue_transform_returns_new_tree",
        );
    }

    #[test]
    fn asan_struct_wrapped_move_out_and_rc_share_no_double_free() {
        // `let t2 = t1` (move-out of a tree), a subtree moved into a parent
        // (RC fan-in), and a builder returning a tree by move. Each node is
        // owned by exactly one live path at a time and freed once.
        assert_clean_asan_run(
            r#"
shared enum Expr { Num(i64), Add(BinOp), Neg(Unary) }
struct BinOp { left: Expr, right: Expr }
struct Unary { operand: Expr }
fn eval(e: Expr) -> i64 {
    match e {
        Num(n) => n,
        Add(b) => eval(b.left) + eval(b.right),
        Neg(u) => 0 - eval(u.operand),
    }
}
fn main() {
    let t1 = Add(BinOp { left: Num(3), right: Num(4) });
    let t2 = t1;
    let sub = Add(BinOp { left: Num(2), right: Num(3) });
    let p1 = Add(BinOp { left: sub, right: Num(10) });
    let p2 = Neg(Unary { operand: Num(7) });
    println(eval(t2));
    println(eval(p1));
    println(eval(p2));
}
"#,
            &["7", "15", "-7"],
            "struct_wrapped_move_out_and_rc_share",
        );
    }

    #[test]
    fn asan_struct_wrapped_recursive_cycle_accepted_and_freed() {
        // B-2026-06-14-28 regression (memory side): the struct-wrapped
        // recursive shape (`shared enum Expr` whose recursive edge passes
        // through a plain `struct BinOp`) is a *breakable* cycle — the
        // ownership checker must accept it (see the ownership.rs unit tests),
        // and the resulting RC tree must be freed exactly once. Looped to
        // surface a per-iteration leak to the Linux-CI LSan gate.
        assert_clean_asan_run(
            r#"
shared enum Expr { Num(i64), Bin(BinOp) }
struct BinOp { left: Expr, right: Expr }
fn eval(e: Expr) -> i64 {
    match e {
        Num(n) => n,
        Bin(b) => eval(b.left) + eval(b.right),
    }
}
fn main() {
    let mut i: i64 = 0;
    let mut total: i64 = 0;
    while i < 40 {
        let t: Expr = Bin(BinOp { left: Num(i), right: Bin(BinOp { left: Num(i), right: Num(2) }) });
        total = total + eval(t);
        i = i + 1;
    }
    println(total);
}
"#,
            &["1640"],
            "struct_wrapped_recursive_cycle_accepted_and_freed",
        );
    }

    #[test]
    fn asan_struct_wrapped_enum_payload_rc_children_freed_no_leak() {
        // B-2026-06-14-28 (leak side) — when a `shared enum`'s variant payload
        // is a plain `struct` that owns `shared` fields (`Add(BinOp)` +
        // `struct BinOp { left: Expr, right: Expr }`), the inline RC children
        // must be rc-dec'd when the enum box is freed. Pre-fix, the
        // shared-enum-box RC drop walker (`emit_shared_enum_rc_drop_fn`)
        // classified the struct payload non-walkable (the value-path struct
        // drop `__karac_drop_struct_<S>` has no shared-field arm; a local
        // binding's shared fields are dec'd by its let cleanup, which an enum
        // payload has not) — so every inline `Expr` child leaked (~192 B /
        // tree on macOS `leaks`, silent under mac ASAN; the Linux-CI LSan job
        // is the gate). Looped to make the per-iteration leak visible.
        assert_clean_asan_run(
            r#"
shared enum Expr { Num(i64), Add(BinOp), Neg(Unary) }
struct BinOp { left: Expr, right: Expr }
struct Unary { operand: Expr }
fn eval(e: Expr) -> i64 {
    match e {
        Num(n) => n,
        Add(b) => eval(b.left) + eval(b.right),
        Neg(u) => 0 - eval(u.operand),
    }
}
fn main() {
    let mut i: i64 = 0;
    let mut total: i64 = 0;
    while i < 60 {
        let t: Expr = Neg(Unary { operand: Add(BinOp { left: Num(i), right: Num(2) }) });
        total = total + eval(t);
        i = i + 1;
    }
    println(total);
}
"#,
            &["-1890"],
            "struct_wrapped_enum_payload_rc_children_freed",
        );
    }

    #[test]
    fn asan_vec_of_shared_enum_elements_freed_no_leak() {
        // B-2026-06-14-28 (Vec side) — a `Vec[Expr]` whose elements are a
        // `shared enum` (the AST-port `Call(args: Vec[Expr])` sequence-child
        // shape). Each element is an 8-byte RC pointer; the per-element drop
        // must rc-dec it (and recurse into the box's children), not value-drop
        // it. Pre-fix, `vec_elem_agg_drop_for_type_expr` routed a shared enum
        // to `emit_enum_drop_switch` (the VALUE drop) — which never
        // decremented the refcount, leaking every element (and its struct-
        // wrapped children). Builds Vecs of heterogeneous variants in a loop,
        // consumes some via a for-loop move-out and drops others whole.
        assert_clean_asan_run(
            r#"
shared enum Expr { Num(i64), Add(BinOp) }
struct BinOp { left: Expr, right: Expr }
fn eval(e: Expr) -> i64 {
    match e {
        Num(n) => n,
        Add(b) => eval(b.left) + eval(b.right),
    }
}
fn build(n: i64) -> Vec[Expr] {
    let mut v: Vec[Expr] = Vec.new();
    let mut i: i64 = 0;
    while i < n {
        v.push(Add(BinOp { left: Num(i), right: Num(1) }));
        i = i + 1;
    }
    v
}
fn main() {
    let mut total: i64 = 0;
    let mut k: i64 = 0;
    while k < 20 {
        let v: Vec[Expr] = build(8);
        // consume via for-loop move-out
        for e in v {
            total = total + eval(e);
        }
        // a second Vec dropped WHOLE (not consumed)
        let w: Vec[Expr] = build(4);
        total = total + (w.len() as i64);
        k = k + 1;
    }
    println(total);
}
"#,
            &["800"],
            "vec_of_shared_enum_elements_freed",
        );
    }

    #[test]
    fn asan_match_byvalue_shared_enum_bind_without_consume_no_leak() {
        // B-2026-06-14-29 — a `match` on a BY-VALUE shared enum whose taken arm
        // BINDS the struct payload (`Add(b) =>`) but does NOT consume its shared
        // children, returning a FRESH tree instead. The original scrutinee box +
        // its children must be freed exactly once at the function's RC cleanup.
        //
        // Root cause / closure: this was a DUPLICATE of B-2026-06-14-28 bug #3
        // (the shared-enum box-drop walker `emit_shared_enum_rc_drop_fn` /
        // `emit_nested_struct_shared_rc_decs` not recursing into a STRUCT
        // payload's shared fields), NOT a distinct `compile_match` suppression
        // bug. A malloc/free-balance bisect over `0890627c` (the B-28 fix)
        // showed the struct-wrapped `Add(BinOp)` shape leaked unconditionally
        // pre-fix (independent of whether the arm consumed `b` — the "leaky"
        // bind-ignore variant and the fully-consuming variant leaked IDENTICALLY,
        // disproving the consumption-gated hypothesis and the
        // `control_flow_match.rs` locus) and is balanced post-fix; the
        // direct-payload `Add(Expr,Expr)` shape never leaked. So the match path
        // needed no change — this test pins the no-leak so any reintroduction in
        // the box-drop walker is caught by the Linux-CI LSan gate. Looped to
        // surface a per-iteration leak. (struct-wrapped shape.)
        assert_clean_asan_run(
            r#"
shared enum Expr { Num(i64), Add(BinOp) }
struct BinOp { left: Expr, right: Expr }
fn eval(e: Expr) -> i64 {
    match e {
        Num(n) => n,
        Add(b) => eval(b.left) + eval(b.right),
    }
}
fn fold(e: Expr) -> Expr {
    match e {
        Num(n) => Num(n),
        Add(b) => Add(BinOp { left: Num(99), right: Num(99) }),
    }
}
fn build(n: i64) -> Expr {
    if n <= 0 { Num(n) }
    else { Add(BinOp { left: Num(n), right: build(n - 1) }) }
}
fn main() {
    let mut i: i64 = 0;
    let mut total: i64 = 0;
    while i < 60 {
        let t: Expr = build(3);
        let t2: Expr = fold(t);
        total = total + eval(t2);
        i = i + 1;
    }
    println(total);
}
"#,
            &["11880"],
            "match_byvalue_bind_without_consume_struct",
        );
    }

    #[test]
    fn asan_match_byvalue_shared_enum_bind_without_consume_direct_payload_no_leak() {
        // B-2026-06-14-29 (direct-payload axis) — the same bind-without-consume
        // shape on a DIRECT-payload shared enum `Add(Expr, Expr)` (no struct
        // wrapper). The ledger flagged this shape as also reproducing; the
        // bisect showed it was in fact already leak-free at the B-28 parent
        // commit (the box-drop walker recursed correctly for direct payloads).
        // Pinned here so the two axes stay covered together.
        assert_clean_asan_run(
            r#"
shared enum Expr { Num(i64), Add(Expr, Expr) }
fn eval(e: Expr) -> i64 {
    match e {
        Num(n) => n,
        Add(l, r) => eval(l) + eval(r),
    }
}
fn fold(e: Expr) -> Expr {
    match e {
        Num(n) => Num(n),
        Add(l, r) => Add(Num(99), Num(99)),
    }
}
fn build(n: i64) -> Expr {
    if n <= 0 { Num(n) }
    else { Add(Num(n), build(n - 1)) }
}
fn main() {
    let mut i: i64 = 0;
    let mut total: i64 = 0;
    while i < 60 {
        let t: Expr = build(3);
        let t2: Expr = fold(t);
        total = total + eval(t2);
        i = i + 1;
    }
    println(total);
}
"#,
            &["11880"],
            "match_byvalue_bind_without_consume_direct",
        );
    }

    #[test]
    fn asan_match_byvalue_shared_enum_fully_consumed_arm_no_double_free() {
        // B-2026-06-14-29 no-regression direction: the already-OK FULLY-CONSUMING
        // arm (`Add(b) => eval(b.left) + eval(b.right)` consumes both shared
        // children). The scrutinee box + its children must be freed exactly once
        // — no double-free of a child that the arm consumed, no leak. Bisect
        // confirmed this leaked equally with the bind-ignore variant pre-B-28 and
        // is balanced post-fix, so it locks in that the box-drop fix did not
        // introduce a double-free in the consuming path. (struct-wrapped shape.)
        assert_clean_asan_run(
            r#"
shared enum Expr { Num(i64), Add(BinOp) }
struct BinOp { left: Expr, right: Expr }
fn fold(e: Expr) -> Expr {
    match e {
        Num(n) => Num(n),
        Add(b) => Add(BinOp { left: fold(b.left), right: fold(b.right) }),
    }
}
fn eval(e: Expr) -> i64 {
    match e {
        Num(n) => n,
        Add(b) => eval(b.left) + eval(b.right),
    }
}
fn build(n: i64) -> Expr {
    if n <= 0 { Num(n) }
    else { Add(BinOp { left: Num(n), right: build(n - 1) }) }
}
fn main() {
    let mut i: i64 = 0;
    let mut total: i64 = 0;
    while i < 60 {
        let t: Expr = build(3);
        let t2: Expr = fold(t);
        total = total + eval(t2);
        i = i + 1;
    }
    println(total);
}
"#,
            &["360"],
            "match_byvalue_fully_consumed_arm",
        );
    }

    #[test]
    fn asan_match_byvalue_shared_enum_reconstruct_from_fresh_locals_no_leak() {
        // B-2026-06-14-29 no-regression direction: the already-OK
        // RECONSTRUCT-FROM-FRESH-LOCALS arm — `Add(b)` binds `b`, ignores it, and
        // rebuilds from fresh `let`-bound locals (`let l = Num(1); let r = Num(2);
        // Add(BinOp { left: l, right: r })`). The fresh locals are moved into the
        // new tree (no double-free) and the original box + children freed once
        // (no leak). The `Num(n)` arm (no shared child) is exercised by `build`.
        assert_clean_asan_run(
            r#"
shared enum Expr { Num(i64), Add(BinOp) }
struct BinOp { left: Expr, right: Expr }
fn eval(e: Expr) -> i64 {
    match e {
        Num(n) => n,
        Add(b) => eval(b.left) + eval(b.right),
    }
}
fn fold(e: Expr) -> Expr {
    match e {
        Num(n) => Num(n),
        Add(b) => {
            let l: Expr = Num(1);
            let r: Expr = Num(2);
            Add(BinOp { left: l, right: r })
        }
    }
}
fn build(n: i64) -> Expr {
    if n <= 0 { Num(n) }
    else { Add(BinOp { left: Num(n), right: build(n - 1) }) }
}
fn main() {
    let mut i: i64 = 0;
    let mut total: i64 = 0;
    while i < 60 {
        let t: Expr = build(3);
        let t2: Expr = fold(t);
        total = total + eval(t2);
        i = i + 1;
    }
    println(total);
}
"#,
            &["180"],
            "match_byvalue_reconstruct_from_fresh_locals",
        );
    }

    #[test]
    fn asan_struct_wrapped_vec_field_in_struct_payload_no_leak() {
        // B-2026-06-14-31 (leak #1) — a `Vec[Expr]` FIELD inside a struct
        // payload of a shared enum (`Call(CallExpr { args: Vec[Expr] })`, the
        // AST-port sequence-child shape), with the enum box dropped WHOLE
        // (never consumed). Pre-fix, `emit_nested_struct_shared_rc_decs` had no
        // Vec arm: the inline Vec's `{data,len,cap}` buffer (an 80-byte direct
        // alloc) AND every element box leaked when the shared-enum box freed —
        // silent under mac ASAN, caught by the Linux-CI LSan gate. Also covers
        // a struct whose ONLY shared content is a `Vec[shared]` field
        // (`Wrap { items: Vec[Expr] }`) — that requires the
        // `field_owns_shared`/`struct_owns_shared_field` Vec[shared] classifier
        // arm, or the variant gets no drop block at all. Looped to surface a
        // per-iteration leak.
        assert_clean_asan_run(
            r#"
shared enum Expr { Num(i64), Call(CallExpr), Wrapped(Wrap) }
struct CallExpr { callee: Expr, args: Vec[Expr] }
struct Wrap { items: Vec[Expr] }
fn sum_args(e: Expr) -> i64 {
    match e {
        Num(n) => n,
        Call(c) => c.args.len() as i64,
        Wrapped(w) => w.items.len() as i64,
    }
}
fn main() {
    let mut total: i64 = 0;
    let mut k: i64 = 0;
    while k < 25 {
        let mut args: Vec[Expr] = Vec.new();
        let mut i: i64 = 0;
        while i < 10 { args.push(Num(i)); i = i + 1; }
        let call: Expr = Call(CallExpr { callee: Num(100), args: args });
        total = total + sum_args(call);
        // a struct whose ONLY shared content is a Vec[shared] field
        let mut items: Vec[Expr] = Vec.new();
        let mut j: i64 = 0;
        while j < 5 { items.push(Num(j)); j = j + 1; }
        let wrapped: Expr = Wrapped(Wrap { items: items });
        total = total + sum_args(wrapped);
        k = k + 1;
    }
    println(total);
}
"#,
            &["375"],
            "struct_wrapped_vec_field_in_struct_payload",
        );
    }

    #[test]
    fn asan_struct_wrapped_move_out_then_consume_no_leak() {
        // B-2026-06-14-31 (leak #2) — `let t2 = t1` (a shared-enum local MOVED
        // to another local) that is then CONSUMED by-value (`eval(t2)`).
        // Pre-fix, the shared-enum let-binding's Identifier-RHS path called the
        // value-enum move suppressor, which emitted a SPURIOUS aliasing-acquire
        // `emit_refcount_inc` on the source on TOP of the destination inc the
        // shared-info path already emitted — pinning the box at rc=1 after both
        // scope-exit `RcDec`s, leaking the whole tree (silent under mac ASAN).
        // The no-double-free crux: the SAME shape WITHOUT consume, and the
        // subtree-into-parent move (a separate edge), must stay leak-free AND
        // not double-free. Looped to surface a per-iteration leak.
        assert_clean_asan_run(
            r#"
shared enum Expr { Num(i64), Add(BinOp), Neg(Unary) }
struct BinOp { left: Expr, right: Expr }
struct Unary { operand: Expr }
fn eval(e: Expr) -> i64 {
    match e {
        Num(n) => n,
        Add(b) => eval(b.left) + eval(b.right),
        Neg(u) => 0 - eval(u.operand),
    }
}
fn main() {
    let mut total: i64 = 0;
    let mut i: i64 = 0;
    while i < 40 {
        // move-out then consume
        let t1 = Add(BinOp { left: Num(3), right: Num(4) });
        let t2 = t1;
        total = total + eval(t2);
        // move-out then drop WHOLE (no consume)
        let u1 = Neg(Unary { operand: Num(5) });
        let u2 = u1;
        match u2 { Num(n) => total = total + n, Add(b) => total = total + 0, Neg(x) => total = total + 0 }
        // subtree moved into a parent literal, then consumed
        let sub = Add(BinOp { left: Num(2), right: Num(3) });
        let p1 = Add(BinOp { left: sub, right: Num(10) });
        total = total + eval(p1);
        i = i + 1;
    }
    println(total);
}
"#,
            &["880"],
            "struct_wrapped_move_out_then_consume",
        );
    }

    #[test]
    fn asan_value_enum_nested_struct_vec_shared_inplace_drop_no_leak_no_double_free() {
        // B-2026-06-14-34 — a NON-shared enum (`Stmt`) whose variant wraps a
        // struct (`CallExpr`) that owns BOTH an inline `shared` field (`callee:
        // Expr`) AND a `Vec[shared]` (`args: Vec[Expr]`), dropped IN PLACE via
        // the value-drop synthesizer `emit_enum_drop_switch`. This is the shape
        // the self-host lexer hit: the B-31 `Vec[shared]` drain arm of
        // `emit_nested_struct_shared_rc_decs` (1) appended its blocks to
        // `self.current_fn` (the OUTER fn, not the drop fn) → cross-function
        // basic-block reference → module-verification failure, masked on
        // `--test codegen` because `compile_to_object` skips verification and
        // the optimizer DCE'd the orphan blocks; and (2) unconditionally
        // `free`d the Vec buffer that `__karac_drop_struct_CallExpr` ALSO freed
        // → double-free once the blocks became reachable, plus a use-after-free
        // if the drain ran after the buffer free. The fix threads the real
        // `drop_fn` + scoped `current_fn` through the walker, gates the buffer
        // `free` on `owns_buffer_free` (false in the value path — the struct
        // drop owns it), and orders the element-drain BEFORE the struct drop.
        // On Linux CI/LSan this faults if the element rc-dec drain regresses (a
        // leak); on macOS it is the double-free / UAF gate.
        assert_clean_asan_run(
            r#"
shared enum Expr { Num(i64), Add(BinOp) }
struct BinOp { left: Expr, right: Expr }
enum Stmt { Call(CallExpr) }
struct CallExpr { callee: Expr, args: Vec[Expr] }
fn main() {
    let mut total: i64 = 0;
    let mut j: i64 = 0;
    while j < 20 {
        let mut args: Vec[Expr] = Vec.new();
        let mut i: i64 = 0;
        while i < 3 { args.push(Num(i)); i = i + 1; }
        // Dropped IN PLACE (wildcard match consumes nothing) — exercises the
        // VALUE-drop of `Stmt`, whose `Call(CallExpr)` payload owns a
        // `Vec[shared Expr]`: the walker must rc-dec the inline `callee` Expr
        // box AND drain the `args` element boxes, while `__karac_drop_struct_
        // CallExpr` frees the `args` buffer exactly once (and runs AFTER the
        // drain, so the drain reads a live buffer).
        let s = Call(CallExpr { callee: Num(100), args: args });
        let k = match s { Call(_) => 1 };
        total = total + k;
        j = j + 1;
    }
    println(total);
}
"#,
            &["20"],
            "value_enum_nested_struct_vec_shared_inplace_drop",
        );
    }

    #[test]
    fn asan_self_field_vec_index_match_move_out_no_double_free() {
        // #32 (phase-12 self-hosting, parser stage): reading + matching a token
        // through a `self`-field-rooted Vec index — `self.toks[self.pos].tok` —
        // is the parser's core token-access shape. A scalar field read and a
        // payload-binding match both go through `compile_field_access`'s generic
        // value path, which now resolves the element struct type via
        // `type_name_of_expr`'s `Index` arm (the fix). The payload-binding match
        // moves a `String` out of the element via the existing #16/#25 source
        // suppression; this asserts no double-free (macOS ASAN) and no leak
        // (Linux LSan). Every heap element is CONSUMED by `take` so the dropped
        // Vec holds no live enum-String payloads — isolating the #32 read +
        // move-out from the SEPARATE pre-existing Vec[struct-with-enum-field]-
        // element-drop leak ([#35]), which a Vec left holding live heap-enum
        // elements would otherwise trip.
        assert_clean_asan_run(
            r#"
enum Tk { A, Id(String), Num(i64) }
struct Sp { tok: Tk, off: i64 }
struct P { toks: Vec[Sp], pos: i64 }
impl P {
    fn off_now(ref self) -> i64 { self.toks[self.pos].off }
    fn kind_now(ref self) -> i64 {
        match self.toks[self.pos].tok { Id(_) => 1, Num(_) => 2, A => 3 }
    }
    fn take(mut ref self) -> String {
        match self.toks[self.pos].tok {
            Id(s) => { self.pos = self.pos + 1; s }
            Num(n) => { self.pos = self.pos + 1; n.to_string() }
            A => { self.pos = self.pos + 1; "a".to_string() }
        }
    }
}
fn main() {
    let mut w: Vec[Sp] = Vec.new();
    w.push(Sp { tok: Tk.Id("hello".to_string()), off: 5 });
    w.push(Sp { tok: Tk.Id("world".to_string()), off: 9 });
    let mut p = P { toks: w, pos: 0 };
    println(p.off_now().to_string());
    println(p.kind_now().to_string());
    println(p.take());
    println(p.off_now().to_string());
    println(p.take());
}
"#,
            &["5", "1", "hello", "9", "world"],
            "self_field_vec_index_match_move_out",
        );
    }

    #[test]
    fn asan_borrowed_index_field_enum_scrutinee_binding_outlives_container_no_uaf() {
        // #38 (phase-12 self-hosting, parser stage): matching a `.token`
        // FieldAccess rooted on a Vec `Index` (`self.toks[self.pos].tok`) on a
        // BORROWED receiver, binding a `String` payload that ESCAPES the call
        // (returned out, outliving the `Parser`'s token `Vec`). Without the
        // `clone_borrowed_index_field_enum_scrutinee` clone, the binding
        // shallow-ALIASES the Vec element's `{ptr,len,cap}`; when the container
        // drops it frees that buffer, leaving the escaped String dangling — a
        // use-after-free on the next read and a double-free at its own drop.
        //
        // This isolated ASAN test was IMPOSSIBLE before [#35] was fixed: the
        // old struct-field Vec drop freed only the buffer and leaked the live
        // `Id(String)` element, so the aliased buffer stayed allocated and the
        // dangle was never an observable UAF. Now that [#35] drains each
        // element's payload on the container's drop, the alias (if the #38
        // clone is removed) becomes a genuine ASAN-flaggable UAF + double-free.
        // The two fixes are complementary: #35 frees the original element once,
        // #38 gives the escaped binding an independent buffer freed once.
        // Looped + heap-sized payloads so a per-iteration fault is unmissable.
        assert_clean_asan_run(
            r#"
enum Tk { Id(String), Num(i64) }
struct Sp { tok: Tk, off: i64 }
struct P { toks: Vec[Sp], pos: i64 }
impl P {
    fn name_now(ref self) -> String {
        match self.toks[self.pos].tok {
            Id(s) => s,
            Num(n) => n.to_string(),
        }
    }
}
fn mk() -> Vec[Sp] {
    let mut w: Vec[Sp] = Vec.new();
    w.push(Sp { tok: Tk.Id("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string()), off: 5 });
    w
}
fn grab() -> String {
    let p = P { toks: mk(), pos: 0 };
    p.name_now()
}
fn main() {
    let mut total: i64 = 0;
    let mut i: i64 = 0;
    while i < 50 {
        let n = grab();
        total = total + n.len();
        i = i + 1;
    }
    println(total);
}
"#,
            &["2000"],
            "borrowed_index_field_enum_scrutinee_binding_outlives_container",
        );
    }

    #[test]
    fn asan_match_variant_name_shared_across_enums_string_payload_no_leak() {
        // #39 (phase-12 self-hosting, parser stage): a bare variant name shared
        // by a value enum (`Tok.Str`) and a shared enum (`Expr.Str`) used to
        // bind a shared-enum String payload off the WRONG enum's word offsets —
        // reading a single word for a multi-word `SLit` and reconstructing a
        // garbage buffer pointer. Now that resolution pins to the match
        // scrutinee's own enum, the bound `n.value` owns a real buffer; the
        // arm consumes it (returns it out) so the LSan gate proves the
        // String is freed exactly once per iteration — no leak, no double-free.
        // Looped so a per-iteration leak accumulates visibly. This is the
        // parser's `Token.Str`/`Expr.Str` payload-read shape.
        assert_clean_asan_run(
            r#"
struct Sp { line: i64, column: i64, offset: i64, length: i64 }
struct SLit { value: String, span: Sp }
enum Tok { Str(String, Sp), Int(i64) }
shared enum Expr { Int(i64), Boolish(i64), Str(SLit) }
fn text_of(e: Expr) -> String {
    match e {
        Int(v) => v.to_string(),
        Boolish(v) => v.to_string(),
        Str(n) => n.value,
    }
}
fn tok_kind(t: Tok) -> i64 {
    match t { Str(s, sp) => 1, Int(v) => 2 }
}
fn main() {
    let mut total: i64 = 0;
    let mut i: i64 = 0;
    while i < 50 {
        let t = Tok.Str("tok".to_string(), Sp { line: 1, column: 1, offset: 0, length: 3 });
        total = total + tok_kind(t);
        let n = SLit { value: "hello world".to_string(), span: Sp { line: 1, column: 1, offset: 7, length: 11 } };
        let e = Expr.Str(n);
        let s = text_of(e);
        total = total + s.len();
        i = i + 1;
    }
    println(total);
}
"#,
            &["600"],
            "match_variant_name_shared_across_enums_string_payload",
        );
    }

    #[test]
    fn asan_shared_enum_recursive_struct_payload_string_freed_no_leak() {
        // A `shared enum E` with a recursive Binary variant (`Add(Bin)`,
        // `Bin { left: E, right: E }`) AND a leaf variant whose plain-struct
        // payload owns a String (`Ident(Id)`, `Id { name: String }`) — the
        // self-hosted parser's `Expr.Binary(BinaryExpr)` / `Expr.Ident(IdentExpr
        // { name })` shape. When a leaf is a CHILD of a Binary box, the box's
        // recursive rc-drop frees the child box but used to SKIP its inline
        // struct payload's String (`field_is_walkable` only flagged structs
        // owning a SHARED field, not a String). The single-level case was freed
        // via the top-level match path, masking it. Fix: `field_is_walkable` /
        // the rc-drop struct branch now also walk a struct payload that owns a
        // String/Vec/heap field (`type_expr_has_drop_heap`), and
        // `emit_nested_struct_shared_rc_decs` gained a direct-String arm. Long
        // identifiers (≥36 B) so the leak is unambiguous under LSan (short ones
        // evade it via freed-but-reachable pointers).
        assert_clean_asan_run(
            r#"
struct Id { name: String, off: i64 }
struct Bin { left: E, right: E, off: i64 }
shared enum E { Ident(Id), Add(Bin) }
fn render(e: E) -> String {
    let mut out = "".to_string();
    match e {
        Ident(n) => { out.push_str(n.name); }
        Add(b) => {
            out.push_str(render(b.left));
            out.push_str(render(b.right));
        }
    }
    out
}
fn ident(s: String) -> E {
    E.Ident(Id { name: s, off: 0 })
}
fn make() -> E {
    E.Add(Bin {
        left: ident("left_identifier_long_enough_to_force_heap".to_string()),
        right: ident("right_identifier_long_enough_to_force_heap".to_string()),
        off: 0,
    })
}
fn main() {
    let mut total: i64 = 0;
    let mut i: i64 = 0;
    while i < 20 {
        total = total + render(make()).len();
        i = i + 1;
    }
    println(total);
}
"#,
            &["1660"],
            "shared_enum_recursive_struct_payload_string_freed",
        );
    }

    #[test]
    fn asan_shared_enum_struct_variant_whole_binding_readonly_no_leak() {
        // B-2026-06-19-4 (the ledger's EXACT repro): a `shared enum E` whose
        // struct-variant payload owns a String (`Ident(Id { name: String })`),
        // bound WHOLE as `n` in a match arm that only READS it (`n.name.len()`)
        // — NOT consumed, and NOT a child of a recursive box. This is the
        // single-level direct case `render(make())` the sibling recursive test
        // (`..._recursive_struct_payload_string_freed`) does not exercise: there
        // the arm CONSUMES via `push_str(n.name)` and the leaf is a CHILD of a
        // Binary box. Here `n` is a shallow by-value VIEW of the still-live RC
        // box's inline payload; `bind_pattern_values` deliberately does NOT
        // `track_struct_var` it for a shared-enum scrutinee (pattern_binding.rs,
        // `!pattern_binding_scrutinee_is_shared_enum`), so the box's rc-drop
        // walker is the SOLE owner of `name`'s buffer. The box-walk frees it
        // (8a78ee6d / phase-12 #41: `type_expr_has_drop_heap` now flags a
        // String-owning plain-struct payload walkable). Exactly one free → no
        // leak (this test) and no double-free (the read-only binding never
        // frees). ≥36 B name so the leak is unambiguous under LSan.
        assert_clean_asan_run(
            r#"
struct Id { name: String, off: i64 }
shared enum E { Ident(Id) }
fn render(e: E) -> i64 {
    match e {
        Ident(n) => { n.name.len() }
    }
}
fn make() -> E {
    E.Ident(Id { name: "an_identifier_long_enough_to_force_heap".to_string(), off: 0 })
}
fn main() {
    let mut total: i64 = 0;
    let mut i: i64 = 0;
    while i < 20 {
        total = total + render(make());
        i = i + 1;
    }
    println(total);
}
"#,
            &["780"],
            "shared_enum_struct_variant_whole_binding_readonly",
        );
    }

    #[test]
    fn asan_struct_field_vec_of_struct_with_enum_field_drop_no_leak() {
        // #35 (phase-12 self-hosting, parser stage): a `Vec[Sp]`
        // (`Sp { tok: Tk, off }`, heap enum `Tk`) held in a struct FIELD
        // (`P { toks: Vec[Sp] }`), dropped with live `Id(String)` elements
        // still in the buffer. The owning struct's synthesized drop
        // (`__karac_drop_struct_P`) used to free only the Vec's `{ptr,len,cap}`
        // buffer and NEVER drain elements, so every unconsumed element's String
        // payload leaked — the Vec-element peer of the #15 / #18 / #21
        // struct-drop-ignores-heap-leaf family. The fix drains each element
        // through `vec_elem_agg_drop_for_type_expr` (→ `Sp`'s
        // `__karac_drop_struct_Sp` → `Tk`'s `__karac_drop_Tk` switch) before
        // the buffer free, so each live element's payload frees exactly once.
        // Looped so a per-iteration leak accumulates visibly under LSan; the
        // dropped Vec keeps ALL elements live (nothing consumed) to exercise
        // the element-drain path. This is the parser's own shape: the `Parser`
        // drops its `Vec[SpannedToken]` with unconsumed trailing tokens whose
        // `Token` enums carry Strings.
        // `mk` returns the `Vec[Sp]` (its local cleanup is move-suppressed),
        // so inside the loop the ONLY live owner of the buffer is `p`, and the
        // ONLY cleanup is `__karac_drop_struct_P` — there is no sibling local
        // Vec-of-aggs drain to mask the gap. This is the parser's exact shape:
        // `Parser { tokens: Vec[SpannedToken] }` holds the sole reference and
        // is dropped with unconsumed tokens.
        assert_clean_asan_run(
            r#"
enum Tk { A, Id(String), Num(i64) }
struct Sp { tok: Tk, off: i64 }
struct P { toks: Vec[Sp], pos: i64 }
fn mk() -> Vec[Sp] {
    let mut w: Vec[Sp] = Vec.new();
    w.push(Sp { tok: Tk.Id("ident_long_enough_to_force_a_heap_buffer".to_string()), off: 5 });
    w.push(Sp { tok: Tk.Id("another_heap_allocated_identifier_string".to_string()), off: 9 });
    w.push(Sp { tok: Tk.Num(7), off: 3 });
    w
}
fn main() {
    let mut total: i64 = 0;
    let mut i: i64 = 0;
    while i < 50 {
        let p = P { toks: mk(), pos: 0 };
        total = total + p.toks[0].off + p.toks[1].off + p.toks[2].off;
        i = i + 1;
    }
    println(total);
}
"#,
            &["850"],
            "struct_field_vec_of_struct_with_enum_field_drop",
        );
    }

    // PINNED REPRODUCER for phase-12 #43 (label-in-plain-drop leak), deferred.
    // `#[ignore]`d because it FAILS under LeakSanitizer (the leak it pins is not
    // yet fixed) — leaving it un-ignored would red the Linux-CI `memory-sanitizer`
    // gate. Run it deliberately under the authoritative LSan harness to observe
    // the leak:
    //   scripts/lsan-local.sh asan_vec_of_struct_labeled_plain_drop_leaks_pinned_43 -- --ignored
    // (On macOS — no LeakSanitizer — it passes vacuously; the leak only shows
    // under LSan.) **Un-ignore when #43 lands.**
    //
    // The leak: a LABELED call node (`Arg { label: Some(..), value: Expr }`) is
    // built and PLAIN-DROPPED without consuming (read by `ref`, no `for a in
    // args` / render). The shared-enum RC box-walker rc-dec's the shared `value`
    // (refcount-safe) and frees the Vec buffer, but does NOT free the plain
    // `Option[String]` label — re-adding that free (#42's removed arm) instead
    // double-frees it against a by-value consumer that ALSO frees it, because
    // the for-loop's move-out suppression can't reach the box's retained payload
    // alias. The render/CONSUME path (the real parser path) is leak-free
    // (`asan_vec_of_struct_shared_and_option_field_consumed_no_leak`); only this
    // build-then-discard-labeled-call shape leaks. Closing it needs field-granular
    // move-out suppression for shared-enum payloads (zero the plain-heap caps,
    // keep the shared-edge pointers) — its own slice. See phase-12 §"Parser in
    // Kāra" #43. All Strings >=36 bytes for LSan visibility.
    #[test]
    #[ignore = "phase-12 #43: known label-plain-drop leak, deferred; reproduces under LSan; un-ignore when the field-granular shared-enum move-out suppression lands"]
    fn asan_vec_of_struct_labeled_plain_drop_leaks_pinned_43() {
        assert_clean_asan_run(
            r#"
shared enum Expr { Lit(LitNode), Call(CallNode) }
struct LitNode { name: String, val: i64 }
struct Arg { label: Option[String], value: Expr }
struct CallNode { callee: Expr, args: Vec[Arg], nargs: i64 }
fn lit(s: String, v: i64) -> Expr { Expr.Lit(LitNode { name: s, val: v }) }
fn mk() -> Expr {
    let mut args: Vec[Arg] = Vec.new();
    args.push(Arg { label: Some("first_argument_label_long_enough_to_force_heap".to_string()), value: lit("first_arg_value_identifier_long_enough_for_heap".to_string(), 1) });
    args.push(Arg { label: None, value: lit("second_arg_value_identifier_long_enough_for_heap".to_string(), 2) });
    Expr.Call(CallNode { callee: lit("callee_identifier_name_long_enough_for_heap_buf".to_string(), 100), args: args, nargs: 2 })
}
fn root(e: ref Expr) -> i64 {
    match e {
        Lit(n) => n.val,
        Call(c) => c.nargs,
    }
}
fn main() {
    let mut total: i64 = 0;
    let mut i: i64 = 0;
    while i < 50 {
        let e = mk();
        total = total + root(e);
        i = i + 1;
    }
    println(total);
}
"#,
            &["100"],
            "vec_of_struct_labeled_plain_drop_leaks_pinned_43",
        );
    }

    #[test]
    fn asan_vec_of_struct_with_shared_field_drop_no_leak() {
        // B-2026-06-19 (phase-12 parser slice 2a): the self-hosted parser's
        // `Call(CallExpr { args: Vec[CallArg] })` shape — a `Vec[Arg]` whose
        // element struct `Arg { value: Expr }` owns a SHARED-enum field, the Vec
        // living inside a shared-enum struct payload (`Call(CallNode)`). The
        // whole `Call` tree is built and dropped on the PLAIN scope-exit path
        // (read by `ref`, never consumed), so its cleanup is the shared-enum RC
        // drop walker. The Vec element's value drop (`__karac_drop_struct_Arg`)
        // skips its shared `value` field by design (a local struct's shared
        // fields are rc-dec'd by the `let` cleanup — B-2026-06-14-28 #3), but a
        // Vec ELEMENT has no let-cleanup, so each arg's shared box leaked once
        // per element. `vec_elem_agg_drop_for_type_expr` now routes a
        // shared-owning struct element through `__karac_vec_elem_full_drop_Arg`
        // (the value drop PLUS `emit_nested_struct_shared_rc_decs`, which
        // rc-dec's the shared field). The drain is refcount-safe in BOTH the
        // pure-drop path (here) and the by-value-consume path (the renderer /
        // later compiler phases) — the consume site rc-incs the shared handle
        // on the element copy, balancing the box-walker's dec.
        // Looped so a per-iteration leak accumulates visibly under the
        // authoritative Linux-CI LSan gate; every String is >=36 bytes so the
        // freed-but-reachable short-String LSan blind spot can't mask it.
        // (NOTE: the `CallArg.label: Option[String]` field is intentionally
        // omitted here. The shared-value drain above is consume-safe; an
        // Option[String]/String *plain* heap field is NOT refcount-protected,
        // so freeing it in the box-walker double-frees against a by-value
        // consumer that also frees it — the render/consume path stays leak-free
        // because the consumer frees the label, but a labeled call built and
        // plain-dropped without consuming leaks its label. Closing that residual
        // needs move-out suppression at the Vec[struct] consume site — tracked
        // as a phase-12 parser follow-on, not a regression here.)
        assert_clean_asan_run(
            r#"
shared enum Expr { Lit(LitNode), Call(CallNode) }
struct LitNode { name: String, val: i64 }
struct Arg { value: Expr }
struct CallNode { callee: Expr, args: Vec[Arg], nargs: i64 }
fn lit(s: String, v: i64) -> Expr { Expr.Lit(LitNode { name: s, val: v }) }
fn mk() -> Expr {
    let mut args: Vec[Arg] = Vec.new();
    args.push(Arg { value: lit("first_arg_value_identifier_long_enough_for_heap".to_string(), 1) });
    args.push(Arg { value: lit("second_arg_value_identifier_long_enough_for_heap".to_string(), 2) });
    Expr.Call(CallNode { callee: lit("callee_identifier_name_long_enough_for_heap_buf".to_string(), 100), args: args, nargs: 2 })
}
fn root(e: ref Expr) -> i64 {
    match e {
        Lit(n) => n.val,
        Call(c) => c.nargs,
    }
}
fn main() {
    let mut total: i64 = 0;
    let mut i: i64 = 0;
    while i < 50 {
        let e = mk();
        total = total + root(e);
        i = i + 1;
    }
    println(total);
}
"#,
            &["100"],
            "vec_of_struct_with_shared_field_drop",
        );
    }

    #[test]
    fn asan_vec_of_struct_shared_and_option_field_consumed_no_leak() {
        // B-2026-06-19 (phase-12 parser slice 2a) — the CONSUME peer of
        // `asan_vec_of_struct_with_shared_field_drop_no_leak`, and the shape the
        // self-hosted parser's renderer / later phases actually exercise:
        // `Arg { label: Option[String], value: Expr }` elements of a
        // `Vec[Arg]` are MOVED OUT (by-value match + `for a in args` +
        // destructure) and consumed — the `Option[String]` label is read then
        // dropped, the shared `value` recursively consumed. Here the consumer
        // frees the label (so the Option[String] is leak-free on THIS path,
        // unlike the pure-drop path where it's a documented residual), and the
        // shared-value recursion frees each box AND its inner `LitNode.name`
        // String (the force-synth pre-pass in `emit_nested_struct_shared_rc_decs`
        // makes the rc-dec dispatch to the recursive `__karac_rc_drop_Expr`
        // rather than inline-`free`ing the box and stranding the name). Looped;
        // all Strings >=36 bytes for LSan visibility.
        assert_clean_asan_run(
            r#"
shared enum Expr { Lit(LitNode), Call(CallNode) }
struct LitNode { name: String, val: i64 }
struct Arg { label: Option[String], value: Expr }
struct CallNode { callee: Expr, args: Vec[Arg], nargs: i64 }
fn lit(s: String, v: i64) -> Expr { Expr.Lit(LitNode { name: s, val: v }) }
fn mk() -> Expr {
    let mut args: Vec[Arg] = Vec.new();
    args.push(Arg { label: Some("first_argument_label_long_enough_to_force_heap".to_string()), value: lit("first_arg_value_identifier_long_enough_for_heap".to_string(), 1) });
    args.push(Arg { label: None, value: lit("second_arg_value_identifier_long_enough_for_heap".to_string(), 2) });
    Expr.Call(CallNode { callee: lit("callee_identifier_name_long_enough_for_heap_buf".to_string(), 100), args: args, nargs: 2 })
}
fn consume(e: Expr) -> i64 {
    match e {
        Lit(n) => n.val,
        Call(c) => {
            let CallNode { callee, args, nargs } = c;
            let mut acc = consume(callee) + nargs;
            for a in args {
                let Arg { label, value } = a;
                match label {
                    Some(l) => { if l.len() > 0 { acc = acc + 1000; } }
                    None => {}
                }
                acc = acc + consume(value);
            }
            acc
        }
    }
}
fn main() {
    let mut total: i64 = 0;
    let mut i: i64 = 0;
    while i < 50 {
        let e = mk();
        total = total + consume(e);
        i = i + 1;
    }
    println(total);
}
"#,
            &["55250"],
            "vec_of_struct_shared_and_option_field_consumed",
        );
    }

    // A heap value (`String`) moved into a `tg.spawn` closure INSIDE A LOOP:
    // the per-iteration `let addr = base.clone()` is freed at loop-body scope
    // exit, but the spawned task now owns that buffer (the env got a bitwise
    // copy of the `{data,len,cap}` header). Ownership must transfer cleanly
    // from parent to task — exactly once across the two of them:
    //
    //   1. The parent's per-iteration `FreeVecBuffer` is suppressed (the
    //      original B-2026-06-18-8 half): a non-suppressed parent frees the
    //      buffer the task still reads. A single non-loop spawn masked it (the
    //      `TaskGroup` join precedes the parent free); the loop drains each
    //      iteration's frame first → ASAN use-after-free.
    //   2. The task wrapper must then free whatever the body does not itself
    //      consume (the completing half). The handler here only *reads* the
    //      captured string (`addr: String` is inferred `ref`), so nothing in
    //      the body owns it — without a wrapper-side free the buffer leaks once
    //      per spawn. macOS ASAN has no LeakSanitizer, so the suppress-only fix
    //      looked green locally while leaking under the Linux/LSan gate
    //      (`scripts/lsan-local.sh`); `lower_spawn_shared` now re-registers the
    //      parent's `FreeVecBuffer` against the wrapper-local binding to close
    //      it. The move-into-callee sibling below guards the no-double-free
    //      half of that same transfer.
    //
    // The handler body compares its captured string to the known content (a
    // buffer read — poisoned-memory access if freed under ASAN) and stays
    // silent unless it mismatches, so a regression shows as an ASAN
    // use-after-free / leak / a `CORRUPT` line. This is the canonical
    // `loop { let s = …; tg.spawn(|| use(s)) }` server-handler shape — exactly
    // `examples/relay/relay.kara`'s round-robin accept loop.
    #[test]
    fn asan_taskgroup_spawn_heap_capture_in_loop_coro_no_uaf() {
        assert_clean_asan_run(
            r#"
fn check(addr: String) {
    sleep_ms(5);
    if addr == "relay-upstream-127.0.0.1-9000" {
    } else {
        println("CORRUPT");
    }
}
fn main() {
    let base = "relay-upstream-127.0.0.1-9000";
    let mut tg: TaskGroup = TaskGroup.new();
    let mut i: i64 = 0;
    loop {
        let addr = base.clone();
        i = i + 1;
        tg.spawn(|| check(addr));
        if i >= 6 { break; }
    }
    println("ok");
}
"#,
            &["ok"],
            "taskgroup_spawn_heap_capture_in_loop_coro",
        );
    }

    // Non-coroutine sibling of the above: the handler does not suspend, so it
    // lowers through the run-to-completion spawn path rather than the coro
    // park path. Same double-ownership hole, same fix (the `FreeVecBuffer`
    // suppression is shared by both paths in `lower_spawn_shared`).
    #[test]
    fn asan_taskgroup_spawn_heap_capture_in_loop_noncoro_no_uaf() {
        assert_clean_asan_run(
            r#"
fn check(addr: String) {
    if addr == "relay-upstream-127.0.0.1-9000" {
    } else {
        println("CORRUPT");
    }
}
fn main() {
    let base = "relay-upstream-127.0.0.1-9000";
    let mut tg: TaskGroup = TaskGroup.new();
    let mut i: i64 = 0;
    loop {
        let addr = base.clone();
        i = i + 1;
        tg.spawn(|| check(addr));
        if i >= 6 { break; }
    }
    println("ok");
}
"#,
            &["ok"],
            "taskgroup_spawn_heap_capture_in_loop_noncoro",
        );
    }

    // Companion to the two loop-capture cases above: there the spawned body
    // only *borrows* the captured `String` (`check(addr)` — a `ref` param), so
    // the task wrapper is the sole owner and must free it. Here the body
    // *moves* the capture into a consuming callee (`sink` pushes it into a
    // local `Vec`, taking ownership), so `sink`'s own scope-exit drop frees the
    // buffer. The wrapper's transferred `FreeVecBuffer` (the same ownership
    // hand-off the loop tests exercise) MUST then be a no-op — the move into
    // `sink` zeros the capture's `cap`, so the `cap > 0` drain guard skips it.
    // If the wrapper freed regardless, this is a double-free (ASAN: `attempting
    // double-free`); if neither freed, a leak (LSan). Exactly one free is the
    // pass. Guards the move-suppression half of the B-2026-06-18-8 follow-up.
    #[test]
    fn asan_taskgroup_spawn_heap_capture_moved_into_callee_single_free() {
        assert_clean_asan_run(
            r#"
fn sink(addr: String) {
    let mut held: Vec[String] = Vec.new();
    held.push(addr);
    if held[0] == "relay-upstream-127.0.0.1-9000" {
    } else {
        println("CORRUPT");
    }
}
fn main() {
    let base = "relay-upstream-127.0.0.1-9000";
    let mut tg: TaskGroup = TaskGroup.new();
    let mut i: i64 = 0;
    loop {
        let addr = base.clone();
        i = i + 1;
        tg.spawn(|| sink(addr));
        if i >= 4 { break; }
    }
    println("ok");
}
"#,
            &["ok"],
            "taskgroup_spawn_heap_capture_moved_into_callee",
        );
    }

    // ── Cross-task shared (aliased) heap capture — read-only ───────
    //
    // The companion to the move cases above: here ONE heap buffer is captured
    // by MULTIPLE sibling tasks that only *read* it (the closures pass it to a
    // `ref` param), and the parent keeps owning it. This is the canonical
    // parallel-stencil fan-out — split a shared input grid into bands, one task
    // per band — and the shape the Slipstream LBM dogfood (examples/slipstream)
    // drove out. Before the capture-mode fix in `codegen/task_group.rs`, the
    // spawn lowering treated EVERY capture as a move: each task re-registered a
    // free of the shared buffer and the parent's free was suppressed, so N
    // tasks freed the one buffer N times — a double-free / use-after-free that
    // produced wrong sums and an allocator "failed to lock mutex" abort. The
    // fix: a borrowed capture stays owned by the parent (freed once after the
    // join barrier, the same `Copy`-capture rule a `par {}` branch uses), so
    // the buffer is freed exactly once. This asserts BOTH value-correctness
    // (the band sums total 4950 = sum 0..99 — a miscompiled shared read returns
    // garbage) AND ASAN/LSan cleanliness (no double-free, no leak). The Vec is
    // 100×i64 = 800 bytes, well past any allocator-freelist threshold, so a
    // leak regression surfaces on LSan.
    #[test]
    fn asan_taskgroup_spawn_shared_vec_read_across_tasks_single_free() {
        assert_clean_asan_run(
            r#"
fn band_sum(data: ref Vec[i64], lo: i64, hi: i64) -> i64 {
    let mut acc = 0;
    let mut i = lo;
    while i < hi { acc = acc + data[i]; i = i + 1; }
    acc
}
fn main() with panics {
    let mut data: Vec[i64] = Vec.new();
    let mut i = 0;
    while i < 100 { data.push(i); i = i + 1; }
    let mut pool: TaskGroup = TaskGroup.new();
    let mut handles: Vec[TaskHandle[i64]] = Vec.new();
    let mut k = 0;
    while k < 4 {
        let lo = k * 25;
        let hi = lo + 25;
        handles.push(pool.spawn(|| band_sum(data, lo, hi)));
        k = k + 1;
    }
    let mut total = 0;
    for h in handles { total = total + h.join(); }
    if total == 4950 { println("total 4950"); } else { println("WRONG"); }
}
"#,
            &["total 4950"],
            "taskgroup_spawn_shared_vec_read_across_tasks",
        );
    }

    // String variant of the shared read-only capture: three tasks each borrow
    // the same captured `String` (a `ref` param compare). Same double-free class
    // as the Vec case (the `{data,len,cap}` header is shared), and the same
    // single-free expectation. Payload is >= 40 bytes so an LSan leak regression
    // clears the reachable-short-String freelist threshold
    // (`lsan-reachability-short-string-leaks`).
    #[test]
    fn asan_taskgroup_spawn_shared_string_read_across_tasks_single_free() {
        assert_clean_asan_run(
            r#"
fn match_addr(addr: ref String) -> i64 {
    if addr == "relay-upstream-host-127.0.0.1-port-9000-ok" { 1 } else { 0 }
}
fn main() with panics {
    let addr: String = "relay-upstream-host-127.0.0.1-port-9000-ok";
    let mut pool: TaskGroup = TaskGroup.new();
    let mut handles: Vec[TaskHandle[i64]] = Vec.new();
    let mut k = 0;
    while k < 3 {
        handles.push(pool.spawn(|| match_addr(addr)));
        k = k + 1;
    }
    let mut hits = 0;
    for h in handles { hits = hits + h.join(); }
    if hits == 3 { println("hits 3"); } else { println("WRONG"); }
}
"#,
            &["hits 3"],
            "taskgroup_spawn_shared_string_read_across_tasks",
        );
    }

    // ── Relay per-request parse + atomic soak ─────────────────────
    //
    // High-volume leak stress for the exact allocation churn a Relay
    // (`examples/relay`) connection handler runs per request: the
    // request-line peek `Vec.from_slice(buf[0..n])` -> `String.from_utf8`
    // -> `.split(' ')` -> `parts[i].clone()`, plus the shared-`Metrics`
    // `Atomic[i64].fetch_add`/`.load`. Each of those is a heap allocation
    // (Vec[u8], String, Vec[String] + per-part Strings, the cloned path),
    // and every iteration must reclaim ALL of them — a single missing free
    // in any drop path compounds 4000x and LSan (Linux CI / `scripts/
    // lsan-local.sh`) flags it. This is the steady-state proxy loop in a
    // bottle: no networking, just the parse/atomic allocators on repeat.
    //
    // Payloads are deliberately >= 40 bytes: LSan misses *reachable*
    // short-String leaks (the small-string buffer can stay pinned by an
    // allocator freelist), so a leak regression only surfaces with a
    // payload past the small-string threshold — see the user-memory note
    // `lsan-reachability-short-string-leaks`. The buffer plants two spaces
    // so `split(' ')` yields a >= 40-byte middle token that `clone()`
    // returns, keeping the leak-candidate object well past the threshold.
    //
    // The fd path (`connect_start`/`connect_finish` close-on-failure) is
    // intentionally NOT soaked here: it needs a live reactor to drive the
    // write-readiness park, which a bare ASAN `main` has no harness for.
    // That path is covered by the relay E2E under a real reactor and the
    // existing tcp/park ASAN cases.
    #[test]
    fn asan_relay_request_parse_atomic_soak_no_leak() {
        assert_clean_asan_run(
            r#"
par struct Counters {
    n: Atomic[i64],
}
fn request_path(bytes: Vec[u8]) -> String {
    match String.from_utf8(bytes) {
        Result.Ok(line) => {
            let parts = line.split(' ');
            if parts.len() >= 2 {
                return parts[1].clone();
            }
            return "/";
        }
        Result.Err(_) => {
            return "/";
        }
    }
}
fn main() {
    // 48 'A' (0x41, valid UTF-8); spaces at 3 and 44 split it into
    // ["AAA", <40-byte middle>, "AAA"] — parts[1] is the >= 36-byte
    // leak-candidate the LSan short-string blind spot needs.
    let mut buf: Array[u8, 48] = [65u8; 48];
    buf[3] = 32u8;
    buf[44] = 32u8;
    let counters = Counters { n: Atomic.new(0) };
    let mut i: i64 = 0;
    loop {
        if i >= 4000 { break; }
        let bytes: Vec[u8] = Vec.from_slice(buf[0..48]);
        let path = request_path(bytes);
        if path.len() >= 40 {
            let _ = counters.n.fetch_add(1, MemoryOrdering.Relaxed);
        }
        i = i + 1;
    }
    println(counters.n.load(MemoryOrdering.Relaxed));
}
"#,
            &["4000"],
            "relay_request_parse_atomic_soak",
        );
    }

    // ── `Vec[v; n]` repeat literal — heap fill lifecycle ──────────────
    // `Vec[v; n]` / `vec![v; n]` allocate a heap buffer via the shared
    // `build_vec_filled` (same path as `Vec.filled(n, v)`). This pins that the
    // resulting `{ptr, len, cap}` Vec is dropped exactly once: a fill, a
    // push-after-fill (grow-realloc reclaims the original buffer), an index
    // read, and a `vec![v; n]` with a larger payload all in one scope. On
    // Linux CI LSan additionally catches a missed free of the fill buffer.
    #[test]
    fn asan_vec_repeat_literal_fill_push_index() {
        assert_clean_asan_run(
            r#"
fn main() {
    let mut g: Vec[i64] = Vec[5; 8];
    g.push(1);
    g.push(2);
    let mut total = 0;
    for x in g { total = total + x; }
    println(total);
    let n = 100;
    let r: Vec[i64] = vec![3; n];
    println(r.len());
    println(r[99]);
}
"#,
            &["43", "100", "3"],
            "vec_repeat_literal_fill_push_index",
        );
    }

    /// `Vec.filled(rows, Vec.filled(cols, x))` — the canonical 2D DP table.
    /// Before the per-slot deep-clone fix, codegen bit-copied the inner heap
    /// Vec into every row, so all rows aliased ONE backing buffer: writes to
    /// one row corrupted the others and the N rows N-fold-freed the same buffer
    /// on drop (AOT SIGTRAP). ASAN must confirm each row owns a distinct buffer
    /// — no aliasing (rows independent), no double-free, no leak. Inner rows
    /// carry 5 i64s (40 bytes > the LSan short-alloc floor) so a wrongly-shared
    /// or wrongly-freed row buffer is caught.
    #[test]
    fn asan_vec_filled_2d_rows_are_independent_buffers() {
        assert_clean_asan_run(
            r#"
fn main() {
    let rows = 4i64;
    let cols = 5i64;
    let mut dp: Vec[Vec[i64]] = Vec.filled(rows, Vec.filled(cols, 0i64));
    let mut i = 0i64;
    while i < rows {
        let mut j = 0i64;
        while j < cols {
            dp[i][j] = i * 10i64 + j;
            j = j + 1i64;
        }
        i = i + 1i64;
    }
    println(dp[0][0]);
    println(dp[3][4]);
    println(dp[2][3]);
}
"#,
            &["0", "34", "23"],
            "vec_filled_2d_rows_independent",
        );
    }

    // ── `String ==` / `!=` must clamp its memcmp span to min(len) ──
    //
    // The equality path used to `memcmp(l_ptr, r_ptr, l_len)` unconditionally
    // (the `Lt/Gt` path already clamped to `min_len`). When `l_len > r_len`
    // that reads past the end of the SHORTER right buffer — a heap-buffer-
    // overflow (ASAN-caught here, a latent OOB read in release). The
    // `len_eq && data_eq` AND already makes unequal-length operands compare
    // `false`, so clamping the compare span to `min(l_len, r_len)` is both
    // memory-safe and semantics-preserving.
    //
    // This surfaced during phase-12 self-hosting slice 3a: the parser's
    // `current_ident_matches` borrow-matches the `String` payload out of an
    // indexed place (`self.tokens[self.pos].token`) and compares it against a
    // short keyword (`dup_str(n) == "Fn"`). A type name longer than the keyword
    // (`l_len > r_len`) overran the keyword literal's buffer. It was originally
    // mis-attributed to a double-free of the borrowed payload; the borrow-match
    // itself is sound (the indexed enum scrutinee is deep-cloned, so the bound
    // payload owns an independent buffer freed exactly once). The faithful
    // failing shape is reproduced below: a multi-variant enum whose `String`
    // payload is borrow-matched out of a `Vec` element a struct owns, then
    // compared against shorter literals. The payloads share a prefix with the
    // shorter literal (`"OnceFnHandler"` vs `"OnceFn"`) so the read runs past
    // the short buffer's end rather than stopping at a leading mismatch.
    #[test]
    fn asan_string_eq_mismatched_len_no_overread_in_indexed_payload_match() {
        assert_clean_asan_run(
            r#"
enum Tok {
    KwFn,
    KwOnceFn,
    Ident(String),
    Punct(String),
    Eof,
}
struct Span { line: i64, col: i64 }
struct SpannedTok { tok: Tok, span: Span }
struct Lexer { toks: Vec[SpannedTok], pos: i64 }

fn dup_str(s: ref String) -> String {
    let mut out = "".to_string();
    out.push_str(s);
    out
}

impl Lexer {
    // Borrow-match the payload out of an indexed place, then compare it (by an
    // owned dup) against a SHORTER literal. The dup's length exceeds the
    // literal's, so the pre-fix `memcmp(.., l_len)` overran the literal buffer.
    fn ident_matches(ref self, target: String) -> bool {
        match self.toks[self.pos].tok {
            Ident(n) => {
                let got = dup_str(n);
                got == target
            }
            _ => false,
        }
    }
}

fn mk(name: String) -> SpannedTok {
    SpannedTok { tok: Tok.Ident(name), span: Span { line: 1, col: 1 } }
}

fn main() {
    let mut toks = Vec.new();
    toks.push(mk("OnceFnHandler".to_string()));
    toks.push(mk("FnPtrFactory".to_string()));
    let lx = Lexer { toks: toks, pos: 0 };
    // Long payload (shares a prefix with the short literal) vs short keyword:
    // the comparison span must clamp to the keyword length, not the payload's.
    let m_fn = lx.ident_matches("Fn".to_string());
    let m_once = lx.ident_matches("OnceFn".to_string());
    let m_exact = lx.ident_matches("OnceFnHandler".to_string());
    println(m_fn);
    println(m_once);
    println(m_exact);
}
"#,
            &["false", "false", "true"],
            "asan_string_eq_mismatched_len_no_overread_in_indexed_payload_match",
        );
    }

    // Two `shared enum`s whose heap layouts are STRUCTURALLY IDENTICAL
    // (`Alfa` and `Bravo` are variant-for-variant layout-twins: each is
    // `{ Leaf(String), Node(struct { Vec[Self], String }) }`) must NOT share
    // one LLVM heap `StructType`. They did before the fix — shared heap types
    // were anonymous (`context.struct_type`), which LLVM uniques by structure
    // — so the refcount-drop dispatch, which recovers a shared type's name
    // from its heap type by object identity, confused the two: dropping a
    // `Vec[Alfa]` element ran it through `__karac_rc_drop_Bravo` (or vice
    // versa), reading the wrong variant tag/offsets and double-freeing. This
    // was the slice-3b self-host type-oracle crash (B-2026-06-20-6,
    // `Pattern` vs `TypeExpr`, both 12 payload words); fixed by giving each
    // shared type a uniquely NAMED heap struct (`%karac.shared.<T>`). Here
    // we BUILD and DROP many
    // recursive trees of both twins (each `make_*(4)` nests `Vec[Self]`
    // children that rc-dec on scope exit) and assert a clean ASAN run.
    #[test]
    fn asan_layout_twin_shared_enums_drop_through_correct_rc_drop() {
        assert_clean_asan_run(
            "shared enum Alfa { ALeaf(String), ANode(NodeA) }\n\
             shared enum Bravo { BLeaf(String), BNode(NodeB) }\n\
             struct NodeA { kids: Vec[Alfa], name: String }\n\
             struct NodeB { kids: Vec[Bravo], name: String }\n\
             fn make_a(d: i64) -> Alfa {\n\
             \x20   if d <= 0 { return Alfa.ALeaf(\"alfa-leaf-payload-string-long\".to_string()); }\n\
             \x20   let mut kids: Vec[Alfa] = Vec.new();\n\
             \x20   kids.push(make_a(d - 1));\n\
             \x20   kids.push(make_a(d - 1));\n\
             \x20   Alfa.ANode(NodeA { kids: kids, name: \"alfa-node-name-payload\".to_string() })\n\
             }\n\
             fn make_b(d: i64) -> Bravo {\n\
             \x20   if d <= 0 { return Bravo.BLeaf(\"bravo-leaf-payload-string-long\".to_string()); }\n\
             \x20   let mut kids: Vec[Bravo] = Vec.new();\n\
             \x20   kids.push(make_b(d - 1));\n\
             \x20   kids.push(make_b(d - 1));\n\
             \x20   Bravo.BNode(NodeB { kids: kids, name: \"bravo-node-name-payload\".to_string() })\n\
             }\n\
             fn main() {\n\
             \x20   let mut i = 0;\n\
             \x20   while i < 20 {\n\
             \x20       let a = make_a(4);\n\
             \x20       let b = make_b(4);\n\
             \x20       i = i + 1;\n\
             \x20   }\n\
             \x20   println(\"done\");\n\
             }\n",
            &["done"],
            "asan_layout_twin_shared_enums_drop_through_correct_rc_drop",
        );
    }

    /// Moving a heap field OUT of an owned by-value struct param (deep-copied at
    /// entry, #14/#17) while the param's scope-exit `StructDrop` still freed that
    /// field double-freed the moved-out buffer — surfaced by phase-12 selfhost
    /// slice 3c-ii (`render_variant` / `render_struct_field`), minimal
    /// `fn f(s: S) -> String { s.a }`. Exercises all three move-out shapes:
    /// field-access return (`p.a`), destructure-then-return (`let Pair{a,b}=p; a`),
    /// and a CONSUMED `Vec` field destructured from the param (`for t in tags`)
    /// alongside a moved `String` field — in a loop with ≥36-byte payloads so a
    /// leak (LSan) or double-free (ASAN) trips. Fixed by the `FieldAccess`
    /// move-out suppressor arm + the callee-owned place-source struct-destructure
    /// transfer (`zero_struct_field_move_cap`).
    #[test]
    fn asan_by_value_struct_field_moveout_no_double_free() {
        assert_clean_asan_run(
            "struct Pair { a: String, b: String }\n\
             struct Node { tags: Vec[String], name: String }\n\
             fn pick_a(p: Pair) -> String { p.a }\n\
             fn pick_a_destructured(p: Pair) -> String { let Pair { a, b } = p; a }\n\
             fn join_tags(n: Node) -> String {\n\
             \x20   let Node { tags, name } = n;\n\
             \x20   let mut out = name;\n\
             \x20   for t in tags { out.push_str(t); }\n\
             \x20   out\n\
             }\n\
             fn main() {\n\
             \x20   let mut i = 0;\n\
             \x20   while i < 50 {\n\
             \x20       let p1 = Pair { a: \"pair-a-field-payload-long-enough-string\".to_string(), b: \"pair-b-field-payload-long-enough-string\".to_string() };\n\
             \x20       let r1 = pick_a(p1);\n\
             \x20       let p2 = Pair { a: \"destructure-a-payload-long-enough-string\".to_string(), b: \"destructure-b-payload-long-enough-string\".to_string() };\n\
             \x20       let r2 = pick_a_destructured(p2);\n\
             \x20       let mut tags: Vec[String] = Vec.new();\n\
             \x20       tags.push(\"tag-element-payload-long-enough-string-one\".to_string());\n\
             \x20       tags.push(\"tag-element-payload-long-enough-string-two\".to_string());\n\
             \x20       let n = Node { tags: tags, name: \"node-name-payload-long-enough-string\".to_string() };\n\
             \x20       let r3 = join_tags(n);\n\
             \x20       i = i + 1;\n\
             \x20   }\n\
             \x20   println(\"done\");\n\
             }\n",
            &["done"],
            "asan_by_value_struct_field_moveout_no_double_free",
        );
    }

    #[test]
    fn asan_shared_enum_string_payload_moveout_no_double_free() {
        // B-2026-06-20: moving a `String`/`Vec` payload OUT of a SHARED-enum RC
        // box (`match e { S(s) => s }`, returned) only neutralized the LOCAL
        // binding's cap — the box's payload words still pointed at the moved-out
        // buffer, so the box's `__karac_rc_drop_<E>` (Vec/String arm frees
        // `cap > 0`) re-freed it after the caller already had: a double-free.
        // The minimal isolated shape (the recursive self-host render leak hit it
        // nested, where the boxed-struct-drop gap masked it as a leak instead).
        // Fixed by `suppress_shared_enum_payload_move_out`: zero the field's words
        // in the BOX so its rc-drop skips the buffer the binding now owns. The
        // ≥36-byte payload makes the (pre-fix) freed-then-freed buffer a real heap
        // block ASAN flags; Linux LSan covers the symmetric no-leak arm.
        assert_clean_asan_run(
            r#"
shared enum E { S(String), Other }
fn get(e: E) -> String {
    match e {
        S(s) => s,
        Other => "other".to_string(),
    }
}
fn main() {
    let e = E.S("shared-enum-moveout-payload-long-enough-string".to_string());
    println(get(e));
}
"#,
            &["shared-enum-moveout-payload-long-enough-string"],
            "shared_enum_string_payload_moveout_no_double_free",
        );
    }

    #[test]
    fn asan_letbound_result_option_heap_unwrap_no_double_free() {
        // B-2026-07-10-2: `unwrap`/`unwrap_err`/`expect` on a LET-BOUND
        // Option/Result receiver with a HEAP payload. The extracted String is a
        // shallow alias of the receiver's inline buffer; unwrap CONSUMES the
        // receiver, so its scope-exit drop must be disarmed or it double-frees the
        // buffer the returned value now owns. Fix:
        // `suppress_inline_option_result_binding_move` zeros the tracked receiver
        // slot. Looped x20 under LSan.
        assert_clean_asan_run(
            r#"
fn rok(ok: bool) -> Result[String, i64] {
    if ok { Result.Ok("the ok payload padded out".to_string()) } else { Result.Err(1) }
}
fn rerr(ok: bool) -> Result[i64, String] {
    if ok { Result.Ok(1) } else { Result.Err("the err payload padded out".to_string()) }
}
fn opt(some: bool) -> Option[String] {
    if some { Some("the some payload padded".to_string()) } else { None }
}
fn main() {
    let mut total: i64 = 0;
    let mut i: i64 = 0;
    while i < 20 {
        let a = rok(true);
        let sa = a.unwrap();
        let b = rerr(false);
        let sb = b.unwrap_err();
        let c = opt(true);
        let sc = c.expect("wanted some");
        total = total + (sa.len() as i64) + (sb.len() as i64) + (sc.len() as i64);
        i = i + 1;
    }
    println(total);
}
"#,
            &["1480"],
            "letbound_result_option_heap_unwrap_no_double_free",
        );
    }

    #[test]
    fn asan_letbound_struct_vec_shared_elem_moved_into_enum_ctor_no_uaf() {
        // B-2026-07-10-1: a LET-BOUND struct with a `Vec[<enum owning a shared
        // field>]` field AND an `Option[shared]` field, MOVED whole into a
        // shared-enum ctor (`let b = Block { stmts, tail, span }; Expr.Blk(b)`).
        // The struct's combined drop (`__karac_vec_elem_full_drop_<S>`) frees the
        // Vec buffer under a `cap > 0` guard but runs a SEPARATE, LEN-driven
        // per-element rc-dec walk for the shared-bearing elements. The whole-struct
        // move-suppression (`zero_struct_move_caps`) zeroed only the Vec `cap`, so
        // that len-driven walk still rc-dec'd the moved-out elements' shared handles
        // — which the boxed enum payload co-owns — a use-after-free (the self-hosted
        // parser's `{ a; }` statement-expr read back as a garbage `Error` node under
        // AOT while correct under interp). Fix: `zero_struct_move_caps` also zeroes
        // the Vec `len`. Looped x20 under the LSan gate; the shared-elem count must
        // survive the move so the reader sees the real payload.
        assert_clean_asan_run(
            r#"
struct IdentExpr { name: String, val: i64 }
shared enum Expr { Ident(IdentExpr), Blk(Block) }
struct ExprStmt { expr: Expr }
enum Stmt { Exp(ExprStmt) }
struct Block { stmts: Vec[Stmt], tail: Option[Expr], span: i64 }
fn render_expr(e: Expr) -> i64 {
    match e {
        Ident(n) => n.val,
        Blk(b) => {
            let Block { stmts, tail, span } = b;
            let mut acc = span;
            for s in stmts { match s { Exp(n) => { let ExprStmt { expr } = n; acc = acc + render_expr(expr); } } }
            acc
        }
    }
}
fn mk_expr() -> Expr {
    let mut stmts: Vec[Stmt] = Vec.new();
    stmts.push(Stmt.Exp(ExprStmt { expr: Expr.Ident(IdentExpr { name: "aaaaaaaaaaaaaaaaaaaaaaaa".to_string(), val: 7 }) }));
    stmts.push(Stmt.Exp(ExprStmt { expr: Expr.Ident(IdentExpr { name: "bbbbbbbbbbbbbbbbbbbbbbbb".to_string(), val: 11 }) }));
    let block = Block { stmts: stmts, tail: None, span: 100 };
    Expr.Blk(block)
}
fn main() {
    let mut total: i64 = 0;
    let mut i: i64 = 0;
    while i < 20 { total = total + render_expr(mk_expr()); i = i + 1; }
    println(total);
}
"#,
            &["2360"],
            "letbound_struct_vec_shared_elem_moved_into_enum_ctor",
        );
    }

    #[test]
    fn asan_shared_enum_view_destructure_bare_shared_child_no_double_free() {
        // B-2026-07-09-12 (clone-on-extract half): a shared-enum whose struct
        // payload carries BARE-SHARED children (`BinNode { left: Expr, right: Expr
        // }`, the AST binary-node shape). The arm binds the payload as a VIEW
        // (`Bin(b)`, not deep-cloned because it is shared-bearing) then
        // DESTRUCTURES it (`let BinNode { left, right, op } = b`) and CONSUMES the
        // moved-out shared children (`eval(left)`, `eval(right)`). Pre-fix the
        // extracted `left`/`right` aliased the RC box's inline handles, so the
        // recursive consume rc-dec AND the box's rc-drop both freed each child box
        // — a double-free. The clone-on-extract fix rc-INCs each bare-shared child
        // at the destructure (`clone_on_extract_view_field`) so the leaf co-owns
        // the box; the leaf's consume balances the inc, the box's rc-drop balances
        // its original ref. Also covers the String LEAF (`n.name`) via the Lit arm.
        // Looped x20 so a per-iteration double-free / leak hits the LSan gate.
        assert_clean_asan_run(
            r#"
struct LitNode { name: String, val: i64 }
shared enum Expr { Lit(LitNode), Bin(BinNode) }
struct BinNode { left: Expr, right: Expr, op: i64 }
fn eval(e: Expr) -> i64 {
    match e {
        Lit(n) => n.val,
        Bin(b) => {
            let BinNode { left, right, op } = b;
            eval(left) + eval(right) + op
        }
    }
}
fn main() {
    let mut total: i64 = 0;
    let mut i: i64 = 0;
    while i < 20 {
        let a = Expr.Lit(LitNode { name: "a_long_name_for_heap_visibility_xxxxxxxx".to_string(), val: 3 });
        let bn = Expr.Lit(LitNode { name: "b_long_name_for_heap_visibility_xxxxxxxx".to_string(), val: 4 });
        let inner = Expr.Bin(BinNode { left: a, right: bn, op: 10 });
        let c = Expr.Lit(LitNode { name: "c_long_name_for_heap_visibility_xxxxxxxx".to_string(), val: 5 });
        let tree = Expr.Bin(BinNode { left: inner, right: c, op: 100 });
        total = total + eval(tree);
        i = i + 1;
    }
    println(total);
}
"#,
            &["2440"],
            "shared_enum_view_destructure_bare_shared_child",
        );
    }

    #[test]
    fn asan_shared_enum_view_field_access_move_vec_shared_no_double_free() {
        // B-2026-07-09-12 (clone-on-extract half — FIELD-ACCESS-MOVE form): a
        // `Vec[shared]` field moved out of a shared-enum-payload VIEW by field
        // access into a `let` (`let a = c.args`), NOT a destructure. The moved-out
        // Vec aliased the box's buffer + element handles; the leaf's per-element
        // rc-dec drop AND the box's rc-drop both freed each element box (SEGV /
        // heap corruption). `deep_copy_owned_struct_param_field_move`, extended to
        // view sources, deep-copies the buffer and rc-INCs each element. The bare-
        // shared and Option[shared] field-move forms are already balanced by
        // `compile_field_access`'s read-inc. Looped x20 under LSan.
        assert_clean_asan_run(
            r#"
shared enum Expr { Lit(i64), Call(CallNode) }
struct CallNode { args: Vec[Expr], tag: i64 }
fn ev(e: Expr) -> i64 {
    match e {
        Lit(n) => n,
        Call(c) => {
            let a = c.args;
            let mut acc = c.tag;
            for x in a { acc = acc + ev(x); }
            acc
        }
    }
}
fn main() {
    let mut total: i64 = 0;
    let mut i: i64 = 0;
    while i < 20 {
        let mut v: Vec[Expr] = Vec.new();
        v.push(Expr.Lit(5)); v.push(Expr.Lit(6)); v.push(Expr.Lit(7));
        total = total + ev(Expr.Call(CallNode { args: v, tag: 100 }));
        i = i + 1;
    }
    println(total);
}
"#,
            &["2360"],
            "shared_enum_view_field_access_move_vec_shared",
        );
    }

    #[test]
    fn asan_shared_enum_view_destructure_vec_shared_and_option_shared_no_double_free() {
        // B-2026-07-09-12 (clone-on-extract half — Vec[shared] + Option[shared]
        // leaves): the AST sequence-child (`CallNode { args: Vec[Expr] }`) and
        // optional-child (`NodeData { tail: Option[Expr] }`) shapes, both moved out
        // of a shared-enum-payload VIEW via destructure and consumed. Pre-fix the
        // extracted `args` elements / `tail` `Some` box aliased the RC box's heap,
        // so the leaf's consume AND the box's rc-drop double-freed them. The
        // clone-on-extract fix deep-copies the Vec buffer + rc-INCs each element
        // (`rc_inc_vec_shared_elements`) and rc-INCs the `Some` box
        // (`deep_copy_option_inline_payload_in_place` + `track_rc_option_var`).
        // Looped x20 under the LSan gate.
        assert_clean_asan_run(
            r#"
shared enum Expr { Lit(i64), Call(CallNode), Node(NodeData) }
struct CallNode { args: Vec[Expr], tag: i64 }
struct NodeData { val: i64, tail: Option[Expr] }
fn ev(e: Expr) -> i64 {
    match e {
        Lit(n) => n,
        Call(c) => {
            let CallNode { args, tag } = c;
            let mut acc = tag;
            for a in args { acc = acc + ev(a); }
            acc
        }
        Node(nd) => {
            let NodeData { val, tail } = nd;
            match tail { Some(t) => val + ev(t), None => val }
        }
    }
}
fn main() {
    let mut total: i64 = 0;
    let mut i: i64 = 0;
    while i < 20 {
        let mut v: Vec[Expr] = Vec.new();
        v.push(Expr.Lit(5)); v.push(Expr.Lit(6)); v.push(Expr.Lit(7));
        let leaf = Expr.Node(NodeData { val: 9, tail: None });
        v.push(Expr.Node(NodeData { val: 3, tail: Some(leaf) }));
        let call = Expr.Call(CallNode { args: v, tag: 100 });
        total = total + ev(call);
        i = i + 1;
    }
    println(total);
}
"#,
            &["2600"],
            "shared_enum_view_destructure_vec_shared_and_option_shared",
        );
    }

    #[test]
    fn asan_shared_enum_struct_payload_child_moveout_no_double_free() {
        // B-2026-07-09-12 (copy-supported half): a for-loop over a `Vec[shared
        // enum]` whose arm DESTRUCTURES the struct payload and MOVES a heap child
        // OUT of it (`let Id { name, .. } = n; name` returns the extracted
        // String). The reconstructed struct payload `n` is a by-value VIEW of the
        // RC box's inline buffer; without the fix the returned `name` and the
        // box's rc-drop both free the same String buffer — a double-free (the
        // minimal parser-runtime repro). The fix upgrades the view to an OWNED
        // deep clone at the shared-enum match bind, gated on
        // `struct_clone_fully_duplicates` (payload heap is String / Vec[non-
        // shared] / nested such — reproduced exactly by `emit_struct_clone_fn`).
        // Second variant exercises the Vec[non-shared] clone leg (`Row { cells:
        // Vec[i64] }`, sum a moved-out Vec). Looped so a per-iteration double-free
        // or leak is visible to the Linux-CI LSan gate.
        assert_clean_asan_run(
            r#"
struct Id { name: String, span: i64 }
struct Row { first: String, cells: Vec[i64] }
shared enum Node { Ident(Id), Rowed(Row) }
fn render_ident(e: Node) -> String {
    match e {
        Ident(n) => { let Id { name, span } = n; name }
        Rowed(r) => { let Row { first, cells } = r; first }
    }
}
fn sum_row(e: Node) -> i64 {
    match e {
        Ident(_) => 0,
        Rowed(r) => {
            let Row { first, cells } = r;
            let mut acc: i64 = 0;
            for c in cells { acc = acc + c; }
            acc
        }
    }
}
fn main() {
    let mut total: i64 = 0;
    let mut last = "".to_string();
    let mut k: i64 = 0;
    while k < 20 {
        let mut out = "".to_string();
        let mut v: Vec[Node] = Vec.new();
        v.push(Node.Ident(Id { name: "hi".to_string(), span: 1 }));
        let mut cs: Vec[i64] = Vec.new();
        cs.push(3); cs.push(4);
        v.push(Node.Rowed(Row { first: "row".to_string(), cells: cs }));
        for e in v { out.push_str(render_ident(e)); }
        let mut v2: Vec[Node] = Vec.new();
        let mut cs2: Vec[i64] = Vec.new();
        cs2.push(5); cs2.push(6); cs2.push(7);
        v2.push(Node.Rowed(Row { first: "r2".to_string(), cells: cs2 }));
        for e in v2 { total = total + sum_row(e); }
        last = out;
        k = k + 1;
    }
    println(last);
    println(total);
}
"#,
            &["hirow", "360"],
            "shared_enum_struct_payload_child_moveout_no_double_free",
        );
    }

    #[test]
    fn asan_shared_enum_boxed_struct_payload_moveout_no_double_free() {
        // B-2026-06-20: a shared-enum variant whose struct payload is heap-BOXED
        // (`Blk(Block)` / `Iff(IfNode)` — wider than the payload area). The box
        // rc-drop must unbox + free the box AND reclaim its DIRECT Vec/String
        // buffers and shared/`Option[shared]` children (the tests-1/2 leak), but
        // must NOT recurse into a nested heap struct field that the match moved
        // out (`let tb = nd.then_block`, freed by `tb`'s own drop) — re-freeing it
        // double-frees. Pins the build + match + scope-exit drop of both the
        // direct boxed payload and the nested (Iff) one with no double-free and no
        // leak. The IR sibling of `test_e2e_shared_enum_payload_with_nested_heap_
        // struct_field` (codegen.rs), under the LSan + ASAN gate.
        assert_clean_asan_run(
            "struct Span { a: i64, b: i64, c: i64, d: i64 }\n\
             shared enum E { Lit(i64), Iff(IfNode), Blk(Block) }\n\
             struct Block { stmts: Vec[i64], tail: Option[E], span: Span }\n\
             struct IfNode { cond: E, then_block: Block, span: Span }\n\
             fn mk_block(first: i64, sp: i64) -> Block {\n\
             \x20   let mut s: Vec[i64] = Vec.new();\n\
             \x20   s.push(first); s.push(first + 1);\n\
             \x20   Block { stmts: s, tail: Some(E.Lit(99)), span: Span { a: sp, b: 0, c: 0, d: 0 } }\n\
             }\n\
             fn main() {\n\
             \x20   let be = E.Blk(mk_block(10, 1));\n\
             \x20   match be {\n\
             \x20       Lit(n) => println(n),\n\
             \x20       Iff(nd) => println(nd.span.a),\n\
             \x20       Blk(b) => {\n\
             \x20           println(b.span.a);\n\
             \x20           println(b.stmts[0]);\n\
             \x20           println(b.stmts[1]);\n\
             \x20           match b.tail { Some(t) => match t { Lit(v) => println(v), Iff(_) => println(-1), Blk(_) => println(-2) }, None => println(-3) }\n\
             \x20       }\n\
             \x20   }\n\
             \x20   let ife = E.Iff(IfNode { cond: E.Lit(7), then_block: mk_block(20, 2), span: Span { a: 5, b: 0, c: 0, d: 0 } });\n\
             \x20   match ife {\n\
             \x20       Lit(n) => println(n),\n\
             \x20       Iff(nd) => {\n\
             \x20           println(nd.span.a);\n\
             \x20           let tb = nd.then_block;\n\
             \x20           println(tb.span.a);\n\
             \x20           println(tb.stmts[0]);\n\
             \x20       }\n\
             \x20       Blk(_) => println(-9)\n\
             \x20   }\n\
             }",
            &["1", "10", "11", "99", "5", "2", "20"],
            "shared_enum_boxed_struct_payload_moveout_no_double_free",
        );
    }

    /// slice-3c-iv: a heap-BOXED `Option[Wide]` local moved whole into a
    /// struct literal (`Holder { body: body }` for `let mut body =
    /// Some(mk_wide())`) must transfer the box's ownership to the new struct.
    /// Before the fix the builder fn's `BoxedEnumDrop` freed the box at scope
    /// exit while the returned `Holder` still referenced it — a use-after-free
    /// the reader then dereferenced (selfhost slice 3c-iv's `parse_trait_method`
    /// → `render_block` garbage / SIGSEGV). The boxed `Wide` owns a ≥36-byte
    /// `String` so a wrongly-freed-or-leaked box is visible to LSan (Linux) and
    /// the UAF read is caught by macOS ASAN; the 50-iter loop forces allocator
    /// reuse so a dangling box reads back corrupted.
    #[test]
    fn asan_boxed_option_moved_into_struct_literal_no_uaf() {
        assert_clean_asan_run(
            "struct Wide { tag: i64, payload: String }\n\
             struct Holder { name: String, body: Option[Wide] }\n\
             fn mk_wide() -> Wide {\n\
             \x20   Wide { tag: 7, payload: \"boxed-option-payload-string-long-enough-aaaa\".to_string() }\n\
             }\n\
             fn build() -> Holder {\n\
             \x20   let mut body: Option[Wide] = None;\n\
             \x20   body = Some(mk_wide());\n\
             \x20   Holder { name: \"holder-name-payload-string-long-enough\".to_string(), body: body }\n\
             }\n\
             fn read(h: Holder) -> i64 {\n\
             \x20   let Holder { name, body } = h;\n\
             \x20   let n = name.len() as i64;\n\
             \x20   match body {\n\
             \x20       Some(w) => { let Wide { tag, payload } = w; tag + (payload.len() as i64) + n }\n\
             \x20       None => n,\n\
             \x20   }\n\
             }\n\
             fn main() {\n\
             \x20   let mut acc = 0i64;\n\
             \x20   let mut i = 0;\n\
             \x20   while i < 50 {\n\
             \x20       acc = acc + read(build());\n\
             \x20       i = i + 1;\n\
             \x20   }\n\
             \x20   println(acc);\n\
             }\n",
            &["4450"],
            "asan_boxed_option_moved_into_struct_literal_no_uaf",
        );
    }

    #[test]
    fn asan_mapval_inner_map_insert_get_no_uaf() {
        // Slice 3r leg 1 (gap (d) sibling): `m.insert(k, inner)` where `inner`
        // is a Map binding never suppressed the source's `FreeMapHandle` — the
        // inner handle was freed at the builder's scope exit and the outer
        // map's stored handle dangled (SIGSEGV on `m.get(k)` read-back; the
        // suppression walk had arms for Vec/String/shared/enum/struct/tuple
        // but none for a Map/Set-handle binding). Fixed with a branch-safe
        // null-store of the source slot (`karac_map_free*` null-checks).
        assert_clean_asan_run(
            r#"
fn build(n: i64) -> Map[i64, Map[i64, String]] {
    let mut inner: Map[i64, String] = Map.new();
    inner.insert(n, f"inner payload padded out beyond thirty-six bytes {n}");
    let mut m: Map[i64, Map[i64, String]] = Map.new();
    m.insert(n, inner);
    m
}
fn main() {
    let mut i = 0;
    while i < 3 {
        let m = build(i);
        match m.get(i) {
            Some(inner) => {
                match inner.get(i) {
                    Some(s) => { println(s.len()); },
                    None => { println("inner-missing"); },
                }
            },
            None => { println("outer-missing"); },
        }
        i = i + 1;
    };
}
"#,
            &["50", "50", "50"],
            "mapval_inner_map_insert_get_no_uaf",
        );
    }

    #[test]
    fn asan_mapval_struct_double_get_no_double_free() {
        // Slice 3r leg 2: `Option[Holder]` is a WIDE payload (4 words > the
        // 3-word inline area), so a `match m.get(k)` scrutinee boxes the
        // bit-copied value — and `track_freshtemp_boxed_enum_scrutinee` armed
        // the box drop's INNER struct walk, freeing the `name` buffer the box
        // merely borrows from the bucket. The second `get` double-freed it
        // (exit 133 pre-fix). A borrow-call scrutinee now gets a box-only free.
        assert_clean_asan_run(
            r#"
struct Holder {
    name: String,
    id: i64,
}
fn build(n: i64) -> Map[i64, Holder] {
    let mut m: Map[i64, Holder] = Map.new();
    let h = Holder { name: f"holder payload padded out beyond thirty-six bytes {n}", id: n };
    m.insert(n, h);
    m
}
fn main() {
    let mut i = 0;
    while i < 3 {
        let m = build(i);
        match m.get(i) {
            Some(h) => { println(h.name.len() + h.id); },
            None => { println("missing"); },
        }
        match m.get(i) {
            Some(h) => { println(h.id); },
            None => { println("missing"); },
        }
        i = i + 1;
    };
}
"#,
            &["51", "0", "52", "1", "53", "2"],
            "mapval_struct_double_get_no_double_free",
        );
    }

    #[test]
    fn asan_mapval_struct_scope_exit_drop_no_leak() {
        // Slice 3r leg 3 (deferred gap (d)): a struct value's heap content was
        // never freed by the map's scope-exit cleanup — `val_is_vec` only
        // covers the `{ptr,len,cap}` overlay, and `Holder` isn't that shape.
        // The FreeMapHandle arm now routes through
        // `karac_map_free_with_val_drop_fn` with the synthesized
        // `karac_drop_*` value drop. LSan-RED pre-fix (one name buffer per
        // build). Runtime f-string payloads per the 3p spelling-trap
        // discipline.
        assert_clean_asan_run(
            r#"
struct Holder {
    name: String,
    id: i64,
}
fn build(n: i64) -> Map[i64, Holder] {
    let mut m: Map[i64, Holder] = Map.new();
    let h = Holder { name: f"holder payload padded out beyond thirty-six bytes {n}", id: n };
    m.insert(n, h);
    m
}
fn main() {
    let mut i = 0;
    while i < 3 {
        let m = build(i);
        println(m.len());
        i = i + 1;
    };
}
"#,
            &["1", "1", "1"],
            "mapval_struct_scope_exit_drop_no_leak",
        );
    }

    #[test]
    fn asan_mapval_nested_vec_scope_exit_drop_no_leak() {
        // Slice 3r leg 3 (deferred gap (d), the `Map[K, Vec[Vec[T]]]` value
        // leg): `val_is_vec = 1` freed only the value's OUTER buffer; the
        // middle Vec's element buffers and their strings leaked (176 bytes per
        // build pre-fix). The per-value drop fn is the recursive
        // `karac_drop_Vec_<elem>` from the slice-3n family.
        assert_clean_asan_run(
            r#"
fn build(n: i64) -> Map[i64, Vec[Vec[String]]] {
    let mut inner: Vec[String] = Vec.new();
    inner.push(f"nested payload padded out beyond thirty-six bytes {n}");
    let mut outer: Vec[Vec[String]] = Vec.new();
    outer.push(inner);
    let mut m: Map[i64, Vec[Vec[String]]] = Map.new();
    m.insert(n, outer);
    m
}
fn main() {
    let mut i = 0;
    while i < 3 {
        let m = build(i);
        println(m.len());
        i = i + 1;
    };
}
"#,
            &["1", "1", "1"],
            "mapval_nested_vec_scope_exit_drop_no_leak",
        );
    }

    #[test]
    fn asan_mapval_inner_map_scope_exit_drop_no_leak() {
        // Slice 3r leg 3 (deferred gap (d)): an inner-Map VALUE — once leg 1's
        // insert move-suppression makes the bucket the handle's owner — must be
        // freed by the outer map's cleanup via the per-value drop fn
        // (`karac_drop_Map_*`, which recursively releases the inner map's own
        // String values). LSan-RED after leg 1 alone (inner handle + its
        // stored strings leak per build).
        assert_clean_asan_run(
            r#"
fn build(n: i64) -> Map[i64, Map[i64, String]] {
    let mut inner: Map[i64, String] = Map.new();
    inner.insert(n, f"inner payload padded out beyond thirty-six bytes {n}");
    let mut m: Map[i64, Map[i64, String]] = Map.new();
    m.insert(n, inner);
    m
}
fn main() {
    let mut i = 0;
    while i < 3 {
        let m = build(i);
        println(m.len());
        i = i + 1;
    };
}
"#,
            &["1", "1", "1"],
            "mapval_inner_map_scope_exit_drop_no_leak",
        );
    }

    #[test]
    fn asan_mapval_vec_of_struct_valued_maps_no_leak() {
        // Slice 3r: `Vec[Map[i64, Holder]]` — the Vec's element-drop loop
        // frees each handle via `emit_free_one_map_handle` with the same
        // per-value drop fn a standalone binding gets
        // (`vec_elem_map_drop_for_type_expr` → `map_temp_cleanup_parts`).
        assert_clean_asan_run(
            r#"
struct Holder { name: String, id: i64 }
fn build(n: i64) -> Map[i64, Holder] {
    let mut m: Map[i64, Holder] = Map.new();
    let h = Holder { name: f"holder payload padded out beyond thirty-six bytes {n}", id: n };
    m.insert(n, h);
    m
}
fn main() {
    let mut v: Vec[Map[i64, Holder]] = Vec.new();
    v.push(build(1));
    v.push(build(2));
    println(v.len());
}
"#,
            &["2"],
            "mapval_vec_of_struct_valued_maps_no_leak",
        );
    }

    #[test]
    fn asan_mapval_iterate_struct_values_no_double_free() {
        // Slice 3r: `for (k, v) in m` over a struct-valued map with the
        // per-value drop armed — the loop binding is a bit-copy of the
        // bucket's value; it must alias (not own), or every iteration
        // double-frees against the map's scope-exit value drop.
        assert_clean_asan_run(
            r#"
struct Holder { name: String, id: i64 }
fn main() {
    let mut m: Map[i64, Holder] = Map.new();
    let h = Holder { name: f"holder payload padded out beyond thirty-six bytes {7}", id: 7 };
    m.insert(7, h);
    let mut total = 0;
    for (k, v) in m {
        total = total + k + v.id + (v.name.len() as i64);
    }
    println(total);
}
"#,
            &["65"],
            "mapval_iterate_struct_values_no_double_free",
        );
    }

    #[test]
    fn asan_mapval_overwrite_displaced_struct_value_no_leak() {
        // Slice 3r: inserting over an existing key displaces the OLD value
        // into the discarded `Option[Holder]` result (boxed — Holder is a
        // wide payload); the fresh-temp boxed-Option machinery must free
        // both the box and the displaced value's interior heap.
        assert_clean_asan_run(
            r#"
struct Holder { name: String, id: i64 }
fn main() {
    let mut m: Map[i64, Holder] = Map.new();
    let h1 = Holder { name: f"first payload padded out beyond thirty-six bytes {7}", id: 7 };
    let h2 = Holder { name: f"second payload padded out beyond thirty-six bytes {8}", id: 8 };
    m.insert(7, h1);
    m.insert(7, h2);
    println(m.len());
}
"#,
            &["1"],
            "mapval_overwrite_displaced_struct_value_no_leak",
        );
    }

    #[test]
    fn asan_mapval_triple_nested_map_value_no_leak() {
        // Slice 3r: `Map[i64, Map[i64, Map[i64, String]]]` — the upgraded
        // `karac_drop_Map_<K>_<V>` recurses through
        // `map_val_drop_fn_for_type_expr` per level (the 0.c placeholder
        // freed only the handle), so the deepest strings drop.
        assert_clean_asan_run(
            r#"
fn main() {
    let mut inner2: Map[i64, String] = Map.new();
    inner2.insert(1, f"deepest payload padded out beyond thirty-six bytes {1}");
    let mut inner1: Map[i64, Map[i64, String]] = Map.new();
    inner1.insert(2, inner2);
    let mut m: Map[i64, Map[i64, Map[i64, String]]] = Map.new();
    m.insert(3, inner1);
    println(m.len());
}
"#,
            &["1"],
            "mapval_triple_nested_map_value_no_leak",
        );
    }

    #[test]
    fn asan_mapval_string_key_struct_value_no_leak() {
        // Slice 3r: `Map[String, Holder]` — the key half keeps the
        // `drop_key` flag contract while the value half rides the drop fn;
        // `karac_map_free_with_val_drop_fn` handles both simultaneously.
        assert_clean_asan_run(
            r#"
struct Holder { name: String, id: i64 }
fn main() {
    let mut m: Map[String, Holder] = Map.new();
    let h = Holder { name: f"holder payload padded out beyond thirty-six bytes {7}", id: 7 };
    m.insert(f"key padded out beyond thirty-six bytes for lsan {7}", h);
    println(m.len());
}
"#,
            &["1"],
            "mapval_string_key_struct_value_no_leak",
        );
    }

    #[test]
    fn asan_mapval_remove_struct_value_no_leak() {
        // Slice 3r: `m.remove(k)` moves the stored value out into a
        // DISCARDED `Option[Holder]` (boxed — wide payload); the
        // discarded-boxed-Option tracker must free the box AND the
        // payload's interior heap (`try_track_discarded_boxed_option`).
        assert_clean_asan_run(
            r#"
struct Holder { name: String, id: i64 }
fn main() {
    let mut m: Map[i64, Holder] = Map.new();
    let h = Holder { name: f"holder payload padded out beyond thirty-six bytes {7}", id: 7 };
    m.insert(7, h);
    m.remove(7);
    println(m.len());
}
"#,
            &["0"],
            "mapval_remove_struct_value_no_leak",
        );
    }

    #[test]
    fn asan_mapval_clear_struct_value_no_leak() {
        // Slice 3r: `m.clear()` on a struct-valued map — the clear arm now
        // routes through `karac_map_clear_with_val_drop_fn` (the clear
        // sibling of the scope-exit per-value walk); the flag-based clear
        // leaked every stored value's heap (64 bytes here, visible even to
        // macOS `leaks`).
        assert_clean_asan_run(
            r#"
struct Holder { name: String, id: i64 }
fn main() {
    let mut m: Map[i64, Holder] = Map.new();
    let h = Holder { name: f"holder payload padded out beyond thirty-six bytes {7}", id: 7 };
    m.insert(7, h);
    m.clear();
    println(m.len());
}
"#,
            &["0"],
            "mapval_clear_struct_value_no_leak",
        );
    }

    #[test]
    fn asan_getmove_match_string_payload_moveout_no_double_free() {
        // Slice 3s (B-2026-07-01-12): `let s = match m.get(k) { Some(x) => x,
        // … }` — Map.get is VALUE-typed (`Option[V]`), so the typechecker
        // blesses the move-out, but codegen bound `x` as an ALIAS of the
        // bucket's value (borrow-mode bind) and the escaping arm-tail handed
        // that alias to `s`, which frees it — double-free against the map's
        // `drop_val` walk (exit 133 pre-fix). The arm now deep-clones the
        // payload when the arm body moves the binding.
        assert_clean_asan_run(
            r#"
fn main() {
    let mut i = 0;
    while i < 3 {
        let mut m: Map[i64, String] = Map.new();
        m.insert(i, f"map string payload padded beyond thirty-six bytes {i}");
        let s = match m.get(i) {
            Some(x) => x,
            None => f"none-{i}",
        };
        println(s.len());
        i = i + 1;
    };
}
"#,
            &["51", "51", "51"],
            "getmove_match_string_payload_moveout_no_double_free",
        );
    }

    #[test]
    fn asan_getmove_match_struct_payload_moveout_no_double_free() {
        // Slice 3s: the struct-payload sibling — `Holder` rides the boxed
        // wide-payload path through the scrutinee, and the bound copy's
        // `name` aliases the bucket's until the arm-move clone.
        assert_clean_asan_run(
            r#"
struct Holder { name: String, id: i64 }
fn main() {
    let mut i = 0;
    while i < 3 {
        let mut m: Map[i64, Holder] = Map.new();
        let h = Holder { name: f"holder payload padded beyond thirty-six bytes {i}", id: i };
        m.insert(i, h);
        let out = match m.get(i) {
            Some(x) => x,
            None => Holder { name: f"none-{i}", id: 0 },
        };
        println(out.name.len() + out.id);
        i = i + 1;
    };
}
"#,
            &["47", "48", "49"],
            "getmove_match_struct_payload_moveout_no_double_free",
        );
    }

    #[test]
    fn asan_getmove_iflet_string_payload_moveout_no_double_free() {
        // Slice 3s: the if-let form of the arm-tail move-out.
        assert_clean_asan_run(
            r#"
fn main() {
    let mut i = 0;
    while i < 3 {
        let mut m: Map[i64, String] = Map.new();
        m.insert(i, f"map string payload padded beyond thirty-six bytes {i}");
        let s = if let Some(x) = m.get(i) { x } else { f"none-{i}" };
        println(s.len());
        i = i + 1;
    };
}
"#,
            &["51", "51", "51"],
            "getmove_iflet_string_payload_moveout_no_double_free",
        );
    }

    #[test]
    fn asan_getmove_match_arm_push_consume_no_double_free() {
        // Slice 3s: the NON-tail consume — the arm body pushes the bound
        // payload into a Vec. The move-detector must classify a method-arg
        // occurrence as a move (the Vec bit-copies + suppresses, so without
        // the clone the Vec and the map both own the same buffer).
        assert_clean_asan_run(
            r#"
fn main() {
    let mut out: Vec[String] = Vec.new();
    let mut i = 0;
    while i < 3 {
        let mut m: Map[i64, String] = Map.new();
        m.insert(i, f"map string payload padded beyond thirty-six bytes {i}");
        match m.get(i) {
            Some(x) => { out.push(x); },
            None => {},
        }
        i = i + 1;
    };
    println(out.len());
}
"#,
            &["3"],
            "getmove_match_arm_push_consume_no_double_free",
        );
    }

    #[test]
    fn asan_getmove_get_or_result_binding_no_double_free() {
        // Slice 3s adjacency probe: `let s = m.get_or(k, default)` byte-copies
        // the stored value out as the RESULT — the binding must own an
        // independent copy (or the map's value walk double-frees).
        assert_clean_asan_run(
            r#"
fn main() {
    let mut i = 0;
    while i < 3 {
        let mut m: Map[i64, String] = Map.new();
        m.insert(i, f"map string payload padded beyond thirty-six bytes {i}");
        let s = m.get_or(i, f"default-{i}");
        println(s.len());
        i = i + 1;
    };
}
"#,
            &["51", "51", "51"],
            "getmove_get_or_result_binding_no_double_free",
        );
    }

    #[test]
    fn asan_getmove_iter_value_push_no_double_free() {
        // Slice 3s adjacency probe: `for (k, v) in m { out.push(v); }` — the
        // iteration value binding pushed into a Vec must not leave the Vec
        // and the map co-owning one buffer.
        assert_clean_asan_run(
            r#"
fn main() {
    let mut out: Vec[String] = Vec.new();
    let mut m: Map[i64, String] = Map.new();
    let mut i = 0;
    while i < 3 {
        m.insert(i, f"map string payload padded beyond thirty-six bytes {i}");
        i = i + 1;
    };
    for (k, v) in m {
        out.push(v);
    }
    println(out.len());
}
"#,
            &["3"],
            "getmove_iter_value_push_no_double_free",
        );
    }

    #[test]
    fn asan_getmove_iflet_readonly_no_double_free() {
        // Slice 3s: READ-ONLY `if let Some(x) = m.get(k)` crashed pre-fix —
        // if-let (unlike match) never consulted `scrutinee_is_borrow_call`,
        // so the aliased payload got an owned track and the arm-end drain
        // freed the bucket's buffer (exit 133 on plain Map[i64, String]).
        assert_clean_asan_run(
            r#"
fn main() {
    let mut i = 0;
    while i < 3 {
        let mut m: Map[i64, String] = Map.new();
        m.insert(i, f"map string payload padded beyond thirty-six bytes {i}");
        if let Some(x) = m.get(i) {
            println(x.len());
        }
        i = i + 1;
    };
}
"#,
            &["51", "51", "51"],
            "getmove_iflet_readonly_no_double_free",
        );
    }

    #[test]
    fn asan_getmove_iflet_owned_tail_move_no_double_free() {
        // Slice 3s: `if let Some(x) = v.pop() { x }` with an OWNED scrutinee
        // crashed pre-fix — if-let had NO then-tail move suppression (match
        // has had it for arms all along), so the drain freed the escaping
        // buffer and the caller's binding double-freed. Distinct from the
        // borrow-clone leg: this is the owned-binding tail-move hole.
        assert_clean_asan_run(
            r#"
fn main() {
    let mut i = 0;
    while i < 3 {
        let mut v: Vec[String] = Vec.new();
        v.push(f"vec string payload padded beyond thirty-six bytes {i}");
        let s = if let Some(x) = v.pop() { x } else { f"none-{i}" };
        println(s.len());
        i = i + 1;
    };
}
"#,
            &["51", "51", "51"],
            "getmove_iflet_owned_tail_move_no_double_free",
        );
    }

    #[test]
    fn asan_getmove_whilelet_get_readonly_no_double_free() {
        // Slice 3s: `while let Some(x) = m.get(i)` — the while-let bind site
        // gets the same borrow-call classification as match/if-let.
        assert_clean_asan_run(
            r#"
fn main() {
    let mut m: Map[i64, String] = Map.new();
    m.insert(0, f"map string payload padded beyond thirty-six bytes {0}");
    let mut i = 0;
    while let Some(x) = m.get(i) {
        println(x.len());
        i = i + 1;
    }
    println(i);
}
"#,
            &["51", "1"],
            "getmove_whilelet_get_readonly_no_double_free",
        );
    }

    #[test]
    fn asan_getmove_letelse_get_escaping_binding_no_double_free() {
        // Slice 3s: a `let Some(s) = m.get(k) else { … }` binding escapes
        // into the enclosing scope by construction — the payload clone is
        // unconditional there (no arm to analyze).
        assert_clean_asan_run(
            r#"
fn read(m: ref Map[i64, String]) -> i64 {
    let Some(s) = m.get(7) else { return 0 }
    s.len()
}
fn main() {
    let mut i = 0;
    while i < 3 {
        let mut m: Map[i64, String] = Map.new();
        m.insert(7, f"map string payload padded beyond thirty-six bytes {7}");
        println(read(m));
        i = i + 1;
    };
}
"#,
            &["51", "51", "51"],
            "getmove_letelse_get_escaping_binding_no_double_free",
        );
    }

    #[test]
    fn asan_structpat_option_destructure_no_double_free() {
        // Slice 3t: `Some(Holder { name, id })` was UNIMPLEMENTED in codegen
        // (the payload-width helpers defaulted a Struct pattern to one word,
        // the reconstruction bound the raw word, and every field stayed
        // unbound — "Undefined variable"). Once fields bind, the named
        // binding's BoxedEnumDrop inner walk also freed the consumed fields
        // (double-free, DCE-masked unless the payload is observed) —
        // `suppress_boxed_payload_struct_destructure` zeroes consumed field
        // caps inside the box.
        assert_clean_asan_run(
            r#"
struct Holder { name: String, id: i64 }
fn main() {
    let mut i = 0;
    while i < 3 {
        let o: Option[Holder] = Some(Holder { name: f"holder payload padded beyond thirty-six bytes {i}", id: i });
        match o {
            Some(Holder { name, id }) => { println(name.len() + id); },
            None => { println("missing"); },
        }
        i = i + 1;
    };
}
"#,
            &["47", "48", "49"],
            "structpat_option_destructure_no_double_free",
        );
    }

    #[test]
    fn asan_structpat_partial_destructure_unbound_field_no_leak() {
        // Slice 3t: `Some(Pair2 { first, .. })` — the UNBOUND `second` stays
        // owned by the box; the per-field cap-zero must not disarm it (the
        // box's inner walk is its only free).
        assert_clean_asan_run(
            r#"
struct Pair2 { first: String, second: String }
fn main() {
    let mut i = 0;
    while i < 3 {
        let o: Option[Pair2] = Some(Pair2 { first: f"first payload padded beyond thirty-six bytes {i}", second: f"second payload padded beyond thirty-six bytes {i}" });
        match o {
            Some(Pair2 { first, .. }) => { println(first.len()); },
            None => { println("missing"); },
        }
        i = i + 1;
    };
}
"#,
            &["46", "46", "46"],
            "structpat_partial_destructure_unbound_field_no_leak",
        );
    }

    #[test]
    fn asan_structpat_result_ok_destructure_no_double_free() {
        // Slice 3t: the Result sibling — `Ok(Holder { name, id })` over a
        // named `Result[Holder, i64]` binding (boxed Ok payload).
        assert_clean_asan_run(
            r#"
struct Holder { name: String, id: i64 }
fn main() {
    let mut i = 0;
    while i < 3 {
        let r: Result[Holder, i64] = Ok(Holder { name: f"holder payload padded beyond thirty-six bytes {i}", id: i });
        match r {
            Ok(Holder { name, id }) => { println(name.len() + id); },
            Err(e) => { println(e); },
        }
        i = i + 1;
    };
}
"#,
            &["47", "48", "49"],
            "structpat_result_ok_destructure_no_double_free",
        );
    }

    #[test]
    fn asan_structpat_mapget_destructure_readonly_no_double_free() {
        // Slice 3t: destructuring a `Map.get` payload READ-ONLY — the field
        // bindings alias the bucket (borrow mode), the scrutinee's box is
        // box-only-freed, the map keeps sole ownership.
        assert_clean_asan_run(
            r#"
struct Holder { name: String, id: i64 }
fn main() {
    let mut i = 0;
    while i < 3 {
        let mut m: Map[i64, Holder] = Map.new();
        m.insert(i, Holder { name: f"holder payload padded beyond thirty-six bytes {i}", id: i });
        match m.get(i) {
            Some(Holder { name, id }) => { println(name.len() + id); },
            None => { println("missing"); },
        }
        i = i + 1;
    };
}
"#,
            &["47", "48", "49"],
            "structpat_mapget_destructure_readonly_no_double_free",
        );
    }

    #[test]
    fn asan_structpat_mapget_field_escape_no_double_free() {
        // Slice 3t: an ESCAPING destructured field over a `Map.get`
        // scrutinee (`Some(Holder { name, .. }) => name`) — the 3s clone
        // fixup extended to FIELD granularity (read-only fields stay
        // zero-cost aliases; the escapee owns an independent copy).
        assert_clean_asan_run(
            r#"
struct Holder { name: String, id: i64 }
fn main() {
    let mut i = 0;
    while i < 3 {
        let mut m: Map[i64, Holder] = Map.new();
        m.insert(i, Holder { name: f"holder payload padded beyond thirty-six bytes {i}", id: i });
        let s = match m.get(i) {
            Some(Holder { name, .. }) => name,
            None => f"none-{i}",
        };
        println(s.len());
        i = i + 1;
    };
}
"#,
            &["47", "47", "47"],
            "structpat_mapget_field_escape_no_double_free",
        );
    }

    #[test]
    fn asan_structpat_iflet_destructure_no_double_free() {
        // Slice 3t: the if-let form of the boxed-payload struct destructure.
        assert_clean_asan_run(
            r#"
struct Holder { name: String, id: i64 }
fn main() {
    let mut i = 0;
    while i < 3 {
        let o: Option[Holder] = Some(Holder { name: f"holder payload padded beyond thirty-six bytes {i}", id: i });
        if let Some(Holder { name, id }) = o {
            println(name.len() + id);
        }
        i = i + 1;
    };
}
"#,
            &["47", "48", "49"],
            "structpat_iflet_destructure_no_double_free",
        );
    }

    #[test]
    fn asan_structpat_vec_field_destructure_no_double_free() {
        // Slice 3t: a `Vec[String]` FIELD destructured out — the consumed
        // field's cap-zero must recurse correctly (the box walk would
        // otherwise free the Vec's buffer AND its element strings that the
        // binding now owns).
        assert_clean_asan_run(
            r#"
struct Bag { items: Vec[String], id: i64 }
fn main() {
    let mut i = 0;
    while i < 3 {
        let mut v: Vec[String] = Vec.new();
        v.push(f"bag item payload padded beyond thirty-six bytes {i}");
        let o: Option[Bag] = Some(Bag { items: v, id: i });
        match o {
            Some(Bag { items, id }) => { println((items[0].len() as i64) + id); },
            None => { println("missing"); },
        }
        i = i + 1;
    };
}
"#,
            &["49", "50", "51"],
            "structpat_vec_field_destructure_no_double_free",
        );
    }

    #[test]
    fn asan_structpat_whilelet_pop_destructure_no_double_free() {
        // Slice 3t: `while let Some(Holder { name, id }) = v.pop()` — the
        // fresh-temp boxed scrutinee path (box-only free; fields owned by
        // their bindings), looped.
        assert_clean_asan_run(
            r#"
struct Holder { name: String, id: i64 }
fn main() {
    let mut v: Vec[Holder] = Vec.new();
    let mut i = 0;
    while i < 3 {
        v.push(Holder { name: f"holder payload padded beyond thirty-six bytes {i}", id: i });
        i = i + 1;
    };
    let mut total = 0;
    while let Some(Holder { name, id }) = v.pop() {
        total = total + (name.len() as i64) + id;
    }
    println(total);
}
"#,
            &["144"],
            "structpat_whilelet_pop_destructure_no_double_free",
        );
    }

    #[test]
    fn asan_boxelem_vec_option_wide_struct_scope_drop_no_leak() {
        // Slice 3u: `Vec[Option[Holder]]` — Holder (4 words) exceeds
        // Option's 3-word inline area, so each Some element carries a heap
        // BOX. The 3p element-drop gate admitted only inline String/Vec
        // payloads; boxed elements leaked box + interior (336 bytes / 3
        // iterations pre-fix). The extended `karac_drop_Option_Holder`
        // walks the box (inner struct drop + free) on the Some tag.
        assert_clean_asan_run(
            r#"
struct Holder { name: String, id: i64 }
fn build(n: i64) -> Vec[Option[Holder]] {
    let mut v: Vec[Option[Holder]] = Vec.new();
    v.push(Some(Holder { name: f"holder payload padded beyond thirty-six bytes {n}", id: n }));
    v.push(None);
    v
}
fn main() {
    let mut i = 0;
    while i < 3 {
        let v = build(i);
        println(v.len());
        i = i + 1;
    };
}
"#,
            &["2", "2", "2"],
            "boxelem_vec_option_wide_struct_scope_drop_no_leak",
        );
    }

    #[test]
    fn asan_boxelem_vec_result_inline_struct_scope_drop_no_leak() {
        // Slice 3u: `Vec[Result[Holder, i64]]` — Holder (4 words) FITS
        // Result's 5-word area, so the Ok payload is INLINE — the
        // struct-payload flavor the 3q gate (String/Vec overlays only)
        // declined. The payload words overlay w0.. contiguously, so the
        // element drop GEPs to w0 and calls the struct's drop in place.
        assert_clean_asan_run(
            r#"
struct Holder { name: String, id: i64 }
fn build(n: i64) -> Vec[Result[Holder, i64]] {
    let mut v: Vec[Result[Holder, i64]] = Vec.new();
    v.push(Ok(Holder { name: f"holder payload padded beyond thirty-six bytes {n}", id: n }));
    v.push(Err(n));
    v
}
fn main() {
    let mut i = 0;
    while i < 3 {
        let v = build(i);
        println(v.len());
        i = i + 1;
    };
}
"#,
            &["2", "2", "2"],
            "boxelem_vec_result_inline_struct_scope_drop_no_leak",
        );
    }

    #[test]
    fn asan_boxelem_vec_option_inline_struct_scope_drop_no_leak() {
        // Slice 3u: `Vec[Option[Pair]]` — Pair (3 words) fits Option's
        // inline area: the Option-side inline-STRUCT payload flavor.
        assert_clean_asan_run(
            r#"
struct Pair { s: String }
fn build(n: i64) -> Vec[Option[Pair]] {
    let mut v: Vec[Option[Pair]] = Vec.new();
    v.push(Some(Pair { s: f"pair payload padded beyond thirty-six bytes {n}" }));
    v.push(None);
    v
}
fn main() {
    let mut i = 0;
    while i < 3 {
        let v = build(i);
        println(v.len());
        i = i + 1;
    };
}
"#,
            &["2", "2", "2"],
            "boxelem_vec_option_inline_struct_scope_drop_no_leak",
        );
    }

    #[test]
    fn asan_boxelem_push_boxed_binding_no_double_free() {
        // Slice 3u: `let o = Some(Holder{...}); v.push(o)` — the moved
        // BOXED binding's `BoxedEnumDrop` must disarm (tag=None store, the
        // boxed sibling of the inline cap-zero) or it double-frees against
        // the newly-armed element drop.
        assert_clean_asan_run(
            r#"
struct Holder { name: String, id: i64 }
fn main() {
    let mut i = 0;
    while i < 3 {
        let mut v: Vec[Option[Holder]] = Vec.new();
        let o: Option[Holder] = Some(Holder { name: f"holder payload padded beyond thirty-six bytes {i}", id: i });
        v.push(o);
        println(v.len());
        i = i + 1;
    };
}
"#,
            &["1", "1", "1"],
            "boxelem_push_boxed_binding_no_double_free",
        );
    }

    #[test]
    fn asan_boxelem_loop_elem_destructure_consume_no_double_free() {
        // Slice 3u: `for o in v { match o { Some(Holder { name, id }) => …
        // } }` — the loop binding is a bit-copy of the element; with the
        // element drop armed it must be marked as a BORROW
        // (`for_loop_borrow_vars`, extended to boxed/inline-struct
        // payloads) so the destructured fields alias.
        assert_clean_asan_run(
            r#"
struct Holder { name: String, id: i64 }
fn main() {
    let mut v: Vec[Option[Holder]] = Vec.new();
    let mut i = 0;
    while i < 3 {
        v.push(Some(Holder { name: f"holder payload padded beyond thirty-six bytes {i}", id: i }));
        i = i + 1;
    };
    let mut total = 0;
    for o in v {
        match o {
            Some(Holder { name, id }) => { total = total + (name.len() as i64) + id; },
            None => {},
        }
    }
    println(total);
}
"#,
            &["144"],
            "boxelem_loop_elem_destructure_consume_no_double_free",
        );
    }

    #[test]
    fn asan_boxelem_tuple_payload_escape_no_double_free() {
        // Slice 3u leg A: `Some((a, b)) => a` over `m.get(k)` — the tuple
        // flavor of the 3s escaping-borrow clone (exit 133 pre-fix). The
        // escaping tuple ELEMENT is cloned; read-only elements stay
        // aliases.
        assert_clean_asan_run(
            r#"
fn main() {
    let mut i = 0;
    while i < 3 {
        let mut m: Map[i64, (String, i64)] = Map.new();
        m.insert(i, (f"tuple payload padded beyond thirty-six bytes {i}", i));
        let s = match m.get(i) {
            Some((a, b)) => a,
            None => f"none-{i}",
        };
        println(s.len());
        i = i + 1;
    };
}
"#,
            &["46", "46", "46"],
            "boxelem_tuple_payload_escape_no_double_free",
        );
    }

    #[test]
    fn asan_tail3v_vec_of_vecdeque_scope_drop_no_leak() {
        // Slice 3v: `Vec[VecDeque[String]]` — VecDeque shares Vec's linear
        // {ptr,len,cap} layout (memmove push_front, not a ring), so the
        // recursive element drop is exact; the gates simply never admitted
        // the VecDeque head (strings leaked, one per element, pre-fix).
        assert_clean_asan_run(
            r#"
fn build(n: i64) -> Vec[VecDeque[String]] {
    let mut d: VecDeque[String] = VecDeque.new();
    d.push_back(f"deque payload padded beyond thirty-six bytes {n}");
    let mut v: Vec[VecDeque[String]] = Vec.new();
    v.push(d);
    v
}
fn main() {
    let mut i = 0;
    while i < 3 {
        let v = build(i);
        println(v.len());
        i = i + 1;
    };
}
"#,
            &["1", "1", "1"],
            "tail3v_vec_of_vecdeque_scope_drop_no_leak",
        );
    }

    #[test]
    fn asan_tail3v_vecdeque_option_elem_no_leak() {
        // Slice 3v: `Vec[Option[VecDeque[String]]]` — the VecDeque admission
        // must reach through the Option payload gates too.
        assert_clean_asan_run(
            r#"
fn build(n: i64) -> Vec[Option[VecDeque[String]]] {
    let mut d: VecDeque[String] = VecDeque.new();
    d.push_back(f"deque payload padded beyond thirty-six bytes {n}");
    let mut v: Vec[Option[VecDeque[String]]] = Vec.new();
    v.push(Some(d));
    v
}
fn main() {
    let mut i = 0;
    while i < 3 {
        let v = build(i);
        println(v.len());
        i = i + 1;
    };
}
"#,
            &["1", "1", "1"],
            "tail3v_vecdeque_option_elem_no_leak",
        );
    }

    #[test]
    fn asan_tail3v_get_unchecked_binding_no_double_free() {
        // Slice 3v: `let s = v.get_unchecked(0)` bound a shallow element
        // alias with an OWNED track — exit 133 at the two scope exits. Now
        // routes through the same deep-clone as `let s = v[i]`.
        assert_clean_asan_run(
            r#"
fn main() {
    let mut i = 0;
    while i < 3 {
        let mut v: Vec[String] = Vec.new();
        v.push(f"unchecked payload padded beyond thirty-six bytes {i}");
        unsafe {
            let s = v.get_unchecked(0);
            println(s.len());
        }
        println(v.len());
        i = i + 1;
    };
}
"#,
            &["50", "1", "50", "1", "50", "1"],
            "tail3v_get_unchecked_binding_no_double_free",
        );
    }

    #[test]
    fn asan_tail3v_whole_tuple_nontail_consume_no_leak() {
        // Slice 3v: `Some(x) => { take(x); }` over a Map.get tuple payload —
        // the whole-tuple clone (3u leg A) is now TRACKED via
        // `synthesize_tuple_drop_fn_te`, whose `type_expr_has_drop_heap`
        // guard needed the inferred `str` spelling (4th/5th trap sites:
        // the classifier, the tuple drop emitter, and its cap-zero dual).
        assert_clean_asan_run(
            r#"
fn take(t: (String, i64)) -> i64 {
    t.1
}
fn main() {
    let mut i = 0;
    while i < 3 {
        let mut m: Map[i64, (String, i64)] = Map.new();
        m.insert(i, (f"tuple payload padded beyond thirty-six bytes {i}", i));
        match m.get(i) {
            Some(x) => { println(take(x)); },
            None => { println(0 - 1); },
        }
        i = i + 1;
    };
}
"#,
            &["0", "1", "2"],
            "tail3v_whole_tuple_nontail_consume_no_leak",
        );
    }

    #[test]
    fn asan_tail3v_whole_tuple_tail_move_no_double_free() {
        // Slice 3v: the tail-move sibling — `let t = match m.get(i) {
        // Some(x) => x, … }` then consume `t`; the cloned tuple's track and
        // the arm-tail move suppression must compose (single free).
        assert_clean_asan_run(
            r#"
fn take(t: (String, i64)) -> i64 { t.1 }
fn main() {
    let mut i = 0;
    while i < 3 {
        let mut m: Map[i64, (String, i64)] = Map.new();
        m.insert(i, (f"tuple payload padded beyond thirty-six bytes {i}", i));
        let t = match m.get(i) {
            Some(x) => x,
            None => (f"none-{i}", 0 - 1),
        };
        println(take(t));
        i = i + 1;
    };
}
"#,
            &["0", "1", "2"],
            "tail3v_whole_tuple_tail_move_no_double_free",
        );
    }

    #[test]
    fn asan_statsref_reuse_and_fresh_temp_no_leak_no_double_free() {
        // B-2026-07-01-10: `Stats.*` params are now `ref Slice[f64]` and
        // the baked modes reach the ownership pass — reusing one dataset
        // across calls must neither leak nor double-free (the borrow arg
        // is NOT freed by the callee; a FRESH temp arg still frees via the
        // ref-rvalue materialization).
        assert_clean_asan_run(
            r#"
fn make(n: i64) -> Vec[f64] {
    let mut v: Vec[f64] = Vec.new();
    let mut i = 0;
    while i < n {
        v.push(1.5);
        i = i + 1;
    };
    v
}
fn main() {
    let mut v: Vec[f64] = Vec.new();
    v.push(1.0);
    v.push(2.0);
    v.push(3.0);
    println(Stats.sum(v));
    println(Stats.mean(v));
    let sorted = Stats.sort(v);
    println(sorted.len());
    println(Stats.sum(make(4)));
    println(v.len());
}
"#,
            &["6", "2", "3", "6", "3"],
            "statsref_reuse_and_fresh_temp_no_leak_no_double_free",
        );
    }

    #[test]
    fn asan_fnret_drop_temp_arg_passthrough_and_discard_single_fire() {
        // B-2026-07-01-7: fn-call-RETURNED Drop temps — as a consume arg
        // (drops once after the call), DISCARDED at statement position
        // (drops once), and passed THROUGH a `pass(g) -> Guard { g }`
        // into a binding (drops exactly once via the binding; the
        // passthrough guard skips the arg-temp registration — pre-guard
        // this shape double-fired AND double-freed the heap field on
        // both surfaces, probe f6). Heap-carrying Guard so the wrapper's
        // field cleanup is exercised; program structured so NLL and
        // scope-exit drop orders coincide (output is surface-identical).
        assert_clean_asan_run(
            r#"
struct Guard { name: String, id: i64 }
impl Drop for Guard {
    fn drop(mut ref self) { println(self.id); }
}
fn make(n: i64) -> Guard {
    Guard { name: f"guard payload padded beyond thirty-six bytes {n}", id: n }
}
fn consume(g: Guard) { println(100 + g.id); }
fn pass(g: Guard) -> Guard { g }
fn main() {
    let mut i = 0;
    while i < 3 {
        consume(make(i));
        i = i + 1;
    };
    make(60);
    println(999);
    let x = pass(make(50));
    println(x.name.len());
}
"#,
            &["100", "0", "101", "1", "102", "2", "60", "999", "47", "50"],
            "fnret_drop_temp_arg_passthrough_and_discard_single_fire",
        );
    }

    #[test]
    fn asan_gsort_vec_of_vec_string_sorts_and_frees() {
        // B-2026-06-30-15: `Vec[Vec[String]].sort()` — codegen previously
        // errored ("supports integer and String element types") and the
        // INTERPRETER silently no-op'd (value_compare had no Array arm, so
        // nested Vecs compared Equal and stable sort preserved insertion
        // order). Both fixed: the recursive `karac_cmp_Vec_String`
        // lexicographic comparator + the interp Array/Slice compare arms.
        // Assertions drain via pop() (owned move-out) — index reads of
        // heap elements have a PRE-EXISTING leak class unrelated to sort.
        assert_clean_asan_run(
            r#"
fn main() {
    let mut outer: Vec[Vec[String]] = Vec.new();
    let mut a: Vec[String] = Vec.new();
    a.push(f"banana payload padded beyond thirty-six bytes {1}");
    a.push(f"apple payload padded beyond thirty-six bytes {1}");
    let mut b: Vec[String] = Vec.new();
    b.push(f"apple payload padded beyond thirty-six bytes {1}");
    let mut c: Vec[String] = Vec.new();
    outer.push(a);
    outer.push(b);
    outer.push(c);
    outer.sort();
    while let Some(row) = outer.pop() {
        println(row.len());
    }
    println(outer.len());
}
"#,
            &["2", "1", "0", "0"],
            "gsort_vec_of_vec_string_sorts_and_frees",
        );
    }

    #[test]
    fn asan_gsort_tuple_and_float_elements() {
        // B-2026-06-30-15: tuple elements (per-field lexicographic — (1,z)
        // < (2,a) < (2,b)) and float elements both sort via the comparator
        // family now; both previously errored in codegen.
        assert_clean_asan_run(
            r#"
fn main() {
    let mut v: Vec[(i64, String)] = Vec.new();
    v.push((2, f"bb padded beyond thirty-six bytes junk {1}"));
    v.push((1, f"zz padded beyond thirty-six bytes junk {1}"));
    v.push((2, f"aa padded beyond thirty-six bytes junk {1}"));
    v.sort();
    for t in v {
        println(t.0);
    }
    let mut f: Vec[f64] = Vec.new();
    f.push(2.5);
    f.push(1.5);
    f.push(3.5);
    f.sort();
    println(f[0]);
    println(f[2]);
}
"#,
            &["1", "2", "2", "1.5", "3.5"],
            "gsort_tuple_and_float_elements",
        );
    }

    #[test]
    fn asan_parvec_autopar_slot_keeps_elem_agg_drop() {
        // B-2026-07-02-4: the auto-par dispatch's parent-side re-track
        // used plain one-level `track_vec_var`, DOWNGRADING a
        // `Vec[Vec[String]]` slot's cleanup — every nested string leaked
        // whenever the statement shape parallelized (KARAC_AUTO_PAR=0 was
        // clean, which is how the class masqueraded as "index-read
        // leaks"). This program's independent-lets shape triggers
        // auto-par grouping; the re-track now mirrors the LET-site
        // agg/map/tensor element dispatch. NOTE: leak tests must include
        // auto-par-TRIGGERING shapes — loop-based builders serialize and
        // never covered this path.
        assert_clean_asan_run(
            r#"
fn main() {
    let mut outer: Vec[Vec[String]] = Vec.new();
    let mut a: Vec[String] = Vec.new();
    a.push(f"payload padded beyond thirty-six bytes {1}");
    outer.push(a);
    println(outer.len());
}
"#,
            &["1"],
            "parvec_autopar_slot_keeps_elem_agg_drop",
        );
    }

    #[test]
    fn asan_parvec_autopar_for_loop_after_crossing() {
        // B-2026-07-02-4: the full-content variant — three rows crossing
        // the auto-par boundary then iterated (192 bytes leaked pre-fix).
        assert_clean_asan_run(
            r#"
fn main() {
    let mut outer: Vec[Vec[String]] = Vec.new();
    let mut a: Vec[String] = Vec.new();
    a.push(f"banana payload padded beyond thirty-six bytes {1}");
    a.push(f"apple payload padded beyond thirty-six bytes {1}");
    let mut b: Vec[String] = Vec.new();
    b.push(f"apple payload padded beyond thirty-six bytes {1}");
    let mut c: Vec[String] = Vec.new();
    outer.push(a);
    outer.push(b);
    outer.push(c);
    for row in outer {
        println(row.len());
    }
}
"#,
            &["2", "1", "0"],
            "parvec_autopar_for_loop_after_crossing",
        );
    }

    #[test]
    fn asan_parvec_explicit_par_join_slots_freed() {
        // B-2026-07-02-4 explicit-par sibling: `par { let x = build(1);
        // let y = build(2); x.len() + y.len() }` — Step 6 bound the Vec
        // slots into the parent with NO cleanup at all (544 bytes/call
        // pre-fix). The same rich element dispatch now registers there.
        assert_clean_asan_run(
            r#"
fn build(n: i64) -> Vec[Vec[String]] {
    let mut outer: Vec[Vec[String]] = Vec.new();
    let mut a: Vec[String] = Vec.new();
    a.push(f"payload padded beyond thirty-six bytes {n}");
    outer.push(a);
    outer
}
fn main() {
    let total = par {
        let x = build(1);
        let y = build(2);
        x.len() + y.len()
    };
    println(total);
}
"#,
            &["2"],
            "parvec_explicit_par_join_slots_freed",
        );
    }

    #[test]
    fn asan_narrow_literal_arg_sink_buffers() {
        // B-2026-07-02-6: collection literals compiled directly at call-arg
        // sinks (by-value `Vec[i32]`, borrow-only `ref Vec[i32]` and
        // `Slice[i32]` params, bare `[v; n]` repeat in arg position) each
        // malloc a heap buffer at the call site; the borrow sinks must free
        // the temp after the call (the callee never owns it) and the
        // by-value sink's callee-drop must fire exactly once.
        assert_clean_asan_run(
            r#"
fn by_val(v: Vec[i32]) -> i64 {
    let mut t = 0;
    for x in v {
        t = t + (x as i64);
    }
    return t;
}

fn by_ref(v: ref Vec[i32]) -> i64 {
    let mut t = 0;
    for x in v {
        t = t + (x as i64);
    }
    return t;
}

fn by_slice(v: Slice[i32]) -> i64 {
    let mut t = 0;
    for x in v {
        t = t + (x as i64);
    }
    return t;
}

struct Acc {
    base: i64,
}

impl Acc {
    fn tally(self, v: Vec[i32]) -> i64 {
        let mut t = self.base;
        for x in v {
            t = t + (x as i64);
        }
        return t;
    }
}

fn main() {
    let a = by_val([10, 20, 30]);
    let b = by_ref([10, 20, 30]);
    let c = by_slice([10, 20, 30]);
    let d = by_val([7; 3]);
    let acc = Acc { base: 0 };
    let e = acc.tally([1, 2, 3]);
    println(a + b + c + d + e);
}
"#,
            &["207"],
            "narrow_literal_arg_sink_buffers",
        );
    }

    #[test]
    fn asan_parvec_deep_nested_slot() {
        // B-2026-07-02-4: `Vec[Vec[Vec[i64]]]` slot + index-read binding
        // (the 16-byte w1 repro).
        assert_clean_asan_run(
            r#"
fn main() {
    let mut v: Vec[Vec[Vec[i64]]] = Vec.new();
    let mut a: Vec[Vec[i64]] = Vec.new();
    a.push(Vec[2]);
    v.push(a);
    let first = v[0];
    println(first.len());
}
"#,
            &["1"],
            "parvec_deep_nested_slot",
        );
    }

    #[test]
    fn asan_chained_call_struct_heap_field_no_leak() {
        // B-2026-07-03-3: a chained `n.relabel().name` reads a HEAP (String)
        // field off a method-call temporary that was never exercised before
        // the fix (it returned 0). Loop it with a >=36-byte payload so any
        // leak of the temp struct's String buffer (or a double-free of the
        // extracted field) trips Linux LSan / macOS ASan.
        assert_clean_asan_run(
            r#"
struct N { name: String, id: i64 }
impl N {
    fn relabel(self) -> N {
        N { name: "a sufficiently long heap payload for lsan", id: self.id + 1 }
    }
}
fn main() {
    let mut i = 0i64;
    let mut acc = 0i64;
    while i < 50i64 {
        let n = N { name: "seed string that is also quite long", id: i };
        acc = acc + n.relabel().name.len() + n.relabel().id;
        i = i + 1i64;
    }
    println(f"{acc}");
}
"#,
            &["3325"],
            "chained_call_struct_heap_field_no_leak",
        );
    }

    #[test]
    fn asan_generic_bound_default_method_string_no_leak() {
        // B-2026-07-03-11: a trait DEFAULT method (`greeting`) dispatched
        // through a generic BOUND (`describe[G: Greeter]`) on a String-carrying
        // implementor. `greeting()` concatenates a fresh heap String
        // (`"hi " + self.name()`) which flows out through the mono return. Loop
        // it with a >=36-byte payload so any leak of the returned String buffer
        // (or the intermediate `name()` result) trips Linux LSan / macOS ASan.
        assert_clean_asan_run(
            r#"
trait Greeter {
    fn name(self) -> String;
    fn greeting(self) -> String { "hi " + self.name() }
}
struct Person { id: i64 }
impl Greeter for Person {
    fn name(self) -> String { "a sufficiently long greeter name payload" }
}
fn describe[G: Greeter](g: G) -> String { g.greeting() }
fn main() {
    let mut i = 0i64;
    let mut acc = 0i64;
    while i < 50i64 {
        let g = describe(Person { id: i });
        acc = acc + g.len();
        i = i + 1i64;
    }
    println(f"{acc}");
}
"#,
            &["2150"],
            "generic_bound_default_method_string_no_leak",
        );
    }

    #[test]
    fn asan_generic_slice_elem_string_return_no_leak() {
        // B-2026-07-03-22: a generic `-> T` whose `T` binds from a `Slice[T]`
        // param element (`gsum[T](s: Slice[T]) -> T { s[0] }`) called with a
        // `Vec[String]` now resolves `T = String`, so `s[0]` returns a genuine
        // String struct rather than reading the element's 8-byte heap pointer
        // as an `i64`. `s[0]` must CLONE the element out (the Vec still owns its
        // copy); loop it with a >=36-byte payload so any missing clone (alias →
        // double-free with the Vec's drop) or leaked clone trips macOS ASan /
        // Linux LSan.
        assert_clean_asan_run(
            r#"
fn gsum[T](s: Slice[T]) -> T { s[0] }
fn main() {
    let mut i = 0i64;
    let mut acc = 0i64;
    while i < 50i64 {
        let vs: Vec[String] = ["a sufficiently long slice element payload", "second sufficiently long element payload"];
        let e = gsum(vs);
        acc = acc + e.len();
        i = i + 1i64;
    }
    println(f"{acc}");
}
"#,
            &["2050"],
            "generic_slice_elem_string_return_no_leak",
        );
    }

    #[test]
    fn asan_column_tensor_map_freed_no_leak() {
        // S6c-2: `Column.map` / `Tensor.map` each allocate a FRESH result
        // container (control + data buffer, plus a validity bitmap for the
        // column). The result binds to a `let` and must be freed at scope exit
        // via the same `track_column_var` / tensor cleanup the binop results
        // use — this asserts no leak / double-free of the map-allocated
        // buffers. Looped so a per-iteration leak accumulates for LSan.
        assert_clean_asan_run(
            r#"
fn inner() -> i64 {
    let c: Column[i64] = Column.from_vec([1, 2, 3, 4]);
    let d = c.map(|x| x * 2);
    let t: Tensor[i64, [4]] = Tensor.from([1, 2, 3, 4]);
    let e = t.map(|x| x + 1);
    d.sum() + e.sum()
}
fn main() {
    let mut acc: i64 = 0;
    let mut i: i64 = 0;
    while i < 20 {
        acc = acc + inner();
        i = i + 1;
    }
    println(f"{acc}");
}
"#,
            &["680"], // (20 + 14) * 20
            "asan_column_tensor_map_freed_no_leak",
        );
    }

    #[test]
    fn asan_column_tensor_zip_with_freed_no_leak() {
        // S6c-2b: `Column.zip_with` / `Tensor.zip_with` each allocate a FRESH
        // result container while READING (borrowing) both operands. This
        // asserts: (1) the fresh result binds to a `let` and frees at scope
        // exit (like the map / binop results); (2) the `other` operand — a
        // `ref` arg — is NOT double-freed (it's a borrow, freed once as its own
        // binding). Looped so any per-iteration leak accumulates for LSan.
        assert_clean_asan_run(
            r#"
fn inner() -> i64 {
    let a: Column[i64] = Column.from_vec([1, 2, 3, 4]);
    let b: Column[i64] = Column.from_vec([10, 20, 30, 40]);
    let c = a.zip_with(b, |x, y| x + y);
    let t: Tensor[i64, [4]] = Tensor.from([1, 2, 3, 4]);
    let u: Tensor[i64, [4]] = Tensor.from([2, 2, 2, 2]);
    let v = t.zip_with(u, |x, y| x * y);
    c.sum() + v.sum()
}
fn main() {
    let mut acc: i64 = 0;
    let mut i: i64 = 0;
    while i < 20 {
        acc = acc + inner();
        i = i + 1;
    }
    println(f"{acc}");
}
"#,
            &["2600"], // (110 + 20) * 20
            "asan_column_tensor_zip_with_freed_no_leak",
        );
    }

    #[test]
    fn asan_ewmap_trait_bound_map_zip_freed_no_leak() {
        // S6c: `map` / `zip_with` on the `ElementwiseMap` trait — the fresh
        // result container is allocated INSIDE a bound-generic fn and RETURNED
        // (`-> C`) to the caller. This is a new drop shape: the callee's
        // scope-exit cleanup must NOT free the returned container (it's moved
        // out on return), and the caller's `let`-binding must free it exactly
        // once. Also asserts the `ref` operands (`c` / `a` / `b`) aren't
        // double-freed across the generic boundary. Looped for LSan.
        assert_clean_asan_run(
            r#"
fn doubled[C: ElementwiseMap[i64]](c: ref C) -> C {
    c.map(|x| x * 2)
}
fn combine[C: ElementwiseMap[i64]](a: ref C, b: ref C) -> C {
    a.zip_with(b, |x, y| x + y)
}
fn inner() -> i64 {
    let col: Column[i64] = Column.from_vec([1, 2, 3, 4]);
    let dc: Column[i64] = doubled(col);
    let t: Tensor[i64, [4]] = Tensor.from([1, 2, 3, 4]);
    let dt: Tensor[i64, [4]] = doubled(t);
    let a: Column[i64] = Column.from_vec([1, 2, 3, 4]);
    let b: Column[i64] = Column.from_vec([10, 20, 30, 40]);
    let z: Column[i64] = combine(a, b);
    dc.sum() + dt.sum() + z.sum()
}
fn main() {
    let mut acc: i64 = 0;
    let mut i: i64 = 0;
    while i < 20 {
        acc = acc + inner();
        i = i + 1;
    }
    println(f"{acc}");
}
"#,
            &["3000"], // (20 + 20 + 110) * 20
            "asan_ewmap_trait_bound_map_zip_freed_no_leak",
        );
    }

    #[test]
    fn asan_column_tensor_argmin_freed_no_leak() {
        // S6c: `Column.argmin`/`argmax` and `Tensor.argmin`/`argmax` return a
        // POD `Option[i64]` (no heap), but the receiver columns / tensors own
        // heap. This asserts the bound receivers free at scope exit AND that a
        // FRESH `Column.from_vec(...).argmin()` temp receiver is freed by the
        // owned-temp machinery (not leaked). Looped so a per-iteration leak
        // accumulates for LSan.
        assert_clean_asan_run(
            r#"
fn idx(o: Option[i64]) -> i64 {
    match o {
        Some(i) => i,
        None => -1,
    }
}
fn inner() -> i64 {
    let c: Column[i64] = Column.from_vec([5, 9, 3, 3, 8, 1]);
    let a = idx(c.argmin()) + idx(c.argmax());
    let t: Tensor[i64, [6]] = Tensor.from([4, 2, 7, 2, 9, 9]);
    let b = idx(t.argmin()) + idx(t.argmax());
    // Fresh-temp receiver — must be freed by the owned-temp path.
    let d = idx(Column.from_vec([2, 8, 1, 8]).argmax());
    a + b + d
}
fn main() {
    let mut acc: i64 = 0;
    let mut i: i64 = 0;
    while i < 20 {
        acc = acc + inner();
        i = i + 1;
    }
    println(f"{acc}");
}
"#,
            // c: 5+1=6; t: 1+4=5; d: argmax of [2,8,1,8] first 8 at idx 1 = 1.
            // (6 + 5 + 1) * 20 = 240.
            &["240"],
            "asan_column_tensor_argmin_freed_no_leak",
        );
    }

    #[test]
    fn asan_column_tensor_sorted_argsort_freed_no_leak() {
        // S6c: `Column.sorted`/`argsort` and `Tensor.sorted`/`argsort` each
        // allocate a FRESH result `Vec` (a malloc'd buffer). Each result binds
        // to a `let` and must be freed at scope exit via the standard `Vec`
        // cleanup — this asserts no leak / double-free of the sort-allocated
        // buffers over a loop. Results are `let`-bound then indexed (the
        // standard `Stats.sort` idiom; the auto-par slot-published Column/Tensor
        // early-free that once corrupted fresh-temp-arg patterns is fixed —
        // B-2026-07-03-32).
        assert_clean_asan_run(
            r#"
fn inner() -> i64 {
    let c: Column[i64] = Column.from_vec([5, 9, 3, 3, 8, 1]);
    let cs: Vec[i64] = c.sorted();
    let ca: Vec[i64] = c.argsort();
    let mut n: Column[i64] = Column.with_capacity(5);
    n.push(10); n.push_null(); n.push(5); n.push_null(); n.push(20);
    let ns: Vec[i64] = n.sorted();
    let na: Vec[i64] = n.argsort();
    let t: Tensor[i64, [6]] = Tensor.from([4, 2, 7, 2, 9, 9]);
    let ts: Vec[i64] = t.sorted();
    let ta: Vec[i64] = t.argsort();
    cs[0] + ca[0] + ns[0] + na[0] + ts[0] + ta[0]
}
fn main() {
    let mut acc: i64 = 0;
    let mut i: i64 = 0;
    while i < 20 {
        acc = acc + inner();
        i = i + 1;
    }
    println(f"{acc}");
}
"#,
            // cs[0]=1, ca[0]=5, ns[0]=5, na[0]=2, ts[0]=2, ta[0]=1 → 16; *20 = 320.
            &["320"],
            "asan_column_tensor_sorted_argsort_freed_no_leak",
        );
    }

    #[test]
    fn asan_column_sorted_argsort_narrow_widths_no_leak() {
        // S6c follow-on: the NARROW-width `Column.sorted` path mallocs a separate
        // `Vec[T]`-width buffer and frees the 8-byte scratch key buffer; the
        // narrow `argsort` path mallocs a widened full-length key view and frees
        // it after the sort. This asserts neither the narrow-back buffer, the
        // widened key view, nor the result `Vec` leaks or double-frees over a
        // loop — for i32, u32, and f32 columns (each with a null). `let`-bound +
        // indexed idiom (the standard `Stats.sort` idiom).
        assert_clean_asan_run(
            r#"
fn inner() -> i64 {
    let mut ci: Column[i32] = Column.with_capacity(4);
    ci.push(5); ci.push(1); ci.push_null(); ci.push(3);
    let cs: Vec[i32] = ci.sorted();
    let ca: Vec[i64] = ci.argsort();
    let cu: Column[u32] = Column.from_vec([30, 10, 20]);
    let us: Vec[u32] = cu.sorted();
    let ua: Vec[i64] = cu.argsort();
    let mut cf: Column[f32] = Column.with_capacity(4);
    cf.push(2.5); cf.push_null(); cf.push(1.5); cf.push(0.5);
    let fs: Vec[f32] = cf.sorted();
    let fa: Vec[i64] = cf.argsort();
    cs.len() + ca[0] + us.len() + ua[0] + fs.len() + fa[0]
}
fn main() {
    let mut acc: i64 = 0;
    let mut i: i64 = 0;
    while i < 20 {
        acc = acc + inner();
        i = i + 1;
    }
    println(f"{acc}");
}
"#,
            // cs.len()=3, ca[0]=1, us.len()=3, ua[0]=1, fs.len()=3, fa[0]=3 → 14;
            // *20 = 280.
            &["280"],
            "asan_column_sorted_argsort_narrow_widths_no_leak",
        );
    }

    #[test]
    fn asan_tensor_sorted_argsort_narrow_widths_no_leak() {
        // B-2026-07-03-35 fixed: narrow-width `Tensor.sorted` mallocs a widened
        // 8-byte key copy, sorts it, narrows back into a `Vec[T]`-width buffer,
        // and frees the scratch; narrow `argsort` mallocs a widened key view and
        // frees it after the sort. Plus the tensor itself (`Tensor.from`) is a
        // malloc'd block freed at scope exit. This asserts no leak / double-free
        // of any of those over a loop — i32, u32, and f32 tensors. `let`-bound +
        // indexed idiom.
        assert_clean_asan_run(
            r#"
fn inner() -> i64 {
    let ti: Tensor[i32, [4]] = Tensor.from([40, 10, 30, 20]);
    let si: Vec[i32] = ti.sorted();
    let ai: Vec[i64] = ti.argsort();
    let tu: Tensor[u32, [3]] = Tensor.from([30, 10, 20]);
    let us: Vec[u32] = tu.sorted();
    let ua: Vec[i64] = tu.argsort();
    let tf: Tensor[f32, [4]] = Tensor.from([2.5, 0.5, 3.5, 1.5]);
    let fs: Vec[f32] = tf.sorted();
    let fa: Vec[i64] = tf.argsort();
    si.len() + ai[0] + us.len() + ua[0] + fs.len() + fa[0]
}
fn main() {
    let mut acc: i64 = 0;
    let mut i: i64 = 0;
    while i < 20 {
        acc = acc + inner();
        i = i + 1;
    }
    println(f"{acc}");
}
"#,
            // si.len()=4, ai[0]=1, us.len()=3, ua[0]=1, fs.len()=4, fa[0]=1 → 14;
            // *20 = 280.
            &["280"],
            "asan_tensor_sorted_argsort_narrow_widths_no_leak",
        );
    }

    /// B-2026-07-03-30 (Vec-element drain) — a struct field `Vec[String]` (whose
    /// elements own heap the outer buffer-free misses) is DRAINED per element by
    /// the synthesized struct drop when the owning struct is PLAIN-dropped (a
    /// `Vec[A]` element). Before the fix, `emit_struct_drop_synthesis`'s
    /// VecOrString arm freed only the `{ptr,len,cap}` buffer, leaking every
    /// element's char buffer (`vec_elem_agg_drop_for_type_expr` returned `None`
    /// for a direct `String` element). Payloads >=40 bytes for LSan visibility.
    #[test]
    fn asan_struct_vec_string_field_plain_drop_drains_elements() {
        assert_clean_asan_run(
            r#"
struct A { path: Vec[String] }
fn main() {
    let mut v: Vec[A] = Vec.new();
    let mut i = 0;
    while i < 6 {
        let mut p: Vec[String] = Vec.new();
        p.push("struct_vec_string_plaindrop_element_payload".to_string());
        p.push("struct_vec_string_plaindrop_element_second_".to_string());
        v.push(A { path: p });
        i = i + 1;
    }
    println(v.len());
}
"#,
            &["6"],
            "struct_vec_string_field_plain_drop_drains_elements",
        );
    }

    /// B-2026-07-03-30 (Vec-element drain) — destructure-consume peer of the plain-drop
    /// test: `let A { path } = a` (with `a` a callee-owned by-value param that is
    /// deep-copied at entry) then `for s in path` consumes the elements. The
    /// entry-copy is element-DEEP for the drained `Vec[String]` field
    /// (`param_own.rs`, restoring the copy-depth == drop-depth invariant), so the
    /// callee's copy owns independent char buffers — no double-free against the
    /// caller's retained original, no leak.
    #[test]
    fn asan_struct_vec_string_field_destructure_consume_clean() {
        assert_clean_asan_run(
            r#"
struct A { path: Vec[String] }
fn f(a: A) -> i64 {
    let A { path } = a;
    let mut t = 0;
    for s in path { if s.len() >= 0 { t = t + 1; } }
    t
}
fn build() -> Vec[A] {
    let mut v: Vec[A] = Vec.new();
    let mut i = 0;
    while i < 6 {
        let mut p: Vec[String] = Vec.new();
        p.push("struct_vec_string_destructure_element_payld".to_string());
        v.push(A { path: p });
        i = i + 1;
    }
    v
}
fn main() {
    let xs = build();
    let mut t = 0;
    for a in xs { t = t + f(a); }
    println(t);
}
"#,
            &["6"],
            "struct_vec_string_field_destructure_consume_clean",
        );
    }

    /// B-2026-07-03-30 (Vec-element drain) — a struct field `Vec[Map[i64, String]]`,
    /// plain-dropped: each element Map's buckets (and their String values) drain
    /// via the recursive drop family (`emit_drop_fn_for_type_expr`), which
    /// `vec_elem_agg_drop_for_type_expr` alone did not reach for a direct Map
    /// element.
    #[test]
    fn asan_struct_vec_map_field_plain_drop_drains_elements() {
        assert_clean_asan_run(
            r#"
struct A { rows: Vec[Map[i64, String]] }
fn main() {
    let mut v: Vec[A] = Vec.new();
    let mut i = 0;
    while i < 6 {
        let mut rows: Vec[Map[i64, String]] = Vec.new();
        let mut m: Map[i64, String] = Map.new();
        m.insert(1, "struct_vec_map_field_string_value_payload_x".to_string());
        rows.push(m);
        v.push(A { rows: rows });
        i = i + 1;
    }
    println(v.len());
}
"#,
            &["6"],
            "struct_vec_map_field_plain_drop_drains_elements",
        );
    }

    /// B-2026-07-03-28 Facet A — plain-drop of a struct whose only heap is an
    /// `Option[String]` field (a `Vec[A]` element). The struct is copy-supported,
    /// so it is callee-owned and its synthesized struct drop frees the `Some`
    /// payload (`OptionInline`); before the fix the payload leaked.
    #[test]
    fn asan_option_field_plain_drop_freed() {
        assert_clean_asan_run(
            r#"
struct A { sv: Option[String] }
fn main() {
    let mut v: Vec[A] = Vec.new();
    let mut i = 0;
    while i < 6 { v.push(A { sv: Some("facet_a_plaindrop_option_string_payload_x".to_string()) }); i = i + 1; }
    println(v.len());
}
"#,
            &["6"],
            "option_field_plain_drop_freed",
        );
    }

    /// B-2026-07-03-28 Facet A — by-value param destructured, the `Option` leaf
    /// matched+consumed. The param is entry-copied (independent `Option` payload),
    /// the destructure zeros the source tag, and the `match` frees the leaf —
    /// no double-free (the pre-fix prototype double-freed here), no leak.
    #[test]
    fn asan_option_field_destructure_match_consume_clean() {
        assert_clean_asan_run(
            r#"
struct A { path: Vec[String], sv: Option[String] }
fn f(a: A) -> i64 {
    let A { path, sv } = a;
    let mut t = 0;
    for s in path { if s.len() >= 0 { t = t + 1; } }
    match sv { Some(x) => { if x.len() >= 0 { t = t + 1; } } None => {} }
    t
}
fn build() -> Vec[A] {
    let mut v: Vec[A] = Vec.new();
    let mut i = 0;
    while i < 6 {
        let mut p: Vec[String] = Vec.new();
        p.push("facet_a_destructure_vec_payload_alpha_aaaa".to_string());
        v.push(A { path: p, sv: Some("facet_a_destructure_option_payload_beta_bb".to_string()) });
        i = i + 1;
    }
    v
}
fn main() {
    let xs = build();
    let mut t = 0;
    for a in xs { t = t + f(a); }
    println(t);
}
"#,
            &["12"],
            "option_field_destructure_match_consume",
        );
    }

    /// B-2026-07-03-28 Facet A — destructured, the `Option` leaf never consumed:
    /// its tracked inline-Option cleanup frees the payload at scope exit while the
    /// source struct drop skips it (tag zeroed). No leak, no double-free.
    #[test]
    fn asan_option_field_destructure_unused_freed() {
        assert_clean_asan_run(
            r#"
struct A { path: Vec[String], sv: Option[String] }
fn f(a: A) -> i64 {
    let A { path, sv } = a;
    0
}
fn build() -> Vec[A] {
    let mut v: Vec[A] = Vec.new();
    let mut i = 0;
    while i < 6 {
        let mut p: Vec[String] = Vec.new();
        p.push("facet_a_unused_vec_payload_gamma_cccccccccc".to_string());
        v.push(A { path: p, sv: Some("facet_a_unused_option_payload_delta_dddddddd".to_string()) });
        i = i + 1;
    }
    v
}
fn main() {
    let xs = build();
    let mut t = 0;
    for a in xs { t = t + f(a); }
    println(t);
}
"#,
            &["0"],
            "option_field_destructure_unused_freed",
        );
    }

    /// B-2026-07-03-28 Facet A — `let x = a.sv` moves the `Option` field out of a
    /// callee-owned struct; the field-access move-out zeros the source tag so the
    /// struct drop skips it, and `x` owns the payload. No double-free, no leak.
    #[test]
    fn asan_option_field_moveout_clean() {
        assert_clean_asan_run(
            r#"
struct A { keep: i64, sv: Option[String] }
fn f(a: A) -> i64 {
    let x = a.sv;
    match x { Some(s) => { if s.len() >= 0 { 1 } else { 0 } } None => 0 }
}
fn build() -> Vec[A] {
    let mut v: Vec[A] = Vec.new();
    let mut i = 0;
    while i < 6 { v.push(A { keep: i, sv: Some("facet_a_field_moveout_option_payload_eta_ee".to_string()) }); i = i + 1; }
    v
}
fn main() {
    let xs = build();
    let mut t = 0;
    for a in xs { t = t + f(a); }
    println(t);
}
"#,
            &["6"],
            "option_field_moveout_clean",
        );
    }

    /// B-2026-07-10-3 — a `Result`/`Option` scrutinee whose INLINE struct payload
    /// is bound WHOLE as `e` and a heap field is read as a DIRECT call argument
    /// (`println(e.msg)`). The inline `Result`/`Option` cleanup frees only a bare
    /// `{ptr,len,cap}` payload, not a struct payload's own fields, and the
    /// consuming-arm suppressor zeroed the source anyway — so `e.msg` was owned by
    /// nobody and leaked. The bound struct is now `track_struct_var`-tracked so its
    /// scope-exit drop frees the field. Covers the fresh-temp scrutinee (this test),
    /// the `Option` sibling, the `{code,msg}` 4-word inline-`Result` payload (still
    /// inline for `Result`'s area of 5), and the move-out shapes that must NOT
    /// double-free (whole `e` into a by-value callee; whole `e` out as the match
    /// value).
    #[test]
    fn asan_result_struct_payload_direct_field_arg_no_leak() {
        assert_clean_asan_run(
            r#"
struct AppError { msg: String }
struct E2 { code: i64, msg: String }
fn run(x: i64) -> Result[i64, AppError] {
    if x > 0i64 { Result.Ok(x + 1i64) } else { Result.Err(AppError { msg: "neg_error_payload_alpha_aaaaaaaa".to_string() }) }
}
fn run_opt(x: i64) -> Option[AppError] {
    if x > 0i64 { Option.None } else { Option.Some(AppError { msg: "neg_option_payload_beta_bbbbbbbb".to_string() }) }
}
fn run2(x: i64) -> Result[i64, E2] {
    if x > 0i64 { Result.Ok(x + 1i64) } else { Result.Err(E2 { code: 7i64, msg: "neg_error_two_field_gamma_cccccccc".to_string() }) }
}
fn take(a: AppError) -> i64 { if a.msg.len() >= 0 { 1 } else { 0 } }
fn main() {
    let mut t = 0;
    // direct-field-arg read (the reported leak)
    match run(-1i64) { Ok(v) => { if v >= 0i64 { t = t + 1; } } Err(e) => { if e.msg.len() >= 0 { t = t + 1; } } }
    // Option sibling
    match run_opt(-1i64) { Some(e) => { if e.msg.len() >= 0 { t = t + 1; } } None => {} }
    // 4-word inline-Result struct payload
    match run2(-1i64) { Ok(v) => { if v >= 0i64 { t = t + 1; } } Err(e) => { if e.msg.len() >= 0 { t = t + 1; } } }
    // move whole `e` into a by-value callee (must not double-free)
    match run(-1i64) { Ok(v) => { if v >= 0i64 { t = t + 1; } } Err(e) => { t = t + take(e); } }
    // move whole `e` out as the match value (must not double-free)
    let held = match run(-1i64) { Ok(_v) => AppError { msg: "ok_payload_delta_dddddddddddd".to_string() }, Err(e) => e };
    if held.msg.len() >= 0 { t = t + 1; }
    println(t);
}
"#,
            &["5"],
            "result_struct_payload_direct_field_arg",
        );
    }

    #[test]
    fn asan_forloop_struct_element_whole_move_no_double_free() {
        // B-2026-07-04-17: iterating an owned `Vec[<heap struct>]` by value and
        // MOVING the loop element whole into a NEW owner (`let x = a`) must not
        // double-free at teardown — the element binding is a bit-copy alias of
        // the container slot, so `x`'s scope drop and the container's per-element
        // drain would free the same String buffer twice.
        assert_clean_asan_run(
            r#"
struct A { s: String }
fn build() -> Vec[A] {
    let mut v: Vec[A] = Vec.new();
    let mut i = 0;
    while i < 6 { v.push(A { s: "forloop_element_whole_move_payload_theta_xx".to_string() }); i = i + 1; }
    v
}
fn main() {
    let items = build();
    let mut n: i64 = 0;
    for a in items {
        let x = a;
        n = n + x.s.len();
    }
    println(n);
}
"#,
            &["258"], // 6 * len("forloop_element_whole_move_payload_theta_xx") = 6 * 43
            "forloop_struct_element_whole_move_no_double_free",
        );
    }

    #[test]
    fn asan_forloop_struct_element_field_move_no_double_free() {
        // B-2026-07-04-17, the field-move form: moving a heap field OUT of a
        // for-loop element into a fresh struct literal (`let w = A { s: a.s }`)
        // must not double-free — same aliasing hazard as the whole-move.
        assert_clean_asan_run(
            r#"
struct A { s: String }
fn build() -> Vec[A] {
    let mut v: Vec[A] = Vec.new();
    let mut i = 0;
    while i < 6 { v.push(A { s: "forloop_element_field_move_payload_iota_xx".to_string() }); i = i + 1; }
    v
}
fn main() {
    let items = build();
    let mut n: i64 = 0;
    for a in items {
        let w = A { s: a.s };
        n = n + w.s.len();
    }
    println(n);
}
"#,
            &["252"], // 6 * len("forloop_element_field_move_payload_iota_xx") = 6 * 42
            "forloop_struct_element_field_move_no_double_free",
        );
    }

    #[test]
    fn asan_forloop_struct_element_nested_and_option_field_no_double_free() {
        // B-2026-07-04-17 variants named in the ledger: a NESTED heap struct
        // field and an `Option[String]` field. Moving such an element to a new
        // owner deep-copies recursively (copy-depth == drop-depth), so neither
        // the nested String nor the Option payload double-frees.
        assert_clean_asan_run(
            r#"
struct Inner { s: String }
struct Outer { inner: Inner, tag: Option[String] }
fn build() -> Vec[Outer] {
    let mut v: Vec[Outer] = Vec.new();
    let mut i = 0;
    while i < 5 {
        v.push(Outer {
            inner: Inner { s: "nested_inner_heap_payload_kappa_field_xx".to_string() },
            tag: Some("outer_option_string_payload_lambda_field_yy".to_string()),
        });
        i = i + 1;
    }
    v
}
fn main() {
    let items = build();
    let mut n: i64 = 0;
    for a in items {
        let x = a;
        n = n + x.inner.s.len();
        match x.tag { Some(t) => { n = n + t.len(); } None => {} }
    }
    println(n);
}
"#,
            &["420"], // 5 * (40 + 44)
            "forloop_struct_element_nested_and_option_field_no_double_free",
        );
    }

    #[test]
    fn asan_forloop_struct_element_clean_shapes_stay_clean() {
        // Regression guard for the shapes that were already CLEAN (must stay
        // clean, NOT over-copied into a leak): a for-loop DESTRUCTURE and a
        // pass-by-value call (the callee entry-copies). Neither is a move into a
        // new local owner, so the defensive copy must NOT fire.
        assert_clean_asan_run(
            r#"
struct A { s: String }
fn take(a: A) -> i64 { a.s.len() }
fn build() -> Vec[A] {
    let mut v: Vec[A] = Vec.new();
    let mut i = 0;
    while i < 4 { v.push(A { s: "clean_shape_regression_guard_payload_mu_zz".to_string() }); i = i + 1; }
    v
}
fn main() {
    let items = build();
    let mut n: i64 = 0;
    for a in items {
        let A { s } = a;
        n = n + s.len();
    }
    let more = build();
    for a in more {
        n = n + take(a);
    }
    println(n);
}
"#,
            &["336"], // 4 * 42 (destructure) + 4 * 42 (pass-by-value)
            "forloop_struct_element_clean_shapes_stay_clean",
        );
    }

    #[test]
    fn asan_forloop_element_destructure_option_string_field_match_consume_no_double_free() {
        // B-2026-07-10-4 (the residual attr-item double-free, minimal E1 form):
        // DESTRUCTURING an `Option[String]` field OUT of a for-loop element and
        // then match-consuming it. Bare `for` BORROWS the collection (design.md
        // §2601/§2751), so `a` is a bit-copy VIEW of `items`'s element slot; the
        // extracted `string_value` aliases the element's `Option[String]` buffer,
        // which `items`'s scope-exit per-element drain frees. Without the
        // clone-on-extract routing (`for_loop_owned_agg_vars` → `view_src` +
        // `clone_on_extract_view_field`'s `Option[inline-heap]` leg) the match
        // frees the aliased buffer AND the drain frees it again — a double-free
        // (`corrupted size vs. prev_size in fastbins`). This is the exact shape of
        // the 12 residual attribute-item crashers (`AttrNode.string_value`).
        assert_clean_asan_run(
            r#"
struct AttrNode { string_value: Option[String] }
fn parse_attrs() -> Vec[AttrNode] {
    let mut a: Vec[AttrNode] = Vec.new();
    let mut i = 0;
    while i < 6 {
        a.push(AttrNode { string_value: Some("forloop_destructure_option_string_payload_xx".to_string()) });
        i = i + 1;
    }
    a
}
fn main() {
    let items = parse_attrs();
    let mut n: i64 = 0;
    for a in items {
        let AttrNode { string_value } = a;
        match string_value {
            Some(s) => { n = n + s.len(); }
            None => {}
        }
    }
    println(n);
}
"#,
            &["264"], // 6 * len("forloop_destructure_option_string_payload_xx") = 6 * 44
            "forloop_element_destructure_option_string_field_match_consume_no_double_free",
        );
    }

    #[test]
    fn asan_forloop_element_destructure_string_field_move_into_collector_no_double_free() {
        // B-2026-07-10-4 sibling (E4 form): DESTRUCTURE a `String` field out of a
        // for-loop element and MOVE it into a collector Vec. The borrowed-element
        // leaf aliases `items`'s per-element buffer; moving it into `collected`
        // gives `collected` an aliasing owner, so `collected`'s drain AND `items`'s
        // per-element drain free the same buffer. Clone-on-extract's String leg
        // deep-copies the leaf so `collected` owns an independent buffer. (The
        // borrow-only sibling — `let A { s } = a; use s` without a move — is
        // covered by `..._clean_shapes_stay_clean`, which the clone must keep leak-
        // free: the extra copy is freed at scope exit.)
        assert_clean_asan_run(
            r#"
struct A { s: String }
fn build() -> Vec[A] {
    let mut v: Vec[A] = Vec.new();
    let mut i = 0;
    while i < 5 { v.push(A { s: "forloop_destructure_string_move_into_collector_x".to_string() }); i = i + 1; }
    v
}
fn main() {
    let items = build();
    let mut collected: Vec[String] = Vec.new();
    for a in items {
        let A { s } = a;
        collected.push(s);
    }
    let mut n: i64 = 0;
    for c in collected {
        n = n + c.len();
    }
    println(n);
}
"#,
            &["240"], // 5 * len("forloop_destructure_string_move_into_collector_x") = 5 * 48
            "forloop_element_destructure_string_field_move_into_collector_no_double_free",
        );
    }

    #[test]
    fn asan_option_string_field_survives_caller_retains_vec_copy() {
        // B-2026-07-10-4 final residual (the last 2 attr-item crashers):
        // a `Vec[<struct{Option[String]}>]` local moved into a by-value
        // callee that WRAPS it into a returned struct. By-value Vec params
        // are caller-retains, so the consume site deep-copies — but (1)
        // `emit_vecstr_defensive_copy`'s aggregate-element leg was gated on
        // `type_expr_has_drop_heap`, which hardcodes Option => false, so an
        // Option-only-heap element skipped the per-element deep clone
        // entirely; and (2) even when the leg fired (element also owns a
        // `Vec[String]` field, the real `AttrNode.path`),
        // `karac_clone_struct_<S>`'s `Option[String]` field child fell
        // through to the SHALLOW primitive clone (the type-erased `Option`
        // layout records no heap kinds). Either way both copies' drops freed
        // the same `Some` payload. Fixed by `emit_option_value_clone_fn`
        // (tag-guarded deep clone) + the `te_owns_option_heap_payload`
        // copy-side gate. Covers both shapes: Option-only element (gate) and
        // Vec[String]+Option element (clone-fn child), plus a None element
        // (tag guard no-op) and a method-call chain (the self-host parser's
        // `parse_item` → `parse_trait_def(attrs)` shape).
        assert_clean_asan_run(
            r#"
struct AttrNode { path: Vec[String], string_value: Option[String] }
struct Bare { string_value: Option[String] }
struct Node { attributes: Vec[AttrNode] }
struct BareNode { attributes: Vec[Bare] }
struct P { pos: i64 }
impl P {
    fn wrap(mut ref self, attrs: Vec[AttrNode]) -> Node {
        self.pos = self.pos + 1;
        Node { attributes: attrs }
    }
}
fn wrap_bare(attrs: Vec[Bare]) -> BareNode {
    BareNode { attributes: attrs }
}
fn build() -> Vec[AttrNode] {
    let mut v: Vec[AttrNode] = Vec.new();
    let mut p: Vec[String] = Vec.new();
    p.push("path_segment_payload_alpha_x".to_string());
    v.push(AttrNode { path: p, string_value: Some("option_string_payload_beta_yy".to_string()) });
    v.push(AttrNode { path: Vec.new(), string_value: None });
    v
}
fn build_bare() -> Vec[Bare] {
    let mut v: Vec[Bare] = Vec.new();
    v.push(Bare { string_value: Some("bare_option_payload_gamma_zzz".to_string()) });
    v
}
fn main() {
    let mut n: i64 = 0;
    let mut i = 0;
    while i < 6 {
        let mut prs = P { pos: 0 };
        let attrs = build();
        let node = prs.wrap(attrs);
        let Node { attributes } = node;
        for a in attributes {
            let AttrNode { path, string_value } = a;
            for seg in path { n = n + seg.len(); }
            match string_value { Some(s) => { n = n + s.len(); } None => {} }
        }
        let battrs = build_bare();
        let bnode = wrap_bare(battrs);
        let BareNode { attributes } = bnode;
        for b in attributes {
            let Bare { string_value } = b;
            match string_value { Some(s) => { n = n + s.len(); } None => {} }
        }
        i = i + 1;
    }
    println(n);
}
"#,
            &["516"], // 6 * (28 + 29 + 29)
            "option_string_field_survives_caller_retains_vec_copy",
        );
    }

    #[test]
    fn asan_forloop_bare_enum_element_whole_move_no_double_free() {
        // B-2026-07-05-2: the residual B-2026-07-04-17 left open — a BARE
        // `Vec[<user enum>]` element moved WHOLE to a new owner. `x` aliases the
        // container's live-variant payload; without the enum-let-binding
        // deep-copy, `x`'s EnumDrop and the container's per-element drain free
        // the same `String` buffer (double-free). Repeated across a
        // heap-payload variant and a scalar variant so both the copied and the
        // no-op paths run.
        assert_clean_asan_run(
            r#"
enum E { Tag(String), Num(i64) }
fn build() -> Vec[E] {
    let mut v: Vec[E] = Vec.new();
    let mut i = 0;
    while i < 6 {
        if i % 2 == 0 {
            v.push(E.Tag("bare_enum_variant_heap_payload_omicron_field".to_string()));
        } else {
            v.push(E.Num(i));
        }
        i = i + 1;
    }
    v
}
fn main() {
    let items = build();
    let mut n: i64 = 0;
    for a in items {
        let x = a;
        match x {
            E.Tag(s) => { n = n + s.len(); }
            E.Num(k) => { n = n + k; }
        }
    }
    println(n);
}
"#,
            &["141"], // 3 * 44 (Tag payload len) + (1 + 3 + 5) Num
            "forloop_bare_enum_element_whole_move_no_double_free",
        );
    }

    #[test]
    fn asan_forloop_string_element_whole_move_let_no_double_free() {
        // B-2026-07-05-2 sibling (Vec/String leg): `for s in words { let x = s }`
        // — the Vec/String element type the struct fix did not touch. Only the
        // push/insert/entry consume sites were covered; the plain whole-move
        // let-bind aliased the container element and double-freed.
        assert_clean_asan_run(
            r#"
fn build() -> Vec[String] {
    let mut v: Vec[String] = Vec.new();
    let mut i = 0;
    while i < 5 {
        let mut s = String.new();
        s.push_str("string_element_whole_move_heap_payload_pi_");
        s.push_str(i.to_string());
        v.push(s);
        i = i + 1;
    }
    v
}
fn main() {
    let words = build();
    let mut n: i64 = 0;
    for s in words {
        let x = s;
        n = n + x.len();
    }
    println(n);
}
"#,
            &["215"], // 5 * 43 (each payload is 42 + 1 digit)
            "forloop_string_element_whole_move_let_no_double_free",
        );
    }

    #[test]
    fn asan_forloop_enum_element_nested_struct_payload_no_double_free() {
        // B-2026-07-05-2, the NestedStruct-payload variant: an enum whose live
        // variant carries a heap-bearing struct inline (`Wrap(Inner)`). Exercises
        // the `NestedStruct` arm of `deep_copy_enum_heap_payload_in_place` via the
        // for-loop-element path — a distinct branch from the sibling
        // `asan_forloop_bare_enum_element_whole_move_no_double_free`, which only
        // covers a `VecOrString` payload. The deep-copy must recurse into the
        // inline struct's own heap fields (copy-depth == drop-depth) so the inner
        // String does not double-free on a whole-element move.
        assert_clean_asan_run(
            r#"
struct Inner { s: String }
enum Node { Leaf, Wrap(Inner) }
fn build() -> Vec[Node] {
    let mut v: Vec[Node] = Vec.new();
    let mut i = 0;
    while i < 5 { v.push(Node.Wrap(Inner { s: "forloop_enum_nested_struct_payload_iota_field_yy".to_string() })); i = i + 1; }
    v
}
fn main() {
    let items = build();
    let mut n: i64 = 0;
    for a in items {
        let x = a;
        match x { Node.Wrap(inner) => { n = n + inner.s.len(); } Node.Leaf => {} }
    }
    println(n);
}
"#,
            &["240"], // 5 * len("forloop_enum_nested_struct_payload_iota_field_yy") = 5 * 48
            "forloop_enum_element_nested_struct_payload_no_double_free",
        );
    }

    #[test]
    fn asan_fresh_some_shared_reused_across_consuming_calls_no_double_free() {
        // B-2026-07-11-21: a fresh `let orig = Some(Node { .. })` (an untyped
        // `Some(<shared struct literal>)` binding) passed BY VALUE to a
        // recursive consumer that clones the matched subtree, TWICE. Before the
        // fix the binding was never registered as `Option[shared]` (no call-site
        // retain, no scope-exit dec), so each consuming call's param drop
        // decremented `orig`'s refcount — the first call freed the tree, the
        // second double-freed it (glibc "malloc(): unaligned tcache chunk").
        // The interpreter was correct throughout; codegen (JIT+AOT) corrupted
        // the heap. Registering the fresh-Some binding into the caller-retains
        // model (one arg-site inc per pass, one scope-exit dec) balances it.
        assert_clean_asan_run(
            r#"
shared struct Node { val: i64, mut left: Option[Node], mut right: Option[Node] }
fn clone_offset(node: Option[Node], delta: i64) -> Option[Node] {
    match node {
        None => None,
        Some(n) => Some(Node { val: n.val + delta, left: clone_offset(n.left, delta), right: clone_offset(n.right, delta) }),
    }
}
fn count_nodes(node: Option[Node]) -> i64 {
    match node { None => 0, Some(n) => 1 + count_nodes(n.left) + count_nodes(n.right) }
}
fn main() {
    let orig = Some(Node { val: 1, left: Some(Node { val: 2, left: None, right: None }), right: None });
    let c1 = clone_offset(orig, 10);
    let c2 = clone_offset(orig, 20);
    println(count_nodes(c1) + count_nodes(c2));
}
"#,
            &["4"], // two 2-node clones
            "fresh_some_shared_reused_across_consuming_calls_no_double_free",
        );
    }

    #[test]
    fn asan_vecvec_heap_element_consumed_by_value_no_double_free() {
        // B-2026-07-11-24 (borrow-elision leg): a `let r = grid[i]` inner
        // `Vec[String]` read out of a `Vec[Vec[String]]`, whose element `r[j]` is
        // then passed BY VALUE to a consuming callee (`take(r[j])`). The
        // borrow-elision pre-pass used to treat `r[j]` as a read and borrow-elide
        // `r` into a shallow alias of the container's buffer; `take`'s owned
        // `String` param then freed a buffer the container still owned →
        // double-free. The element-copyability oracle now recognises the
        // heap-element consume and forces the deep clone, so `r` owns an
        // independent buffer freed exactly once. A trivially-copyable element
        // (`Vec[Vec[i64]]`, `acc + m[i][j]`) stays borrow-elided — covered by the
        // `borrow_elision_elides_read_only_vecvec_index_binding` codegen test.
        assert_clean_asan_run(
            r#"
fn take(s: String) -> i64 { s.len() as i64 }
fn main() {
    let mut grid: Vec[Vec[String]] = Vec.new();
    let mut row: Vec[String] = Vec.new();
    row.push("hello".to_string());
    row.push("world".to_string());
    grid.push(row);
    let mut total = 0;
    let mut i = 0;
    while i < grid.len() {
        let r = grid[i];
        let mut j = 0;
        while j < r.len() { total = total + take(r[j]); j = j + 1; }
        i = i + 1;
    }
    println(total);
}
"#,
            &["10"], // len("hello") + len("world") = 5 + 5
            "vecvec_heap_element_consumed_by_value_no_double_free",
        );
    }

    #[test]
    fn asan_vec_option_shared_index_reused_across_consuming_calls_no_uaf() {
        // B-2026-07-11-29 layer 3 (corruption): a `Vec[Option[shared]]` ELEMENT
        // read by index (`src[0]`) passed BY VALUE to a consuming (cloning)
        // callee TWICE. The niche Vec-element read loads the inner pointer
        // WITHOUT an inc, so the callee's `Option[shared]` param `RcDecOption`
        // over-decremented the element the container still owns — freeing it
        // mid-sequence; a later alloc reused the slot and the second `src[0]`
        // read returned the wrong node (interpreter `4`, codegen corrupted /
        // use-after-free). The Index companion `share_option_shared_index_ref_for_arg`
        // now retains the loaded inner per pass, mirroring the Identifier /
        // FieldAccess arg companions.
        assert_clean_asan_run(
            r#"
shared struct Node { val: i64, mut left: Option[Node], mut right: Option[Node] }
fn clone_offset(node: Option[Node], delta: i64) -> Option[Node] {
    match node {
        None => None,
        Some(n) => Some(Node { val: n.val + delta, left: clone_offset(n.left, delta), right: clone_offset(n.right, delta) }),
    }
}
fn count_nodes(node: Option[Node]) -> i64 {
    match node { None => 0, Some(n) => 1 + count_nodes(n.left) + count_nodes(n.right) }
}
fn main() {
    let mut src: Vec[Option[Node]] = Vec.new();
    src.push(Some(Node { val: 1, left: Some(Node { val: 2, left: None, right: None }), right: None }));
    let l0 = clone_offset(src[0], 10);
    let l1 = clone_offset(src[0], 20);
    println(count_nodes(l0) + count_nodes(l1));
}
"#,
            &["4"], // two 2-node clones off the same live element
            "vec_option_shared_index_reused_across_consuming_calls_no_uaf",
        );
    }

    #[test]
    fn asan_vecvec_option_shared_scope_exit_drop_no_leak() {
        // B-2026-07-11-29 layer 4 (leak): dropping a `Vec[Vec[Option[shared]]]`
        // local only freed the inner Vec BUFFERS one level deep and treated
        // their `Option[shared]` elements as opaque, leaking every shared node
        // inside (LSan). `te_recursive_drop_fully_supported` now accepts an
        // `Option[shared T]` payload, so the outer drop routes to the
        // strictly-recursive `emit_vec_drop_fn` (→ `emit_option_drop_fn`, which
        // tag-guards and rc-decs the boxed shared payload) instead of the
        // one-level buffer-only fast path.
        assert_clean_asan_run(
            r#"
shared struct Node { val: i64, mut left: Option[Node], mut right: Option[Node] }
fn count_nodes(node: Option[Node]) -> i64 {
    match node { None => 0, Some(n) => 1 + count_nodes(n.left) + count_nodes(n.right) }
}
fn mk() -> Vec[Vec[Option[Node]]] {
    let mut shapes: Vec[Vec[Option[Node]]] = Vec.new();
    let mut base: Vec[Option[Node]] = Vec.new();
    base.push(Some(Node { val: 1, left: Some(Node { val: 2, left: None, right: None }), right: None }));
    shapes.push(base);
    shapes
}
fn main() {
    let shapes = mk();
    let lefts = shapes[0];
    println(count_nodes(lefts[0]));
}
"#,
            &["2"], // the shared node + its child, dropped exactly once
            "vecvec_option_shared_scope_exit_drop_no_leak",
        );
    }

    #[test]
    fn asan_let_bound_vec_option_shared_reused_no_uaf() {
        // B-2026-07-11-29 (`let s = v[i]` reuse leg): binding a
        // `Vec[Option[shared]]` element (`let s = src[0]`) retain-clones the
        // inner (`karac_clone_Option_Node`) but the binding was left UNREGISTERED
        // in `var_option_shared_heap`, so subsequent by-value passes got no
        // caller-retains arg-inc and the callee's exit-dec over-decremented on
        // the second pass → use-after-free. Case (f) in stmts.rs now registers
        // the `let s = v[i]` binding into the caller-retains model.
        assert_clean_asan_run(
            r#"
shared struct Node { val: i64, mut left: Option[Node], mut right: Option[Node] }
fn clone_offset(node: Option[Node], delta: i64) -> Option[Node] {
    match node {
        None => None,
        Some(n) => Some(Node { val: n.val + delta, left: clone_offset(n.left, delta), right: clone_offset(n.right, delta) }),
    }
}
fn count_nodes(node: Option[Node]) -> i64 {
    match node { None => 0, Some(n) => 1 + count_nodes(n.left) + count_nodes(n.right) }
}
fn main() {
    let mut src: Vec[Option[Node]] = Vec.new();
    src.push(Some(Node { val: 1, left: Some(Node { val: 2, left: None, right: None }), right: None }));
    let s = src[0];
    let l0 = clone_offset(s, 10);
    let l1 = clone_offset(s, 20);
    println(count_nodes(l0) + count_nodes(l1));
}
"#,
            &["4"],
            "let_bound_vec_option_shared_reused_no_uaf",
        );
    }

    #[test]
    fn asan_push_bound_option_shared_binding_no_uaf() {
        // B-2026-07-11-29 (push-move leg): pushing a tracked `Option[shared]`
        // BINDING into a `Vec[Option[shared]]` (`out.push(orig)`) co-owns the
        // node under reference semantics, but push (a builtin) never emitted the
        // caller-retains inc that consuming CALL sites do, while the source
        // binding's scope-exit `RcDecOption` still fired — freeing the node while
        // the container still pointed at it (use-after-free). The push arm now
        // emits `share_option_shared_ref_for_arg` for the moved binding.
        assert_clean_asan_run(
            r#"
shared struct Node { val: i64, mut left: Option[Node], mut right: Option[Node] }
fn count_nodes(node: Option[Node]) -> i64 {
    match node { None => 0, Some(n) => 1 + count_nodes(n.left) + count_nodes(n.right) }
}
fn main() {
    let mut out: Vec[Option[Node]] = Vec.new();
    let orig = Some(Node { val: 1, left: Some(Node { val: 2, left: None, right: None }), right: None });
    out.push(orig);
    println(count_nodes(out[0]));
}
"#,
            &["2"],
            "push_bound_option_shared_binding_no_uaf",
        );
    }

    #[test]
    fn asan_struct_field_shares_vec_option_shared_index_no_leak() {
        // B-2026-07-11-29 (struct-field share leg): sharing a
        // `Vec[Option[shared]]` element DIRECTLY into a struct-literal field
        // (`Node { left: src[0] }`) double-inc'd the node — `maybe_defensive_copy_param_arg`
        // retain-cloned it (`karac_clone_Option_Node`) AND the field capture-inc
        // fired — with no binding to carry a matching dec, so the node's rc never
        // returned to zero and it leaked (LSan). The capture-inc is now skipped
        // for an already-retained `v[i]` field value.
        assert_clean_asan_run(
            r#"
shared struct Node { val: i64, mut left: Option[Node], mut right: Option[Node] }
fn count_nodes(node: Option[Node]) -> i64 {
    match node { None => 0, Some(n) => 1 + count_nodes(n.left) + count_nodes(n.right) }
}
fn main() {
    let mut src: Vec[Option[Node]] = Vec.new();
    src.push(Some(Node { val: 1, left: Some(Node { val: 2, left: None, right: None }), right: None }));
    let mut cur: Vec[Option[Node]] = Vec.new();
    cur.push(Some(Node { val: 0, left: src[0], right: None }));
    cur.push(Some(Node { val: 9, left: src[0], right: None }));
    println(count_nodes(cur[0]) + count_nodes(cur[1]));
}
"#,
            &["6"], // two 3-node trees sharing the same left subtree
            "struct_field_shares_vec_option_shared_index_no_leak",
        );
    }

    // ── B-2026-07-11-32: non-Copy element index-swap / projection-assign ──
    // An index-read of a NON-COPY Vec element in ASSIGNMENT-RHS position
    // (`s = v[i]`, `v[i] = v[j]`) aliased the source slot's buffer — the assign
    // path only loaded the `{ptr,len,cap}` header, unlike the Let arm which
    // deep-clones — so the destination and the source element co-owned the
    // buffer and double-freed at scope exit. The natural in-place swap idiom
    // `let t = v[i]; v[i] = v[j]; v[j] = t;` over any non-Copy element (String,
    // Vec, struct) was therefore a silent double-free (correct output, then
    // `free(): double free detected` on a hardened allocator / ASAN). Separately,
    // an f-string TEMPORARY stored into a projection place (`v[i] = f"…"`,
    // `p.field = f"…"`) never had its accumulator cap zeroed for the index /
    // AoS-field targets, double-freeing the acc buffer. Fixed in
    // src/codegen/stmts.rs (clone the index-read assign-RHS; generalise the
    // acc-zero to the index/field stores).

    #[test]
    fn asan_index_swap_string_no_double_free() {
        // The flagship: the classic swap idiom over `Vec[String]`. Before the
        // fix `v[0] = v[1]` aliased slot 1's buffer, double-freeing at scope
        // exit. Value semantics (interpreter oracle): the sequence swaps slots
        // 0 and 1.
        assert_clean_asan_run(
            r#"
fn main() {
    let mut v: Vec[String] = [f"alpha-padding", f"bravo-padding", f"charlie-pad"];
    let t = v[0];
    v[0] = v[1];
    v[1] = t;
    println(v[0]);
    println(v[1]);
    println(v[2]);
}
"#,
            &["bravo-padding", "alpha-padding", "charlie-pad"],
            "index_swap_string_no_double_free",
        );
    }

    #[test]
    fn asan_index_swap_vecvec_no_double_free() {
        // Same idiom with a `Vec[Vec[i64]]` (a non-Copy INNER Vec element): the
        // inner `{ptr,len,cap}` was aliased on `v[0] = v[1]`. Confirms the clone
        // fires for any non-trivially-copyable element, not just String.
        assert_clean_asan_run(
            r#"
fn main() {
    let mut v: Vec[Vec[i64]] = [[10, 11], [20, 21], [30, 31]];
    let t = v[0];
    v[0] = v[1];
    v[1] = t;
    println(v[0][0]);
    println(v[1][0]);
    println(v[2][0]);
}
"#,
            &["20", "10", "30"],
            "index_swap_vecvec_no_double_free",
        );
    }

    #[test]
    fn asan_index_read_into_var_no_double_free_or_leak() {
        // `s = v[i]` — an index-read into an EXISTING (already heap-owning)
        // binding. Two hazards in one: the RHS clone must fire (else `s` and
        // `v[1]` alias → double-free), AND the overwritten old `s` buffer must
        // be eagerly freed (else LeakSanitizer flags the orphaned "old-padding").
        assert_clean_asan_run(
            r#"
fn main() {
    let mut v: Vec[String] = [f"aa-padding", f"bb-padding"];
    let mut s: String = f"old-padding";
    s = v[1];
    println(s);
    println(v[0]);
    println(v[1]);
}
"#,
            &["bb-padding", "aa-padding", "bb-padding"],
            "index_read_into_var_no_double_free_or_leak",
        );
    }

    #[test]
    fn asan_fstring_into_vec_element_no_double_free() {
        // `v[i] = f"…"` — an f-string TEMPORARY stored into a Vec element slot.
        // The store moves the acc buffer into the slot; the acc's own scope-exit
        // free double-freed it until the index-store arm learned to zero the acc
        // cap (mirroring the Identifier arm). The old element must also be freed
        // (no leak).
        assert_clean_asan_run(
            r#"
fn main() {
    let mut v: Vec[String] = [f"aa-padding", f"bb-padding"];
    v[0] = f"zz-padding";
    println(v[0]);
    println(v[1]);
}
"#,
            &["zz-padding", "bb-padding"],
            "fstring_into_vec_element_no_double_free",
        );
    }

    #[test]
    fn asan_fstring_into_struct_field_no_double_free() {
        // `p.name = f"…"` — an f-string temporary stored into an AoS struct
        // field. Same acc double-free as the Vec-element case; the field-store
        // arm previously zeroed the acc only for SoA element fields.
        assert_clean_asan_run(
            r#"
struct P { name: String }
fn main() {
    let mut p = P { name: f"aa-padding" };
    p.name = f"zz-padding";
    println(p.name);
}
"#,
            &["zz-padding"],
            "fstring_into_struct_field_no_double_free",
        );
    }

    #[test]
    fn asan_return_field_index_element_no_double_free() {
        // B-2026-07-11-35 (return leg): a method/fn that returns a field-rooted
        // index element (`fn get(ref self) -> String { self.xs[i] }`,
        // `fn getf(h: ref H, i) -> String { h.xs[i] }`) used to return an ALIAS
        // of the container's element — a `ref self`/`ref Struct` can't move it
        // out, so the returned owned `String` and the container's element both
        // freed the buffer at scope exit (double-free; the bare-`v[i]` return
        // already produced an independent value). The fn-tail now deep-clones a
        // field-rooted index read. Covers both the `self.field[i]` and the
        // `h.field[i]`-via-ref-param shapes, with the returned value bound AND
        // consumed directly, over heap String elements built and dropped.
        assert_clean_asan_run(
            r#"
struct H { xs: Vec[String] }
impl H {
    fn get(ref self, i: i64) -> String { self.xs[i] }
}
fn getf(h: ref H, i: i64) -> String { h.xs[i] }
fn main() {
    let mut r: i64 = 0;
    while r < 3 {
        let h = H { xs: [f"alpha-{r}-pad", f"bravo-{r}-pad", f"charlie-{r}"] };
        println(h.get(0));
        let bound: String = getf(h, 1);
        println(bound);
        println(h.xs[2]);
        r = r + 1;
    }
}
"#,
            &[
                "alpha-0-pad",
                "bravo-0-pad",
                "charlie-0",
                "alpha-1-pad",
                "bravo-1-pad",
                "charlie-1",
                "alpha-2-pad",
                "bravo-2-pad",
                "charlie-2",
            ],
            "return_field_index_element_no_double_free",
        );
    }

    #[test]
    fn asan_generic_container_method_push_no_leak() {
        // B-2026-07-11-35 (push leg): a GENERIC container `Box[T] { xs: Vec[T] }`
        // built via a generic constructor (`Box.new()`) and filled through a
        // generic method (`fn add(mut ref self, x: T) { self.xs.push(x) }`) with
        // NON-COPY (String) elements. Two coupled defects: (1) the mono param
        // prologue registered `x: T` off the bare `T`, so `self.xs.push(x)` MOVED
        // the caller's buffer (garbage reads — the correctness leg, pinned in
        // `tests/codegen.rs`); (2) once the push deep-copies (the fix), the
        // element buffers leaked because the struct DROP was synthesized ONCE per
        // struct NAME and resolved the `Vec[T]` field from bare `T`, never
        // draining the concrete `Vec[String]`. Per-monomorph struct-drop synthesis
        // (`__karac_drop_struct_Box$String`, distinct from `Box$i64`) now drains
        // each element. Loops so any per-iteration leak accumulates for LSan, and
        // coexists `Box[String]` with `Box[i64]` so the String drain never runs
        // over the i64 Vec (a name-shared drop would `free` each i64 as a bogus
        // `{ptr,len,cap}` — a heap-buffer-overflow / invalid-free, not just a leak).
        assert_clean_asan_run(
            r#"
struct Box[T] { xs: Vec[T] }
impl[T] Box[T] {
    fn new() -> Box[T] { Box { xs: Vec.new() } }
    fn add(mut ref self, x: T) { self.xs.push(x); }
    fn at(ref self, i: i64) -> T { self.xs[i] }
    fn size(ref self) -> i64 { self.xs.len() }
}
fn main() {
    let mut r: i64 = 0;
    while r < 3 {
        let mut s: Box[String] = Box.new();
        s.add(f"row-{r}-aaaaaa");
        s.add(f"row-{r}-bbbbbb");
        s.add(f"row-{r}-cccccc");
        let mut n: Box[i64] = Box.new();
        n.add(r * 10);
        n.add(r * 10 + 1);
        println(s.at(0));
        println(s.at(2));
        println(f"{n.at(1)} {s.size()} {n.size()}");
        r = r + 1;
    }
}
"#,
            &[
                "row-0-aaaaaa",
                "row-0-cccccc",
                "1 3 2",
                "row-1-aaaaaa",
                "row-1-cccccc",
                "11 3 2",
                "row-2-aaaaaa",
                "row-2-cccccc",
                "21 3 2",
            ],
            "generic_container_method_push_no_leak",
        );
    }

    #[test]
    fn asan_return_owned_generic_param_no_double_free() {
        // B-2026-07-11-35 (return-owned-`T`-param leg): returning an owned heap
        // (String / Vec) PARAM from a generic fn (`fn echo[T](x: T) -> T { x }`)
        // handed back the caller's moved-in buffer, which the caller then freed a
        // second time (double-free abort). The mono tail now deep-copies the
        // returned owned-vecstr param, mirroring the non-generic path. Loops so a
        // leak (the dual failure — an over-suppressed copy) accumulates for LSan,
        // and exercises String, Vec[i64], and Vec[String] in one program so the
        // per-instantiation mono symbols (the collision the copy exposed) are all
        // live — a shared body would run one element stride over the others.
        assert_clean_asan_run(
            r#"
fn echo[T](x: T) -> T { x }
fn main() {
    let mut r: i64 = 0;
    while r < 3 {
        let a: String = echo(f"fresh-{r}-aaaa");
        println(a);
        let s: String = f"local-{r}-bbbb";
        let b: String = echo(s);
        println(b);
        let vi: Vec[i64] = echo([r, r + 1, r + 2]);
        println(f"{vi[2]}");
        let vs: Vec[String] = echo([f"e-{r}-x", f"e-{r}-y"]);
        println(vs[1]);
        r = r + 1;
    }
}
"#,
            &[
                "fresh-0-aaaa",
                "local-0-bbbb",
                "2",
                "e-0-y",
                "fresh-1-aaaa",
                "local-1-bbbb",
                "3",
                "e-1-y",
                "fresh-2-aaaa",
                "local-2-bbbb",
                "4",
                "e-2-y",
            ],
            "return_owned_generic_param_no_double_free",
        );
    }

    #[test]
    fn asan_struct_field_by_ref_to_free_fn_no_double_free() {
        // B-2026-07-12-1: passing a struct FIELD (`self.names`, a `Vec[String]`)
        // by ref to a FREE function double-freed the field's backing Vec under
        // AOT (`free(): double free detected`) — the `ref`-arg rvalue path
        // shallow-copied the field header into a temp and freed its buffer at
        // scope exit, double-freeing what the receiver's field-drop still owns.
        // Now the field is borrowed in place (a GEP off the receiver). Loops so
        // any double-free / leak accumulates for ASAN+LSan; drives both the read
        // (`ref Vec[String]`) shape (the reported repro, over heap String
        // elements) and a `mut ref Vec[i64]` field whose in-place mutation must
        // survive without freeing the shared buffer.
        assert_clean_asan_run(
            r#"
fn scan(names: ref Vec[String], name: ref String) -> i64 {
    let mut i = 0;
    loop {
        if i >= names.len() { return -1; }
        if names[i] == name { return i; }
        i = i + 1;
    }
}
fn addall(v: mut ref Vec[i64], n: i64) {
    let mut i = 0;
    loop { if i >= v.len() { return; } v[i] = v[i] + n; i = i + 1; }
}
struct T { names: Vec[String], xs: Vec[i64] }
impl T {
    fn find(ref self, q: ref String) -> i64 { scan(self.names, q) }
    fn bump(mut ref self, n: i64) { addall(mut self.xs, n) }
}
fn main() {
    let mut r: i64 = 0;
    while r < 3 {
        let mut t = T { names: Vec.new(), xs: Vec.new() };
        t.names.push(f"row-{r}-alpha");
        t.names.push(f"row-{r}-bravo");
        t.xs.push(r); t.xs.push(r + 1);
        let q = f"row-{r}-bravo";
        println(f"{t.find(q)}");
        t.bump(10);
        println(f"{t.xs[0]}");
        r = r + 1;
    }
}
"#,
            &["1", "10", "1", "11", "1", "12"],
            "struct_field_by_ref_to_free_fn_no_double_free",
        );
    }

    #[test]
    fn asan_char_to_string_no_leak() {
        // `From[char] for String`: `String.from(c)` / `c.into()` allocate a
        // fresh heap String per call. Loops both surfaces so any per-iteration
        // leak or bad free accumulates for ASAN + LSan. The bound String is
        // consumed (`.len()`) and dropped each iteration; a multibyte char
        // exercises a >1-byte allocation.
        assert_clean_asan_run(
            r#"
fn main() {
    let mut i: i64 = 0;
    let mut total: i64 = 0;
    while i < 200 {
        let a: String = String.from('A');
        total = total + a.len();
        let c: char = '😀';
        let b: String = c.into();
        total = total + b.len();
        i = i + 1;
    }
    println(f"{total}");
}
"#,
            &["1000"],
            "char_to_string_no_leak",
        );
    }

    #[test]
    fn asan_numeric_try_from_err_string_no_leak() {
        // Numeric narrowing `T.try_from(x) -> Result[T, String]`: the `Err`
        // payload is a static (`cap=0`) String, so the failure path must
        // allocate nothing and free nothing. Loops the Err arm (out-of-range)
        // and the Ok arm many times; any per-iteration String leak or bad free
        // accumulates for ASAN + LSan. Both the match-consumed `e` and the
        // discarded-Result shapes are exercised.
        assert_clean_asan_run(
            r#"
fn main() {
    let mut i: i64 = 0;
    let mut oks: i64 = 0;
    let mut errs: i64 = 0;
    while i < 200 {
        match i8.try_from(i) {
            Ok(v) => { oks = oks + 1; }
            Err(e) => { if e.len() > 0 { errs = errs + 1; } }
        }
        i = i + 1;
    }
    println(f"{oks}");
    println(f"{errs}");
}
"#,
            &["128", "72"],
            "numeric_try_from_err_string_no_leak",
        );
    }

    #[test]
    fn asan_enum_self_to_string_no_leak() {
        // `self.to_string()` inside an impl method renders a payload
        // `#[derive(Display)]` enum into a fresh heap String each call
        // (B-2026-07-12-15). Loops both variants so any per-iteration leak or
        // bad free accumulates for ASAN + LSan; the rendered String is consumed
        // (`.len()`) directly — not through a generic f-string, whose
        // interpolation leak (B-2026-07-12-18) is a separate pre-existing path.
        assert_clean_asan_run(
            r#"
#[derive(Display)]
enum IoErr { NotFound, Other(String) }
trait Error { fn message(ref self) -> String; }
impl Error for IoErr { fn message(ref self) -> String { self.to_string() } }
fn main() {
    let mut i: i64 = 0;
    let mut total: i64 = 0;
    while i < 200 {
        let a: IoErr = IoErr.NotFound;
        let b: IoErr = IoErr.Other(String.from("disk full"));
        total = total + a.message().len();
        total = total + b.message().len();
        i = i + 1;
    }
    println(f"{total}");
}
"#,
            &["4800"],
            "enum_self_to_string_no_leak",
        );
    }

    #[test]
    fn asan_struct_to_string_returned_from_fn_no_double_free() {
        // B-2026-07-12-17: a struct `.to_string()` returned directly from a
        // function double-freed the rendered buffer (the return-position
        // fstr-acc ownership transfer missed the `.to_string()` shape). Loops a
        // `ref`-param return and a `self.to_string()` return so any double-free
        // / leak accumulates for ASAN + LSan; the rendered String is consumed
        // (`.len()`) each iteration.
        assert_clean_asan_run(
            r#"
#[derive(Display)]
struct Point { x: i64, y: i64 }
impl Point { fn describe(ref self) -> String { self.to_string() } }
fn render(p: ref Point) -> String { p.to_string() }
fn main() {
    let mut i: i64 = 0;
    let mut total: i64 = 0;
    while i < 200 {
        let p: Point = Point { x: 3, y: 4 };
        total = total + render(p).len();
        total = total + p.describe().len();
        i = i + 1;
    }
    println(f"{total}");
}
"#,
            &["8000"],
            "struct_to_string_returned_from_fn_no_double_free",
        );
    }

    #[test]
    fn asan_generic_ref_enum_display_no_leak() {
        // B-2026-07-12-18: rendering a payload `#[derive(Display)]` enum through
        // a generic `ref E` param (`f"{e}"`) leaked the render buffer under
        // codegen (a symptom of reading the value from the wrong address; fixed
        // via `get_data_ptr`). Loops the generic-ref f-string so any per-call
        // leak accumulates for ASAN + LSan; the rendered String is consumed
        // (`.len()`).
        assert_clean_asan_run(
            r#"
#[derive(Display)]
enum IoErr { NotFound, Other(String) }
fn wrap[E: Display](e: ref E) -> String { f"error: {e}" }
fn main() {
    let mut i: i64 = 0;
    let mut total: i64 = 0;
    while i < 200 {
        let b: IoErr = IoErr.Other(String.from("boom"));
        total = total + wrap(b).len();
        i = i + 1;
    }
    println(f"{total}");
}
"#,
            &["3600"],
            "generic_ref_enum_display_no_leak",
        );
    }

    #[test]
    fn asan_field_read_option_shared_push_no_leak_or_uaf() {
        // B-2026-07-12-4: pushing a FIELD-READ `Option[shared]` (`stack.push(
        // n.left)`) onto a `Vec[Option[shared]]` and dropping the Vec with
        // residual elements is aliasing co-ownership — the pushed handle stays
        // live at its source node `n`. `Vec.push` is a builtin that bypassed the
        // generic method-arg retain, so the field read went un-inc'd: the Vec's
        // per-element drop AND `n`'s own drop both released the node — a
        // use-after-free (read of a freed 32-byte block; a leak before the Vec
        // per-element drop began releasing residuals). The fix inc's the
        // field-read inner on push (`share_option_shared_field_ref_for_arg`).
        // Looped to amplify any per-iteration imbalance well past noise; reads
        // the pushed values back so a wrong-node miscompile would also surface.
        assert_clean_asan_run(
            r#"
shared struct Node { val: i64, mut left: Option[Node], mut right: Option[Node] }
fn main() {
    let mut i: i64 = 0;
    let mut total: i64 = 0;
    while i < 200 {
        let root = Some(Node {
            val: 1,
            left: Some(Node { val: 2, left: None, right: None }),
            right: Some(Node { val: 3, left: None, right: None }),
        });
        let mut stack: Vec[Option[Node]] = Vec.new();
        match root {
            None => {}
            Some(n) => { stack.push(n.left); stack.push(n.right); }
        }
        for item in stack {
            match item { None => {} Some(node) => { total = total + node.val; } }
        }
        i = i + 1;
    }
    println(f"{total}");
}
"#,
            &["1000"],
            "field_read_option_shared_push_no_leak_or_uaf",
        );
    }

    #[test]
    fn asan_direct_index_match_option_shared_no_leak() {
        // B-2026-07-12-21: a direct `match vec[i]` whose scrutinee is an
        // `Option[shared]` index-read LEAKED the extracted node once per match.
        // The index read deep-cloned the element (rc-INC via the concrete
        // `Option[shared Node]` clone fn), but the fresh-temp match scrutinee's
        // drop was resolved from the ERASED generic `Option` layout (all-`None`
        // drop-kinds), so `has_droppable` was false and the retained rc was
        // never released. Fix (lowering): rewrite `match vec[i] { … }` into the
        // proven-clean let-bound form `{ let s = vec[i]; match s { … } }`, whose
        // binding carries the concrete type so its cleanup releases the clone.
        // Looped in a HELPER fn (not `main`) so the per-iteration leak is real
        // and not masked by `main`'s final-scope drop elision; reads `nd.val`
        // back so a wrong-node miscompile would also surface.
        assert_clean_asan_run(
            r#"
shared struct Node { val: i64, mut left: Option[Node], mut right: Option[Node] }
fn xfer() -> i64 {
    let mut dst: Vec[Option[Node]] = Vec.new();
    dst.push(Some(Node { val: 10, left: None, right: None }));
    let mut r: i64 = 0;
    match dst[0] {
        None => {}
        Some(nd) => { r = nd.val; }
    }
    r
}
fn main() {
    let mut i: i64 = 0;
    let mut t: i64 = 0;
    while i < 200 {
        t = t + xfer();
        i = i + 1;
    }
    println(f"{t}");
}
"#,
            &["2000"],
            "direct_index_match_option_shared_no_leak",
        );
    }

    #[test]
    fn asan_shared_scrutinee_shadowed_by_local() {
        // B-2026-07-12-6: a `match e { … }` arm over a by-value shared-enum
        // param `e` that declares a same-named local (`let mut e = 0`) shadows
        // the scrutinee's pointer slot. The param's scope-exit RC-dec reloaded
        // its pointer BY NAME from `variables["e"]`, which the shadow had
        // repointed at an `i64` alloca — so the dec walked an integer-as-pointer
        // and corrupted the heap (segfault at O2, hang at O0). The fix gates the
        // reload on the slot being pointer-typed, falling back to the pointer
        // captured at registration. Looped so each call allocates + frees the
        // shared node; a garbage-pointer RC-dec surfaces as ASAN
        // use-after-free / heap corruption and a wrong-slot drop as an LSan
        // leak. Kept a NON-recursive enum with i64 payloads so this isolates
        // the shadow RC-dec — a recursive `Node(E)` shape would also exercise
        // the separate shared-enum recursive-payload-drop path.
        assert_clean_asan_run(
            r#"
shared enum E { A(i64), B(i64) }
fn chk(e: E) -> i64 {
    match e {
        A(n) => n,
        B(m) => {
            let mut e = 0;
            let mut i = 0;
            loop {
                if i >= 3 { break; }
                e = e + i;
                i = i + 1;
            }
            m + e
        }
    }
}
fn main() {
    let mut i: i64 = 0;
    let mut t: i64 = 0;
    while i < 200 {
        t = t + chk(B(10));
        i = i + 1;
    }
    println(f"{t}");
}
"#,
            // (10 payload + 3 from 0+1+2) * 200 = 13 * 200 = 2600.
            &["2600"],
            "shared_scrutinee_shadowed_by_local",
        );
    }

    #[test]
    fn asan_recursive_shared_enum_arg_no_leak() {
        // B-2026-07-12-25: a freshly-constructed `shared enum` value passed by
        // value into a recursive self-call leaked the whole RC chain at ODD
        // constructor-nesting depth. `fresh_arg_bare_shared_heap_type`'s
        // passthrough self-exclusion (correct for a `g(make())` function chain)
        // recursed through the constructor's payload arg and flipped Some/None
        // per level, so the caller-side RC-dec was registered only at even
        // depth; odd depths (`Node(Leaf)`, `Node(Node(Node(Leaf)))`) registered
        // nothing and leaked every node. Fix: skip the guard for a variant
        // constructor, which owns its payload via its recursive drop. Uses an
        // ODD (depth-3) chain — the leaking case pre-fix — looped so LSan sees
        // the per-iteration leak.
        assert_clean_asan_run(
            r#"
shared enum E { Leaf(i64), Node(E) }
fn chk(e: E) -> i64 {
    match e {
        Leaf(n) => n,
        Node(x) => chk(x)
    }
}
fn main() {
    let mut i: i64 = 0;
    let mut t: i64 = 0;
    while i < 200 {
        t = t + chk(Node(Node(Node(Leaf(1)))));
        i = i + 1;
    }
    println(f"{t}");
}
"#,
            &["200"],
            "recursive_shared_enum_arg_no_leak",
        );
    }

    // ── RC-elision payload-escape guard (KARAC_RC_ELIDE_REF_PARAMS) ───────
    // Condition 4 (src/rc_elide.rs) closes the "known residual" from
    // docs/spikes/rc-elide-ref-params.md: a match-binding of the candidate
    // param passed BY VALUE to a consuming callee (`match p { Some(n) =>
    // consume(n) }`). The guard makes elision sound BY CONSTRUCTION — it
    // DECLINES to elide any param whose payload is moved out as a bare value, so
    // `probe`/`probe2`/`probe3` below run on the normal balanced-RC path (the
    // elidable set is empty for them, verified in src/rc_elide.rs unit tests).
    // The positive control (is_mirror/is_symmetric — payloads used only via
    // projections) IS elided. Run the whole suite with
    // `KARAC_RC_ELIDE_REF_PARAMS=1`: all must be byte-for-byte as clean as
    // flag-off (Linux LSan). Together with the unit tests these pin both halves:
    // the guard declines the escaping shapes, and the elided walkers stay
    // leak-free. (Runtime was already balanced even pre-guard — the guard
    // removes the reliance on codegen's payload re-share, not a live leak.)

    #[test]
    fn asan_rc_elide_consumed_payload_projection_caller_no_double_free() {
        // Residual shape, direct consume: `match p { Some(n) => sink(n) }` moves
        // payload `n` by value into owned `sink`. Condition 4 declines to elide
        // `probe`, so it runs balanced. Alternating idx over a 2-node pool, 200
        // reps: 100*5 + 100*9.
        assert_clean_asan_run(
            r#"
shared struct Node { val: i64, mut left: Option[Node], mut right: Option[Node] }
fn sink(x: Node) -> i64 { x.val }
fn probe(p: Option[Node]) -> i64 { match p { None => 0i64, Some(n) => sink(n) } }
fn main() {
    let mut pool: Vec[Option[Node]] = Vec.new();
    pool.push(Some(Node { val: 5i64, left: None, right: None }));
    pool.push(Some(Node { val: 9i64, left: None, right: None }));
    let mut t: i64 = 0i64;
    let mut rep: i64 = 0i64;
    while rep < 200i64 { let idx = rep % 2i64; t = t + probe(pool[idx]); rep = rep + 1i64; }
    println(f"{t}")
}
"#,
            &["1400"],
            "rc_elide_consumed_payload_projection_caller_no_double_free",
        );
    }

    #[test]
    fn asan_rc_elide_forwarded_payload_no_double_free() {
        // Residual shape, two-level consume chain: payload `n` forwarded through
        // `forward` into `sink`. Condition 4 declines to elide `probe2` (payload
        // moved out), so it runs on the balanced-RC path. Prints 1400.
        assert_clean_asan_run(
            r#"
shared struct Node { val: i64, mut left: Option[Node], mut right: Option[Node] }
fn sink(x: Node) -> i64 { x.val }
fn forward(y: Node) -> i64 { sink(y) }
fn probe2(p: Option[Node]) -> i64 { match p { None => 0i64, Some(n) => forward(n) } }
fn main() {
    let mut pool: Vec[Option[Node]] = Vec.new();
    pool.push(Some(Node { val: 5i64, left: None, right: None }));
    pool.push(Some(Node { val: 9i64, left: None, right: None }));
    let mut t: i64 = 0i64;
    let mut rep: i64 = 0i64;
    while rep < 200i64 { let idx = rep % 2i64; t = t + probe2(pool[idx]); rep = rep + 1i64; }
    println(f"{t}")
}
"#,
            &["1400"],
            "rc_elide_forwarded_payload_no_double_free",
        );
    }

    #[test]
    fn asan_rc_elide_if_let_consumed_payload_no_leak() {
        // Residual shape via if-let: `if let Some(n) = p { r = sink(n); }`. `p`
        // is scrutinee-only (condition 2 holds) but its payload is moved out, so
        // condition 4 declines to elide `probe3`; runs balanced. Prints 1400.
        assert_clean_asan_run(
            r#"
shared struct Node { val: i64, mut left: Option[Node], mut right: Option[Node] }
fn sink(x: Node) -> i64 { x.val }
fn probe3(p: Option[Node]) -> i64 { let mut r = 0i64; if let Some(n) = p { r = sink(n); } r }
fn main() {
    let mut pool: Vec[Option[Node]] = Vec.new();
    pool.push(Some(Node { val: 5i64, left: None, right: None }));
    pool.push(Some(Node { val: 9i64, left: None, right: None }));
    let mut t: i64 = 0i64;
    let mut rep: i64 = 0i64;
    while rep < 200i64 { let idx = rep % 2i64; t = t + probe3(pool[idx]); rep = rep + 1i64; }
    println(f"{t}")
}
"#,
            &["1400"],
            "rc_elide_if_let_consumed_payload_no_leak",
        );
    }

    #[test]
    fn asan_rc_elide_is_mirror_symmetric_walk_preserved_no_leak() {
        // Positive control — the #101 win. With the flag on, `is_symmetric[root]`
        // and `is_mirror[a,b]` are BOTH elided (verified via KARAC_RC_ELIDE_DEBUG):
        // the two hot, bool-returning, scrutinee-only walkers whose payloads are
        // used ONLY via field projections (`an.left`, `n.right`) into `ref`
        // positions — never moved out. Must stay leak-free: a hand-built
        // symmetric tree, is_symmetric 200x → 200 trues.
        assert_clean_asan_run(
            r#"
shared struct Node { val: i64, mut left: Option[Node], mut right: Option[Node] }
fn is_mirror(a: Option[Node], b: Option[Node]) -> bool {
    match a {
        None => { match b { None => true, Some(_) => false } }
        Some(an) => { match b { None => false, Some(bn) => an.val == bn.val and is_mirror(an.left, bn.right) and is_mirror(an.right, bn.left) } }
    }
}
fn is_symmetric(root: Option[Node]) -> bool { match root { None => true, Some(n) => is_mirror(n.left, n.right) } }
fn main() {
    let leftsub = Some(Node { val: 2i64, left: Some(Node { val: 3i64, left: None, right: None }), right: None });
    let rightsub = Some(Node { val: 2i64, left: None, right: Some(Node { val: 3i64, left: None, right: None }) });
    let root = Some(Node { val: 1i64, left: leftsub, right: rightsub });
    let mut pool: Vec[Option[Node]] = Vec.new();
    pool.push(root);
    let mut t: i64 = 0i64;
    let mut rep: i64 = 0i64;
    while rep < 200i64 { let sym = is_symmetric(pool[0i64]); t = t + (if sym { 1i64 } else { 0i64 }); rep = rep + 1i64; }
    println(f"{t}")
}
"#,
            &["200"],
            "rc_elide_is_mirror_symmetric_walk_preserved_no_leak",
        );
    }

    #[test]
    fn asan_map_get_unwrap_heap_value_no_double_free() {
        // B-2026-07-14-15: `let r = m.get(k).unwrap()` on a Map whose VALUE is a
        // NON-shared heap type (`Vec`/`String`) double-freed — `map.get` returns
        // a BORROW, so `r` shallow-aliased the map's buffer while being registered
        // as an owned Vec/String (scope-exit drop), and both `r`'s drop and the
        // map's value-drop freed the same buffer. `r` is now treated as a
        // borrow-elided alias (no owned drop; the map stays sole owner). Covers a
        // `Vec` value and a `String` value; must be double-free-clean.
        assert_clean_asan_run(
            r#"
fn main() {
    let mut mv: Map[String, Vec[i64]] = Map.new();
    let mut v: Vec[i64] = Vec.new();
    v.push(1);
    v.push(2);
    mv.insert("a", v);
    let rv = mv.get("a").unwrap();
    println(rv.len());

    let mut ms: Map[String, String] = Map.new();
    ms.insert("k", "hello world");
    let rs = ms.get("k").unwrap();
    println(rs.len());
}
"#,
            &["2", "11"],
            "map_get_unwrap_heap_value_no_double_free",
        );
    }

    #[test]
    fn asan_generic_fn_string_temp_arg_no_leak() {
        // B-2026-07-14-12: a fresh-heap `String` TEMP arg (a fn-return, not a
        // named binding) passed to a GENERIC fn leaked the temp's buffer — the
        // mono body clones the `String` param into its owned copy, orphaning the
        // caller's temp, which the generic-call path (unlike the non-generic one)
        // never materialized a drop for. Exercises the multi-use (`dup`) shape
        // and the passthrough (`passthru`) shape, both with a temp arg. Must be
        // leak-clean.
        assert_clean_asan_run(
            r#"
fn mk() -> String { let mut s = String.from(""); s.push_str("abcdefghijklmno"); s }
fn wrap[T](x: T) -> Vec[T] { let mut v: Vec[T] = Vec.new(); v.push(x); v }
fn passthru[T](x: T) -> T { x }
fn main() {
    let a = wrap(mk());
    let b = passthru(mk());
    println(a.len());
    println(b.len());
}
"#,
            &["1", "15"],
            "generic_fn_string_temp_arg_no_leak",
        );
    }

    #[test]
    fn asan_fold_string_accumulator_no_double_free() {
        // B-2026-07-13-18: `iter().fold(String.from(""), |acc,x| f"{acc}-{x}")`
        // — a heap-accumulator string-join fold. Codegen desugars it AFTER
        // typecheck, so without the accumulator's recorded type the synthetic
        // `let mut acc` never registered as a tracked String and the Assign
        // move-machinery was skipped: the accumulator buffer double-freed
        // (`free(): double free`), and an intermediate restructure leaked every
        // middle buffer. The fix stamps the typechecker-recorded accumulator
        // type on the synthetic `let` and lowers to the self-referential
        // `acc = f"{acc}-{x}"` shape a hand-written loop uses. Must be
        // double-free-AND-leak-clean.
        assert_clean_asan_run(
            r#"
fn main() {
    let v: Vec[i64] = [1, 2, 3];
    let j = v.iter().fold(String.from(""), |acc, x| f"{acc}-{x}");
    println(j);
}
"#,
            &["-1-2-3"],
            "fold_string_accumulator_no_double_free",
        );
    }

    #[test]
    fn asan_channel_sent_unreceived_heap_payload_no_leak() {
        // B-2026-07-13-17: a heap payload SENT on a channel but never RECEIVED
        // has no owner to free it (send moves it into the queue; the source's
        // free is suppressed). The channel destructor now drains any still-queued
        // payloads through the element's drop fn. A Vec sent and never recv'd,
        // then the channel drops at scope exit — must be leak-clean.
        assert_clean_asan_run(
            r#"
fn main() {
    let (tx, rx): (Sender[Vec[i64]], Receiver[Vec[i64]]) = Channel.new();
    let mut v = Vec.new();
    v.push(1);
    v.push(2);
    tx.send(v);
    println("sent");
}
"#,
            &["sent"],
            "channel_sent_unreceived_heap_payload_no_leak",
        );
    }

    #[test]
    fn asan_channel_balanced_send_recv_no_double_free() {
        // The dual guard: a RECEIVED payload is owned by its receiver binding and
        // must NOT also be freed by the channel destructor (the received blob was
        // dequeued, so it is not on the queue at drop). Balanced send→recv over a
        // Vec payload, plus a second sent-but-unreceived Vec that the destructor
        // drains — exercises both halves in one program, leak- and
        // double-free-clean.
        assert_clean_asan_run(
            r#"
fn main() {
    let (tx, rx): (Sender[Vec[i64]], Receiver[Vec[i64]]) = Channel.new();
    let mut a = Vec.new();
    a.push(1);
    let mut b = Vec.new();
    b.push(2);
    b.push(3);
    tx.send(a);
    tx.send(b);
    let got = rx.recv();
    println(f"got len: {got.len()}");
}
"#,
            &["got len: 1"],
            "channel_balanced_send_recv_no_double_free",
        );
    }

    #[test]
    fn asan_vec_shared_whole_variable_reassign_no_leak() {
        // B-2026-07-12-30: overwriting a `Vec[shared]` local (`current = next`,
        // the BFS-worklist idiom) freed the OLD buffer but skipped its
        // per-element rc-release, stranding every shared node the overwritten Vec
        // held. The Assign overwrite now runs the same element-releasing walk the
        // scope-exit `FreeVecBuffer` cleanup does. A single `current = next` over a
        // `Vec[Node]` holding one shared node must be leak-clean.
        assert_clean_asan_run(
            r#"
shared struct Node { val: i64, mut left: Option[Node], mut right: Option[Node] }
fn main() {
    let root = Some(Node { val: 1, left: Some(Node { val: 2, left: None, right: None }), right: None });
    let mut current: Vec[Node] = Vec.new();
    match root { None => {} Some(n) => { current.push(n); } }
    let mut next: Vec[Node] = Vec.new();
    match current[0].left { None => {} Some(l) => { next.push(l); } }
    current = next;
    println(f"len: {current.len()}");
}
"#,
            &["len: 1"],
            "vec_shared_whole_variable_reassign_no_leak",
        );
    }

    #[test]
    fn asan_vec_shared_bfs_level_order_loop_no_leak() {
        // The real kata #102 shape: a `while` BFS that rebuilds `next` each level
        // and does `current = next`. Every level's overwrite must release the
        // prior level's shared nodes. A 3-node tree traversed to completion,
        // leak-clean, summing all values.
        assert_clean_asan_run(
            r#"
shared struct Node { val: i64, mut left: Option[Node], mut right: Option[Node] }
fn main() {
    let root = Node { val: 1, left: Some(Node { val: 2, left: None, right: None }), right: Some(Node { val: 3, left: None, right: None }) };
    let mut current: Vec[Node] = Vec.new();
    current.push(root);
    let mut total = 0;
    while current.len() > 0 {
        let mut next: Vec[Node] = Vec.new();
        let mut i = 0;
        while i < current.len() {
            total = total + current[i].val;
            match current[i].left { None => {} Some(l) => { next.push(l); } }
            match current[i].right { None => {} Some(r) => { next.push(r); } }
            i = i + 1;
        }
        current = next;
    }
    println(f"total: {total}");
}
"#,
            &["total: 6"],
            "vec_shared_bfs_level_order_loop_no_leak",
        );
    }

    #[test]
    fn asan_question_nested_option_string_payload_no_leak() {
        // B-2026-07-13-19: `?` on `Result[Option[String], E]` rebuilds the
        // extracted `Option[String]` from the Result's payload words. A wrong
        // reconstruction (dropped `cap` word, or truncation to `w0`) would leave
        // the String with a garbage cap → invalid free / leak. Must round-trip
        // the heap String through `?`, the `match`, and the returned `Ok(s)`
        // leak-clean.
        assert_clean_asan_run(
            r#"
enum E { X }
fn inner(n: i64) -> Result[Option[String], E] {
    if n < 0 { return Err(E.X); }
    Ok(Some(f"got-{n}"))
}
fn outer(n: i64) -> Result[String, E] {
    let opt = inner(n)?;
    match opt { Some(s) => Ok(s), None => Ok(f"empty") }
}
fn main() {
    match outer(7) { Ok(v) => println(v), Err(_) => println("e") }
}
"#,
            &["got-7"],
            "question_nested_option_string_payload_no_leak",
        );
    }

    #[test]
    fn asan_fold_string_accumulator_over_map_no_leak() {
        // Sibling of the above with a fused `map` adaptor ahead of the fold, so
        // the loop var is the adaptor's param (`y`) and the accumulator is the
        // fold's own param (`acc`). Exercises the collision-free direct-acc path
        // with a non-trivial fused chain. Leak/double-free-clean.
        assert_clean_asan_run(
            r#"
fn main() {
    let v: Vec[i64] = [1, 2, 3];
    let j = v.iter().map(|y| y * 2).fold(String.from("<"), |acc, x| f"{acc}{x},");
    println(j);
}
"#,
            &["<2,4,6,"],
            "fold_string_accumulator_over_map_no_leak",
        );
    }
}
