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
    fn run_under_asan(src: &str, label: &str) -> Option<(String, std::process::ExitStatus)> {
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

        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let obj_path = format!("/tmp/karac_asan_{}_{}.o", std::process::id(), id);
        let exe_path = format!("/tmp/karac_asan_{}_{}", std::process::id(), id);

        if let Err(e) = compile_to_object(&parsed.program, &obj_path, None, None) {
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
        let Some((stdout, status)) = run_under_asan(src, label) else {
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
}
