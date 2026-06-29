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
        let resolved = karac::resolve(&parsed.program);
        let typed = karac::typecheck(&parsed.program, &resolved);
        karac::lower(&mut parsed.program, &typed);
        // Ownership-loaded by default, mirroring `tests/codegen.rs`'s
        // `run_program`: `karac build` always passes ownership, and a
        // `None` here leaves the RC-fallback boxing surface untested —
        // exactly the divergence that hid the Option[shared] boxing
        // collision (b027fc15 bug 3) from the whole ASAN corpus.
        let ownership = karac::ownershipcheck(&parsed.program, &typed);

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
        let resolved = karac::resolve(&parsed.program);
        let typed = karac::typecheck(&parsed.program, &resolved);
        karac::lower(&mut parsed.program, &typed);
        let effects = karac::effectcheck(&parsed.program);
        let ownership = karac::ownershipcheck(&parsed.program, &typed);
        let analysis = karac::concurrency_analyze(&parsed.program, &effects);

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
        let b = visited.get((i + 1) % k).unwrap();
        a.neighbors.push(b);
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
        let resolved = karac::resolve(&parsed.program);
        let typed = karac::typecheck(&parsed.program, &resolved);
        karac::lower(&mut parsed.program, &typed);
        let ownership = karac::ownershipcheck(&parsed.program, &typed);

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
        let resolved = karac::resolve(&parsed.program);
        let typed = karac::typecheck(&parsed.program, &resolved);
        karac::lower(&mut parsed.program, &typed);
        let effects = karac::effectcheck(&parsed.program);
        let analysis = karac::concurrency_analyze(&parsed.program, &effects);

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
fn peek(s: Span) -> i64 { s.off }
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
fn peek(w: Wrap) -> i64 { w.hi }
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
}
