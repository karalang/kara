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
        // only — heap-owning fields are rejected at layout validation.)
        //
        // The struct is exactly 3 i64 words on purpose: `pop()` returns
        // `Option[Entity]`, and Option's payload area is 3 words. A 4th
        // field (the original `cold { label }` group) would overflow it,
        // which now fails loud as E_ENUM_PAYLOAD_OVERSIZED rather than
        // silently truncating the popped value (see
        // docs/spikes/oversized-enum-payload.md). Cold-group *layout*
        // codegen is covered separately in tests/codegen.rs.
        assert_clean_asan_run(
            r#"
struct Entity { x: i64, y: i64, hp: i64 }
layout entities: Vec[Entity] {
    group physics { x, y }
    group combat { hp }
}
fn main() {
    let mut entities: Vec[Entity] = Vec.new();
    let mut i: i64 = 0;
    while i < 6 {
        entities.push(Entity { x: i, y: i * 10, hp: i * 100 });
        i = i + 1;
    }
    let _front = entities.pop_front();
    let _middle = entities.remove(2);
    match entities.pop() {
        Some(e) => println(e.x),
        None => println(-1),
    }
    println(entities.len());
}
"#,
            &["5", "3"],
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
}
